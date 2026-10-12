import os

# Clean all inherited identities, harness settings and proxies before importing App code.
for key in list(os.environ):
    if key.startswith(
        ("TREER_", "AGENT_UI_", "CODEX_UI_", "EMAIL_ENDPOINT_")
    ) or key.lower() in ("http_proxy", "https_proxy", "all_proxy"):
        del os.environ[key]
import sys
import tempfile
import threading
import unittest
import json
import urllib.request
import urllib.error
from pathlib import Path
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import email_endpoint as e


class FakeAPI:
    def __init__(self):
        self.messages = {}
        self.requests = []
        self.fail = None
        self.email_requests = []

    def request(self, method, path, access_token=None, body=None, form=None):
        self.requests.append((method, path, access_token, body))
        if path.endswith("identity/verify"):
            token = body["token"]
            if token not in ("agent", "other-agent", "human", "other-human", "admin"):
                return {"active": False}
            human = token in ("human", "other-human", "admin")
            return {
                "active": True,
                "claims": {
                    "sub": "u1"
                    if token in ("human", "admin")
                    else "u2"
                    if token == "other-human"
                    else token,
                    "principal_kind": "human" if human else "agent",
                    "service_id": "svc_test",
                    "workspace_id": "ws_test",
                    "name": token,
                    "role": "owner" if token == "admin" else "member",
                    "exp": 9999999999,
                },
            }
        if path.endswith("/directory"):
            return {
                "principals": [
                    {"kind": "human", "id": "u1", "name": "Alice"},
                    {"kind": "human", "id": "u2", "name": "Bob"},
                ]
            }
        if path.endswith("/email"):
            self.email_requests.append(body)
            if self.fail:
                raise e.mail.AppApiError(self.fail, "fake failure")
            return {"status": "sent", "email_id": "proxy1"}
        if method == "POST":
            for m in self.messages.values():
                if m["key"] == body["idempotency_key"]:
                    return {"message": m}
            mid = "msg_" + str(len(self.messages) + 1)
            m = {
                "message_id": mid,
                "body": body["body"],
                "sender": {
                    "kind": "human" if access_token == "human" else "agent",
                    "id": access_token,
                    "name": access_token,
                },
                "recipients": body["recipients"],
                "context_ids": body["context_ids"],
                "created_at": "2026-10-12T00:00:00Z",
                "key": body["idempotency_key"],
            }
            self.messages[mid] = m
            return {"message": m}
        return {"message": self.messages[path.rsplit("/", 1)[1]]}


class Tests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        c = e.mail.Config(
            "127.0.0.1",
            0,
            "svc_test",
            "http://127.0.0.1/",
            "http://fake.invalid/",
            e.ROOT / "web",
            Path(self.tmp.name) / "state.sqlite3",
        )
        self.api = FakeAPI()
        self.app = e.Application(
            e.Settings(
                c,
                "ws_test",
                {"u1": "alice@example.test", "u2": "bob@example.test"},
                ["@example.test"],
                [],
                retry_seconds=0.01,
                max_attempts=2,
            ),
            self.api,
        )
        self.human = e.mail.BrowserSession(
            "", "human", "ws_test", "svc_test", "u1", "Alice", "member", 9999999999
        )

    def tearDown(self):
        self.tmp.cleanup()

    def request(self, **extra):
        return {
            "to": "Alice",
            "subject": "Please review",
            "body": "sensitive private full body <script>bad()</script>",
            **extra,
        }

    def test_identity_and_unknown_fields(self):
        for token in ("", "forged", "human"):
            with self.assertRaises(e.Error):
                self.app.notify(token, self.request(), "key")
        with self.assertRaises(e.Error):
            self.app.notify("agent", self.request(sender="other-agent"), "key")

    def test_recipients_and_both_rates(self):
        for to in ("someone@evil.test", "Missing", "unknown@example.test"):
            with self.assertRaises(e.Error):
                self.app.notify("agent", self.request(to=to), "key")
        self.app.settings.allowlist = ["@other.test"]
        with self.assertRaises(e.Error):
            self.app.notify("agent", self.request(), "key")
        self.app.settings.allowlist = []
        self.app.settings.agent_limit = 1
        self.app.notify("agent", self.request(), "a")
        with self.assertRaises(e.Error) as c:
            self.app.notify("agent", self.request(), "b")
        self.assertEqual(c.exception.status, 429)
        self.app.settings.recipient_limit = 1
        with self.assertRaises(e.Error):
            self.app.notify("other-agent", self.request(), "b")

    def test_mapping_append_idempotency_and_no_body_in_email(self):
        n = self.app.notify("agent", self.request(), "a")
        self.assertEqual(self.app.notify("agent", self.request(), "a"), n)
        self.app.deliver()
        email = (Path(self.tmp.name) / "outbox" / (n["id"] + ".eml")).read_text()
        self.assertNotIn("sensitive private", email)
        self.assertIn(n["thread_id"], email)
        r = self.app.reply(n["thread_id"], self.human, {"body": "reply"}, "b")
        self.assertEqual(r["wake"], "disabled")
        second = self.app.notify("agent", self.request(thread_id=n["thread_id"]), "c")
        self.assertEqual(second["thread_id"], n["thread_id"])
        m = self.api.messages[second["message"]]
        self.assertEqual(m["context_ids"], [r["message_id"]])
        self.assertEqual(
            len(self.app.thread(n["thread_id"], self.human)["messages"]), 3
        )
        self.assertEqual(self.api.messages[r["message_id"]]["recipients"], ["agent"])
        self.assertEqual(
            self.app.reply(n["thread_id"], self.human, {"body": "reply"}, "b"), r
        )
        with self.assertRaises(e.Error):
            self.app.notify("agent", self.request(body="changed"), "a")
        with self.assertRaises(e.Error):
            self.app.notify("other-agent", self.request(thread_id=n["thread_id"]), "d")

    def test_core_response_loss_recovers_fixed_context_without_duplicate(self):
        original = self.app.core

        def lose_response(*args, **kwargs):
            original(*args, **kwargs)
            raise e.mail.AppApiError("app_api_unavailable", "lost response")

        with patch.object(self.app, "core", side_effect=lose_response):
            with self.assertRaises(e.mail.AppApiError):
                self.app.notify("agent", self.request(), "lost")
        with self.app.db() as db:
            n = dict(db.execute("SELECT * FROM notifications").fetchone())
        self.assertTrue(self.app.notification_result(n)["core_pending"])
        recovered = e.Application(self.app.settings, self.api)
        result = recovered.notify("agent", self.request(), "lost")
        self.assertFalse(result["core_pending"])
        self.assertEqual(len(self.api.messages), 1)
        self.assertEqual(
            len(recovered.thread(result["thread_id"], self.human)["messages"]), 1
        )

    def test_thread_access_denied(self):
        n = self.app.notify("agent", self.request(), "a")
        other = e.mail.BrowserSession(
            "", "other-human", "ws_test", "svc_test", "u2", "Bob", "owner", 9999999999
        )
        with self.assertRaises(e.Error):
            self.app.thread(n["thread_id"], other)
        with self.assertRaises(e.Error):
            self.app.reply(n["thread_id"], other, {"body": "stolen"}, "k")

    def test_proxy_fallback_retry_failed_and_restart(self):
        self.app.settings.mode = "proxy"
        with patch("email_endpoint.subprocess.run") as run:
            run.return_value.stdout = "app-token\n"
            self.api.fail = "email_not_configured"
            n = self.app.notify("agent", self.request(), "a")
            self.app.deliver()
            status = self.app.status(n["id"], "agent")
            self.assertEqual(status["transport"], "outbox")
            self.assertIn("no email was sent", status["error"])
            self.api.fail = "email_delivery_failed"
            n = self.app.notify("agent", self.request(), "b")
            self.app.deliver()
            self.assertEqual(self.app.status(n["id"], "agent")["status"], "queued")
            with self.app.db() as db:
                db.execute("UPDATE notifications SET next_try=0")
            recovered = e.Application(self.app.settings, self.api)
            recovered.deliver()
            self.assertEqual(recovered.status(n["id"], "agent")["status"], "failed")
            self.assertNotIn("sensitive", json.dumps(self.api.email_requests))
            self.api.fail = None
            n = self.app.notify("agent", self.request(), "c")
            self.app.deliver()
            self.assertEqual(self.app.status(n["id"], "agent")["transport"], "proxy")

    def test_wake_degrades_without_body(self):
        self.app.settings.wake_agents = ["agent"]
        n = self.app.notify("agent", self.request(), "a")
        with patch("email_endpoint.subprocess.run") as run:
            run.return_value.returncode = 1
            r = self.app.reply(
                n["thread_id"], self.human, {"body": "SECRET REPLY"}, "k"
            )
            self.assertEqual(r["wake"], "denied_or_unavailable_message_delivered")
            self.assertNotIn("SECRET", str(run.call_args))
            self.assertIn(r["message_id"], str(run.call_args))

    def test_http_negotiation_header_auth_csrf_and_denied_page(self):
        server = e.Server(("127.0.0.1", 0), self.app)
        port = server.server_address[1]
        self.app.config = e.mail.Config(
            "127.0.0.1",
            port,
            "svc_test",
            f"http://127.0.0.1:{port}/",
            "http://fake.invalid/",
            e.ROOT / "web",
            Path(self.tmp.name) / "state.sqlite3",
        )
        threading.Thread(target=server.serve_forever, daemon=True).start()

        def get(path, headers={}, body=None):
            req = urllib.request.Request(
                self.app.config.public_url + path,
                headers=headers,
                data=json.dumps(body).encode() if body else None,
            )
            try:
                return urllib.request.urlopen(req)
            except urllib.error.HTTPError as exc:
                return exc

        try:
            self.assertIn("text/markdown", get("").headers["Content-Type"])
            self.assertIn(
                "text/html", get("", {"Accept": "text/html"}).headers["Content-Type"]
            )
            n = self.app.notify("agent", self.request(), "a")
            path = "t/" + n["thread_id"] + "/"
            self.assertEqual(get(path, {"X-Treer-Identity-Token": "human"}).status, 200)
            self.assertEqual(
                get(path, {"X-Treer-Identity-Token": "other-human"}).code, 403
            )
            self.assertEqual(
                get(
                    "v1/threads/" + n["thread_id"], {"X-Treer-Identity-Token": "human"}
                ).status,
                200,
            )
            self.assertEqual(
                get(
                    "v1/threads/" + n["thread_id"],
                    {
                        "X-Treer-Identity-Token": "human",
                        "Content-Type": "application/json",
                        "Idempotency-Key": "r",
                    },
                    {"body": "a"},
                ).code,
                403,
            )
            self.assertEqual(
                get(
                    "v1/notify",
                    {
                        "Content-Type": "application/json",
                        "Idempotency-Key": "fake",
                        "X-Treer-Agent-Id": "agent",
                    },
                    self.request(),
                ).code,
                401,
            )
            self.assertEqual(get("unknown").code, 404)
            self.assertEqual(
                get("v1/admin/outbox", {"X-Treer-Identity-Token": "human"}).code, 403
            )
            self.assertEqual(
                get("v1/admin/outbox", {"X-Treer-Identity-Token": "admin"}).status, 200
            )
        finally:
            server.shutdown()
            server.server_close()


if __name__ == "__main__":
    unittest.main()
