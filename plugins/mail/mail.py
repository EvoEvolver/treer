#!/usr/bin/env python3
"""Treer Mail HTTP compatibility surface implemented only through treer CLI calls."""

from __future__ import annotations

import hashlib
import http.cookies
import json
import mimetypes
import os
import secrets
import sqlite3
import subprocess
import sys
import threading
import time
import urllib.parse
from dataclasses import dataclass
from datetime import datetime, timezone
from http import HTTPStatus
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Any


SESSION_COOKIE = "treer_mail_session"
MAX_REQUEST_BYTES = 128 * 1024
MAX_MESSAGE_BODY_BYTES = 32 * 1024
MAX_RECIPIENTS = 32
MAX_CONTEXTS = 32
MAX_PAGE_SIZE = 100
CLI_TIMEOUT_SECONDS = 125


class MailError(Exception):
    def __init__(self, status: int, message: str, code: str = "mail_request_failed") -> None:
        super().__init__(message)
        self.status = status
        self.message = message
        self.code = code


class CliError(Exception):
    def __init__(self, code: str, message: str) -> None:
        super().__init__(message)
        self.code = code
        self.message = message


@dataclass(frozen=True)
class Config:
    listen_host: str
    listen_port: int
    service_id: str
    public_url: str
    proxy_public_url: str
    web_dir: Path
    database_path: Path

    @property
    def callback_url(self) -> str:
        return urllib.parse.urljoin(self.public_url, "api/auth/callback")

    @property
    def secure_cookie(self) -> bool:
        return urllib.parse.urlsplit(self.public_url).scheme == "https"


@dataclass(frozen=True)
class BrowserSession:
    token_hash: str
    capability: str
    workspace_id: str
    service_id: str
    user_id: str
    preferred_name: str
    role: str
    expires_at: float


class TreerCli:
    def __init__(self, executable: str) -> None:
        self.executable = executable

    def run(
        self,
        arguments: list[str],
        *,
        stdin: str | None = None,
        human_session: str | None = None,
    ) -> dict[str, Any]:
        environment = os.environ.copy()
        if human_session:
            environment["TREER_PLUGIN_HUMAN_SESSION"] = human_session
        else:
            environment.pop("TREER_PLUGIN_HUMAN_SESSION", None)
        try:
            completed = subprocess.run(
                [self.executable, *arguments],
                input=stdin,
                capture_output=True,
                text=True,
                timeout=CLI_TIMEOUT_SECONDS,
                check=False,
                env=environment,
            )
        except (OSError, subprocess.TimeoutExpired) as error:
            raise CliError("plugin_cli_unavailable", "Treer CLI is unavailable") from error
        if completed.returncode != 0:
            code = "plugin_cli_failed"
            message = "Treer rejected the request"
            try:
                failure = json.loads(completed.stderr.strip())
                error = failure.get("error", {})
                code = str(error.get("code") or code)
                message = str(error.get("message") or message)
            except (TypeError, ValueError):
                pass
            raise CliError(code, message)
        try:
            value = json.loads(completed.stdout)
        except ValueError as error:
            raise CliError("plugin_cli_invalid_response", "Treer CLI returned invalid JSON") from error
        if not isinstance(value, dict):
            raise CliError("plugin_cli_invalid_response", "Treer CLI returned an invalid object")
        return value


class SessionStore:
    def __init__(self, path: Path) -> None:
        self.path = path
        self._lock = threading.Lock()
        path.parent.mkdir(parents=True, exist_ok=True)
        with self._connect() as connection:
            connection.executescript(
                """
                PRAGMA journal_mode = WAL;
                PRAGMA foreign_keys = ON;
                CREATE TABLE IF NOT EXISTS pending_oauth (
                    state_hash TEXT PRIMARY KEY,
                    return_path TEXT NOT NULL,
                    expires_at REAL NOT NULL
                );
                CREATE TABLE IF NOT EXISTS browser_sessions (
                    token_hash TEXT PRIMARY KEY,
                    capability TEXT NOT NULL,
                    workspace_id TEXT NOT NULL,
                    service_id TEXT NOT NULL,
                    user_id TEXT NOT NULL,
                    preferred_name TEXT NOT NULL,
                    role TEXT NOT NULL,
                    expires_at REAL NOT NULL,
                    created_at REAL NOT NULL
                );
                CREATE INDEX IF NOT EXISTS browser_sessions_expiry
                    ON browser_sessions(expires_at);
                """
            )

    def _connect(self) -> sqlite3.Connection:
        connection = sqlite3.connect(self.path, timeout=10)
        connection.row_factory = sqlite3.Row
        return connection

    def save_oauth(self, state: str, return_path: str, expires_at: float) -> None:
        now = time.time()
        with self._lock, self._connect() as connection:
            connection.execute("DELETE FROM pending_oauth WHERE expires_at <= ?", (now,))
            connection.execute(
                "INSERT INTO pending_oauth(state_hash, return_path, expires_at) VALUES (?, ?, ?)",
                (_secret_hash(state), return_path, expires_at),
            )

    def consume_oauth(self, state: str) -> str | None:
        now = time.time()
        state_hash = _secret_hash(state)
        with self._lock, self._connect() as connection:
            connection.execute("BEGIN IMMEDIATE")
            row = connection.execute(
                "SELECT return_path FROM pending_oauth WHERE state_hash = ? AND expires_at > ?",
                (state_hash, now),
            ).fetchone()
            connection.execute("DELETE FROM pending_oauth WHERE state_hash = ?", (state_hash,))
            return str(row["return_path"]) if row else None

    def save_session(self, raw_token: str, capability: str, session: dict[str, Any]) -> BrowserSession:
        principal = _object(session.get("principal"), "OAuth session principal")
        expires_at = _timestamp(session.get("expires_at"), "OAuth session expiry")
        record = BrowserSession(
            token_hash=_secret_hash(raw_token),
            capability=_bounded_string(capability, "session capability", 512),
            workspace_id=_bounded_string(session.get("workspace_id"), "workspace ID", 256),
            service_id=_bounded_string(session.get("service_id"), "service ID", 256),
            user_id=_bounded_string(principal.get("id"), "user ID", 256),
            preferred_name=_bounded_string(principal.get("name"), "user name", 256),
            role=_bounded_string(principal.get("role") or "member", "user role", 64),
            expires_at=expires_at,
        )
        with self._lock, self._connect() as connection:
            connection.execute("DELETE FROM browser_sessions WHERE expires_at <= ?", (time.time(),))
            connection.execute(
                """INSERT INTO browser_sessions(
                       token_hash, capability, workspace_id, service_id, user_id,
                       preferred_name, role, expires_at, created_at
                   ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)""",
                (
                    record.token_hash,
                    record.capability,
                    record.workspace_id,
                    record.service_id,
                    record.user_id,
                    record.preferred_name,
                    record.role,
                    record.expires_at,
                    time.time(),
                ),
            )
        return record

    def session(self, raw_token: str) -> BrowserSession | None:
        with self._lock, self._connect() as connection:
            row = connection.execute(
                """SELECT token_hash, capability, workspace_id, service_id, user_id,
                          preferred_name, role, expires_at
                   FROM browser_sessions WHERE token_hash = ? AND expires_at > ?""",
                (_secret_hash(raw_token), time.time()),
            ).fetchone()
        return BrowserSession(**dict(row)) if row else None

    def update_principal(self, record: BrowserSession, name: str, role: str) -> BrowserSession:
        updated = BrowserSession(
            token_hash=record.token_hash,
            capability=record.capability,
            workspace_id=record.workspace_id,
            service_id=record.service_id,
            user_id=record.user_id,
            preferred_name=name,
            role=role,
            expires_at=record.expires_at,
        )
        with self._lock, self._connect() as connection:
            connection.execute(
                "UPDATE browser_sessions SET preferred_name = ?, role = ? WHERE token_hash = ?",
                (name, role, record.token_hash),
            )
        return updated

    def delete_session(self, raw_token: str) -> None:
        with self._lock, self._connect() as connection:
            connection.execute(
                "DELETE FROM browser_sessions WHERE token_hash = ?", (_secret_hash(raw_token),)
            )


class MailApplication:
    def __init__(self, config: Config, cli: TreerCli, sessions: SessionStore) -> None:
        self.config = config
        self.cli = cli
        self.sessions = sessions

    def start_oauth(self, return_to: str) -> str:
        return_path = _local_return_path(return_to)
        response = self.cli.run(
            [
                "plugin",
                "auth",
                "start",
                "--service",
                self.config.service_id,
                "--redirect-uri",
                self.config.callback_url,
            ]
        )
        authorize_url = _bounded_string(response.get("authorize_url"), "authorize URL", 8192)
        parsed = urllib.parse.urlsplit(authorize_url)
        state = urllib.parse.parse_qs(parsed.query).get("state", [""])[0]
        if parsed.scheme not in {"http", "https"} or not parsed.netloc or not state:
            raise MailError(502, "Treer returned an invalid authorization URL", "oauth_start_invalid")
        expires_at = _timestamp(response.get("expires_at"), "OAuth state expiry")
        self.sessions.save_oauth(state, return_path, expires_at)
        return authorize_url

    def finish_oauth(self, code: str, state: str) -> tuple[str, str, int]:
        code = _bounded_string(code, "OAuth code", 4096)
        state = _bounded_string(state, "OAuth state", 4096)
        return_path = self.sessions.consume_oauth(state)
        if return_path is None:
            raise MailError(401, "OAuth state is invalid or expired", "oauth_state_invalid")
        response = self.cli.run(
            [
                "plugin",
                "auth",
                "exchange",
                "--service",
                self.config.service_id,
                "--code",
                code,
                "--state",
                state,
            ]
        )
        capability = _bounded_string(response.get("session_capability"), "session capability", 512)
        raw_token = "mas_" + secrets.token_urlsafe(48)
        record = self.sessions.save_session(
            raw_token, capability, _object(response.get("session"), "OAuth session")
        )
        if record.service_id != self.config.service_id:
            self.sessions.delete_session(raw_token)
            raise MailError(401, "OAuth session has the wrong service", "oauth_session_invalid")
        max_age = max(1, int(record.expires_at - time.time()))
        return raw_token, return_path, max_age

    def authenticated_session(self, raw_token: str | None) -> BrowserSession:
        if not raw_token:
            raise MailError(401, "Mail login required", "mail_login_required")
        record = self.sessions.session(raw_token)
        if record is None:
            raise MailError(401, "Mail login required", "mail_login_required")
        try:
            response = self.cli.run(["human", "list"], human_session=record.capability)
        except CliError as error:
            if error.code == "plugin_session_invalid":
                self.sessions.delete_session(raw_token)
            raise
        humans = _list(response.get("humans"), "human directory")
        for item in humans:
            human = _object(item, "human directory item")
            if human.get("user_id") == record.user_id:
                name = _bounded_string(human.get("preferred_name"), "user name", 256)
                role = _bounded_string(human.get("role"), "user role", 64)
                if name != record.preferred_name or role != record.role:
                    return self.sessions.update_principal(record, name, role)
                return record
        self.sessions.delete_session(raw_token)
        raise MailError(401, "Mail login required", "mail_membership_removed")

    def directory(self, record: BrowserSession) -> dict[str, Any]:
        human_response = self.cli.run(["human", "list"], human_session=record.capability)
        agent_response = self.cli.run(["agent", "list"], human_session=record.capability)
        principals: list[dict[str, Any]] = []
        for value in _list(agent_response.get("agents"), "Agent directory"):
            agent = _object(value, "Agent directory item")
            principals.append(
                {
                    "kind": "agent",
                    "id": _bounded_string(agent.get("agent_id"), "Agent ID", 256),
                    "name": _bounded_string(agent.get("name"), "Agent name", 256),
                }
            )
        for value in _list(human_response.get("humans"), "human directory"):
            human = _object(value, "human directory item")
            principals.append(
                {
                    "kind": "human",
                    "id": _bounded_string(human.get("user_id"), "user ID", 256),
                    "name": _bounded_string(human.get("preferred_name"), "user name", 256),
                    "role": _bounded_string(human.get("role"), "user role", 64),
                }
            )
        return {"principals": principals}

    def send_message(self, record: BrowserSession, request: dict[str, Any], key: str | None) -> dict[str, Any]:
        recipients = _string_list(request.get("recipients", []), "recipients", MAX_RECIPIENTS)
        contexts = _string_list(request.get("context_ids", []), "context IDs", MAX_CONTEXTS)
        body = request.get("body")
        if not isinstance(body, str) or not body.strip() or len(body.encode("utf-8")) > MAX_MESSAGE_BODY_BYTES:
            raise MailError(400, "message body must contain 1-32768 bytes", "message_body_invalid")
        if not recipients:
            raise MailError(400, "message requires 1-32 recipients", "message_recipients_invalid")
        idempotency_key = key or "mail-web-" + secrets.token_hex(16)
        _bounded_string(idempotency_key, "idempotency key", 256)
        arguments = ["message", "send"]
        for recipient in recipients:
            arguments.extend(["--to", recipient])
        for context in contexts:
            arguments.extend(["--context", context])
        arguments.extend(["--idempotency-key", idempotency_key, "--body-file", "-"])
        response = self.cli.run(arguments, stdin=body, human_session=record.capability)
        return {"message": _object(response.get("message"), "sent Message")}

    def recent_messages(self, record: BrowserSession, limit: int) -> dict[str, Any]:
        limit = _page_limit(limit)
        history = self.cli.run(
            ["message", "list", "--limit", str(limit)], human_session=record.capability
        )
        messages = [
            _object(value, "Message")
            for value in _list(history.get("messages"), "Message history")
        ]
        messages = [
            message
            for message in messages
            if any(
                isinstance(recipient, dict)
                and recipient.get("kind") == "human"
                and recipient.get("id") == record.user_id
                for recipient in message.get("recipients", [])
            )
        ]
        received = self.cli.run(
            ["message", "receive", "--limit", str(MAX_PAGE_SIZE)],
            human_session=record.capability,
        )
        visible_ids = {str(message.get("message_id")) for message in messages}
        delivery_ids: list[str] = []
        unread_ids: set[str] = set()
        for value in _list(received.get("deliveries"), "Message deliveries"):
            delivery = _object(value, "Message delivery")
            message = _object(delivery.get("message"), "delivered Message")
            message_id = str(message.get("message_id") or "")
            if message_id in visible_ids:
                delivery_ids.append(_bounded_string(delivery.get("delivery_id"), "delivery ID", 256))
                unread_ids.add(message_id)
        self._ack(record, delivery_ids)
        remaining = _nonnegative_int(
            received.get("remaining_unacknowledged"), "remaining unacknowledged count"
        )
        return {
            "deliveries": [
                {"message": message, "unread": str(message.get("message_id")) in unread_ids}
                for message in messages
            ],
            "remaining_unread": max(0, remaining - len(delivery_ids)),
        }

    def unread_inbox(self, record: BrowserSession, limit: int) -> dict[str, Any]:
        limit = _page_limit(limit)
        response = self.cli.run(
            ["message", "receive", "--limit", str(limit)], human_session=record.capability
        )
        deliveries = [_object(value, "Message delivery") for value in _list(response.get("deliveries"), "Message deliveries")]
        delivery_ids = [
            _bounded_string(delivery.get("delivery_id"), "delivery ID", 256)
            for delivery in deliveries
        ]
        self._ack(record, delivery_ids)
        remaining = _nonnegative_int(
            response.get("remaining_unacknowledged"), "remaining unacknowledged count"
        )
        return {
            "deliveries": [
                {"message": _object(delivery.get("message"), "delivered Message"), "unread": True}
                for delivery in deliveries
            ],
            "remaining_unread": max(0, remaining - len(delivery_ids)),
        }

    def _ack(self, record: BrowserSession, delivery_ids: list[str]) -> None:
        if not delivery_ids:
            return
        self.cli.run(
            [
                "message",
                "ack",
                *delivery_ids,
                "--operation-id",
                "mail-read-" + secrets.token_hex(16),
            ],
            human_session=record.capability,
        )

    def logout(self, raw_token: str | None) -> None:
        if not raw_token:
            return
        record = self.sessions.session(raw_token)
        if record is not None:
            try:
                self.cli.run(["plugin", "auth", "revoke", record.capability])
            except CliError as error:
                if error.code != "plugin_session_invalid":
                    raise
        self.sessions.delete_session(raw_token)


class MailServer(ThreadingHTTPServer):
    daemon_threads = True

    def __init__(self, address: tuple[str, int], application: MailApplication) -> None:
        self.application = application
        super().__init__(address, MailHandler)


class MailHandler(BaseHTTPRequestHandler):
    server: MailServer
    protocol_version = "HTTP/1.1"

    def do_GET(self) -> None:  # noqa: N802
        self._dispatch("GET")

    def do_HEAD(self) -> None:  # noqa: N802
        self._dispatch("HEAD")

    def do_POST(self) -> None:  # noqa: N802
        self._dispatch("POST")

    def log_message(self, _format: str, *args: object) -> None:
        return

    def _dispatch(self, method: str) -> None:
        try:
            parsed = urllib.parse.urlsplit(self.path)
            path = parsed.path
            query = urllib.parse.parse_qs(parsed.query, keep_blank_values=True)
            if path == "/api/health" and method in {"GET", "HEAD"}:
                self._json(200, {"status": "ok", "service": "treer-mail"}, head=method == "HEAD")
            elif path == "/api/config" and method == "GET":
                config = self.server.application.config
                self._json(
                    200,
                    {"service_id": config.service_id, "proxy_public_url": config.proxy_public_url},
                )
            elif path == "/api/auth/start" and method == "GET":
                location = self.server.application.start_oauth(query.get("return_to", ["/"])[0])
                self._redirect(location)
            elif path == "/api/auth/callback" and method == "GET":
                raw, location, max_age = self.server.application.finish_oauth(
                    query.get("code", [""])[0], query.get("state", [""])[0]
                )
                cookie = self._session_cookie(raw, max_age)
                self._redirect(location, cookie)
            elif path == "/api/auth/session" and method == "GET":
                record = self._session()
                self._json(200, _session_response(record))
            elif path == "/api/auth/logout" and method == "POST":
                raw = self._raw_session()
                self.server.application.logout(raw)
                self._empty(204, self._session_cookie("", 0))
            elif path == "/api/directory" and method == "GET":
                record = self._session()
                self._json(200, self.server.application.directory(record))
            elif path == "/api/messages" and method == "POST":
                record = self._session()
                request = self._json_body()
                key = self.headers.get("Idempotency-Key")
                self._json(200, self.server.application.send_message(record, request, key))
            elif path == "/api/messages" and method == "GET":
                record = self._session()
                limit = _query_int(query, "limit", 100)
                self._json(200, self.server.application.recent_messages(record, limit))
            elif path == "/api/inbox" and method == "POST":
                record = self._session()
                request = self._json_body()
                self._json(
                    200,
                    self.server.application.unread_inbox(record, request.get("limit", 50)),
                )
            elif path.startswith("/api/"):
                raise MailError(404, "Mail API route not found", "route_not_found")
            elif method in {"GET", "HEAD"}:
                self._static(path, head=method == "HEAD")
            else:
                raise MailError(405, "method not allowed", "method_not_allowed")
        except CliError as error:
            self._error(_cli_mail_error(error))
        except MailError as error:
            self._error(error)
        except (BrokenPipeError, ConnectionResetError):
            return
        except Exception as error:  # Keep HTTP internals out of client responses.
            print(f"Treer Mail request failed: {type(error).__name__}", file=sys.stderr)
            self._error(MailError(500, "mail service operation failed", "mail_internal_error"))

    def _raw_session(self) -> str | None:
        header = self.headers.get("Cookie")
        if not header:
            return None
        cookies = http.cookies.SimpleCookie()
        try:
            cookies.load(header)
        except http.cookies.CookieError:
            return None
        morsel = cookies.get(SESSION_COOKIE)
        return morsel.value if morsel else None

    def _session(self) -> BrowserSession:
        return self.server.application.authenticated_session(self._raw_session())

    def _json_body(self) -> dict[str, Any]:
        raw_length = self.headers.get("Content-Length")
        if raw_length is None:
            raise MailError(400, "Content-Length is required", "request_body_invalid")
        try:
            length = int(raw_length)
        except ValueError as error:
            raise MailError(400, "Content-Length is invalid", "request_body_invalid") from error
        if length < 0 or length > MAX_REQUEST_BYTES:
            raise MailError(413, "request body is too large", "request_body_too_large")
        try:
            value = json.loads(self.rfile.read(length))
        except (UnicodeDecodeError, ValueError) as error:
            raise MailError(400, "request body must be a JSON object", "request_body_invalid") from error
        return _object(value, "request body")

    def _static(self, request_path: str, *, head: bool) -> None:
        web_root = self.server.application.config.web_dir.resolve()
        relative = urllib.parse.unquote(request_path).lstrip("/") or "index.html"
        candidate = (web_root / relative).resolve()
        try:
            candidate.relative_to(web_root)
        except ValueError:
            raise MailError(404, "not found", "static_not_found")
        if not candidate.is_file():
            candidate = web_root / "index.html"
        if not candidate.is_file():
            raise MailError(503, "Mail frontend is not built", "frontend_unavailable")
        content = candidate.read_bytes()
        content_type = mimetypes.guess_type(candidate.name)[0] or "application/octet-stream"
        self._bytes(200, content_type, content, head=head)

    def _security_headers(self) -> None:
        self.send_header("X-Content-Type-Options", "nosniff")
        self.send_header("Referrer-Policy", "same-origin")
        self.send_header(
            "Content-Security-Policy",
            "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; "
            "img-src 'self' data:; connect-src 'self'; frame-ancestors 'none'",
        )

    def _json(self, status: int, value: dict[str, Any], *, head: bool = False) -> None:
        content = json.dumps(value, separators=(",", ":"), ensure_ascii=False).encode("utf-8")
        self._bytes(status, "application/json; charset=utf-8", content, head=head)

    def _error(self, error: MailError) -> None:
        self._json(error.status, {"error": {"code": error.code, "message": error.message}})

    def _bytes(self, status: int, content_type: str, content: bytes, *, head: bool = False) -> None:
        self.send_response(status)
        self._security_headers()
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(content)))
        self.end_headers()
        if not head:
            self.wfile.write(content)

    def _redirect(self, location: str, cookie: str | None = None) -> None:
        self.send_response(302)
        self._security_headers()
        self.send_header("Location", location)
        if cookie:
            self.send_header("Set-Cookie", cookie)
        self.send_header("Content-Length", "0")
        self.end_headers()

    def _empty(self, status: int, cookie: str | None = None) -> None:
        self.send_response(status)
        self._security_headers()
        if cookie:
            self.send_header("Set-Cookie", cookie)
        self.send_header("Content-Length", "0")
        self.end_headers()

    def _session_cookie(self, raw: str, max_age: int) -> str:
        cookie = http.cookies.SimpleCookie()
        cookie[SESSION_COOKIE] = raw
        morsel = cookie[SESSION_COOKIE]
        morsel["path"] = "/"
        morsel["httponly"] = True
        morsel["samesite"] = "Lax"
        morsel["max-age"] = str(max_age)
        if self.server.application.config.secure_cookie:
            morsel["secure"] = True
        return morsel.OutputString()


def _session_response(record: BrowserSession) -> dict[str, Any]:
    return {
        "workspace_id": record.workspace_id,
        "service_id": record.service_id,
        "user": {
            "kind": "human",
            "id": record.user_id,
            "name": record.preferred_name,
            "role": record.role,
        },
    }


def _cli_mail_error(error: CliError) -> MailError:
    if error.code in {"plugin_session_invalid", "plugin_bridge_agent_required"}:
        return MailError(401, "Mail login required", error.code)
    if error.code in {"policy_denied", "plugin_session_command_denied", "plugin_command_denied"}:
        return MailError(403, error.message, error.code)
    if error.code.endswith("_not_found") or error.code in {"message_recipient_unavailable"}:
        return MailError(404, error.message, error.code)
    if error.code.startswith("message_") or error.code.startswith("plugin_oauth_"):
        return MailError(400, error.message, error.code)
    return MailError(502, "Treer control plane is unavailable", error.code)


def _secret_hash(value: str) -> str:
    return hashlib.sha256(value.encode("utf-8")).hexdigest()


def _object(value: Any, label: str) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise MailError(502, f"Treer returned an invalid {label}", "plugin_cli_invalid_response")
    return value


def _list(value: Any, label: str) -> list[Any]:
    if not isinstance(value, list):
        raise MailError(502, f"Treer returned an invalid {label}", "plugin_cli_invalid_response")
    return value


def _bounded_string(value: Any, label: str, maximum: int) -> str:
    if not isinstance(value, str) or not value or len(value.encode("utf-8")) > maximum:
        raise MailError(400, f"{label} is empty or too long", "request_field_invalid")
    return value


def _string_list(value: Any, label: str, maximum: int) -> list[str]:
    if not isinstance(value, list) or len(value) > maximum:
        raise MailError(400, f"{label} must contain at most {maximum} values", "request_field_invalid")
    result = [_bounded_string(item, label, 256) for item in value]
    if len(set(result)) != len(result):
        raise MailError(400, f"{label} must be unique", "request_field_invalid")
    return result


def _nonnegative_int(value: Any, label: str) -> int:
    if not isinstance(value, int) or isinstance(value, bool) or value < 0:
        raise MailError(502, f"Treer returned an invalid {label}", "plugin_cli_invalid_response")
    return value


def _timestamp(value: Any, label: str) -> float:
    text = _bounded_string(value, label, 128)
    try:
        return datetime.fromisoformat(text.replace("Z", "+00:00")).timestamp()
    except ValueError as error:
        raise MailError(502, f"Treer returned an invalid {label}", "plugin_cli_invalid_response") from error


def _page_limit(value: Any) -> int:
    if not isinstance(value, int) or isinstance(value, bool) or value < 1 or value > MAX_PAGE_SIZE:
        raise MailError(400, "limit must be between 1 and 100", "message_limit_invalid")
    return value


def _query_int(query: dict[str, list[str]], name: str, default: int) -> int:
    raw = query.get(name, [str(default)])[0]
    try:
        return int(raw)
    except ValueError as error:
        raise MailError(400, f"{name} must be an integer", "query_invalid") from error


def _local_return_path(value: str) -> str:
    if not value.startswith("/") or value.startswith("//") or len(value) > 4096 or any(
        ord(character) < 32 for character in value
    ):
        raise MailError(400, "return_to must be a local absolute path", "return_path_invalid")
    return value


def _normalized_url(value: Any, label: str) -> str:
    text = _bounded_string(value, label, 8192)
    parsed = urllib.parse.urlsplit(text)
    if parsed.scheme not in {"http", "https"} or not parsed.netloc or parsed.username or parsed.password:
        raise ValueError(f"{label} must be an absolute HTTP or HTTPS URL without credentials")
    path = parsed.path if parsed.path.endswith("/") else parsed.path + "/"
    return urllib.parse.urlunsplit((parsed.scheme, parsed.netloc, path, "", ""))


def _listen(value: Any) -> tuple[str, int]:
    text = _bounded_string(value, "listen address", 512)
    try:
        host, port_text = text.rsplit(":", 1)
        port = int(port_text)
    except (ValueError, TypeError) as error:
        raise ValueError("listen must use HOST:PORT") from error
    if not host or port < 1 or port > 65535:
        raise ValueError("listen must use HOST:PORT with a valid port")
    return host, port


def load_config() -> Config:
    config_path = os.environ.get("TREER_PLUGIN_CONFIG")
    state_dir = os.environ.get("TREER_PLUGIN_STATE_DIR")
    cli = os.environ.get("TREER_CLI")
    if not config_path or not state_dir or not cli:
        raise ValueError("mail plugin must run through `treer plugin run`")
    with open(config_path, "rb") as handle:
        value = json.load(handle)
    if not isinstance(value, dict):
        raise ValueError("plugin config must contain a JSON object")
    host, port = _listen(value.get("listen", "127.0.0.1:8788"))
    package = Path(__file__).resolve().parent
    web_value = value.get("web_dir", "web/dist")
    if not isinstance(web_value, str) or not web_value:
        raise ValueError("web_dir must be a non-empty path")
    web_dir = Path(web_value)
    if not web_dir.is_absolute():
        web_dir = package / web_dir
    service_id = _bounded_string(value.get("service_id"), "service ID", 128)
    if not service_id.startswith("svc_"):
        raise ValueError("service_id must be a registered Treer service ID")
    return Config(
        listen_host=host,
        listen_port=port,
        service_id=service_id,
        public_url=_normalized_url(value.get("public_url"), "public URL"),
        proxy_public_url=_normalized_url(value.get("proxy_public_url"), "Proxy public URL"),
        web_dir=web_dir,
        database_path=Path(state_dir) / "mail-state.sqlite3",
    )


def main() -> int:
    try:
        config = load_config()
        application = MailApplication(
            config,
            TreerCli(os.environ["TREER_CLI"]),
            SessionStore(config.database_path),
        )
        server = MailServer((config.listen_host, config.listen_port), application)
        print(
            f"Treer Mail listening on {config.listen_host}:{server.server_address[1]}",
            file=sys.stderr,
            flush=True,
        )
        server.serve_forever(poll_interval=0.25)
        return 0
    except KeyboardInterrupt:
        return 0
    except Exception as error:
        print(f"Treer Mail failed to start: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
