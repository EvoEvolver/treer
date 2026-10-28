#!/usr/bin/env python3
"""Treer Mail application backed by the Treer App and Core Message APIs."""

from __future__ import annotations

import base64
import hashlib
import http.cookies
import json
import mimetypes
import os
import re
import secrets
import sqlite3
import subprocess
import sys
import threading
import time
import urllib.parse
import urllib.error
import urllib.request
from dataclasses import dataclass
from datetime import datetime, timezone
from http import HTTPStatus
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Any

from email_route import EmailRoute, treer_token


SESSION_COOKIE = "treer_mail_session"
MAX_REQUEST_BYTES = 128 * 1024
MAX_MESSAGE_BODY_BYTES = 32 * 1024
MAX_RECIPIENTS = 32
MAX_CONTEXTS = 32
MAX_PAGE_SIZE = 100
MAX_EMAIL_BYTES = 4096  # Core's limit for the whole plain-text email
EMAIL_FIELDS = {"to", "to_member", "subject", "text", "message_id", "idempotency_key"}
MESSAGE_ID = re.compile(r"[A-Za-z0-9_.-]{1,128}")
EMAIL_KEY = re.compile(r"[A-Za-z0-9:_.-]{1,128}")
API_TIMEOUT_SECONDS = 125
APP_ROOT = Path(__file__).resolve().parent
RFC3339_NANOSECONDS = re.compile(
    r"^(?P<seconds>\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2})"
    r"(?P<fraction>\.\d+)(?P<zone>Z|[+-]\d{2}:\d{2})$"
)


def root_representation(accept: str, user_agent: str) -> str:
    choices: list[tuple[float, int, str]] = []
    for order, raw_entry in enumerate(accept.split(",")):
        parts = [part.strip().lower() for part in raw_entry.split(";")]
        media_type = parts[0]
        if media_type not in {"text/html", "text/markdown"}:
            continue
        quality = 1.0
        for parameter in parts[1:]:
            if parameter.startswith("q="):
                try:
                    quality = float(parameter[2:])
                except ValueError:
                    quality = 0.0
        if 0 < quality <= 1:
            choices.append((-quality, order, media_type))
    if choices:
        return min(choices)[2]
    return "text/html" if "mozilla/" in user_agent.lower() else "text/markdown"


class MailError(Exception):
    def __init__(self, status: int, message: str, code: str = "mail_request_failed") -> None:
        super().__init__(message)
        self.status = status
        self.message = message
        self.code = code


class AppApiError(Exception):
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
    workspace_id: str | None = None
    browser_origins: tuple[str, ...] = ()
    email_capability: str = "app.email.v1"
    treer_bin: str = "treer"

    @property
    def callback_url(self) -> str:
        return urllib.parse.urljoin(self.public_url, "api/auth/callback")

    @property
    def secure_cookie(self) -> bool:
        return urllib.parse.urlsplit(self.public_url).scheme == "https"


@dataclass(frozen=True)
class BrowserSession:
    token_hash: str
    access_token: str
    workspace_id: str
    service_id: str
    user_id: str
    preferred_name: str
    role: str
    expires_at: float


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *args, **kwargs):
        return None


class TreerAppClient:
    def __init__(self, proxy_public_url: str, service_id: str) -> None:
        self.proxy_public_url = proxy_public_url
        self.service_id = service_id

    def request(
        self,
        method: str,
        path: str,
        *,
        access_token: str | None = None,
        body: dict[str, Any] | None = None,
        form: dict[str, str] | None = None,
    ) -> dict[str, Any]:
        headers = {"Accept": "application/json"}
        data = None
        if access_token:
            headers["Authorization"] = f"Bearer {access_token}"
        if body is not None:
            headers["Content-Type"] = "application/json"
            data = json.dumps(body, separators=(",", ":")).encode("utf-8")
        elif form is not None:
            headers["Content-Type"] = "application/x-www-form-urlencoded"
            data = urllib.parse.urlencode(form).encode("ascii")
        request = urllib.request.Request(
            urllib.parse.urljoin(self.proxy_public_url, path.lstrip("/")),
            data=data,
            headers=headers,
            method=method,
        )
        try:
            with urllib.request.build_opener(NoRedirect()).open(request, timeout=API_TIMEOUT_SECONDS) as response:
                payload = response.read(MAX_REQUEST_BYTES)
        except urllib.error.HTTPError as error:
            payload = error.read(MAX_REQUEST_BYTES)
            code = "app_api_failed"
            message = "Treer rejected the request"
            try:
                failure = json.loads(payload)
                error = failure.get("error", {})
                code = str(error.get("code") or code)
                message = str(error.get("message") or message)
            except (TypeError, ValueError):
                pass
            raise AppApiError(code, message)
        except (OSError, urllib.error.URLError) as error:
            raise AppApiError("app_api_unavailable", "Treer API is unavailable") from error
        try:
            value = json.loads(payload)
        except ValueError as error:
            raise AppApiError("app_api_invalid_response", "Treer returned invalid JSON") from error
        if not isinstance(value, dict):
            raise AppApiError("app_api_invalid_response", "Treer returned an invalid object")
        return value


class SessionStore:
    def __init__(self, path: Path) -> None:
        self.path = path
        self._lock = threading.Lock()
        os.umask(0o077)
        path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
        path.parent.chmod(0o700)
        with self._connect() as connection:
            connection.executescript(
                """
                PRAGMA journal_mode = WAL;
                PRAGMA foreign_keys = ON;
                CREATE TABLE IF NOT EXISTS app_pending_oauth (
                    state_hash TEXT PRIMARY KEY,
                    verifier TEXT NOT NULL,
                    return_path TEXT NOT NULL,
                    expires_at REAL NOT NULL
                );
                CREATE TABLE IF NOT EXISTS app_browser_sessions (
                    token_hash TEXT PRIMARY KEY,
                    access_token TEXT NOT NULL,
                    workspace_id TEXT NOT NULL,
                    service_id TEXT NOT NULL,
                    user_id TEXT NOT NULL,
                    preferred_name TEXT NOT NULL,
                    role TEXT NOT NULL,
                    expires_at REAL NOT NULL,
                    created_at REAL NOT NULL
                );
                CREATE INDEX IF NOT EXISTS app_browser_sessions_expiry
                    ON app_browser_sessions(expires_at);
                -- Removed email-notification feature: drop its stored addresses.
                DROP TABLE IF EXISTS notify_attempts;
                DROP TABLE IF EXISTS notify_batches;
                DROP TABLE IF EXISTS notify_pending;
                DROP TABLE IF EXISTS notify_settings;
                """
            )

        with self._connect() as connection:
            cols = {row[1] for row in connection.execute("PRAGMA table_info(app_pending_oauth)")}
            if "browser_hash" not in cols:
                connection.execute("ALTER TABLE app_pending_oauth ADD COLUMN browser_hash TEXT")
        path.chmod(0o600)

    def _connect(self) -> sqlite3.Connection:
        connection = sqlite3.connect(self.path, timeout=10)
        connection.row_factory = sqlite3.Row
        return connection

    def save_oauth(self, state: str, verifier: str, return_path: str, expires_at: float, browser: str = "") -> None:
        now = time.time()
        with self._lock, self._connect() as connection:
            connection.execute("DELETE FROM app_pending_oauth WHERE expires_at <= ?", (now,))
            connection.execute(
                "INSERT INTO app_pending_oauth(state_hash, verifier, return_path, expires_at, browser_hash) VALUES (?, ?, ?, ?, ?)",
                (_secret_hash(state), verifier, return_path, expires_at, _secret_hash(browser) if browser else None),
            )

    def consume_oauth(self, state: str, browser: str = "") -> tuple[str, str] | None:
        if not browser:
            return None
        now = time.time()
        state_hash = _secret_hash(state)
        with self._lock, self._connect() as connection:
            connection.execute("BEGIN IMMEDIATE")
            row = connection.execute(
                "SELECT verifier, return_path FROM app_pending_oauth WHERE state_hash = ? AND browser_hash = ? AND expires_at > ?",
                (state_hash, _secret_hash(browser), now),
            ).fetchone()
            if row:
                connection.execute("DELETE FROM app_pending_oauth WHERE state_hash = ?", (state_hash,))
            return (str(row["verifier"]), str(row["return_path"])) if row else None

    def save_session(
        self, raw_token: str, access_token: str, claims: dict[str, Any], expires_at: float
    ) -> BrowserSession:
        record = BrowserSession(
            token_hash=_secret_hash(raw_token),
            access_token=_bounded_string(access_token, "access token", 8192),
            workspace_id=_bounded_string(claims.get("workspace_id"), "workspace ID", 256),
            service_id=_bounded_string(claims.get("service_id"), "service ID", 256),
            user_id=_bounded_string(claims.get("sub"), "user ID", 256),
            preferred_name=_bounded_string(claims.get("name"), "user name", 256),
            role=_bounded_string(claims.get("role") or "member", "user role", 64),
            expires_at=expires_at,
        )
        with self._lock, self._connect() as connection:
            connection.execute("DELETE FROM app_browser_sessions WHERE expires_at <= ?", (time.time(),))
            connection.execute(
                """INSERT INTO app_browser_sessions(
                       token_hash, access_token, workspace_id, service_id, user_id,
                       preferred_name, role, expires_at, created_at
                   ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)""",
                (
                    record.token_hash,
                    record.access_token,
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
                """SELECT token_hash, access_token, workspace_id, service_id, user_id,
                          preferred_name, role, expires_at
                   FROM app_browser_sessions WHERE token_hash = ? AND expires_at > ?""",
                (_secret_hash(raw_token), time.time()),
            ).fetchone()
        return BrowserSession(**dict(row)) if row else None

    def update_principal(self, record: BrowserSession, name: str, role: str) -> BrowserSession:
        updated = BrowserSession(
            token_hash=record.token_hash,
            access_token=record.access_token,
            workspace_id=record.workspace_id,
            service_id=record.service_id,
            user_id=record.user_id,
            preferred_name=name,
            role=role,
            expires_at=record.expires_at,
        )
        with self._lock, self._connect() as connection:
            connection.execute(
                "UPDATE app_browser_sessions SET preferred_name = ?, role = ? WHERE token_hash = ?",
                (name, role, record.token_hash),
            )
        return updated

    def delete_session(self, raw_token: str) -> None:
        with self._lock, self._connect() as connection:
            connection.execute(
                "DELETE FROM app_browser_sessions WHERE token_hash = ?", (_secret_hash(raw_token),)
            )


class MailApplication:
    def __init__(self, config: Config, api: TreerAppClient, sessions: SessionStore) -> None:
        self.config = config
        self.api = api
        self.sessions = sessions
        self.email = EmailRoute(config.proxy_public_url, config.service_id, config.email_capability,
                                lambda: treer_token(config.treer_bin, config.service_id))

    def verify_agent(self, token: str | None) -> dict[str, Any]:
        """An Agent calling Mail with its workload identity."""
        verified = self.api.request("POST", "/.treer/apps/identity/verify",
                                    body={"token": token or "", "audience": self.config.service_id})
        claims = verified.get("claims") or {}
        if (verified.get("active") is not True or claims.get("principal_kind") != "agent"
                or claims.get("service_id") != self.config.service_id or not claims.get("sub")
                or (self.config.workspace_id and claims.get("workspace_id") != self.config.workspace_id)):
            raise MailError(401, "Invalid Treer identity", "mail_identity_invalid")
        return claims

    def reply_link(self, message_id: str | None) -> str:
        if message_id is None:
            return self.config.public_url
        return self.config.public_url + "?" + urllib.parse.urlencode({"message": message_id})

    def send_email(self, caller: tuple[str, str, str], request: dict[str, Any]) -> tuple[int, dict[str, Any]]:
        """Email a person a pointer to a Core Message (or the inbox) through
        Core. Returns the HTTP status and body; Mail never resends by itself."""
        unknown = sorted(set(request) - EMAIL_FIELDS)
        if unknown:
            raise MailError(400, f"unknown field {unknown[0]}", "request_field_invalid")
        to, member = request.get("to"), request.get("to_member")
        if (to is None) == (member is None):
            raise MailError(400, "exactly one of to or to_member is required", "email_recipient_invalid")
        recipient = ({"to": _bounded_string(to, "to", 254)} if to is not None
                     else {"to_member": _bounded_string(member, "to_member", 256)})
        subject = request.get("subject")
        if (not isinstance(subject, str) or not subject.strip() or len(subject.encode("utf-8")) > 200
                or any(ord(c) < 32 or ord(c) == 127 for c in subject)):
            raise MailError(400, "subject must be 1-200 bytes on one line", "email_subject_invalid")
        text = request.get("text")
        if not isinstance(text, str) or not text.strip():
            raise MailError(400, "text must not be empty", "email_text_invalid")
        message_id = request.get("message_id")
        if message_id is not None and (not isinstance(message_id, str) or not MESSAGE_ID.fullmatch(message_id)):
            raise MailError(400, "message_id is not a Core Message ID", "email_message_id_invalid")
        supplied = request.get("idempotency_key")
        if supplied is not None:
            supplied = _bounded_string(supplied, "idempotency key", 128)
        kind, caller_id, name = caller
        link = self.reply_link(message_id)
        action = "Read and reply" if message_id else "Open Treer Mail"
        body = (f"{text.rstrip()}\n\n{action}: {link}\n"
                f"Sent by {name} through Treer Mail. Replying needs a Treer account in this workspace.\n")
        if len(body.encode("utf-8")) > MAX_EMAIL_BYTES:
            raise MailError(400, f"text is too long: the email with its reply link may have at most {MAX_EMAIL_BYTES} bytes",
                            "email_text_invalid")
        material = [kind, caller_id, json.dumps(recipient, sort_keys=True)]
        material += ["key", supplied] if supplied else ["message", message_id] if message_id else ["content", subject, text]
        key = "mail-" + hashlib.sha256("\0".join(material).encode("utf-8")).hexdigest()[:40]
        if not self.email.available():
            raise MailError(503, "this Treer does not offer the App email route", "email_unavailable")
        outcome = self.email.send(recipient, subject, body, key)
        print(f"Treer Mail email {kind}:{caller_id} -> {next(iter(recipient.values()))} {outcome.result} {outcome.code}",
              file=sys.stderr, flush=True)
        if outcome.result == "sent":
            return 200, {"email_id": outcome.email_id, "status": "sent", "idempotency_key": key}
        if outcome.result == "uncertain":
            return 202, {"email_id": None, "status": "uncertain", "code": outcome.code, "idempotency_key": key}
        passthrough = {400, 403, 404, 409, 413, 422, 429, 503}
        status = (outcome.http_status if outcome.http_status in passthrough
                  else 503 if outcome.result == "retryable" else 502)
        return status, {"error": {"code": outcome.code, "message": outcome.message or "Core did not send the email",
                                  "retryable": outcome.result == "retryable"}, "idempotency_key": key}

    def email_status(self, key: str) -> tuple[int, dict[str, Any]]:
        if not EMAIL_KEY.fullmatch(key):
            raise MailError(400, "invalid idempotency key", "email_key_invalid")
        try:
            return self.email.status(key)
        except (OSError, RuntimeError, subprocess.SubprocessError):
            raise MailError(503, "Core App email route is unavailable", "email_route_unreachable")

    def start_oauth(self, return_to: str, browser: str = "") -> str:
        return_path = _local_return_path(return_to)
        state = secrets.token_urlsafe(32)
        verifier = secrets.token_urlsafe(48)
        challenge = base64.urlsafe_b64encode(
            hashlib.sha256(verifier.encode("ascii")).digest()
        ).rstrip(b"=").decode("ascii")
        query = urllib.parse.urlencode(
            {
                "response_type": "code",
                "client_id": self.config.service_id,
                "redirect_uri": self.config.callback_url,
                "state": state,
                "code_challenge": challenge,
                "code_challenge_method": "S256",
            }
        )
        self.sessions.save_oauth(state, verifier, return_path, time.time() + 600, browser)
        return urllib.parse.urljoin(
            self.config.proxy_public_url, f"api/apps/oauth/authorize?{query}"
        )

    def finish_oauth(self, code: str, state: str, browser: str = "") -> tuple[str, str, int]:
        code = _bounded_string(code, "OAuth code", 4096)
        state = _bounded_string(state, "OAuth state", 4096)
        pending = self.sessions.consume_oauth(state, browser)
        if pending is None:
            raise MailError(401, "OAuth state is invalid or expired", "oauth_state_invalid")
        verifier, return_path = pending
        response = self.api.request(
            "POST",
            "/api/apps/oauth/token",
            form={
                "grant_type": "authorization_code",
                "code": code,
                "client_id": self.config.service_id,
                "redirect_uri": self.config.callback_url,
                "code_verifier": verifier,
            },
        )
        access_token = _bounded_string(response.get("access_token"), "access token", 8192)
        expires_at = _timestamp(response.get("expires_at"), "access token expiry")
        claims = self.verify_human(access_token)
        raw_token = "mas_" + secrets.token_urlsafe(48)
        record = self.sessions.save_session(raw_token, access_token, claims, expires_at)
        if record.service_id != self.config.service_id:
            self.sessions.delete_session(raw_token)
            raise MailError(401, "OAuth session has the wrong service", "oauth_session_invalid")
        max_age = max(1, int(record.expires_at - time.time()))
        return raw_token, return_path, max_age

    def verify_human(self, token):
        if not token:
            raise MailError(401, "Treer human identity required", "mail_identity_invalid")
        verified = self.api.request("POST", "/.treer/apps/identity/verify",
                                    body={"token": token, "audience": self.config.service_id})
        claims = verified.get("claims") or {}
        if (verified.get("active") is not True or claims.get("principal_kind") != "human"
                or claims.get("service_id") != self.config.service_id or not claims.get("sub")
                or not claims.get("workspace_id") or claims.get("exp", 0) <= time.time()
                or (self.config.workspace_id and claims["workspace_id"] != self.config.workspace_id)):
            raise MailError(401, "Invalid Treer human identity", "mail_identity_invalid")
        directory = self.api.request("GET", f"/api/apps/{self.config.service_id}/directory", access_token=token)["principals"]
        principal = next((p for p in directory if p.get("kind") == "human" and p.get("id") == claims["sub"]), None)
        if not principal:
            raise MailError(401, "Human no longer has workspace access", "mail_identity_invalid")
        claims["role"] = principal.get("role", "member")
        return claims

    def authenticated_session(self, raw_token: str | None) -> BrowserSession:
        if not raw_token:
            raise MailError(401, "Mail login required", "mail_login_required")
        record = self.sessions.session(raw_token)
        if record is None:
            raise MailError(401, "Mail login required", "mail_login_required")
        claims = self.verify_human(record.access_token)
        name = _bounded_string(claims.get("name"), "user name", 256)
        role = _bounded_string(claims.get("role") or "member", "user role", 64)
        if (claims.get("sub") != record.user_id or claims.get("service_id") != record.service_id
                or claims.get("workspace_id") != record.workspace_id):
            self.sessions.delete_session(raw_token)
            raise MailError(401, "Mail login required", "mail_identity_changed")
        if name != record.preferred_name or role != record.role:
            return self.sessions.update_principal(record, name, role)
        return record

    def directory(self, record: BrowserSession) -> dict[str, Any]:
        return self.api.request(
            "GET",
            f"/api/apps/{self.config.service_id}/directory",
            access_token=record.access_token,
        )

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
        response = self.api.request(
            "POST",
            f"/api/apps/{self.config.service_id}/messages",
            access_token=record.access_token,
            body={
                "recipients": recipients,
                "context_ids": contexts,
                "body": body,
                "idempotency_key": idempotency_key,
            },
        )
        return {"message": _object(response.get("message"), "sent Message")}

    def message(self, record: BrowserSession, message_id: str) -> dict[str, Any]:
        if not MESSAGE_ID.fullmatch(message_id):
            raise MailError(404, "Message not found", "message_not_found")
        response = self.api.request("GET", f"/api/apps/{self.config.service_id}/messages/{message_id}",
                                    access_token=record.access_token)
        return {"message": _object(response.get("message"), "Message")}

    def recent_messages(self, record: BrowserSession, limit: int) -> dict[str, Any]:
        limit = _page_limit(limit)
        history = self.api.request(
            "GET",
            f"/api/apps/{self.config.service_id}/messages?limit={limit}",
            access_token=record.access_token,
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
        received = self.api.request(
            "POST",
            f"/api/apps/{self.config.service_id}/messages/receive",
            access_token=record.access_token,
            body={"limit": MAX_PAGE_SIZE, "wait_milliseconds": 0},
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
        response = self.api.request(
            "POST",
            f"/api/apps/{self.config.service_id}/messages/receive",
            access_token=record.access_token,
            body={"limit": limit, "wait_milliseconds": 0},
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
        self.api.request(
            "POST",
            f"/api/apps/{self.config.service_id}/messages/ack",
            access_token=record.access_token,
            body={
                "delivery_ids": delivery_ids,
                "operation_id": "mail-read-" + secrets.token_hex(16),
            },
        )

    def logout(self, raw_token: str | None) -> None:
        if not raw_token:
            return
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
            self._mutation = method == "POST"
            if self._mutation:
                # Humans must prove a same-origin browser write; Agent callers
                # (no Origin, workload identity) are checked in _caller().
                config = self.server.application.config
                origin = urllib.parse.urlsplit(config.public_url)._replace(path="", query="", fragment="").geturl()
                allowed = {origin, urllib.parse.urlsplit(config.proxy_public_url)._replace(path="", query="", fragment="").geturl(), *config.browser_origins}
                if ("Origin" in self.headers and self.headers.get("Origin") not in allowed) or self.headers.get("Sec-Fetch-Site") not in (None, "same-origin", "same-site"):
                    raise MailError(403, "Same-origin browser write required", "csrf_denied")
                if self.headers.get("Content-Type", "").split(";")[0] != "application/json":
                    raise MailError(415, "application/json required", "request_body_invalid")
            if path == "/" and method in {"GET", "HEAD"}:
                representation = root_representation(
                    self.headers.get("Accept", ""), self.headers.get("User-Agent", "")
                )
                if representation == "text/html":
                    self._static(path, head=method == "HEAD", vary=True)
                else:
                    self._bytes(
                        200,
                        "text/markdown; charset=utf-8",
                        (APP_ROOT / "AGENT.md").read_bytes(),
                        head=method == "HEAD",
                        vary=True,
                    )
            elif path == "/api/health" and method in {"GET", "HEAD"}:
                self._json(200, {"status": "ok", "service": "treer-mail"}, head=method == "HEAD")
            elif path == "/api/config" and method == "GET":
                config = self.server.application.config
                self._json(
                    200,
                    {"service_id": config.service_id, "proxy_public_url": config.proxy_public_url},
                )
            elif path == "/api/auth/start" and method == "GET":
                browser = secrets.token_urlsafe(32)
                location = self.server.application.start_oauth(query.get("return_to", ["/"])[0], browser)
                self._redirect(location, self._session_cookie(browser, 600).replace(SESSION_COOKIE + "=", "treer_mail_oauth="))
            elif path == "/api/auth/callback" and method == "GET":
                raw, location, max_age = self.server.application.finish_oauth(
                    query.get("code", [""])[0], query.get("state", [""])[0], self._raw_cookie("treer_mail_oauth") or ""
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
            elif path == "/api/email" and method == "POST":
                caller = self._caller()
                self._json(*self.server.application.send_email(caller, self._json_body()))
            elif path.startswith("/api/email/") and method == "GET":
                self._caller()
                key = urllib.parse.unquote(path[len("/api/email/"):])
                self._json(*self.server.application.email_status(key))
            elif path.startswith("/api/messages/") and method == "GET":
                record = self._session()
                self._json(200, self.server.application.message(record, path[len("/api/messages/"):]))
            elif path.startswith("/api/"):
                raise MailError(404, "Mail API route not found", "route_not_found")
            elif method in {"GET", "HEAD"}:
                self._static(path, head=method == "HEAD")
            else:
                raise MailError(405, "method not allowed", "method_not_allowed")
        except AppApiError as error:
            self._error(_app_api_mail_error(error))
        except MailError as error:
            self._error(error)
        except (BrokenPipeError, ConnectionResetError):
            return
        except Exception as error:  # Keep HTTP internals out of client responses.
            print(f"Treer Mail request failed: {type(error).__name__}", file=sys.stderr)
            self._error(MailError(500, "mail service operation failed", "mail_internal_error"))

    def _raw_session(self) -> str | None:
        return self._raw_cookie(SESSION_COOKIE)

    def _raw_cookie(self, name) -> str | None:
        header = self.headers.get("Cookie")
        if not header:
            return None
        cookies = http.cookies.SimpleCookie()
        try:
            cookies.load(header)
        except http.cookies.CookieError:
            return None
        morsel = cookies.get(name)
        return morsel.value if morsel else None

    def _caller(self) -> tuple[str, str, str]:
        """(kind, ID, name) of a signed-in human or an Agent. Agents use their
        workload identity (injected on http://NAME.treer/ or a Bearer from
        `treer token create SERVICE_ID`)."""
        token = self.headers.get("X-Treer-Identity-Token")
        if token is None and self.headers.get("Authorization", "").startswith("Bearer "):
            token = self.headers["Authorization"][7:]
        if token is not None:
            try:
                record = self._session()
                return "human", record.user_id, record.preferred_name
            except MailError:
                claims = self.server.application.verify_agent(token)
                return "agent", str(claims["sub"]), str(claims.get("name") or claims["sub"])
        record = self._session()
        return "human", record.user_id, record.preferred_name

    def _session(self) -> BrowserSession:
        if getattr(self, "_mutation", False) and "Origin" not in self.headers:
            raise MailError(403, "Same-origin browser write required", "csrf_denied")
        if "X-Treer-Identity-Token" in self.headers or "Authorization" in self.headers:
            if "X-Treer-Identity-Token" in self.headers:
                token = self.headers.get("X-Treer-Identity-Token")
            else:
                value = self.headers.get("Authorization", "")
                token = value[7:] if value.startswith("Bearer ") else None
            c = self.server.application.verify_human(token)
            return BrowserSession("", token, c["workspace_id"], c["service_id"], c["sub"], c["name"], c["role"], c["exp"])
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

    def _static(self, request_path: str, *, head: bool, vary: bool = False) -> None:
        web_root = self.server.application.config.web_dir.resolve()
        relative = urllib.parse.unquote(request_path).lstrip("/") or "index.html"
        candidate = (web_root / relative).resolve()
        try:
            candidate.relative_to(web_root)
        except ValueError:
            raise MailError(404, "not found", "static_not_found")
        if not candidate.is_file():
            if relative == "index.html":
                raise MailError(503, "Mail frontend is not built", "frontend_unavailable")
            raise MailError(404, "not found", "static_not_found")
        content = candidate.read_bytes()
        content_type = mimetypes.guess_type(candidate.name)[0] or "application/octet-stream"
        self._bytes(200, content_type, content, head=head, vary=vary)

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

    def _bytes(
        self,
        status: int,
        content_type: str,
        content: bytes,
        *,
        head: bool = False,
        vary: bool = False,
    ) -> None:
        self.send_response(status)
        self._security_headers()
        self.send_header("Content-Type", content_type)
        if vary:
            self.send_header("Vary", "Accept, User-Agent")
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


def _app_api_mail_error(error: AppApiError) -> MailError:
    if error.code == "app_authentication_required":
        return MailError(401, "Mail login required", error.code)
    if error.code == "policy_denied":
        return MailError(403, error.message, error.code)
    if error.code.endswith("_not_found") or error.code in {"message_recipient_unavailable"}:
        return MailError(404, error.message, error.code)
    if error.code.startswith("message_") or error.code.startswith("app_oauth_"):
        return MailError(400, error.message, error.code)
    return MailError(502, "Treer control plane is unavailable", error.code)


def _secret_hash(value: str) -> str:
    return hashlib.sha256(value.encode("utf-8")).hexdigest()


def _object(value: Any, label: str) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise MailError(502, f"Treer returned an invalid {label}", "app_api_invalid_response")
    return value


def _list(value: Any, label: str) -> list[Any]:
    if not isinstance(value, list):
        raise MailError(502, f"Treer returned an invalid {label}", "app_api_invalid_response")
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
        raise MailError(502, f"Treer returned an invalid {label}", "app_api_invalid_response")
    return value


def _timestamp(value: Any, label: str) -> float:
    text = _bounded_string(value, label, 128)
    matched = RFC3339_NANOSECONDS.fullmatch(text)
    if matched is not None and len(matched.group("fraction")) > 7:
        text = matched.group("seconds") + matched.group("fraction")[:7] + matched.group("zone")
    try:
        return datetime.fromisoformat(text.replace("Z", "+00:00")).timestamp()
    except ValueError as error:
        raise MailError(502, f"Treer returned an invalid {label}", "app_api_invalid_response") from error


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
    # The App root, optionally opening one Message (email reply links).
    if value != "/" and not re.fullmatch(r"/\?message=[A-Za-z0-9_.-]{1,128}", value):
        raise MailError(400, "return_to must be the App root", "return_path_invalid")
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
    config_path = os.environ.get("TREER_APP_CONFIG")
    state_dir = os.environ.get("TREER_APP_STATE_DIR")
    if not config_path or not state_dir:
        raise ValueError("TREER_APP_CONFIG and TREER_APP_STATE_DIR are required")
    with open(config_path, "rb") as handle:
        value = json.load(handle)
    if not isinstance(value, dict):
        raise ValueError("App config must contain a JSON object")
    host, port = _listen(value.get("listen", "127.0.0.1:8788"))
    web_value = value.get("web_dir", "web/dist")
    if not isinstance(web_value, str) or not web_value:
        raise ValueError("web_dir must be a non-empty path")
    web_dir = Path(web_value)
    if not web_dir.is_absolute():
        web_dir = APP_ROOT / web_dir
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
        workspace_id=value.get("workspace_id"),
        browser_origins=tuple(_normalized_url(origin, "browser origin").rstrip("/") for origin in value.get("browser_origins", [])),
        email_capability=_bounded_string(value.get("email_capability", "app.email.v1"), "email capability", 128),
        treer_bin=_bounded_string(value.get("treer_bin", "treer"), "treer binary", 4096),
    )


def main() -> int:
    try:
        config = load_config()
        application = MailApplication(
            config,
            TreerAppClient(config.proxy_public_url, config.service_id),
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
