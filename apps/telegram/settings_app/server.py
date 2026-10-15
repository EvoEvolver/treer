"""HTTP server for the settings App: static React page, a small authenticated JSON API, no other surface."""
from __future__ import annotations

import argparse
import json
import mimetypes
import os
import subprocess
import sys
import threading
import urllib.parse
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Any

from telegram_app.clients import BotApi, TreerCli
from telegram_app.common import BridgeError, _http_url
from telegram_app.state import StateStore
from telegram_app.tokenstore import TokenStore

from .api import Settings
from .auth import Auth, Error

ROOT = Path(__file__).resolve().parent.parent
ASSETS = {"/app.js": "app.js", "/app.css": "app.css", "/boot.js": "boot.js"}
CSRF_HEADER = "X-Telegram-Settings"
CSP = "default-src 'self'; script-src 'self'; style-src 'self'; connect-src 'self'; frame-ancestors 'none'; base-uri 'self'; form-action 'self'"


class Application:
    def __init__(self, settings: Settings, auth: Auth | None = None) -> None:
        self.settings, self.auth = settings, auth
        self.lock = threading.Lock()

    def identity_provider(self) -> Auth:
        with self.lock:
            if self.auth is None:
                service_id = os.environ.get("TELEGRAM_SETTINGS_SERVICE_ID")
                public_url = os.environ.get("TELEGRAM_SETTINGS_PUBLIC_URL")
                workspace = os.environ.get("TELEGRAM_SETTINGS_WORKSPACE_ID") or os.environ.get("TREER_WORKSPACE_ID")
                if not service_id or not public_url:
                    # Discover only this Managed App's own service, never a caller-supplied audience.
                    try:
                        result = subprocess.run([os.environ.get("TREER_CLI", "treer"), "app", "list"], capture_output=True, text=True, timeout=10, check=True)
                        app = next(a for a in json.loads(result.stdout)["apps"] if a["runtime_agent_id"] == os.environ.get("TREER_AGENT_ID"))
                        service_id, workspace = app["service_id"], app["workspace_id"]
                        public_url = app.get("public_url") or (os.environ["TELEGRAM_SETTINGS_PROXY_URL"].rstrip("/") + f"/api/workspaces/{workspace}/apps/{app['app_id']}/ui/")
                    except (OSError, ValueError, KeyError, StopIteration, subprocess.SubprocessError):
                        raise Error(503, "app_not_ready", "Managed App identity discovery unavailable") from None
                if not workspace or not public_url:
                    raise Error(503, "app_not_configured", "Workspace and public URL are required")
                self.auth = Auth(os.environ["TELEGRAM_SETTINGS_PROXY_URL"], service_id, workspace, public_url)
            return self.auth


class Handler(BaseHTTPRequestHandler):
    server_version = "TelegramSettings"

    def log_message(self, *_args: Any) -> None:
        pass

    def do_GET(self) -> None:  # noqa: N802
        self.dispatch("GET")

    def do_HEAD(self) -> None:  # noqa: N802
        self.dispatch("HEAD")

    def do_POST(self) -> None:  # noqa: N802
        self.dispatch("POST")

    def do_PUT(self) -> None:  # noqa: N802
        self.dispatch("PUT")

    def do_DELETE(self) -> None:  # noqa: N802
        self.dispatch("DELETE")

    def body(self) -> dict[str, Any]:
        try:
            length = int(self.headers.get("Content-Length", "0"))
        except ValueError:
            raise Error(400, "invalid_body", "Content-Length required") from None
        if not 0 <= length <= 8192:
            raise Error(413 if length > 0 else 400, "body_too_large", "Request body must be at most 8192 bytes")
        if length and self.headers.get("Content-Type", "").split(";")[0] != "application/json":
            raise Error(415, "invalid_content_type", "application/json required")
        try:
            value = json.loads(self.rfile.read(length) or b"{}")
        except (ValueError, UnicodeError):
            raise Error(400, "invalid_body", "JSON object required") from None
        if not isinstance(value, dict):
            raise Error(400, "invalid_body", "JSON object required")
        return value

    def dispatch(self, method: str) -> None:
        try:
            path = urllib.parse.urlsplit(self.path).path
            app: Application = self.server.application  # type: ignore[attr-defined]
            if method in ("GET", "HEAD") and path == "/":
                self.reply(200, (ROOT / "dist/index.html").read_bytes(), "text/html; charset=utf-8")
            elif method in ("GET", "HEAD") and path in ASSETS:
                file = ROOT / "dist" / ASSETS[path]
                self.reply(200, file.read_bytes(), mimetypes.guess_type(str(file))[0] or "application/octet-stream")
            elif method in ("GET", "HEAD") and path == "/health":
                self.json(200, {"status": "ok", "service": "telegram-settings"})
            elif path.startswith("/v1/"):
                auth = app.identity_provider()
                principal = auth.authenticate(self.headers)
                if method not in ("GET", "HEAD"):
                    auth.check_browser_mutation(self.headers, principal, CSRF_HEADER)
                self.api(app.settings, method, path, principal)
            else:
                raise Error(404, "route_not_found", "Route not found")
        except Error as exc:
            self.json(exc.status, {"error": {"code": exc.code, "message": exc.message}})
        except (BrokenPipeError, ConnectionResetError):
            pass
        except Exception as exc:     # never echo exception text: it could carry a token-bearing URL
            print(f"Telegram settings request failed: {type(exc).__name__}", file=sys.stderr, flush=True)
            self.json(500, {"error": {"code": "internal_error", "message": "Operation failed"}})

    def api(self, settings: Settings, method: str, path: str, principal: dict[str, Any]) -> None:
        route = (method, path)
        if route == ("GET", "/v1/status"):
            return self.json(200, settings.status(principal))
        if method == "GET":
            raise Error(404, "route_not_found", "Route not found")
        data = self.body()
        if route == ("PUT", "/v1/token"):
            return self.json(200, settings.set_token(principal, data.get("token")))
        if route == ("DELETE", "/v1/token"):
            return self.json(200, settings.clear_token(principal))
        if route == ("POST", "/v1/pairing"):
            return self.json(201, settings.create_pairing(principal, data.get("poc")))
        if route == ("DELETE", "/v1/pairing"):
            return self.json(200, settings.cancel_pairing(principal))
        if route == ("PUT", "/v1/bindings"):
            return self.json(200, settings.set_poc(principal, data.get("conversation"), data.get("to"), data.get("wake_agent")))
        if route in (("POST", "/v1/bindings/enable"), ("POST", "/v1/bindings/disable")):
            return self.json(200, settings.toggle_binding(principal, data.get("conversation"), path.endswith("enable")))
        if route == ("PUT", "/v1/users"):
            return self.json(200, settings.set_user(principal, data.get("user_id"), data.get("label"), data.get("enabled")))
        if route in (("POST", "/v1/pause"), ("POST", "/v1/resume")):
            return self.json(200, settings.set_paused(principal, path.endswith("pause")))
        raise Error(404, "route_not_found", "Route not found")

    def json(self, status: int, value: Any) -> None:
        self.reply(status, json.dumps(value, ensure_ascii=False).encode(), "application/json; charset=utf-8")

    def reply(self, status: int, content: bytes, media: str) -> None:
        self.send_response(status)
        self.send_header("Content-Type", media)
        self.send_header("Content-Length", str(len(content)))
        self.send_header("Cache-Control", "no-store")
        self.send_header("X-Content-Type-Options", "nosniff")
        self.send_header("Referrer-Policy", "no-referrer")
        self.send_header("Content-Security-Policy", CSP)
        self.end_headers()
        if self.command != "HEAD":
            self.wfile.write(content)


class Server(ThreadingHTTPServer):
    daemon_threads = True

    def __init__(self, address: tuple[str, int], application: Application) -> None:
        self.application = application
        super().__init__(address, Handler)


def load_app_config() -> tuple[dict[str, Any], Path, str]:
    config_path, state_value = os.environ.get("TREER_APP_CONFIG"), os.environ.get("TREER_APP_STATE_DIR")
    if not config_path or not state_value:
        raise ValueError("TREER_APP_CONFIG and TREER_APP_STATE_DIR are required")
    with open(config_path, "rb") as handle:
        value = json.load(handle)
    if not isinstance(value, dict):
        raise ValueError("config must be a JSON object")
    return value, Path(state_value), os.environ.get("TREER_CLI", "treer")


def build_settings(config: dict[str, Any], state_dir: Path, cli_path: str, *, bot_factory: Any = None) -> Settings:
    extra = config.get("settings", {})
    if not isinstance(extra, dict) or set(extra) - {"admin_agent_ids", "default_poc"}:
        raise ValueError("settings supports only admin_agent_ids and default_poc")
    admins = extra.get("admin_agent_ids", [])
    if not isinstance(admins, list) or not all(isinstance(a, str) and a for a in admins):
        raise ValueError("settings.admin_agent_ids must be a list of Agent IDs")
    base = _http_url(config.get("api_base_url", "https://api.telegram.org"))
    state = StateStore(state_dir / "telegram-state.sqlite3")
    return Settings(state, TokenStore(state_dir), TreerCli(cli_path), bot_factory or (lambda token: BotApi(base, token, timeout_seconds=10)),
                    admin_agent_ids=frozenset(admins), default_poc=str(extra.get("default_poc", "")))


def main(argv: list[str] | None = None) -> int:
    os.umask(0o077)
    parser = argparse.ArgumentParser(description="Telegram bridge settings App")
    parser.add_argument("--proxy-url", help="trusted Treer Proxy public URL (default: $TELEGRAM_SETTINGS_PROXY_URL)")
    parser.add_argument("--listen", default=os.environ.get("TELEGRAM_SETTINGS_LISTEN", "127.0.0.1:9441"))
    arguments = parser.parse_args(argv)
    if arguments.proxy_url:
        os.environ["TELEGRAM_SETTINGS_PROXY_URL"] = arguments.proxy_url
    proxy = urllib.parse.urlsplit(os.environ.get("TELEGRAM_SETTINGS_PROXY_URL", ""))
    if proxy.scheme not in ("http", "https") or not proxy.netloc or proxy.username or proxy.password:
        raise ValueError("--proxy-url / TELEGRAM_SETTINGS_PROXY_URL must be an HTTP(S) URL without credentials")
    config, state_dir, cli_path = load_app_config()
    state_dir.mkdir(parents=True, exist_ok=True, mode=0o700)
    host, port = arguments.listen.rsplit(":", 1)
    server = Server((host, int(port)), Application(build_settings(config, state_dir, cli_path)))
    print(f"Telegram settings listening on {host}:{server.server_port}", flush=True)
    server.serve_forever()
    return 0
