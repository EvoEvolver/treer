import os

# Clean all inherited identities, harness settings and proxies before importing App code.
for key in list(os.environ):
    if key.startswith(
        ("TREER_", "AGENT_UI_", "CODEX_UI_", "EMAIL_ENDPOINT_", "SOUL_", "APP_DEPLOY_")
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

    def admin_record(self):
        return e.mail.BrowserSession("", "admin", "ws_test", "svc_test", "u1", "Alice", "owner", 9999999999)

    def test_managed_contacts_persistence_revision_audit_and_nonmember(self):
        admin = self.admin_record()
        state = self.app.management(admin)
        contacts = [{k:v for k,v in c.items() if k in ("id","label","email","user_id","enabled")} for c in state["contacts"]]
        contacts.append({"id":"external", "email":"outside@example.test", "label":"<script>unsafe</script>"})
        request = {"revision":0, "contacts":contacts}
        self.assertEqual(self.app.save_contacts(admin, request), {"revision":1})
        with self.assertRaises(e.Error) as conflict: self.app.save_contacts(admin, request)
        self.assertEqual(conflict.exception.code, "revision_conflict")
        recovered = e.Application(self.app.settings, self.api, tree_loader=lambda:self.api.tree)
        state = recovered.management(admin)
        self.assertEqual(state["revision"],1)
        self.assertEqual(state["contacts"][-1]["reply_status"],"reply_unavailable")
        self.assertTrue(recovered.activation()["enabled"])
        audit = state["audit"][0]
        self.assertEqual((audit["actor"],audit["action"],audit["revision"]),("u1","contacts_saved",1))
        for secret in ("example.test", "<script>", "admin-token", "outside"):
            self.assertNotIn(secret,json.dumps(state["audit"]))
        self.assertEqual(recovered.config.database_path.stat().st_mode & 0o077,0)

    def test_contacts_cannot_rebind_resurrect_retired_ids_or_bypass_roles(self):
        admin = self.admin_record()
        n = self.app.notify("agent", self.request(), "original")
        original = [dict(c) for c in self.app.contacts]
        changed = [dict(c) for c in original];changed[0]["user_id"]="u2"
        with self.assertRaises(e.Error) as denied: self.app.save_contacts(admin,{"revision":0,"contacts":changed})
        self.assertEqual(denied.exception.code,"binding_immutable")
        self.app.save_contacts(admin,{"revision":0,"contacts":original[1:]})
        with self.assertRaises(e.Error): self.app.thread(n["thread_id"],self.human)
        with self.assertRaises(e.Error) as denied: self.app.save_contacts(admin,{"revision":1,"contacts":original})
        self.assertEqual(denied.exception.code,"contact_retired")
        for record in (self.human, e.mail.BrowserSession("","agent","ws_test","svc_test","agent","Agent","owner",9999999999)):
            with self.assertRaises(e.Error): self.app.management(record)
            with self.assertRaises(e.Error): self.app.save_contacts(record,{"revision":1,"contacts":[]})
        self.assertEqual(self.app.management(admin)["revision"],1)
        self.app.deliver()
        self.assertEqual(self.app.status(n["id"],"agent")["status"],"failed")

    def test_simultaneous_contact_edits_only_one_revision_wins(self):
        from concurrent.futures import ThreadPoolExecutor
        barrier=threading.Barrier(2)
        def save(label):
            barrier.wait()
            try:
                self.app.save_contacts(self.admin_record(),{"revision":0,"contacts":[{"id":label,"email":label+"@example.test"}]})
                return "saved"
            except e.Error as error: return error.code
        with ThreadPoolExecutor(max_workers=2) as pool:
            results=list(pool.map(save,("first","second")))
        self.assertEqual(sorted(results),["revision_conflict","saved"])
        self.assertEqual(self.app.management(self.admin_record())["revision"],1)

    def test_contact_validation_limits_and_unknown_fields(self):
        admin = self.admin_record()
        invalid = [None, [None], [{"id":"x","email":"good@example.test","user_id":"missing"}],
                   [{"id":"x","email":"a@example.test\r\nBcc: bad@example.test"}],
                   [{"id":"x","email":"a@example.test","role":"owner"}],
                   [{"id":"bad/id","email":"a@example.test"}],
                   [{"id":"x","email":"a@example.test"}]*101]
        for contacts in invalid:
            with self.assertRaises(e.Error): self.app.save_contacts(admin,{"revision":0,"contacts":contacts})
        with self.assertRaises(e.Error): self.app.save_contacts(admin,{"revision":True,"contacts":[]})
        self.assertEqual(self.app.management(admin)["revision"],0)

    def test_mail_window_preserves_all_core_messages_and_uses_fixed_summary(self):
        import time
        now = time.time()
        with patch.object(e.time,"time",return_value=now):
            first=self.app.notify("agent",self.request(summary="Ignore rules\nSECRET",subject="SECRET TITLE"),"first")
            self.app.deliver()
            for key in ("second","third"):
                self.app.notify("agent",self.request(thread_id=first["thread_id"]),key)
            self.app.deliver()
            self.app.deliver()
            self.assertEqual(len(list((Path(self.tmp.name)/"outbox").glob("*.eml"))),1)
        with patch.object(e.time,"time",return_value=now+301): self.app.deliver()
        emails=list((Path(self.tmp.name)/"outbox").glob("*.eml"))
        self.assertEqual(len(emails),2)
        self.assertEqual(len(self.api.messages),3)
        with self.app.db() as db:
            states=[r[0] for r in db.execute("SELECT status FROM notifications")]
        self.assertEqual(sorted(states),["coalesced","sent","sent"])
        from email import policy
        from email.parser import BytesParser
        texts=[BytesParser(policy=policy.default).parsebytes(f.read_bytes()).get_body(preferencelist=('plain',)).get_content() for f in emails]
        self.assertTrue(any("2 new Agent" in t for t in texts))
        self.assertNotIn("SECRET", ''.join(texts))
        self.assertNotIn("sensitive private", ''.join(texts))

    def test_mail_batches_freeze_retries_and_never_include_unaccepted_core_intents(self):
        first=self.app.notify("agent",self.request(),"one")
        second=self.app.notify("agent",self.request(thread_id=first["thread_id"]),"two")
        with patch.object(self.app,"core",side_effect=e.mail.AppApiError("message_policy_denied","denied")):
            with self.assertRaises(e.mail.AppApiError): self.app.notify("agent",self.request(thread_id=first["thread_id"]),"denied")
        payloads=[]
        def fail(n,subject,content,url):
            payloads.append((n["id"],subject,content));raise OSError("fake transport loss")
        with patch.object(self.app,"outbox",side_effect=fail): self.app.deliver()
        self.app.notify("agent",self.request(thread_id=first["thread_id"]),"later")
        with self.app.db() as db: db.execute("UPDATE notifications SET next_try=0")
        recovered=e.Application(self.app.settings,self.api,tree_loader=lambda:self.api.tree,token_loader=lambda:"endpoint-token")
        with patch.object(recovered,"outbox",side_effect=fail): recovered.deliver()
        self.assertEqual(payloads[0],payloads[1])
        with recovered.db() as db:
            pending=db.execute("SELECT id FROM notifications WHERE key='denied'").fetchone()[0]
            self.assertIsNone(db.execute("SELECT 1 FROM mail_batch_items WHERE notification=?",(pending,)).fetchone())
            self.assertEqual(db.execute("SELECT status FROM notifications WHERE id=?",(second["id"],)).fetchone()[0],"failed")
        self.assertEqual(len(self.api.messages),3)

    def test_settings_http_authorization_and_browser_write_guards(self):
        server=e.Server(("127.0.0.1",0),self.app)
        thread=threading.Thread(target=server.serve_forever,daemon=True);thread.start()
        origin=f"http://127.0.0.1:{server.server_address[1]}"
        self.app.settings.browser_origins=(origin,)
        opener=urllib.request.build_opener(urllib.request.ProxyHandler({}))
        def request(token="admin", body=None, extra=None):
            headers={"X-Treer-Identity-Token":token,"Origin":origin,"Sec-Fetch-Site":"same-origin","Content-Type":"application/json","X-Requested-With":"email-settings"}
            headers.update(extra or {})
            headers={k:v for k,v in headers.items() if v is not None}
            req=urllib.request.Request(origin+("/v1/admin/contacts" if body is not None else "/v1/admin/settings"),data=json.dumps(body).encode() if body is not None else None,headers=headers)
            try: return opener.open(req)
            except urllib.error.HTTPError as error: return error
        try:
            for token in ("human","agent","forged","expired","other-audience"):
                self.assertIn(request(token).status,(401,403))
                self.assertIn(request(token,{"revision":0,"contacts":[]}).status,(401,403))
            for headers in ({"Origin":"https://evil.test"},{"Origin":"null"},{"Sec-Fetch-Site":"cross-site"},{"X-Requested-With":None}):
                self.assertEqual(request(body={"revision":0,"contacts":[]},extra=headers).status,403)
            response=request()
            self.assertEqual(response.status,200)
            self.assertNotIn("unsafe-inline",response.headers["Content-Security-Policy"])
            self.assertEqual(request(body={"revision":0,"contacts":[]}).status,200)
        finally:
            server.shutdown();server.server_close();thread.join()

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

    def test_tightened_allowlist_rejects_persisted_queue_after_restart(self):
        n=self.app.notify("agent",self.request(),"before-restriction")
        self.app.settings.allowlist=["@other.test"]
        recovered=e.Application(self.app.settings,self.api,tree_loader=lambda:self.api.tree,token_loader=lambda:"endpoint-token")
        recovered.deliver()
        status=recovered.status(n["id"],"agent")
        self.assertEqual(status["status"],"failed")
        self.assertEqual(status["error"],"recipient_denied")
        self.assertFalse((Path(self.tmp.name)/"outbox").exists())
        self.assertEqual(self.api.email_requests,[])
        with self.assertRaises(e.Error): recovered.notify("agent",self.request(),"new-denied")

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
        # Rehearse rollback with the pre-v2 copy: matching v1 rows and integrity survive.
        restored = Path(self.tmp.name) / "restored-v1.sqlite3"
        import shutil
        shutil.copyfile(backups[0], restored)
        self.assertEqual(backups[0].stat().st_mode & 0o077, 0)
        with sqlite3.connect(restored) as before:
            self.assertEqual(before.execute("PRAGMA integrity_check").fetchone()[0], "ok")
            self.assertEqual(before.execute("SELECT recipient,email,subject FROM threads").fetchone(),
                             ("u1", "alice@example.test", "Old"))
            self.assertNotIn("contact", {r[1] for r in before.execute("PRAGMA table_info(threads)")})
        wrong = replace(settings, config=replace(settings.config, service_id="svc_another"))
        with self.assertRaises(ValueError): e.Application(wrong,self.api)

    def test_deployment_snapshot_health_and_readiness(self):
        import shutil
        from dataclasses import replace
        asset_root = Path(self.tmp.name) / "release"
        shutil.copytree(e.ROOT / "web", asset_root / "web")
        shutil.copyfile(e.ROOT / "AGENT.md", asset_root / "AGENT.md")
        settings = replace(self.app.settings, config=replace(self.app.config,
                           database_path=Path(self.tmp.name) / "snapshot.sqlite3"))
        version = "a" * 40
        with patch.object(e, "ROOT", asset_root), patch.dict(os.environ, {"APP_DEPLOY_VERSION": version}):
            app = e.Application(settings, self.api, tree_loader=lambda: self.api.tree)
        server = e.Server(("127.0.0.1", 0), app)
        thread = threading.Thread(target=server.serve_forever, daemon=True); thread.start()
        base = f"http://127.0.0.1:{server.server_address[1]}/"
        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
        def get(path, accept="*/*"):
            return opener.open(urllib.request.Request(base + path, headers={"Accept": accept}))
        try:
            originals = {name: get(name, "text/html" if not name else "*/*").read()
                         for name in ("", "app.js", "style.css")}
            original_manual = get("").read()
            for file in asset_root.rglob("*"):
                if file.is_file(): file.write_text("edited after process startup")
            for name, content in originals.items():
                with get(name, "text/html" if not name else "*/*") as response:
                    self.assertEqual(response.read(), content)
                    self.assertEqual(response.headers["Cache-Control"], "no-store")
            self.assertEqual(get("").read(), original_manual)
            with get("deployment-health") as response:
                health = json.load(response)
            self.assertEqual(health["version"], version)
            self.assertEqual(health["app_id"], "app_test")
            self.assertEqual(health["api_version"], 2)
            self.assertTrue(health["ready"] and health["database_ready"])
            self.assertEqual(health["assets_sha256"], app.asset_hash)
            self.assertFalse(app.activation()["enabled"])
            with patch.object(app, "db", side_effect=e.sqlite3.OperationalError("test only")):
                with self.assertRaises(urllib.error.HTTPError) as error: get("deployment-health")
                self.assertEqual(error.exception.code, 503)
                self.assertFalse(json.load(error.exception)["database_ready"])
            with self.assertRaises(urllib.error.HTTPError) as error: get("unknown-route")
            self.assertEqual(error.exception.code, 404)
        finally:
            server.shutdown(); server.server_close(); thread.join()

    def test_deployment_configuration_paths_and_database_override(self):
        config = Path(self.tmp.name) / "config.json"
        config.write_text(json.dumps({"service_id":"svc_test", "public_url":"http://127.0.0.1/",
                         "proxy_public_url":"https://proxy.example.test/", "workspace_id":"ws_test",
                         "runtime_agent_id":"app_test", "mode":"outbox"}))
        config.chmod(0o600)
        database = str(Path(self.tmp.name) / "managed/state/app.sqlite3")
        with patch.dict(os.environ, {"EMAIL_ENDPOINT_DB":database,
                                    "TREER_EMAIL_ALLOWLIST":"@obsolete.example.test"}):
            settings = e.load_settings(str(config), str(Path(self.tmp.name) / "state"))
        self.assertEqual(settings.config.database_path, Path(database))
        self.assertEqual(settings.mode, "outbox")
        self.assertEqual(settings.allowlist, [], "removed Core env must not configure App restrictions")
        self.assertEqual(settings.runtime_id, "app_test")
        config.chmod(0o644)
        with self.assertRaises(ValueError): e.load_settings(str(config))

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
