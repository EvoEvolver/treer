from __future__ import annotations

import http.client
import io
import json
import sys
import tempfile
import unittest
import urllib.parse
from pathlib import Path

from test_mail import FakeProxy, FakeProxyHandler, MailProcessCase

CAPABILITY = "app.email.v1"


class EmailProxy(FakeProxy):
    """FakeProxy plus a fake Core App email route that behaves like Core: the
    same idempotency key with the same content never sends twice."""

    def __init__(self) -> None:
        super().__init__()
        self.RequestHandlerClass = EmailProxyHandler
        self.capabilities = [CAPABILITY]
        self.email_requests: list[dict] = []
        self.sent: dict[str, dict] = {}
        self.script: list[tuple[int, dict]] = []  # forced responses, consumed in order
        self.members = {"user-a": "owner@example.test"}


class EmailProxyHandler(FakeProxyHandler):
    server: EmailProxy

    def do_GET(self) -> None:  # noqa: N802
        path = urllib.parse.urlsplit(self.path).path
        if path == "/api/health":
            return self._send(200, {"status": "ok", "capabilities": self.server.capabilities})
        if path.startswith("/api/apps/svc_mail/email/"):
            assert self.headers.get("Authorization") == "Bearer runtime-token"
            key = urllib.parse.unquote(path.rsplit("/", 1)[1])
            if key in self.server.sent:
                return self._send(200, {"email_id": "email_" + key[-8:], "status": "sent"})
            return self._send(404, {"error": {"code": "email_not_found", "message": "unknown key"}})
        super().do_GET()

    def do_POST(self) -> None:  # noqa: N802
        path = urllib.parse.urlsplit(self.path).path
        raw = self.rfile.read(int(self.headers["Content-Length"]))
        if path == "/api/apps/svc_mail/email":
            body = json.loads(raw)
            assert self.headers.get("Authorization") == "Bearer runtime-token"
            assert set(body) in ({"to", "subject", "text", "idempotency_key"},
                                 {"to_member", "subject", "text", "idempotency_key"}), body
            self.server.email_requests.append(body)
            if self.server.script:
                return self._send(*self.server.script.pop(0))
            if "to_member" in body and body["to_member"] not in self.server.members:
                return self._send(404, {"error": {"code": "email_member_not_found",
                                                  "message": "to_member is not a member of this workspace"}})
            previous = self.server.sent.get(body["idempotency_key"])
            if previous and previous != body:
                return self._send(400, {"error": {"code": "email_idempotency_conflict", "message": "x"}})
            self.server.sent[body["idempotency_key"]] = body
            return self._send(200, {"email_id": "email_" + body["idempotency_key"][-8:], "status": "sent"})
        if path == "/.treer/apps/identity/verify" and json.loads(raw).get("token") == "agent-token":
            return self._send(200, {"active": True, "claims": {
                "workspace_id": "workspace-a", "service_id": "svc_mail", "principal_kind": "agent",
                "sub": "agent-a", "name": "Builder", "exp": 4070908800}})
        self.rfile = io.BytesIO(raw)  # Re-feed the consumed body to the base handler.
        super().do_POST()

    def _send(self, status: int, value: dict) -> None:
        body = json.dumps(value).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


FAKE_TREER = Path(tempfile.mkdtemp(prefix="mail-fake-treer-")) / "treer"
FAKE_TREER.write_text(f"#!{sys.executable}\nprint('runtime-token')\n", encoding="utf-8")
FAKE_TREER.chmod(0o755)


class AgentEmailTest(MailProcessCase):
    """`POST /api/email` and `GET /api/email/KEY` against a running mail.py."""

    proxy_class = EmailProxy
    extra_config = {"treer_bin": str(FAKE_TREER)}

    def agent(self, method: str, path: str, body: dict | None = None,
              token: str = "agent-token") -> tuple[int, dict]:
        connection = http.client.HTTPConnection("127.0.0.1", self.port, timeout=10)
        headers = {"X-Treer-Identity-Token": token}
        encoded = None
        if body is not None:
            encoded = json.dumps(body)
            headers["Content-Type"] = "application/json"
        connection.request(method, path, body=encoded, headers=headers)
        response = connection.getresponse()
        value = json.loads(response.read())
        connection.close()
        return response.status, value

    def test_agent_emails_a_member_a_reply_link(self) -> None:
        status, value = self.agent("POST", "/api/email", {
            "to_member": "user-a", "subject": "Review needed", "text": "Please review the draft.",
            "message_id": "msg_inbound"})
        self.assertEqual(status, 200, value)
        self.assertEqual(value["status"], "sent")
        self.assertTrue(value["email_id"].startswith("email_"))
        key = value["idempotency_key"]
        self.assertTrue(key.startswith("mail-"))
        email = self.proxy.email_requests[-1]
        self.assertEqual(email["to_member"], "user-a")
        self.assertEqual(email["subject"], "Review needed")
        self.assertTrue(email["text"].startswith("Please review the draft.\n\n"))
        self.assertIn(f"Read and reply: http://127.0.0.1:{self.port}/?message=msg_inbound", email["text"])
        self.assertIn("Sent by Builder", email["text"])
        self.assertEqual(email["idempotency_key"], key)

        # The same request is the same email: Core replays it.
        status, again = self.agent("POST", "/api/email", {
            "to_member": "user-a", "subject": "Review needed", "text": "Please review the draft.",
            "message_id": "msg_inbound"})
        self.assertEqual((status, again["idempotency_key"], again["email_id"]),
                         (200, key, value["email_id"]))
        self.assertEqual(len(self.proxy.sent), 1)

        # Status is a thin wrapper over Core's.
        status, looked_up = self.agent("GET", f"/api/email/{key}")
        self.assertEqual((status, looked_up["status"]), (200, "sent"))
        status, missing = self.agent("GET", "/api/email/mail-unknown")
        self.assertEqual((status, missing["error"]["code"]), (404, "email_not_found"))
        status, invalid = self.agent("GET", "/api/email/bad%20key")
        self.assertEqual((status, invalid["error"]["code"]), (400, "email_key_invalid"))

    def test_any_address_inbox_link_and_idempotency_keys(self) -> None:
        status, value = self.agent("POST", "/api/email", {
            "to": "anyone@external.test", "subject": "Hello", "text": "No Message yet."})
        self.assertEqual(status, 200, value)
        email = self.proxy.email_requests[-1]
        self.assertEqual(email["to"], "anyone@external.test")
        self.assertIn(f"Open Treer Mail: http://127.0.0.1:{self.port}/\n", email["text"])
        self.assertNotIn("?message=", email["text"])
        # Different content without a message_id is a different email.
        status, other = self.agent("POST", "/api/email", {
            "to": "anyone@external.test", "subject": "Hello", "text": "Second note."})
        self.assertNotEqual(other["idempotency_key"], value["idempotency_key"])
        # A caller-chosen key is scoped to the caller and replaces content hashing.
        first = self.agent("POST", "/api/email", {
            "to": "anyone@external.test", "subject": "A", "text": "x", "idempotency_key": "job-1"})[1]
        status, conflict = self.agent("POST", "/api/email", {
            "to": "anyone@external.test", "subject": "B", "text": "y", "idempotency_key": "job-1"})
        self.assertEqual((status, conflict["error"]["code"]), (400, "email_idempotency_conflict"))
        self.assertEqual(conflict["idempotency_key"], first["idempotency_key"])
        self.assertFalse(conflict["error"]["retryable"])

    def test_validation_and_core_error_mapping(self) -> None:
        base = {"to_member": "user-a", "subject": "S", "text": "T"}
        for change, code in [
            ({"to": "x@example.test"}, "email_recipient_invalid"),
            ({"to_member": None}, "email_recipient_invalid"),
            ({"subject": "two\nlines"}, "email_subject_invalid"),
            ({"subject": ""}, "email_subject_invalid"),
            ({"text": "  "}, "email_text_invalid"),
            ({"text": "x" * 4000}, "email_text_invalid"),
            ({"message_id": "../evil"}, "email_message_id_invalid"),
            ({"cc": "x@example.test"}, "request_field_invalid"),
        ]:
            request = {**base, **change}
            request = {k: v for k, v in request.items() if v is not None}
            status, value = self.agent("POST", "/api/email", request)
            self.assertEqual((status, value["error"]["code"]), (400, code), change)
        self.assertEqual(self.proxy.email_requests, [])

        status, value = self.agent("POST", "/api/email", {**base, "to_member": "stranger"})
        self.assertEqual((status, value["error"]["code"]), (404, "email_member_not_found"))
        self.proxy.script = [(429, {"error": {"code": "email_rate_limited", "message": "hourly limit"}})]
        status, value = self.agent("POST", "/api/email", base)
        self.assertEqual((status, value["error"]["code"], value["error"]["retryable"]),
                         (429, "email_rate_limited", True))
        # Core may have sent it: report uncertain with the key, never resend.
        self.proxy.script = [(502, {"error": {"code": "email_delivery_failed", "message": "x"}})]
        before = len(self.proxy.email_requests)
        status, value = self.agent("POST", "/api/email", base)
        self.assertEqual((status, value["status"], value["code"]), (202, "uncertain", "email_delivery_failed"))
        self.assertTrue(value["idempotency_key"].startswith("mail-"))
        self.assertEqual(len(self.proxy.email_requests), before + 1)
        # Mail's own runtime credential failing is not the caller's 401.
        self.proxy.script = [(401, {"error": {"code": "app_authentication_required", "message": "x"}})]
        status, value = self.agent("POST", "/api/email", base)
        self.assertEqual((status, value["error"]["retryable"]), (503, True))

    def test_route_absent_and_authentication(self) -> None:
        body = {"to_member": "user-a", "subject": "S", "text": "T"}
        status, value = self.agent("POST", "/api/email", body, token="forged")
        self.assertEqual((status, value["error"]["code"]), (401, "mail_identity_invalid"))
        connection = http.client.HTTPConnection("127.0.0.1", self.port, timeout=5)
        connection.request("POST", "/api/email", body=json.dumps(body), headers={"Content-Type": "application/json"})
        response = connection.getresponse()
        response.read()
        self.assertEqual(response.status, 403)  # No identity and no same-origin browser.
        connection.close()
        self.assertEqual(self.proxy.email_requests, [])
        # A signed-in human may use the same API from the browser.
        self.login()
        status, _, value = self.request("POST", "/api/email", body)
        self.assertEqual((status, value["status"]), (200, "sent"))
        self.assertIn("Sent by Owner", self.proxy.email_requests[-1]["text"])

    def test_capability_absent_reports_unavailable(self) -> None:
        self.proxy.capabilities = []
        status, value = self.agent("POST", "/api/email", {"to_member": "user-a", "subject": "S", "text": "T"})
        self.assertEqual((status, value["error"]["code"]), (503, "email_unavailable"))
        self.assertEqual(self.proxy.email_requests, [])

    def test_reply_links_survive_sign_in(self) -> None:
        status, _, _ = self.request("GET", "/api/auth/start?return_to=" + urllib.parse.quote("/?message=msg_inbound"))
        self.assertEqual(status, 302)
        for bad in ("//evil.example/", "/?view=notifications", "/?message=a&x=1"):
            status, _, _ = self.request("GET", "/api/auth/start?return_to=" + urllib.parse.quote(bad))
            self.assertEqual(status, 400, bad)
        # Removed notification routes are gone.
        self.login()
        status, _, _ = self.request("GET", "/api/notifications")
        self.assertEqual(status, 404)


if __name__ == "__main__":
    unittest.main()
