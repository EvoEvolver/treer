#!/usr/bin/env python3
"""Email notifications over canonical Core Messages; only channel metadata is local."""

from __future__ import annotations
import hashlib
import html
import json
import os
import re
import secrets
import sqlite3
import subprocess
import sys
import threading
import time
import urllib.parse
from dataclasses import dataclass
from email.message import EmailMessage
from pathlib import Path
from http.server import ThreadingHTTPServer

# Reuse the maintained App OAuth/PKCE/session and HTTP primitives, not Core internals.
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "mail"))
import mail

ROOT = Path(__file__).resolve().parent
mail.SESSION_COOKIE = "treer_email_endpoint_session"
mail.API_TIMEOUT_SECONDS = 20
Error = mail.MailError


@dataclass
class Settings:
    config: mail.Config
    workspace: str
    members: dict
    allowlist: list
    wake_agents: list
    mode: str = "outbox"
    agent_limit: int = 10
    recipient_limit: int = 5
    retry_seconds: float = 30
    max_attempts: int = 5
    browser_origins: tuple = ()


def fields(value, allowed, required=()):
    if (
        not isinstance(value, dict)
        or set(value) - set(allowed)
        or any(k not in value for k in required)
    ):
        raise Error(400, "Unknown or missing request fields", "request_invalid")


def text(value, name, size):
    return mail._bounded_string(value, name, size)


class Application(mail.MailApplication):
    def __init__(self, settings, api=None):
        self.settings = settings
        c = settings.config
        os.umask(0o077)
        c.database_path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
        super().__init__(
            c,
            api or mail.TreerAppClient(c.proxy_public_url, c.service_id),
            mail.SessionStore(c.database_path),
        )
        self.lock = threading.RLock()
        self.stop = threading.Event()
        with self.db() as db:
            db.executescript("""
            CREATE TABLE IF NOT EXISTS threads(id TEXT PRIMARY KEY, agent TEXT NOT NULL, recipient TEXT NOT NULL, email TEXT NOT NULL, subject TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS notifications(id TEXT PRIMARY KEY, agent TEXT NOT NULL, key TEXT NOT NULL, fingerprint TEXT NOT NULL, thread TEXT NOT NULL, parent TEXT, message TEXT, subject TEXT NOT NULL, summary TEXT NOT NULL, urgency TEXT NOT NULL, status TEXT NOT NULL, attempts INTEGER NOT NULL DEFAULT 0, next_try REAL NOT NULL, created REAL NOT NULL, transport TEXT, error TEXT, UNIQUE(agent,key));
            CREATE TABLE IF NOT EXISTS thread_messages(seq INTEGER PRIMARY KEY AUTOINCREMENT, thread TEXT NOT NULL, message TEXT UNIQUE NOT NULL);
            CREATE TABLE IF NOT EXISTS replies(principal TEXT NOT NULL, key TEXT NOT NULL, fingerprint TEXT NOT NULL, thread TEXT NOT NULL, parent TEXT, message TEXT, wake TEXT, PRIMARY KEY(principal,key));
            """)

    def db(self):
        db = sqlite3.connect(self.config.database_path, timeout=15)
        db.row_factory = sqlite3.Row
        return db

    def start_oauth(self, return_to):
        # Mail's OAuth implementation validates root-only returns. Generate PKCE with it,
        # then store the validated local thread return, never an arbitrary URL.
        if return_to != "/" and not re.fullmatch(r"/t/th_[a-f0-9]{32}/", return_to):
            raise Error(400, "Invalid login return path", "return_path_invalid")
        location = super().start_oauth("/")
        state = urllib.parse.parse_qs(urllib.parse.urlsplit(location).query)["state"][0]
        with self.sessions._connect() as db:
            db.execute(
                "UPDATE app_pending_oauth SET return_path=? WHERE state_hash=?",
                (return_to, mail._secret_hash(state)),
            )
        return location

    def principal(self, token, kind="agent"):
        if not token:
            raise Error(401, "Treer service identity required", "identity_required")
        result = self.api.request(
            "POST",
            "/.treer/apps/identity/verify",
            body={"token": token, "audience": self.config.service_id},
        )
        c = result.get("claims") or {}
        if (
            result.get("active") is not True
            or c.get("principal_kind") != kind
            or c.get("workspace_id") != self.settings.workspace
            or c.get("service_id") != self.config.service_id
            or not c.get("sub")
        ):
            raise Error(401, "Invalid Treer service identity", "identity_invalid")
        return c

    def authenticated_session(self, raw):
        record = super().authenticated_session(raw)
        if record.workspace_id != self.settings.workspace:
            raise Error(401, "Workspace identity mismatch", "identity_invalid")
        return record

    def core(self, token, method, suffix="", body=None):
        return self.api.request(
            method,
            f"/api/apps/{self.config.service_id}/messages{suffix}",
            access_token=token,
            body=body,
        )

    def recipient(self, target, token):
        directory = self.api.request(
            "GET", f"/api/apps/{self.config.service_id}/directory", access_token=token
        )["principals"]
        humans = [p for p in directory if p["kind"] == "human"]
        # Email addresses are never discovered from Core; administrator owns the bindings.
        matches = [
            p
            for p in humans
            if target in (p["id"], p["name"])
            or self.settings.members.get(p["id"], "").lower() == target.lower()
        ]
        if len(matches) != 1:
            raise Error(
                403,
                "Recipient requires one active workspace member with an administrator-configured email binding",
                "recipient_denied",
            )
        p = matches[0]
        address = self.settings.members.get(p["id"])
        if not address or not re.fullmatch(
            r"[A-Za-z0-9.!#$%&'*+/=?^_`{|}~-]+@[A-Za-z0-9.-]+", address
        ):
            raise Error(
                403,
                "Member has no valid administrator-configured email",
                "recipient_denied",
            )
        # Member bindings are an explicit administrator admission. Optional allowlist
        # further restricts them. Proxy independently validates its verified member data.
        if self.settings.allowlist and not any(
            address.lower() == a.lower()
            or (
                a.startswith("@") and address.rsplit("@", 1)[1].lower() == a[1:].lower()
            )
            for a in self.settings.allowlist
        ):
            raise Error(
                403, "Recipient is outside the configured allowlist", "recipient_denied"
            )
        return p["id"], address.lower()

    def notify(self, token, request, key):
        agent = self.principal(token)["sub"]
        fields(
            request,
            ("to", "subject", "body", "summary", "urgency", "thread_id"),
            ("to", "subject", "body"),
        )
        key = text(key, "Idempotency-Key", 128)
        target = text(request["to"], "recipient", 256)
        subject = text(request["subject"], "subject", 150)
        if any(ord(c) < 32 for c in subject):
            raise Error(400, "Subject contains control characters", "request_invalid")
        body = text(request["body"], "body", 32768)
        summary = request.get(
            "summary", "An Agent needs your attention. Sign in to read and reply."
        )
        text(summary, "summary", 500)
        urgency = request.get("urgency", "normal")
        if urgency not in ("normal", "high"):
            raise Error(400, "urgency must be normal or high", "request_invalid")
        recipient, address = self.recipient(target, token)
        fingerprint = hashlib.sha256(
            json.dumps(request, sort_keys=True).encode()
        ).hexdigest()
        with self.lock, self.db() as db:
            db.execute("BEGIN IMMEDIATE")
            old = db.execute(
                "SELECT * FROM notifications WHERE agent=? AND key=?", (agent, key)
            ).fetchone()
            if old:
                if old["fingerprint"] != fingerprint:
                    raise Error(
                        409, "Idempotency key content changed", "idempotency_conflict"
                    )
                if old["message"]:
                    return self.notification_result(dict(old))
                n = dict(old)
            else:
                now = time.time()
                count = db.execute(
                    "SELECT count(*) FROM notifications WHERE agent=? AND created>?",
                    (agent, now - 3600),
                ).fetchone()[0]
                rc = db.execute(
                    "SELECT count(*) FROM notifications n JOIN threads t ON t.id=n.thread WHERE t.email=? AND n.created>?",
                    (address, now - 3600),
                ).fetchone()[0]
                if (
                    count >= self.settings.agent_limit
                    or rc >= self.settings.recipient_limit
                ):
                    raise Error(
                        429, "Hourly Agent or recipient limit exceeded", "rate_limited"
                    )
                thread_id = request.get("thread_id")
                if thread_id:
                    t = db.execute(
                        "SELECT * FROM threads WHERE id=?", (thread_id,)
                    ).fetchone()
                    if (
                        not t
                        or t["agent"] != agent
                        or t["recipient"] != recipient
                        or t["email"] != address
                    ):
                        raise Error(
                            403,
                            "Thread belongs to another Agent or recipient",
                            "thread_denied",
                        )
                else:
                    thread_id = "th_" + secrets.token_hex(16)
                    db.execute(
                        "INSERT INTO threads VALUES(?,?,?,?,?)",
                        (thread_id, agent, recipient, address, subject),
                    )
                parent = db.execute(
                    "SELECT message FROM thread_messages WHERE thread=? ORDER BY seq DESC LIMIT 1",
                    (thread_id,),
                ).fetchone()
                n = dict(
                    id="nt_" + secrets.token_hex(16),
                    agent=agent,
                    key=key,
                    fingerprint=fingerprint,
                    thread=thread_id,
                    parent=parent[0] if parent else None,
                    subject=subject,
                    summary=summary,
                    urgency=urgency,
                    status="queued",
                    next_try=now,
                    created=now,
                )
                db.execute(
                    "INSERT INTO notifications(id,agent,key,fingerprint,thread,parent,subject,summary,urgency,status,next_try,created) VALUES(:id,:agent,:key,:fingerprint,:thread,:parent,:subject,:summary,:urgency,:status,:next_try,:created)",
                    n,
                )
            db.commit()  # Persist the intent and fixed context before the external Core mutation.
            response = self.core(
                token,
                "POST",
                body={
                    "recipients": [recipient],
                    "context_ids": [n["parent"]] if n["parent"] else [],
                    "body": body,
                    "idempotency_key": "email-notify-" + n["id"],
                },
            )
            message = response["message"]["message_id"]
            db.execute(
                "INSERT OR IGNORE INTO thread_messages(thread,message) VALUES(?,?)",
                (n["thread"], message),
            )
            db.execute(
                "UPDATE notifications SET message=? WHERE id=?", (message, n["id"])
            )
            db.commit()
            return self.notification_result(
                dict(
                    db.execute(
                        "SELECT * FROM notifications WHERE id=?", (n["id"],)
                    ).fetchone()
                )
            )

    def notification_result(self, n):
        return {
            k: n.get(k)
            for k in ("id", "message", "status", "attempts", "transport", "error")
        } | {
            "thread_id": n["thread"],
            "url": urllib.parse.urljoin(
                self.config.public_url, "t/" + n["thread"] + "/"
            ),
            "core_pending": not bool(n.get("message")),
        }

    def thread(self, thread_id, record, before=0):
        with self.db() as db:
            t = db.execute("SELECT * FROM threads WHERE id=?", (thread_id,)).fetchone()
            if not t or t["recipient"] != record.user_id:
                raise Error(
                    403, "You are not the recipient of this thread", "thread_denied"
                )
            rows = db.execute(
                "SELECT seq,message FROM thread_messages WHERE thread=? AND (?=0 OR seq<?) ORDER BY seq DESC LIMIT 50",
                (thread_id, before, before),
            ).fetchall()
        messages = [
            self.core(record.access_token, "GET", "/" + r["message"])["message"]
            for r in reversed(rows)
        ]
        return {
            "thread_id": thread_id,
            "subject": t["subject"],
            "messages": messages,
            "next_before": rows[-1]["seq"] if len(rows) == 50 else None,
        }

    def reply(self, thread_id, record, request, key):
        fields(request, ("body",), ("body",))
        body = text(request["body"], "body", 32768)
        key = text(key, "Idempotency-Key", 128)
        fingerprint = hashlib.sha256((thread_id + "\0" + body).encode()).hexdigest()
        with self.lock, self.db() as db:
            t = db.execute("SELECT * FROM threads WHERE id=?", (thread_id,)).fetchone()
            if not t or t["recipient"] != record.user_id:
                raise Error(
                    403, "You are not the recipient of this thread", "thread_denied"
                )
            old = db.execute(
                "SELECT * FROM replies WHERE principal=? AND key=?",
                (record.user_id, key),
            ).fetchone()
            if old and old["fingerprint"] != fingerprint:
                raise Error(
                    409, "Idempotency key content changed", "idempotency_conflict"
                )
            if old and old["message"]:
                return {
                    "message_id": old["message"],
                    "wake": old["wake"] or "not_attempted",
                }
            if not old:
                parent = db.execute(
                    "SELECT message FROM thread_messages WHERE thread=? ORDER BY seq DESC LIMIT 1",
                    (thread_id,),
                ).fetchone()
                db.execute(
                    "INSERT INTO replies(principal,key,fingerprint,thread,parent) VALUES(?,?,?,?,?)",
                    (
                        record.user_id,
                        key,
                        fingerprint,
                        thread_id,
                        parent[0] if parent else None,
                    ),
                )
                db.commit()
                old = db.execute(
                    "SELECT * FROM replies WHERE principal=? AND key=?",
                    (record.user_id, key),
                ).fetchone()
            response = self.core(
                record.access_token,
                "POST",
                body={
                    "recipients": [t["agent"]],
                    "context_ids": [old["parent"]] if old["parent"] else [],
                    "body": body,
                    "idempotency_key": "email-reply-"
                    + hashlib.sha256((record.user_id + key).encode()).hexdigest(),
                },
            )
            message = response["message"]["message_id"]
            db.execute(
                "INSERT OR IGNORE INTO thread_messages(thread,message) VALUES(?,?)",
                (thread_id, message),
            )
            db.execute(
                "UPDATE replies SET message=?,wake=? WHERE principal=? AND key=?",
                (message, "not_attempted", record.user_id, key),
            )
            db.commit()  # Message is durable before optional terminal interaction.
            wake = self.wake(t["agent"], message)
            db.execute(
                "UPDATE replies SET wake=? WHERE principal=? AND key=?",
                (wake, record.user_id, key),
            )
            return {"message_id": message, "wake": wake}

    def wake(self, agent, message):
        if agent not in self.settings.wake_agents:
            return "disabled"
        try:
            result = subprocess.run(
                [
                    os.environ.get("EMAIL_ENDPOINT_TREER_BIN", "treer"),
                    "agent",
                    "prompt",
                    agent,
                    f"New Core Message {message}; use treer message get to read it.",
                ],
                capture_output=True,
                timeout=20,
            )
            if result.returncode == 0:
                return "prompted"
            return "denied_or_unavailable_message_delivered"
        except (OSError, subprocess.TimeoutExpired):
            return "unavailable_message_delivered"

    def status(self, notification, token):
        agent = self.principal(token)["sub"]
        with self.db() as db:
            n = db.execute(
                "SELECT * FROM notifications WHERE id=? AND agent=?",
                (notification, agent),
            ).fetchone()
        if not n:
            raise Error(404, "Notification not found", "notification_not_found")
        return self.notification_result(dict(n))

    def admin(self, record):
        if record.role not in ("owner", "admin"):
            raise Error(403, "Workspace administrator required", "admin_required")
        with self.db() as db:
            rows = db.execute(
                "SELECT n.*,t.email FROM notifications n JOIN threads t ON t.id=n.thread ORDER BY n.created DESC LIMIT 100"
            ).fetchall()
        return {
            "mode": self.settings.mode,
            "notifications": [
                self.notification_result(dict(r))
                | {"subject": r["subject"], "summary": r["summary"]}
                for r in rows
            ],
        }

    def deliver(self):
        # Single active worker; SQLite serializes mutations with HTTP handlers.
        with self.lock, self.db() as db:
            rows = db.execute(
                "SELECT n.*,t.email FROM notifications n JOIN threads t ON t.id=n.thread WHERE n.status='queued' AND n.message IS NOT NULL AND n.next_try<=? ORDER BY n.created LIMIT 1",
                (time.time(),),
            ).fetchall()
            for row in rows:
                n = dict(row)
                url = urllib.parse.urljoin(
                    self.config.public_url, "t/" + n["thread"] + "/"
                )
                subject = f"[{n['thread']}] {'HIGH: ' if n['urgency'] == 'high' else ''}{n['subject']}"
                content = (
                    n["summary"] + "\n\nRead and reply (Treer login required):\n" + url
                )
                transport = self.settings.mode
                error = None
                try:
                    if transport == "proxy":
                        result = subprocess.run(
                            [
                                os.environ.get("EMAIL_ENDPOINT_TREER_BIN", "treer"),
                                "token",
                                "create",
                                self.config.service_id,
                            ],
                            capture_output=True,
                            text=True,
                            timeout=15,
                            check=True,
                        )
                        try:
                            self.api.request(
                                "POST",
                                f"/api/apps/{self.config.service_id}/email",
                                access_token=result.stdout.strip(),
                                body={
                                    "to": n["email"],
                                    "subject": subject,
                                    "text": content,
                                    "idempotency_key": n["id"],
                                },
                            )
                        except mail.AppApiError as exc:
                            if exc.code != "email_not_configured":
                                raise
                            transport = "outbox"
                            error = "email_not_configured: saved to local outbox; no email was sent"
                    if transport == "outbox":
                        self.outbox(n, subject, content, url)
                    status = "sent"
                except Exception as exc:
                    error = (
                        exc.code
                        if isinstance(exc, mail.AppApiError)
                        else "delivery_unavailable"
                    )
                    status = (
                        "failed"
                        if n["attempts"] + 1 >= self.settings.max_attempts
                        else "queued"
                    )
                db.execute(
                    "UPDATE notifications SET status=?,attempts=attempts+1,next_try=?,transport=?,error=? WHERE id=?",
                    (
                        status,
                        time.time()
                        + min(3600, self.settings.retry_seconds * 2 ** n["attempts"]),
                        transport,
                        error,
                        n["id"],
                    ),
                )
                db.commit()

    def outbox(self, n, subject, content, url):
        directory = self.config.database_path.parent / "outbox"
        directory.mkdir(mode=0o700, exist_ok=True)
        m = EmailMessage()
        m["To"] = n["email"]
        m["Subject"] = subject
        m["Message-ID"] = "<" + n["id"] + "@email-endpoint.local>"
        m.set_content(content)
        m.add_alternative(
            "<p>"
            + html.escape(n["summary"])
            + '</p><p><a href="'
            + html.escape(url, quote=True)
            + '">Read and reply</a> (Treer login required)</p>',
            subtype="html",
        )
        tmp = directory / (n["id"] + ".tmp")
        tmp.write_bytes(m.as_bytes())
        tmp.replace(directory / (n["id"] + ".eml"))

    def worker(self):
        while not self.stop.wait(1):
            try:
                self.deliver()
            except Exception:
                print("email-endpoint: queue worker unavailable", file=sys.stderr)


class Handler(mail.MailHandler):
    def _token(self):
        return self.headers.get("X-Treer-Identity-Token") or self.headers.get(
            "Authorization", ""
        ).removeprefix("Bearer ")

    def _session(self):
        token = self.headers.get("X-Treer-Identity-Token")
        if token:
            c = self.server.application.principal(token, "human")
            return mail.BrowserSession(
                "",
                token,
                c["workspace_id"],
                c["service_id"],
                c["sub"],
                c.get("name", "Member"),
                c.get("role") or "member",
                c["exp"],
            )
        return super()._session()

    def _session_cookie(self, raw, max_age):
        value = super()._session_cookie(raw, max_age)
        path = urllib.parse.urlsplit(self.server.application.config.public_url).path
        return value.replace("Path=/", "Path=" + path)

    def _bytes(self, *args, **kwargs):
        super()._bytes(*args, **kwargs)

    def _static(self, path, *, head, vary=False):
        if path == "/":
            prefix = "../../" if self.path.startswith("/t/") else "./"
            content = (
                (ROOT / "web/index.html")
                .read_text()
                .replace("../../app.js", prefix + "app.js")
                .replace(
                    '<link id="css" rel="stylesheet">',
                    '<link id="css" rel="stylesheet" href="' + prefix + 'style.css">',
                )
            )
            self._bytes(
                200, "text/html; charset=utf-8", content.encode(), head=head, vary=vary
            )
        else:
            super()._static(path, head=head, vary=vary)

    def _dispatch(self, method):
        app = self.server.application
        parsed = urllib.parse.urlsplit(self.path)
        path = parsed.path
        try:
            if method == "POST" and path not in ("/v1/notify",):
                # Require browser same-origin even when cookie is valid.
                allowed_origins = {
                    urllib.parse.urlsplit(app.config.public_url)
                    ._replace(path="", query="", fragment="")
                    .geturl(),
                    *app.settings.browser_origins,
                }
                if self.headers.get("Origin") not in allowed_origins:
                    raise Error(403, "Same-origin request required", "csrf_denied")
            if path == "/v1/notify" and method == "POST":
                self._json(
                    202,
                    app.notify(
                        self._token(),
                        self._json_body(),
                        self.headers.get("Idempotency-Key"),
                    ),
                )
            elif path.startswith("/v1/notifications/") and method == "GET":
                self._json(200, app.status(path.rsplit("/", 1)[1], self._token()))
            elif path == "/v1/admin/outbox" and method == "GET":
                self._json(200, app.admin(self._session()))
            elif re.fullmatch(r"/v1/threads/th_[a-f0-9]{32}", path):
                record = self._session()
                thread_id = path.rsplit("/", 1)[1]
                if method == "GET":
                    before = int(
                        urllib.parse.parse_qs(parsed.query).get("before", ["0"])[0]
                    )
                    self._json(200, app.thread(thread_id, record, before))
                elif method == "POST":
                    self._json(
                        200,
                        app.reply(
                            thread_id,
                            record,
                            self._json_body(),
                            self.headers.get("Idempotency-Key"),
                        ),
                    )
                else:
                    raise Error(405, "Method not allowed", "method_not_allowed")
            elif path in (
                "/api/auth/start",
                "/api/auth/callback",
                "/api/auth/session",
                "/api/auth/logout",
            ):
                super()._dispatch(method)
            elif path == "/health" and method in ("GET", "HEAD"):
                self._json(
                    200,
                    {
                        "status": "ok",
                        "service": "email-endpoint",
                        "mode": app.settings.mode,
                    },
                    head=method == "HEAD",
                )
            elif path == "/" and method in ("GET", "HEAD"):
                representation = mail.root_representation(
                    self.headers.get("Accept", ""), self.headers.get("User-Agent", "")
                )
                if representation == "text/html":
                    self._static("/", head=method == "HEAD", vary=True)
                else:
                    self._bytes(
                        200,
                        "text/markdown; charset=utf-8",
                        (ROOT / "AGENT.md").read_bytes(),
                        head=method == "HEAD",
                        vary=True,
                    )
            elif re.fullmatch(r"/t/th_[a-f0-9]{32}/?", path) and method == "GET":
                if not path.endswith("/"):
                    self._redirect(path + "/")
                    return
                if not self._raw_session() and not self.headers.get(
                    "X-Treer-Identity-Token"
                ):
                    self._static("/", head=False)
                    return
                try:
                    record = self._session()
                except Error as exc:
                    if exc.status != 401 or self.headers.get("X-Treer-Identity-Token"):
                        raise
                    self._static("/", head=False)
                    return
                # Check ownership at navigation as well as at every data mutation/read.
                with app.db() as db:
                    t = db.execute(
                        "SELECT recipient FROM threads WHERE id=?",
                        (path.split("/")[2],),
                    ).fetchone()
                if not t or t[0] != record.user_id:
                    self._bytes(
                        403,
                        "text/html; charset=utf-8",
                        b'<!doctype html><meta name="viewport" content="width=device-width"><h1>Access denied</h1><p>You are not the recipient of this thread.</p>',
                    )
                else:
                    self._static("/", head=False)
            elif path in ("/app.js", "/style.css") and method in ("GET", "HEAD"):
                self._static(path, head=method == "HEAD")
            else:
                raise Error(404, "Route not found", "route_not_found")
        except mail.AppApiError as exc:
            self._error(mail._app_api_mail_error(exc))
        except Error as exc:
            self._error(exc)
        except (BrokenPipeError, ConnectionResetError):
            pass
        except Exception:
            self._error(Error(500, "Email endpoint operation failed", "internal_error"))

    def _security_headers(self):
        super()._security_headers()
        self.send_header("Cache-Control", "no-store")


class Server(ThreadingHTTPServer):
    daemon_threads = True

    def __init__(self, address, application):
        self.application = application
        super().__init__(address, Handler)


def load_settings():
    path = Path(os.environ["EMAIL_ENDPOINT_CONFIG"])
    if path.stat().st_mode & 0o077:
        raise ValueError("EMAIL_ENDPOINT_CONFIG must have permissions 600")
    value = json.loads(path.read_text())
    state = Path(
        os.environ.get(
            "EMAIL_ENDPOINT_STATE_DIR",
            value.get("state_dir", ".treer/apps/email-endpoint"),
        )
    )
    host, port = mail._listen(value.get("listen", "127.0.0.1:9440"))
    # Bootstrap without guessing a platform origin. App can start before operator
    # has written the service/origin returned by create; it discovers only itself.
    if not value.get("service_id") or not value.get("public_url"):
        result = subprocess.run(
            ["treer", "app", "show", os.environ["TREER_AGENT_ID"]],
            capture_output=True,
            text=True,
            timeout=15,
            check=True,
        )
        deployment = json.loads(result.stdout)
        deployment = deployment.get("app", deployment)
        value["service_id"] = deployment["service_id"]
        value["public_url"] = deployment["public_url"]
    c = mail.Config(
        host,
        port,
        value["service_id"],
        mail._normalized_url(value["public_url"], "public URL"),
        mail._normalized_url(value["proxy_public_url"], "Proxy URL"),
        ROOT / "web",
        state / "email-endpoint.sqlite3",
    )
    settings = Settings(
        c,
        value.get("workspace_id") or os.environ["TREER_WORKSPACE_ID"],
        value.get("members", {}),
        [
            entry.strip()
            for entry in os.environ["EMAIL_ENDPOINT_ALLOWLIST"].split(",")
            if entry.strip()
        ]
        if "EMAIL_ENDPOINT_ALLOWLIST" in os.environ
        else value.get("allowlist", []),
        value.get("wake_agents", []),
        value.get("mode", "outbox"),
        int(value.get("agent_limit", 10)),
        int(value.get("recipient_limit", 5)),
        float(value.get("retry_seconds", 30)),
        int(value.get("max_attempts", 5)),
    )
    settings.browser_origins = tuple(value.get("browser_origins", []))
    if (
        settings.mode not in ("outbox", "proxy")
        or min(
            settings.agent_limit,
            settings.recipient_limit,
            settings.retry_seconds,
            settings.max_attempts,
        )
        <= 0
    ):
        raise ValueError("Invalid mode or limits")
    return settings


def main():
    try:
        settings = load_settings()
        app = Application(settings)
        server = Server((settings.config.listen_host, settings.config.listen_port), app)
        threading.Thread(target=app.worker, daemon=True).start()
        print(
            "email-endpoint: listening (no bodies or recipients logged)",
            file=sys.stderr,
            flush=True,
        )
        server.serve_forever()
    except KeyboardInterrupt:
        return 0
    except Exception:
        print(
            "email-endpoint: startup failed; check private configuration",
            file=sys.stderr,
        )
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
