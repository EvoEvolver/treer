"""Test doubles: a fake GitHub (git http-backend + REST), a fake Treer Proxy verifier and a fake `gh`."""

from __future__ import annotations

import base64
import importlib.util
import json
import os
import re
import subprocess
import sys
import tempfile
import textwrap
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Any

for key in list(os.environ):
    if key.startswith(("TREER_", "GATEWAY_", "GIT_")) or key.lower() in ("http_proxy", "https_proxy", "all_proxy", "no_proxy"):
        del os.environ[key]

APP = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(APP))


def load_gateway() -> Any:
    spec = importlib.util.spec_from_file_location("git_gateway_app", APP / "gateway.py")
    module = importlib.util.module_from_spec(spec)
    sys.modules["git_gateway_app"] = module
    spec.loader.exec_module(module)
    return module


def serve(handler: type, state: Any) -> ThreadingHTTPServer:
    class Server(ThreadingHTTPServer):
        daemon_threads = True

    server = Server(("127.0.0.1", 0), handler)
    server.state = state  # type: ignore[attr-defined]
    threading.Thread(target=server.serve_forever, daemon=True).start()
    return server


def read_body(handler: BaseHTTPRequestHandler) -> bytes:
    if "chunked" in (handler.headers.get("Transfer-Encoding") or "").lower():
        data = b""
        while True:
            line = handler.rfile.readline()
            if not line:  # the gateway aborted an over-limit upstream push
                handler.close_connection = True
                return data
            size = int(line.split(b";")[0].strip(), 16)
            if size == 0:
                while handler.rfile.readline() not in (b"\r\n", b"\n", b""):
                    pass
                return data
            data += handler.rfile.read(size)
            handler.rfile.readline()
    return handler.rfile.read(int(handler.headers.get("Content-Length") or 0))


# --------------------------------------------------------------------------- fake GitHub


class FakeGitHubState:
    def __init__(self, root: Path) -> None:
        self.root = root
        self.tokens = {"tok1"}  # accepted credentials (Basic password / Bearer)
        self.requests: list[dict[str, Any]] = []
        self.app_key = None  # RsaPrivateKey whose JWTs are accepted
        self.app_id = 4242
        self.installations = [{"id": 99, "account": {"login": "EvoEvolver", "type": "Organization"},
                               "repository_selection": "selected", "html_url": "https://github.com/x"}]
        self.conversion: dict[str, Any] | None = None
        self.installation_token = "ghs_install_token"
        self.lock = threading.Lock()

    def repo(self, owner: str, name: str) -> Path:
        return self.root / owner / f"{name}.git"

    def create_repo(self, owner: str, name: str) -> Path:
        path = self.repo(owner, name)
        path.parent.mkdir(parents=True, exist_ok=True)
        subprocess.run(["git", "init", "-q", "--bare", "--initial-branch=main", str(path)], check=True)
        subprocess.run(["git", "--git-dir", str(path), "config", "http.receivepack", "true"], check=True)
        return path

    def jwt_ok(self, token: str) -> bool:
        from gw_credentials import rsa_sha256_sign  # noqa: F401  (sanity: module importable)

        if self.app_key is None:
            return False
        try:
            header, claims, signature = token.split(".")
            pad = lambda value: base64.urlsafe_b64decode(value + "=" * (-len(value) % 4))  # noqa: E731
            if json.loads(pad(header))["alg"] != "RS256":
                return False
            payload = json.loads(pad(claims))
            if str(payload["iss"]) != str(self.app_id) or payload["exp"] < time.time():
                return False
            value = int.from_bytes(pad(signature), "big")
            encoded = pow(value, self.app_key.e, self.app_key.n).to_bytes(self.app_key.size, "big")
            import hashlib

            return encoded.endswith(hashlib.sha256(f"{header}.{claims}".encode()).digest())
        except (ValueError, KeyError):
            return False


class FakeGitHub(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    server: Any

    def log_message(self, *_: Any) -> None:
        pass

    def do_GET(self) -> None:  # noqa: N802
        self.handle_any()

    def do_POST(self) -> None:  # noqa: N802
        self.handle_any()

    def do_DELETE(self) -> None:  # noqa: N802
        self.handle_any()

    def do_PATCH(self) -> None:  # noqa: N802
        self.handle_any()

    def credential(self) -> str | None:
        value = self.headers.get("Authorization") or ""
        if value.startswith("Basic "):
            user, _, password = base64.b64decode(value[6:]).decode().partition(":")
            return password if user == "x-access-token" else None
        if value.startswith("Bearer "):
            return value[7:]
        return None

    def send(self, status: int, body: bytes, content_type: str = "application/json", extra: dict[str, str] | None = None) -> None:
        self.send_response(status)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(body)))
        for name, value in (extra or {}).items():
            self.send_header(name, value)
        self.end_headers()
        self.wfile.write(body)

    def handle_any(self) -> None:
        state: FakeGitHubState = self.server.state
        body = read_body(self) if self.command in ("POST", "PATCH", "PUT", "DELETE") else b""
        credential = self.credential()
        with state.lock:
            state.requests.append({"method": self.command, "path": self.path, "credential": credential,
                                   "headers": {k.lower(): v for k, v in self.headers.items()}, "body_bytes": len(body)})
        path = self.path.split("?", 1)[0]
        git = re.fullmatch(r"/([^/]+)/([^/]+)\.git(/.*)", path)
        if git:
            if credential not in state.tokens:
                self.send(401, b"bad credentials", "text/plain", {"WWW-Authenticate": 'Basic realm="GitHub"'})
                return
            if not state.repo(git.group(1), git.group(2)).is_dir():
                self.send(404, b"Repository not found.", "text/plain")
                return
            self.backend(git.group(1), path[1:], body)
            return
        # REST
        conversion = re.fullmatch(r"/app-manifests/([^/]+)/conversions", path)
        if conversion and self.command == "POST":
            if state.conversion is None or conversion.group(1) != "manifest-code":
                self.send(404, b'{"message":"Not Found"}')
            else:
                self.send(201, json.dumps(state.conversion).encode())
            return
        if path.startswith("/app/"):
            if not state.jwt_ok(credential or ""):
                self.send(401, b'{"message":"A JSON web token could not be decoded"}')
                return
            if path == "/app/installations":
                self.send(200, json.dumps(state.installations).encode())
                return
            token = re.fullmatch(r"/app/installations/(\d+)/access_tokens", path)
            if token and self.command == "POST":
                expires = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(time.time() + 3600))
                self.send(201, json.dumps({"token": state.installation_token, "expires_at": expires}).encode())
                return
        if credential not in state.tokens:
            self.send(401, b'{"message":"Bad credentials"}')
            return
        rate = {"X-RateLimit-Limit": "5000", "X-RateLimit-Remaining": "4999", "X-RateLimit-Reset": "1900000000",
                "X-RateLimit-Resource": "core"}
        if path == "/rate_limit":
            self.send(200, json.dumps({"resources": {"core": {"limit": 5000, "remaining": 4998, "reset": 1900000000}}}).encode())
            return
        branch = re.fullmatch(r"/repos/([^/]+)/([^/]+)/branches/(.+)", path)
        if branch and self.command == "GET":
            repo = state.repo(branch.group(1), branch.group(2))
            result = subprocess.run(["git", "--git-dir", str(repo), "rev-parse", "--verify", "-q", f"refs/heads/{branch.group(3)}"],
                                    capture_output=True, text=True)
            if result.returncode != 0:
                self.send(404, b'{"message":"Branch not found"}', extra=rate)
            else:
                self.send(200, json.dumps({"name": branch.group(3), "commit": {"sha": result.stdout.strip()}}).encode(), extra=rate)
            return
        status = re.fullmatch(r"/repos/([^/]+)/([^/]+)/commits/([^/]+)/status", path)
        if status:
            self.send(200, json.dumps({"state": "success", "sha": status.group(3)}).encode(),
                      extra={**rate, "Link": '<https://api.github.com/repos/x/y/commits?page=2>; rel="next"'})
            return
        if re.fullmatch(r"/repos/[^/]+/[^/]+/issues", path) and self.command == "POST":
            self.send(201, json.dumps({"number": 7, "echo": json.loads(body)}).encode(), extra=rate)
            return
        if re.fullmatch(r"/repos/[^/]+/[^/]+/git/refs/heads/.+", path) and self.command == "DELETE":
            self.send(204, b"", extra=rate)
            return
        self.send(404, b'{"message":"Not Found"}', extra=rate)

    def backend(self, owner: str, path_info_suffix: str, body: bytes) -> None:
        state: FakeGitHubState = self.server.state
        environment = {
            **os.environ,
            "GIT_PROJECT_ROOT": str(state.root),
            "GIT_HTTP_EXPORT_ALL": "1",
            "PATH_INFO": "/" + path_info_suffix,
            "QUERY_STRING": self.path.partition("?")[2],
            "REQUEST_METHOD": self.command,
            "CONTENT_TYPE": self.headers.get("Content-Type", ""),
            "CONTENT_LENGTH": str(len(body)),
            "REMOTE_ADDR": "127.0.0.1",
            "REMOTE_USER": "fake",
        }
        if self.headers.get("Content-Encoding"):
            environment["HTTP_CONTENT_ENCODING"] = self.headers["Content-Encoding"]
        if self.headers.get("Git-Protocol"):
            environment["HTTP_GIT_PROTOCOL"] = self.headers["Git-Protocol"]
        completed = subprocess.run(["git", "http-backend"], input=body, capture_output=True, env=environment, timeout=120)
        head, _, payload = completed.stdout.partition(b"\r\n\r\n")
        status, headers = 200, {}
        for line in head.decode("latin-1").splitlines():
            name, _, value = line.partition(":")
            if name.lower() == "status":
                status = int(value.strip().split()[0])
            elif name.lower() not in ("content-length",):
                headers[name.strip()] = value.strip()
        # Chunked response, like GitHub, to exercise the gateway's streaming relay.
        self.send_response(status)
        for name, value in headers.items():
            self.send_header(name, value)
        self.send_header("Transfer-Encoding", "chunked")
        self.end_headers()
        for start in range(0, len(payload), 7000):
            piece = payload[start : start + 7000]
            self.wfile.write(f"{len(piece):x}\r\n".encode() + piece + b"\r\n")
        self.wfile.write(b"0\r\n\r\n")


# --------------------------------------------------------------------------- fake Treer Proxy


PRINCIPALS = {
    "agent-token": {"principal_kind": "agent", "sub": "ag_worker", "name": "ag_worker", "machine_id": "mach_remote"},
    "trusted-token": {"principal_kind": "agent", "sub": "ag_leader", "name": "ag_leader", "machine_id": "mach_mac"},
    "human-token": {"principal_kind": "human", "sub": "u_owner", "name": "Owner", "role": "owner"},
    "human2-token": {"principal_kind": "human", "sub": "u_other", "name": "Other", "role": "member"},
    "wrong-audience": {"principal_kind": "agent", "sub": "ag_x", "name": "ag_x", "service_id": "svc_other"},
}


class FakeProxy(BaseHTTPRequestHandler):
    server: Any

    def log_message(self, *_: Any) -> None:
        pass

    def reply(self, value: Any, status: int = 200) -> None:
        body = json.dumps(value).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_POST(self) -> None:  # noqa: N802
        request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        self.server.state["verifications"] += 1
        claims = PRINCIPALS.get(request["token"])
        if self.path != "/.treer/apps/identity/verify" or claims is None or request["audience"] != "svc_gateway":
            self.reply({"active": False})
            return
        self.reply({"active": True, "claims": {"service_id": "svc_gateway", "workspace_id": "ws_test",
                                               "exp": time.time() + 60, **claims}})

    def do_GET(self) -> None:  # noqa: N802
        if self.path == "/api/apps/svc_gateway/directory":
            self.reply({"principals": [{"kind": "agent", "id": "ag_worker", "name": "Worker", "path": "/team/worker"}]})
        else:
            self.reply({"error": {"code": "not_found"}}, 404)


# --------------------------------------------------------------------------- fake gh


def fake_gh(directory: Path, token: str) -> tuple[Path, Path]:
    """A `gh` that prints the token in ``token.txt`` and counts invocations."""
    token_file = directory / "token.txt"
    token_file.write_text(token)
    script = directory / "gh"
    script.write_text(textwrap.dedent(f"""\
        #!/bin/sh
        [ "$1 $2" = "auth token" ] || {{ echo "unexpected gh args: $*" >&2; exit 2; }}
        echo x >> "{directory}/calls"
        cat "{token_file}"
        """))
    script.chmod(0o700)
    return script, token_file


class Environment:
    """Fake GitHub + Proxy + gh + an in-process gateway."""

    def __init__(self, seed: dict[str, Any] | None = None, gh_refresh: float = 600) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        self.github_state = FakeGitHubState(self.root / "github")
        self.github = serve(FakeGitHub, self.github_state)
        self.proxy_state = {"verifications": 0}
        self.proxy = serve(FakeProxy, self.proxy_state)
        (self.root / "bin").mkdir()
        self.gh_bin, self.token_file = fake_gh(self.root / "bin", "tok1")
        github_url = f"http://127.0.0.1:{self.github.server_port}"
        module = load_gateway()
        self.module = module
        settings = module.Settings(
            listen=("127.0.0.1", 0),
            data_dir=self.root / "state",
            proxy_url=f"http://127.0.0.1:{self.proxy.server_port}",
            service_id="svc_gateway",
            workspace_id="ws_test",
            public_url="http://gateway.example/",
            agent_url="http://git-gateway.treer",
            github_url=github_url,
            github_api_url=github_url,
            gh_bin=str(self.gh_bin),
            gh_refresh_seconds=gh_refresh,
            treer_bin="treer-not-used",
            seed=seed or {},
        )
        self.gateway = module.Gateway(settings)
        self.server = module.GatewayServer(settings.listen, self.gateway, quiet=True)
        threading.Thread(target=self.server.serve_forever, daemon=True).start()
        self.url = f"http://127.0.0.1:{self.server.server_port}"

    def gh_calls(self) -> int:
        try:
            return len((self.root / "bin" / "calls").read_text().splitlines())
        except FileNotFoundError:
            return 0

    def close(self) -> None:
        for server in (self.server, self.github, self.proxy):
            server.shutdown()
            server.server_close()
        self.temporary.cleanup()
