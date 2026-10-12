import os

# Clean all inherited identities, harness settings and proxies before importing App code.
for key in list(os.environ):
    if key.startswith(
        ("TREER_", "AGENT_UI_", "CODEX_UI_", "EMAIL_ENDPOINT_", "SOUL_")
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
        self.humans = [{"kind":"human","id":"u1","name":"Alice","role":"member"},
                       {"kind":"human","id":"u2","name":"Bob","role":"member"}]
        self.tree = {"agents":[{"agent_id":"app_test","kind":"app","name":"email","folder_id":"f1","path":"/team/email"},
                               {"agent_id":"agent","kind":"command","folder_id":"f1","path":"/team/agent"},
                               {"agent_id":"other-agent","kind":"command","folder_id":"f1","path":"/team/other-agent"}]}

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
            humans = [p | {"role":"owner"} if access_token == "admin" and p["id"] == "u1" else p for p in self.humans]
            return {"principals": humans + [{"kind":"agent", "id":a, "name":a} for a in ("agent","other-agent")]}
        if path.endswith("/oauth/token"):
            if form["code"] != "member-code":
                raise e.mail.AppApiError("workspace_membership_required", "not a member")
            return {"access_token":"human", "expires_at":"2099-01-01T00:00:00Z"}
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
                runtime_id="app_test",
            ),
            self.api,
            tree_loader=lambda: self.api.tree, token_loader=lambda: "endpoint-token",
        )
        self.human = e.mail.BrowserSession(
            "", "human", "ws_test", "svc_test", "u1", "Alice", "member", 9999999999
        )

        self.app.activate(e.mail.BrowserSession("", "admin", "ws_test", "svc_test", "u1", "Alice", "owner", 9999999999), {"enabled":True,"folder_id":"f1"})

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
        recovered = e.Application(self.app.settings, self.api, tree_loader=lambda: self.api.tree, token_loader=lambda: "endpoint-token")
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
        self.app.contacts[0]["email"] = "doomspec@outlook.com"
        self.app.settings.allowlist = []
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
            recovered = e.Application(self.app.settings, self.api, tree_loader=lambda: self.api.tree, token_loader=lambda: "endpoint-token")
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

    def test_activation_live_folder_and_queue_rechecks(self):
        admin = e.mail.BrowserSession("", "admin", "ws_test", "svc_test", "u1", "Alice", "owner", 9999999999)
        self.app.activate(admin, {"enabled":False,"folder_id":"f1"})
        with self.assertRaises(e.Error) as denied:
            self.app.notify("agent", self.request(), "disabled")
        self.assertEqual(denied.exception.code, "endpoint_disabled")
        with self.assertRaises(e.Error):
            self.app.activate(self.human, {"enabled":True,"folder_id":"f1"})
        with self.assertRaises(e.Error):
            self.app.activate(admin, {"enabled":True,"folder_id":"stale"})
        self.app.activate(admin, {"enabled":True,"folder_id":"f1"})
        self.api.tree["agents"][1]["folder_id"] = "f2"
        with self.assertRaises(e.Error) as denied:
            self.app.notify("agent", self.request(), "cross-folder")
        self.assertEqual(denied.exception.code, "folder_denied")
        self.api.tree["agents"][1]["folder_id"] = "f1"
        n = self.app.notify("agent", self.request(), "queued")
        self.api.tree["agents"][0]["folder_id"] = "f2"
        self.app.deliver()
        self.assertEqual(self.app.status(n["id"], "agent")["status"], "failed")
        self.assertFalse((Path(self.tmp.name)/"outbox").exists())
        self.api.tree["agents"][1]["folder_id"] = "f2"
        with self.assertRaises(e.Error) as denied:
            self.app.notify("agent", self.request(), "moved")
        self.assertEqual(denied.exception.code, "endpoint_disabled")
        self.app.activate(admin, {"enabled":True,"folder_id":"f2"})
        self.app.notify("agent", self.request(), "re-enabled")

    def test_contact_nonmember_current_binding_and_real_delivery_gate(self):
        self.app.contacts.append({"id":"external","label":"External", "email":"outside@example.test", "enabled":True})
        contacts = self.app.overview(e.mail.BrowserSession("", "admin", "ws_test", "svc_test", "u1", "Alice", "owner", 9999999999))["contacts"]
        self.assertEqual(contacts[-1]["reply_status"], "reply_unavailable")
        with self.assertRaises(e.Error) as denied:
            self.app.notify("agent", self.request(to="external"), "outside")
        self.assertEqual(denied.exception.code, "reply_unavailable")
        n = self.app.notify("agent", self.request(), "binding")
        self.app.contacts[0]["user_id"] = "u2"
        for action in (lambda: self.app.thread(n["thread_id"], self.human),
                       lambda: self.app.reply(n["thread_id"], self.human,{"body":"denied"},"r")):
            with self.assertRaises(e.Error): action()
        self.app.contacts[0]["user_id"] = "u1"
        self.api.humans = [p for p in self.api.humans if p["id"] != "u1"]
        with self.assertRaises(e.Error): self.app.thread(n["thread_id"], self.human)
        self.app.deliver()
        self.assertEqual(self.app.status(n["id"], "agent")["status"], "failed")
        self.api.humans.append({"kind":"human","id":"u1","name":"Alice","role":"member"})
        self.app.settings.mode = "proxy"
        with self.assertRaises(e.Error) as denied:
            self.app.notify("agent", self.request(), "real-blocked")
        self.assertEqual(denied.exception.code, "recipient_denied")
        self.assertEqual(self.api.email_requests, [])

    def test_pending_intent_cannot_be_retargeted_by_contact_rebinding(self):
        request=self.request(to="member-u1")
        with patch.object(self.app,"core",side_effect=e.mail.AppApiError("app_api_unavailable","offline")):
            with self.assertRaises(e.mail.AppApiError): self.app.notify("agent",request,"pending")
        self.app.contacts[0]["user_id"]="u2"
        with self.assertRaises(e.Error) as denied: self.app.notify("agent",request,"pending")
        self.assertEqual(denied.exception.code,"recipient_changed")
        self.assertFalse(self.api.messages)
        self.app.contacts[0]["user_id"]="u1"
        self.app.notify("agent",request,"pending")
        self.assertEqual(next(iter(self.api.messages.values()))["recipients"],["u1"])

    def test_oauth_browser_binding_replay_expiry_and_nonmember(self):
        def start(browser):
            url = self.app.start_oauth("/", browser)
            return e.urllib.parse.parse_qs(e.urllib.parse.urlsplit(url).query)["state"][0]
        state = start("browser-one")
        for browser in ("", "browser-two"):
            with self.assertRaises(e.Error): self.app.finish_oauth("member-code", state, browser)
        self.assertFalse(any(p.endswith("/oauth/token") for _,p,_,_ in self.api.requests))
        raw, location, age = self.app.finish_oauth("member-code", state, "browser-one")
        self.assertEqual(self.app.authenticated_session(raw).user_id, "u1")
        self.assertEqual(location, "/")
        self.assertGreater(age,0)
        with self.assertRaises(e.Error): self.app.finish_oauth("member-code", state, "browser-one")
        state = start("expired")
        with self.app.db() as db: db.execute("UPDATE app_pending_oauth SET expires_at=0")
        with self.assertRaises(e.Error): self.app.finish_oauth("member-code", state, "expired")
        state = start("nonmember")
        with self.assertRaises(e.mail.AppApiError): self.app.finish_oauth("nonmember-code", state, "nonmember")
        with self.assertRaises(e.Error): self.app.start_oauth("https://evil.test/", "browser")

    def test_legacy_migration_backup_default_disabled_and_state_identity(self):
        import sqlite3
        from dataclasses import replace
        legacy = Path(self.tmp.name)/"legacy.sqlite3"
        with sqlite3.connect(legacy) as db:
            db.execute("CREATE TABLE threads(id TEXT PRIMARY KEY,agent TEXT,recipient TEXT,email TEXT,subject TEXT)")
            db.execute("INSERT INTO threads VALUES('th_legacy','agent','u1','alice@example.test','Old')")
        settings = replace(self.app.settings, config=replace(self.app.config,database_path=legacy))
        app = e.Application(settings,self.api,tree_loader=lambda:self.api.tree,token_loader=lambda:"endpoint-token")
        self.assertFalse(app.activation()["enabled"])
        self.assertEqual(app.thread("th_legacy", self.human)["subject"], "Old")
        backups = list(legacy.parent.glob("legacy.sqlite3.pre-v2-*"))
        self.assertEqual(len(backups),1)
        with sqlite3.connect(backups[0]) as db:
            self.assertNotIn("contact",{r[1] for r in db.execute("PRAGMA table_info(threads)")})
            self.assertEqual(db.execute("SELECT subject FROM threads").fetchone()[0],"Old")
        db.close()
        e.Application(settings,self.api,tree_loader=lambda:self.api.tree)
        self.assertEqual(len(list(legacy.parent.glob("legacy.sqlite3.pre-v2-*"))),1)
        wrong = replace(settings, config=replace(settings.config, service_id="svc_another"))
        with self.assertRaises(ValueError): e.Application(wrong,self.api)

    def test_two_instances_independent_contacts_activation_and_threads(self):
        from dataclasses import replace
        api2=FakeAPI()
        api2.tree["agents"][0]["folder_id"]="f2"
        settings=replace(self.app.settings, config=replace(self.app.config,database_path=Path(self.tmp.name)/"second.sqlite3"),
                         members={},contacts=[{"id":"bob","email":"bob@example.test","user_id":"u2"}])
        app2=e.Application(settings,api2,tree_loader=lambda:api2.tree,token_loader=lambda:"endpoint-token")
        self.assertFalse(app2.activation()["enabled"])
        self.assertTrue(self.app.activation()["enabled"])
        self.assertEqual(app2.contacts[0]["id"],"bob")
        self.assertNotEqual(self.app.contacts[0]["id"],"bob")
        admin=e.mail.BrowserSession("", "admin", "ws_test", "svc_test", "u1", "Alice", "owner", 9999999999)
        app2.activate(admin,{"enabled":True,"folder_id":"f2"})
        with self.assertRaises(e.Error): app2.notify("agent",self.request(to="bob"),"wrong-folder")
        api2.tree["agents"][1]["folder_id"]="f2"
        n=app2.notify("agent",self.request(to="bob"),"second")
        with self.assertRaises(e.Error): self.app.status(n["id"],"agent")
        with self.assertRaises(e.Error): app2.thread(n["thread_id"],self.human)
        with self.assertRaises(ValueError):
            e.Application(replace(settings,contacts=[{"id":"x","email":"a@example.test"},{"id":"y","email":"a@example.test"}]),api2)

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
            verified = self.api.request(
                "POST", "/.treer/apps/identity/verify", body={"token": "human"}
            )
            self.app.sessions.save_session(
                "valid-cookie", "human", verified["claims"], 9999999999
            )
            for token in ("", "forged", "expired", "wrong-audience"):
                self.assertEqual(
                    get(
                        "api/auth/session",
                        {
                            "X-Treer-Identity-Token": token,
                            "Cookie": "treer_email_endpoint_session=valid-cookie",
                        },
                    ).code,
                    401,
                )
            self.assertEqual(
                get(
                    "api/auth/session",
                    {"Cookie": "treer_email_endpoint_session=valid-cookie"},
                ).status,
                200,
            )
            for origin, site in (
                ("null", "same-origin"),
                ("https://evil.test", "same-origin"),
                (self.app.config.public_url.rstrip("/"), "cross-site"),
            ):
                self.assertEqual(
                    get(
                        "v1/threads/" + n["thread_id"],
                        {
                            "X-Treer-Identity-Token": "human",
                            "Origin": origin,
                            "Sec-Fetch-Site": site,
                            "Content-Type": "application/json",
                            "Idempotency-Key": "csrf",
                        },
                        {"body": "denied"},
                    ).code,
                    403,
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
