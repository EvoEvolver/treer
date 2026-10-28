from __future__ import annotations

import json
import sys
import tempfile
import threading
import unittest
import urllib.parse
from pathlib import Path

from test_mail import APP_ROOT, FakeProxy, FakeProxyHandler, MailProcessCase, core_message

sys.path.insert(0, str(APP_ROOT))
import mail  # noqa: E402
from email_route import EmailRoute  # noqa: E402
from notify import Notifier, NotifyError  # noqa: E402

CAPABILITY = "app.email.v1"


class EmailProxy(FakeProxy):
    """FakeProxy plus a fake Core App email route that behaves like Core:
    the same idempotency key with the same content never sends twice."""

    def __init__(self) -> None:
        super().__init__()
        self.RequestHandlerClass = EmailProxyHandler
        self.capabilities = [CAPABILITY]
        self.email_requests: list[dict] = []
        self.sent: dict[str, dict] = {}
        self.script: list[tuple[int, dict]] = []  # forced responses, consumed in order
        self.lookup_supported = True


class EmailProxyHandler(FakeProxyHandler):
    server: EmailProxy

    def do_GET(self) -> None:  # noqa: N802
        path = urllib.parse.urlsplit(self.path).path
        if path == "/api/health":
            return self._send(200, {"status": "ok", "capabilities": self.server.capabilities})
        if path.startswith("/api/apps/svc_mail/email/"):
            if not self.server.lookup_supported:
                return self._send(404, {"error": {"code": "route_not_found", "message": "no"}})
            key = urllib.parse.unquote(path.rsplit("/", 1)[1])
            if key in self.server.sent:
                return self._send(200, {"status": "sent"})
            return self._send(404, {"error": {"code": "email_not_found", "message": "unknown key"}})
        super().do_GET()

    def do_POST(self) -> None:  # noqa: N802
        path = urllib.parse.urlsplit(self.path).path
        if path == "/api/apps/svc_mail/email":
            body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            assert self.headers.get("Authorization") == "Bearer runtime-token"
            assert set(body) == {"to", "subject", "text", "idempotency_key"}
            self.server.email_requests.append(body)
            if self.server.script:
                return self._send(*self.server.script.pop(0))
            previous = self.server.sent.get(body["idempotency_key"])
            if previous and previous != body:
                return self._send(400, {"error": {"code": "email_idempotency_conflict", "message": "x"}})
            self.server.sent[body["idempotency_key"]] = body
            return self._send(200, {"email_id": "email_" + body["idempotency_key"][-8:], "status": "sent"})
        if path == "/.treer/apps/identity/verify":
            raw = self.rfile.read(int(self.headers["Content-Length"]))
            if json.loads(raw).get("token") == "agent-token":
                return self._send(200, {"active": True, "claims": {
                    "workspace_id": "workspace-a", "service_id": "svc_mail", "principal_kind": "agent",
                    "sub": "agent-a", "name": "Builder", "exp": 4070908800}})
            # Re-feed the consumed body to the base handler.
            self.rfile = __import__("io").BytesIO(raw)
        super().do_POST()

    def _send(self, status: int, value: dict) -> None:
        body = json.dumps(value).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


def deliver(proxy: FakeProxy, message_id: str, *, recipients=None, body="Ready for review\nsecret second line") -> None:
    message = core_message(message_id)
    message["body"] = body
    if recipients is not None:
        message["recipients"] = recipients
    proxy.messages.append(message)
    proxy.deliveries.append({"delivery_id": "dlv_" + message_id, "message_id": message_id, "acked": False})


def read(proxy: FakeProxy, message_id: str) -> None:
    for delivery in proxy.deliveries:
        if delivery["message_id"] == message_id:
            delivery["acked"] = True


class NotifierTest(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.proxy = EmailProxy()
        threading.Thread(target=self.proxy.serve_forever, daemon=True).start()
        proxy_url = f"http://127.0.0.1:{self.proxy.server_address[1]}/"
        db = Path(self.temporary.name) / "state" / "mail-state.sqlite3"
        config = mail.Config("127.0.0.1", 0, "svc_mail", "https://mail.example/", proxy_url,
                             Path(self.temporary.name), db)
        sessions = mail.SessionStore(db)
        sessions.save_session("mas_raw", "app-access-token",
                              {"workspace_id": "workspace-a", "service_id": "svc_mail", "sub": "user-a",
                               "name": "Owner", "role": "owner"}, 4070908800)
        app = mail.MailApplication(config, mail.TreerAppClient(proxy_url, "svc_mail"), sessions)
        self.now = 1_000_000.0
        self.route = EmailRoute(proxy_url, "svc_mail", CAPABILITY, lambda: "runtime-token")
        self.notifier = Notifier(db, self.route, public_url="https://mail.example/", preview_chars=12,
                                 unread=app.unread_messages, directory_email=app.directory_email,
                                 max_attempts=3, retry_seconds=60, clock=lambda: self.now)

    def tearDown(self) -> None:
        self.proxy.shutdown()
        self.proxy.server_close()
        self.temporary.cleanup()

    def enable(self, **settings) -> None:
        self.notifier.update("user-a", {"enabled": True, "address": "owner@example.test", **settings},
                             by_agent=False)
        self.notifier.tick()  # Baseline: msg_inbound was unread before enabling.

    def advance(self, seconds: float) -> None:
        self.now += seconds
        self.notifier.tick()

    def test_sends_one_short_email_for_an_unread_message(self) -> None:
        self.enable()
        self.assertEqual(self.proxy.email_requests, [])  # Existing unread Messages are not emailed.
        deliver(self.proxy, "msg_new")
        self.advance(1)
        self.assertEqual(len(self.proxy.email_requests), 1)
        email = self.proxy.email_requests[0]
        self.assertEqual(email["to"], "owner@example.test")
        self.assertEqual(email["subject"], "New Treer message from Builder")
        self.assertIn("Builder: Ready for r…", email["text"])
        self.assertIn("https://mail.example/?message=msg_new", email["text"])
        self.assertNotIn("secret", email["text"])
        self.assertTrue(email["idempotency_key"].startswith("mail-notify-"))
        self.advance(60)
        self.assertEqual(len(self.proxy.email_requests), 1)  # Still unread, but already notified.
        status = self.notifier.status("user-a")
        self.assertEqual(status["watch"]["state"], "watching")
        self.assertEqual(status["batches"][0]["status"], "sent")
        self.assertEqual(status["batches"][0]["attempts"][0]["result"], "sent")

    def test_no_email_when_read_within_the_window(self) -> None:
        self.enable(digest_minutes=10)
        deliver(self.proxy, "msg_new")
        self.advance(60)
        read(self.proxy, "msg_new")
        self.advance(600)
        self.advance(600)
        self.assertEqual(self.proxy.email_requests, [])
        self.assertEqual(self.notifier.status("user-a")["pending_messages"], 0)

    def test_digest_batches_messages_into_one_email(self) -> None:
        self.enable(digest_minutes=10)
        deliver(self.proxy, "msg_one")
        self.advance(1)
        deliver(self.proxy, "msg_two")
        self.advance(120)
        self.assertEqual(self.proxy.email_requests, [])
        self.advance(480)
        self.assertEqual(len(self.proxy.email_requests), 1)
        email = self.proxy.email_requests[0]
        self.assertEqual(email["subject"], "2 unread Treer messages")
        self.assertIn("?message=msg_one", email["text"])
        self.assertIn("?message=msg_two", email["text"])
        self.assertEqual(self.notifier.status("user-a")["batches"][0]["message_ids"], ["msg_one", "msg_two"])

    def test_retries_are_identical_requests_under_one_idempotency_key(self) -> None:
        self.enable()
        self.proxy.script = [(503, {"error": {"code": "email_not_configured", "message": "x"}}),
                             (429, {"error": {"code": "email_rate_limited", "message": "x"}})]
        deliver(self.proxy, "msg_new")
        self.advance(1)
        self.advance(59)
        self.assertEqual(len(self.proxy.email_requests), 1)  # Backoff not yet elapsed.
        self.advance(1)
        self.advance(120)
        self.assertEqual(len(self.proxy.email_requests), 3)
        self.assertEqual(len({json.dumps(r, sort_keys=True) for r in self.proxy.email_requests}), 1)
        batch = self.notifier.status("user-a")["batches"][0]
        self.assertEqual(batch["status"], "sent")
        self.assertEqual([a["result"] for a in batch["attempts"]], ["retryable", "retryable", "sent"])
        # Asking again for the same test key never sends a second email.
        first = self.notifier.test("user-a", "click-1")
        again = self.notifier.test("user-a", "click-1")
        self.assertEqual(first["batch_id"], again["batch_id"])
        self.assertEqual(len(self.proxy.email_requests), 4)

    def test_permanent_errors_fail_at_once_and_retryable_ones_are_capped(self) -> None:
        self.enable()
        self.proxy.script = [(400, {"error": {"code": "email_invalid", "message": "x"}})]
        deliver(self.proxy, "msg_bad")
        self.advance(1)
        self.advance(3600)
        batch = self.notifier.status("user-a")["batches"][0]
        self.assertEqual((batch["status"], batch["last_code"], len(batch["attempts"])), ("failed", "email_invalid", 1))
        self.proxy.script = [(429, {"error": {"code": "email_rate_limited", "message": "x"}})] * 5
        deliver(self.proxy, "msg_busy")
        for _ in range(6):
            self.advance(3600)
        batch = self.notifier.status("user-a")["batches"][0]
        self.assertEqual((batch["status"], len(batch["attempts"])), ("failed", 3))
        self.assertEqual(len(self.proxy.email_requests), 4)
        # An explicit retry resends the identical request.
        self.proxy.script = []
        self.assertEqual(self.notifier.retry("user-a", batch["batch_id"])["status"], "sent")

    def test_uncertain_outcome_is_checked_before_any_resend(self) -> None:
        self.enable()
        self.proxy.script = [(502, {"error": {"code": "email_delivery_failed", "message": "x"}})]
        deliver(self.proxy, "msg_new")
        self.advance(1)
        self.assertEqual(self.notifier.status("user-a")["batches"][0]["status"], "uncertain")
        self.proxy.lookup_supported = False
        self.advance(60)
        self.advance(600)
        self.assertEqual(len(self.proxy.email_requests), 1)  # No check available: never resent.
        self.assertEqual(self.notifier.status("user-a")["batches"][0]["status"], "uncertain")
        self.proxy.lookup_supported = True
        self.advance(3600)  # Core says the key was never sent, so it is safe to send again.
        self.assertEqual(len(self.proxy.email_requests), 2)
        self.assertEqual(self.notifier.status("user-a")["batches"][0]["status"], "sent")

    def test_uncertain_outcome_confirmed_sent_is_not_resent(self) -> None:
        self.enable()
        deliver(self.proxy, "msg_new")
        original = self.route.send
        def lost_response(to, subject, text, key):
            original(to, subject, text, key)
            return __import__("email_route").Outcome("uncertain", "email_route_no_response")
        self.route.send = lost_response
        self.advance(1)
        self.route.send = original
        self.advance(60)
        self.assertEqual(len(self.proxy.email_requests), 1)
        batch = self.notifier.status("user-a")["batches"][0]
        self.assertEqual((batch["status"], batch["last_code"]), ("sent", "confirmed_sent"))

    def test_direct_scope_skips_group_messages(self) -> None:
        self.enable()
        group = [{"kind": "human", "id": "user-a", "name": "Owner"}, {"kind": "human", "id": "user-b", "name": "Reviewer"}]
        deliver(self.proxy, "msg_group", recipients=group)
        self.advance(1)
        self.assertEqual(self.proxy.email_requests, [])
        self.notifier.update("user-a", {"scope": "all"}, by_agent=False)
        self.advance(1)
        self.assertEqual(len(self.proxy.email_requests), 1)

    def test_capability_absent_reports_unavailable(self) -> None:
        self.proxy.capabilities = []
        self.enable()
        deliver(self.proxy, "msg_new")
        self.advance(1)
        status = self.notifier.status("user-a")
        self.assertFalse(status["available"])
        self.assertEqual(status["unavailable_reason"], "core_email_route_absent")
        self.assertEqual(status["watch"]["state"], "unavailable")
        with self.assertRaises(NotifyError) as raised:
            self.notifier.test("user-a", None)
        self.assertEqual(raised.exception.code, "notify_unavailable")
        self.assertEqual(self.proxy.email_requests, [])

    def test_agents_cannot_redirect_email_and_disabling_cancels(self) -> None:
        with self.assertRaises(NotifyError):
            self.notifier.update("user-a", {"address": "attacker@example.test"}, by_agent=True)
        self.enable(digest_minutes=5)
        deliver(self.proxy, "msg_new")
        self.advance(1)
        self.notifier.update("user-a", {"enabled": False}, by_agent=True)
        self.advance(600)
        self.assertEqual(self.proxy.email_requests, [])


FAKE_TREER = Path(tempfile.mkdtemp(prefix="mail-fake-treer-")) / "treer"
FAKE_TREER.write_text(f"#!{sys.executable}\nprint('runtime-token')\n", encoding="utf-8")
FAKE_TREER.chmod(0o755)


class NotificationApiTest(MailProcessCase):
    """The HTTP API the UI and Agents share, against a running mail.py."""

    proxy_class = EmailProxy
    extra_config = {"treer_bin": str(FAKE_TREER), "email_preview_chars": 20}

    def test_settings_status_test_send_and_agent_access(self) -> None:
        status, _, _ = self.request("GET", "/api/notifications")
        self.assertEqual(status, 401)
        self.login()
        status, _, value = self.request("GET", "/api/notifications")
        self.assertEqual(status, 200)
        self.assertTrue(value["available"])
        self.assertEqual(value["watch"]["state"], "disabled")
        self.assertEqual(value["preview_chars"], 20)
        status, _, value = self.request("PATCH", "/api/notifications/settings",
                                        {"enabled": True, "address": "owner@example.test", "digest_minutes": 15})
        self.assertEqual(status, 200)
        self.assertEqual(value["settings"]["digest_minutes"], 15)
        self.assertEqual(value["watch"]["state"], "watching")
        status, _, value = self.request("PATCH", "/api/notifications/settings", {"scope": "everything"})
        self.assertEqual((status, value["error"]["code"]), (400, "notify_field_invalid"))
        status, _, value = self.request("POST", "/api/notifications/test", {})
        self.assertEqual(status, 200)
        self.assertEqual(value["batch"]["status"], "sent")
        self.assertEqual(self.proxy.email_requests[-1]["to"], "owner@example.test")

        agent = {"X-Treer-Identity-Token": "agent-token", "Cookie": ""}
        status, _, value = self.request("GET", "/api/notifications/members", request_headers=agent)
        self.assertEqual(status, 200)
        self.assertEqual([m["user_id"] for m in value["members"]], ["user-a"])
        status, _, value = self.agent_request("PATCH", "/api/notifications/settings?user_id=user-a",
                                              {"address": "attacker@example.test"})
        self.assertEqual((status, value["error"]["code"]), (403, "notify_address_human_only"))
        status, _, value = self.agent_request("PATCH", "/api/notifications/settings?user_id=user-a",
                                              {"digest_minutes": 0})
        self.assertEqual((status, value["settings"]["digest_minutes"]), (200, 0))
        status, _, value = self.agent_request("POST", "/api/notifications/test?user_id=user-a", {})
        self.assertEqual((status, value["batch"]["status"]), (200, "sent"))

        status, _, value = self.agent_request("POST", "/api/messages", {"recipients": ["user-a"], "body": "x"})
        self.assertEqual(status, 403)  # Agents send Messages with the treer CLI, not the browser API.

    def agent_request(self, method, path, body):
        import http.client
        connection = http.client.HTTPConnection("127.0.0.1", self.port, timeout=5)
        connection.request(method, path, body=json.dumps(body),
                           headers={"X-Treer-Identity-Token": "agent-token", "Content-Type": "application/json"})
        response = connection.getresponse()
        value = json.loads(response.read())
        connection.close()
        return response.status, None, value


if __name__ == "__main__":
    unittest.main()
