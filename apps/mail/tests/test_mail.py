from __future__ import annotations

import os
for key in list(os.environ):
    if key.startswith(('TREER_', 'AGENT_UI_', 'CODEX_UI_', 'MAIL_', 'POLICY_', 'SOUL_', 'ISSUES_', 'EMAIL_ENDPOINT_')) or key.lower() in ('http_proxy','https_proxy','all_proxy','no_proxy'):
        del os.environ[key]
import http.client
import json
import os
import socket
import subprocess
import sys
import tempfile
import threading
import time
import unittest
import urllib.parse
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path


APP_ROOT = Path(__file__).resolve().parents[1]
MAIL = APP_ROOT / "mail.py"


def core_message(message_id: str = "msg_inbound") -> dict[str, object]:
    return {
        "schema_version": 1,
        "message_id": message_id,
        "workspace_id": "workspace-a",
        "sender": {"kind": "agent", "id": "agent-a", "name": "Builder"},
        "recipients": [
            {"kind": "human", "id": "user-a", "name": "Owner", "role": "owner"}
        ],
        "context_ids": [],
        "body": "Ready for review",
        "created_at": "2026-08-20T12:00:00Z",
    }


class FakeProxy(ThreadingHTTPServer):
    daemon_threads = True

    def __init__(self) -> None:
        self.messages = [core_message()]
        self.deliveries = [
            {"delivery_id": "dlv_inbound", "message_id": "msg_inbound", "acked": False}
        ]
        self.calls: list[dict[str, object]] = []
        super().__init__(("127.0.0.1", 0), FakeProxyHandler)


class FakeProxyHandler(BaseHTTPRequestHandler):
    server: FakeProxy

    def do_GET(self) -> None:  # noqa: N802
        parsed = urllib.parse.urlsplit(self.path)
        if parsed.path == "/api/apps/svc_mail/directory":
            self._require_token()
            self._json(
                {
                    "principals": [
                        {"kind": "agent", "id": "agent-a", "name": "Builder"},
                        {"kind": "human", "id": "user-a", "name": "Owner", "role": "owner"},
                        {"kind": "human", "id": "user-b", "name": "Reviewer", "role": "member"},
                    ]
                }
            )
        elif parsed.path == "/api/apps/svc_mail/messages":
            self._require_token()
            self._json({"messages": self.server.messages, "next_before": None})
        elif parsed.path.startswith("/api/apps/svc_mail/messages/"):
            self._require_token()
            wanted = parsed.path.rsplit("/", 1)[1]
            message = next((m for m in self.server.messages if m["message_id"] == wanted), None)
            if message is None:
                body = json.dumps({"error": {"code": "message_not_found", "message": "no"}}).encode()
                self.send_response(404)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)
                return
            self._json({"message": message})
        else:
            self.send_error(404)

    def do_POST(self) -> None:  # noqa: N802
        parsed = urllib.parse.urlsplit(self.path)
        raw = self.rfile.read(int(self.headers.get("Content-Length", "0")))
        if parsed.path == "/api/apps/oauth/token":
            form = urllib.parse.parse_qs(raw.decode("ascii"))
            if form.get("code") != ["fake-code"] or not form.get("code_verifier"):
                self.send_error(400)
                return
            self._json(
                {
                    "access_token": "app-access-token",
                    "token_type": "Bearer",
                    "expires_in": 3600,
                    "expires_at": "2099-01-01T00:00:00Z",
                    "scope": "workspace:app",
                }
            )
            return
        body = json.loads(raw or b"{}")
        if parsed.path == "/.treer/apps/identity/verify":
            self._json(
                {
                    "active": body.get("token") == "app-access-token",
                    "claims": {
                        "workspace_id": "workspace-a",
                        "service_id": "svc_mail",
                        "principal_kind": "human",
                        "sub": "user-a",
                        "name": "Owner",
                        "role": "owner",
                        "exp": 4070908800,
                    },
                }
            )
            return
        self._require_token()
        self.server.calls.append({"path": parsed.path, "body": body})
        if parsed.path == "/api/apps/svc_mail/messages":
            message = core_message(f"msg_sent_{len(self.server.messages)}")
            message["sender"] = {"kind": "human", "id": "user-a", "name": "Owner", "role": "owner"}
            message["recipients"] = [
                {"kind": "agent", "id": target, "name": "Builder"}
                for target in body["recipients"]
            ]
            message["context_ids"] = body["context_ids"]
            message["body"] = body["body"]
            self.server.messages.append(message)
            self._json({"message": message})
        elif parsed.path == "/api/apps/svc_mail/messages/receive":
            deliveries = []
            for delivery in self.server.deliveries:
                if not delivery["acked"]:
                    message = next(
                        item for item in self.server.messages if item["message_id"] == delivery["message_id"]
                    )
                    deliveries.append({"delivery_id": delivery["delivery_id"], "message": message})
            self._json({"deliveries": deliveries[: body["limit"]], "remaining_unacknowledged": len(deliveries)})
        elif parsed.path == "/api/apps/svc_mail/messages/ack":
            wanted = set(body["delivery_ids"])
            for delivery in self.server.deliveries:
                if delivery["delivery_id"] in wanted:
                    delivery["acked"] = True
            self._json({"acknowledged": len(wanted), "already_acknowledged": 0})
        else:
            self.send_error(404)

    def _require_token(self) -> None:
        if self.headers.get("Authorization") != "Bearer app-access-token":
            raise AssertionError("Mail did not use its App bearer token")

    def _json(self, value: dict[str, object]) -> None:
        body = json.dumps(value).encode("utf-8")
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, _format: str, *_args: object) -> None:
        pass


class MailProcessCase(unittest.TestCase):
    """Runs mail.py against a fake Proxy; subclasses may swap either."""

    proxy_class = FakeProxy
    extra_config: dict[str, object] = {}

    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        self.proxy = self.proxy_class()
        self.proxy_thread = threading.Thread(
            target=self.proxy.serve_forever, daemon=True
        )
        self.proxy_thread.start()
        web = self.root / "web"
        web.mkdir()
        (web / "index.html").write_text("<!doctype html><title>Treer Mail</title>", encoding="utf-8")
        self.port = free_port()
        config = self.root / "config.json"
        config.write_text(
            json.dumps(
                {
                    "listen": f"127.0.0.1:{self.port}",
                    "service_id": "svc_mail",
                    "public_url": f"http://127.0.0.1:{self.port}/",
                    "proxy_public_url": f"http://127.0.0.1:{self.proxy.server_address[1]}/",
                    "web_dir": str(web),
                    **self.extra_config,
                }
            ),
            encoding="utf-8",
        )
        environment = os.environ.copy()
        environment.update(
            {
                "TREER_APP_CONFIG": str(config),
                "TREER_APP_STATE_DIR": str(self.root / "app-state"),
            }
        )
        self.process = subprocess.Popen(
            [sys.executable, str(MAIL)],
            env=environment,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        wait_for_health(self.port, self.process)
        self.cookie = ""

    def tearDown(self) -> None:
        self.process.terminate()
        try:
            self.process.communicate(timeout=5)
        except subprocess.TimeoutExpired:
            self.process.kill()
            self.process.communicate(timeout=5)
        self.proxy.shutdown()
        self.proxy.server_close()
        self.proxy_thread.join(timeout=5)
        self.temporary.cleanup()

    def request(
        self,
        method: str,
        path: str,
        body: dict[str, object] | None = None,
        request_headers: dict[str, str] | None = None,
    ) -> tuple[int, dict[str, str], object | None]:
        connection = http.client.HTTPConnection("127.0.0.1", self.port, timeout=5)
        headers = {"Cookie": self.cookie} if self.cookie else {}
        if method == "POST":
            headers["Origin"] = f"http://127.0.0.1:{self.port}"
        headers.update(request_headers or {})
        encoded = None
        if body is not None:
            encoded = json.dumps(body)
            headers["Content-Type"] = "application/json"
        connection.request(method, path, body=encoded, headers=headers)
        response = connection.getresponse()
        raw = response.read()
        response_headers = {key.lower(): value for key, value in response.getheaders()}
        connection.close()
        content_type = response_headers.get("content-type", "")
        value = json.loads(raw) if raw and "json" in content_type else raw.decode() if raw else None
        return response.status, response_headers, value

    def login(self) -> None:
        status, headers, _ = self.request("GET", "/api/auth/start?return_to=%2F")
        self.assertEqual(status, 302)
        authorize = urllib.parse.urlsplit(headers["location"])
        state = urllib.parse.parse_qs(authorize.query)["state"][0]
        browser_cookie = headers["set-cookie"].split(";", 1)[0]
        status, headers, _ = self.request(
            "GET", f"/api/auth/callback?code=fake-code&state={urllib.parse.quote(state)}", request_headers={"Cookie":browser_cookie}
        )
        self.assertEqual(status, 302)
        self.assertEqual(headers["location"], "/")
        self.assertIn("HttpOnly", headers["set-cookie"])
        self.cookie = headers["set-cookie"].split(";", 1)[0]


class MailServerTest(MailProcessCase):
    def test_browser_contract_uses_app_api_and_preserves_mail_shapes(self) -> None:
        status, _, value = self.request("GET", "/api/health")
        self.assertEqual((status, value), (200, {"status": "ok", "service": "treer-mail"}))
        status, _, _ = self.request("GET", "/api/auth/session")
        self.assertEqual(status, 401)

        self.login()
        status, _, session = self.request("GET", "/api/auth/session")
        self.assertEqual(status, 200)
        self.assertEqual(session["workspace_id"], "workspace-a")
        self.assertEqual(session["user"]["id"], "user-a")

        status, _, directory = self.request("GET", "/api/directory")
        self.assertEqual(status, 200)
        self.assertEqual(
            {(item["kind"], item["id"]) for item in directory["principals"]},
            {("agent", "agent-a"), ("human", "user-a"), ("human", "user-b")},
        )

        status, _, mailbox = self.request("GET", "/api/messages?limit=100")
        self.assertEqual(status, 200)
        self.assertEqual(mailbox["deliveries"][0]["message"]["message_id"], "msg_inbound")
        self.assertTrue(mailbox["deliveries"][0]["unread"])
        self.assertEqual(mailbox["remaining_unread"], 0)

        body = "Review complete\nNo secrets in argv."
        status, _, sent = self.request(
            "POST",
            "/api/messages",
            {"recipients": ["agent-a"], "context_ids": ["msg_inbound"], "body": body},
        )
        self.assertEqual(status, 200)
        self.assertEqual(sent["message"]["context_ids"], ["msg_inbound"])
        self.assertEqual(sent["message"]["body"], body)
        send_call = self.proxy.calls[-1]
        self.assertEqual(send_call["path"], "/api/apps/svc_mail/messages")
        self.assertEqual(send_call["body"]["body"], body)

        self.proxy.messages.append(core_message("msg_second"))
        self.proxy.deliveries.append(
            {"delivery_id": "dlv_second", "message_id": "msg_second", "acked": False}
        )
        status, _, inbox = self.request("POST", "/api/inbox", {"limit": 50})
        self.assertEqual(status, 200)
        self.assertEqual([item["message"]["message_id"] for item in inbox["deliveries"]], ["msg_second"])
        self.assertEqual(inbox["remaining_unread"], 0)

        # A reply link may name a Message outside the recent page.
        self.proxy.messages.append(core_message("msg_old"))
        status, _, value = self.request("GET", "/api/messages/msg_old")
        self.assertEqual((status, value["message"]["message_id"]), (200, "msg_old"))
        status, _, value = self.request("GET", "/api/messages/msg_missing")
        self.assertEqual((status, value["error"]["code"]), (404, "message_not_found"))

        status, _, _ = self.request("POST", "/api/auth/logout", {})
        self.assertEqual(status, 204)
        status, _, _ = self.request("GET", "/api/auth/session")
        self.assertEqual(status, 401)

    def test_root_negotiates_agent_manual_and_human_ui(self) -> None:
        status, headers, manual = self.request("GET", "/")
        self.assertEqual(status, 200)
        self.assertEqual(headers["content-type"], "text/markdown; charset=utf-8")
        self.assertEqual(headers["vary"], "Accept, User-Agent")
        self.assertIn("This index is for Agents", manual)
        self.assertIn("treer message list", manual)

        status, headers, page = self.request(
            "GET", "/", request_headers={"Accept": "text/html"}
        )
        self.assertEqual(status, 200)
        self.assertEqual(headers["content-type"], "text/html")
        self.assertEqual(headers["vary"], "Accept, User-Agent")
        self.assertIn("Treer Mail", page)

        status, _, page = self.request(
            "GET", "/", request_headers={"User-Agent": "Mozilla/5.0"}
        )
        self.assertEqual(status, 200)
        self.assertIn("Treer Mail", page)

        status, headers, _ = self.request(
            "GET",
            "/",
            request_headers={"Accept": "text/markdown", "User-Agent": "Mozilla/5.0"},
        )
        self.assertEqual(status, 200)
        self.assertEqual(headers["content-type"], "text/markdown; charset=utf-8")

        status, _, _ = self.request("GET", "/_human/")
        self.assertEqual(status, 404)

        source = (APP_ROOT / "web" / "src" / "App.tsx").read_text(encoding="utf-8")
        self.assertIn("function applicationUrl(path: string)", source)
        self.assertIn("fetch(applicationUrl(path)", source)
        vite_config = (APP_ROOT / "web" / "vite.config.ts").read_text(encoding="utf-8")
        self.assertIn('base: "./"', vite_config)


def free_port() -> int:
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        return int(listener.getsockname()[1])


def wait_for_health(port: int, process: subprocess.Popen[str]) -> None:
    deadline = time.monotonic() + 10
    while time.monotonic() < deadline:
        if process.poll() is not None:
            _stdout, stderr = process.communicate(timeout=1)
            raise AssertionError(f"Mail server exited during startup: {stderr}")
        try:
            connection = http.client.HTTPConnection("127.0.0.1", port, timeout=0.25)
            connection.request("GET", "/api/health")
            response = connection.getresponse()
            response.read()
            connection.close()
            if response.status == 200:
                return
        except OSError:
            pass
        time.sleep(0.05)
    raise AssertionError("Mail server did not become healthy")


if __name__ == "__main__":
    unittest.main()
