#!/usr/bin/env python3
"""Email notifications over canonical Core Messages; only channel metadata is local."""

from __future__ import annotations
import hashlib
import base64
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
from dataclasses import dataclass, field
from email.message import EmailMessage
from pathlib import Path
from http.server import ThreadingHTTPServer
from http.cookies import SimpleCookie

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
    contacts: list = field(default_factory=list)
    runtime_id: str = ""


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
    def __init__(self, settings, api=None, tree_loader=None, token_loader=None):
        self.settings = settings
        self.tree_loader = tree_loader or self.load_tree
        self.token_loader = token_loader or self.service_token
        self.contacts = self.validate_contacts(settings)
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
        self.migrate()
        with self.db() as db:
            db.executescript("""
            CREATE TABLE IF NOT EXISTS threads(id TEXT PRIMARY KEY, agent TEXT NOT NULL, recipient TEXT NOT NULL, email TEXT NOT NULL, subject TEXT NOT NULL, contact TEXT);
            CREATE TABLE IF NOT EXISTS notifications(id TEXT PRIMARY KEY, agent TEXT NOT NULL, key TEXT NOT NULL, fingerprint TEXT NOT NULL, thread TEXT NOT NULL, parent TEXT, message TEXT, subject TEXT NOT NULL, summary TEXT NOT NULL, urgency TEXT NOT NULL, status TEXT NOT NULL, attempts INTEGER NOT NULL DEFAULT 0, next_try REAL NOT NULL, created REAL NOT NULL, transport TEXT, error TEXT, UNIQUE(agent,key));
            CREATE TABLE IF NOT EXISTS thread_messages(seq INTEGER PRIMARY KEY AUTOINCREMENT, thread TEXT NOT NULL, message TEXT UNIQUE NOT NULL);
            CREATE TABLE IF NOT EXISTS replies(principal TEXT NOT NULL, key TEXT NOT NULL, fingerprint TEXT NOT NULL, thread TEXT NOT NULL, parent TEXT, message TEXT, wake TEXT, PRIMARY KEY(principal,key));
            """)

    @staticmethod
    def validate_contacts(settings):
        contacts = [dict(c) for c in settings.contacts]
        bound = {c.get("user_id") for c in contacts}
        contacts += [{"id": "member-" + uid, "label": uid, "email": email,
                      "user_id": uid} for uid, email in settings.members.items() if uid not in bound]
        ids, emails = set(), set()
        for c in contacts:
            fields(c, ("id", "label", "email", "user_id", "enabled"), ("id", "email"))
            cid = text(c["id"], "contact ID", 128)
            email = text(c["email"], "email", 254).lower()
            if not re.fullmatch(r"[A-Za-z0-9.!#$%&'*+/=?^_`{|}~-]+@[A-Za-z0-9.-]+", email):
                raise ValueError("Invalid contact email")
            if cid in ids or email in emails:
                raise ValueError("Duplicate contact ID or email")
            ids.add(cid); emails.add(email)
            if c.get("user_id") is not None:
                text(c["user_id"], "user ID", 256)
            if not isinstance(c.get("enabled", True), bool):
                raise ValueError("Contact enabled must be boolean")
            c.update(email=email, label=text(c.get("label", cid), "contact label", 256),
                     enabled=c.get("enabled", True))
        return contacts

    def migrate(self):
        # Existing state is backed up before any v2 schema/activation change.
        with self.db() as db:
            old = db.execute("SELECT name FROM sqlite_master WHERE name='threads'").fetchone()
            columns = {r[1] for r in db.execute("PRAGMA table_info(threads)")}
            if old and "contact" not in columns:
                backup = self.config.database_path.with_name(self.config.database_path.name + ".pre-v2-" + secrets.token_hex(8))
                target = sqlite3.connect(backup)
                try:
                    db.backup(target)
                    target.execute("PRAGMA journal_mode=DELETE")
                finally:
                    target.close()
            db.execute("CREATE TABLE IF NOT EXISTS endpoint_state(key TEXT PRIMARY KEY, value TEXT NOT NULL)")
            identity = json.dumps([self.settings.workspace, self.config.service_id])
            row = db.execute("SELECT value FROM endpoint_state WHERE key='identity'").fetchone()
            if row and row[0] != identity:
                raise ValueError("State directory belongs to another endpoint")
            db.execute("INSERT OR IGNORE INTO endpoint_state VALUES('identity',?)", (identity,))
            if old and "contact" not in columns:
                db.execute("ALTER TABLE threads ADD COLUMN contact TEXT")
                for c in self.contacts:
                    db.execute("UPDATE threads SET contact=? WHERE recipient=? AND lower(email)=?",
                               (c["id"], c.get("user_id"), c["email"]))
            # Old unbound OAuth login flows cannot safely cross the upgrade.
            oauth_cols = {r[1] for r in db.execute("PRAGMA table_info(app_pending_oauth)")}
            if "browser_hash" not in oauth_cols:
                db.execute("DELETE FROM app_pending_oauth")
                db.execute("ALTER TABLE app_pending_oauth ADD COLUMN browser_hash TEXT")

    def service_token(self):
        result = subprocess.run([os.environ.get("EMAIL_ENDPOINT_TREER_BIN", "treer"),
                                 "token", "create", self.config.service_id], capture_output=True,
                                text=True, timeout=15, check=True)
        return result.stdout.strip()

    def load_tree(self):
        try:
            result = subprocess.run([os.environ.get("EMAIL_ENDPOINT_TREER_BIN", "treer"),
                                     "agent", "ls", "-R", "/"], capture_output=True,
                                    text=True, timeout=15, check=True)
            return json.loads(result.stdout)
        except (OSError, ValueError, subprocess.SubprocessError):
            raise Error(503, "Current workspace tree unavailable", "tree_unavailable")

    def placement(self, agent=None):
        tree = self.tree_loader()
        rows = tree.get("agents", [])
        own = next((a for a in rows if a.get("agent_id") == self.settings.runtime_id), None)
        if not own or own.get("kind") != "app":
            raise Error(503, "Endpoint runtime is absent from the current tree", "endpoint_unplaced")
        if agent:
            caller = next((a for a in rows if a.get("agent_id") == agent), None)
            if not caller or caller.get("kind") == "app" or caller.get("folder_id") != own.get("folder_id"):
                raise Error(403, "Only Agents in this endpoint's current folder may send", "folder_denied")
        return own

    def activation(self):
        with self.db() as db:
            row = db.execute("SELECT value FROM endpoint_state WHERE key='activation'").fetchone()
        return json.loads(row[0]) if row else {"enabled": False, "folder_id": None}

    def require_enabled(self, agent=None):
        own = self.placement(agent)
        active = self.activation()
        if not active["enabled"] or active["folder_id"] != own.get("folder_id"):
            raise Error(409, "Confirm current placement and enable sending first", "endpoint_disabled")
        return own

    def activate(self, record, request):
        self.require_admin(record)
        fields(request, ("enabled", "folder_id"), ("enabled", "folder_id"))
        if not isinstance(request["enabled"], bool):
            raise Error(400, "enabled must be boolean", "request_invalid")
        own = self.placement()
        if request["folder_id"] != own.get("folder_id"):
            raise Error(409, "Endpoint moved; inspect its placement again", "placement_changed")
        with self.lock, self.db() as db:
            db.execute("INSERT OR REPLACE INTO endpoint_state VALUES('activation',?)",
                       (json.dumps(request),))
        return request

    def contact_view(self, token):
        humans = {p["id"] for p in self.api.request("GET", f"/api/apps/{self.config.service_id}/directory",
                                                   access_token=token)["principals"] if p["kind"] == "human"}
        return [c | {"reply_available": bool(c["enabled"] and c.get("user_id") in humans),
                     "reply_status": "available" if c["enabled"] and c.get("user_id") in humans else "reply_unavailable"}
                for c in self.contacts]

    def require_admin(self, record):
        claims = self.principal(record.access_token, "human")
        if claims["sub"] != record.user_id or claims.get("role") not in ("owner", "admin"):
            raise Error(403, "Workspace administrator required", "admin_required")

    def check_thread(self, t, record):
        claims = self.principal(record.access_token, "human")
        if claims["sub"] != record.user_id:
            raise Error(401, "Identity changed", "identity_invalid")
        c = next((c for c in self.contacts if t and c["id"] == t["contact"]), None)
        if not t or t["recipient"] != record.user_id or not c or not c["enabled"] or c.get("user_id") != record.user_id or c["email"] != t["email"]:
            raise Error(403, "You are not the current recipient of this thread", "thread_denied")

    def overview(self, record):
        claims = self.principal(record.access_token, "human")
        own = self.placement()
        contacts = self.contact_view(record.access_token)
        admin = claims.get("role") in ("owner", "admin")
        if not admin:
            contacts = [c for c in contacts if c.get("user_id") == record.user_id]
        with self.db() as db:
            rows = db.execute("SELECT t.*, max(m.seq) AS recent FROM threads t LEFT JOIN thread_messages m ON m.thread=t.id WHERE recipient=? GROUP BY t.id ORDER BY recent DESC LIMIT 100", (record.user_id,)).fetchall()
        threads = []
        for t in rows:
            try:
                self.check_thread(t, record)
                threads.append({"id": t["id"], "subject": t["subject"]})
            except Error as error:
                if error.status != 403: raise
        return {"endpoint": {"service_id": self.config.service_id, "path": own["path"],
                             "folder_id": own.get("folder_id"), "activation": self.activation()},
                "contacts": contacts, "threads": threads, "admin": admin}

    def db(self):
        db = sqlite3.connect(self.config.database_path, timeout=15)
        db.row_factory = sqlite3.Row
        return db

    def start_oauth(self, return_to, browser):
        # Mail's OAuth implementation validates root-only returns. Generate PKCE with it,
        # then store the validated local thread return, never an arbitrary URL.
        if return_to != "/" and not re.fullmatch(r"/t/th_[a-f0-9]{32}/", return_to):
            raise Error(400, "Invalid login return path", "return_path_invalid")
        location = super().start_oauth("/")
        state = urllib.parse.parse_qs(urllib.parse.urlsplit(location).query)["state"][0]
        with self.sessions._connect() as db:
            db.execute(
                "UPDATE app_pending_oauth SET return_path=?,browser_hash=? WHERE state_hash=?",
                (return_to, mail._secret_hash(browser), mail._secret_hash(state)),
            )
        return location

    def finish_oauth(self, code, state, browser):
        code, state = text(code, "OAuth code", 4096), text(state, "OAuth state", 4096)
        with self.sessions._connect() as db:
            db.execute("BEGIN IMMEDIATE")
            row = db.execute("SELECT * FROM app_pending_oauth WHERE state_hash=? AND browser_hash=?",
                             (mail._secret_hash(state), mail._secret_hash(browser))).fetchone()
            if not row:
                raise Error(401, "OAuth state does not belong to this browser", "oauth_state_invalid")
            db.execute("DELETE FROM app_pending_oauth WHERE state_hash=?", (mail._secret_hash(state),))
        if row["expires_at"] <= time.time():
            raise Error(401, "OAuth state expired", "oauth_state_invalid")
        response = self.api.request("POST", "/api/apps/oauth/token", form={
            "grant_type": "authorization_code", "code": code, "client_id": self.config.service_id,
            "redirect_uri": self.config.callback_url, "code_verifier": row["verifier"]})
        token = text(response.get("access_token"), "access token", 8192)
        claims = self.principal(token, "human")
        expires = min(mail._timestamp(response.get("expires_at"), "token expiry"), claims["exp"])
        if expires <= time.time():
            raise Error(401, "OAuth identity expired", "oauth_identity_invalid")
        raw = "eep_" + secrets.token_urlsafe(48)
        self.sessions.save_session(raw, token, claims, expires)
        return raw, row["return_path"], max(1, int(expires - time.time()))

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
        directory = self.api.request("GET", f"/api/apps/{self.config.service_id}/directory", access_token=token)["principals"]
        p = next((p for p in directory if p.get("id") == c["sub"] and p.get("kind") == kind), None)
        if not p:
            raise Error(401, "Caller is no longer a workspace principal", "identity_invalid")
        if kind == "human": c["role"] = p.get("role", "member")
        return c

    def authenticated_session(self, raw):
        record = super().authenticated_session(raw)
        claims = self.principal(record.access_token, "human")
        if record.workspace_id != self.settings.workspace or claims["sub"] != record.user_id:
            raise Error(401, "Workspace identity mismatch", "identity_invalid")
        return self.sessions.update_principal(record, claims.get("name", record.preferred_name), claims["role"])

    def core(self, token, method, suffix="", body=None):
        return self.api.request(
            method,
            f"/api/apps/{self.config.service_id}/messages{suffix}",
            access_token=token,
            body=body,
        )

    def recipient(self, target, token):
        contacts = self.contact_view(token)
        matches = [c for c in contacts if target in (c["id"], c.get("user_id"), c["label"])
                   or target.lower() == c["email"]]
        # Legacy display names remain accepted only through the current directory.
        if not matches:
            humans = self.api.request("GET", f"/api/apps/{self.config.service_id}/directory", access_token=token)["principals"]
            ids = {p["id"] for p in humans if p["kind"] == "human" and p["name"] == target}
            matches = [c for c in contacts if c.get("user_id") in ids]
        if len(matches) != 1:
            raise Error(403, "Recipient must match one configured contact", "recipient_denied")
        c = matches[0]
        if not c["reply_available"]:
            raise Error(409, "Contact requires active workspace membership and administrator binding", "reply_unavailable")
        address = c["email"]
        if self.settings.allowlist and not any(address == a.lower() or
                (a.startswith("@") and address.rsplit("@", 1)[1] == a[1:].lower()) for a in self.settings.allowlist):
            raise Error(403, "Recipient is outside the configured allowlist", "recipient_denied")
        if self.settings.mode == "proxy" and address != "doomspec@outlook.com":
            raise Error(403, "Real delivery is restricted to the approved recipient", "recipient_denied")
        return c["user_id"], address, c["id"]

    def notify(self, token, request, key):
        agent = self.principal(token)["sub"]
        self.require_enabled(agent)
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
        recipient, address, contact = self.recipient(target, token)
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
                        or t["contact"] != contact
                        or t["email"] != address
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
                        "INSERT INTO threads(id,agent,recipient,email,subject,contact) VALUES(?,?,?,?,?,?)",
                        (thread_id, agent, recipient, address, subject, contact),
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
            self.check_thread(t, record)
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
            self.check_thread(t, record)
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
        self.require_admin(record)
        with self.db() as db:
            rows = db.execute(
                "SELECT n.*,t.email,t.recipient,t.contact FROM notifications n JOIN threads t ON t.id=n.thread ORDER BY n.created DESC LIMIT 100"
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
                "SELECT n.*,t.email,t.recipient,t.contact FROM notifications n JOIN threads t ON t.id=n.thread WHERE n.status='queued' AND n.message IS NOT NULL AND n.next_try<=? ORDER BY n.created LIMIT 1",
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
                    self.require_enabled(n["agent"])
                    token = self.token_loader()
                    contact = next((c for c in self.contact_view(token) if c["id"] == n["contact"] and c["email"] == n["email"] and c.get("user_id") == n["recipient"] and c["reply_available"]), None)
                    if not contact:
                        raise Error(403, "Contact binding revoked", "recipient_denied")
                    if transport == "proxy" and n["email"] != "doomspec@outlook.com":
                        raise Error(403, "Real recipient denied", "recipient_denied")
                    if transport == "proxy":
                        try:
                            self.api.request(
                                "POST",
                                f"/api/apps/{self.config.service_id}/email",
                                access_token=token,
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
                        if isinstance(exc, (mail.AppApiError, Error))
                        else "delivery_unavailable"
                    )
                    status = (
                        "failed"
                        if (isinstance(exc, Error) and exc.status in (403, 409)) or n["attempts"] + 1 >= self.settings.max_attempts
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
        token = self.headers.get("X-Treer-Identity-Token")
        if token is not None:
            return token
        return self.headers.get("Authorization", "").removeprefix("Bearer ")

    def _session(self):
        token = self.headers.get("X-Treer-Identity-Token")
        if token is not None or "Authorization" in self.headers:
            c = self.server.application.principal(self._token(), "human")
            return mail.BrowserSession(
                "",
                self._token(),
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

    def _oauth_cookie(self, raw, max_age):
        return self._session_cookie(raw, max_age).replace(mail.SESSION_COOKIE + "=", "treer_email_endpoint_oauth=")

    def _oauth_browser(self):
        cookie = SimpleCookie()
        try: cookie.load(self.headers.get("Cookie", ""))
        except Exception: return ""
        value = cookie.get("treer_email_endpoint_oauth")
        return value.value if value else ""

    def _redirect_cookies(self, location, cookies):
        self.send_response(303)
        self._security_headers()
        self.send_header("Location", location)
        for cookie in cookies: self.send_header("Set-Cookie", cookie)
        self.send_header("Content-Length", "0")
        self.end_headers()

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
                if self.headers.get("Sec-Fetch-Site") not in (None, "same-origin", "same-site"):
                    raise Error(403, "Cross-site request denied", "csrf_denied")
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
            elif path == "/v1/endpoint" and method == "GET":
                if "X-Treer-Identity-Token" in self.headers or "Authorization" in self.headers:
                    token = self._token()
                    # Endpoint discovery is also useful to same-folder Agent callers.
                    result = app.api.request("POST", "/.treer/apps/identity/verify", body={"token": token, "audience": app.config.service_id})
                    if (result.get("claims") or {}).get("principal_kind") == "agent":
                        agent = app.principal(token)["sub"]
                        own = app.placement(agent)
                        self._json(200, {"endpoint": {"service_id": app.config.service_id, "path": own["path"], "folder_id": own.get("folder_id"), "activation": app.activation()}, "contacts": app.contact_view(token)})
                        return
                self._json(200, app.overview(self._session()))
            elif path == "/v1/admin/activation" and method == "POST":
                self._json(200, app.activate(self._session(), self._json_body()))
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
            elif path == "/api/auth/start" and method == "GET":
                browser = secrets.token_urlsafe(32)
                location = app.start_oauth(urllib.parse.parse_qs(parsed.query).get("return_to", ["/"])[0], browser)
                self._redirect_cookies(location, [self._oauth_cookie(browser, 600)])
            elif path == "/api/auth/callback" and method == "GET":
                query = urllib.parse.parse_qs(parsed.query)
                raw, location, max_age = app.finish_oauth(query.get("code", [""])[0], query.get("state", [""])[0], self._oauth_browser())
                # Return to allocated ingress, not the Proxy root when tunneled.
                self._redirect_cookies(urllib.parse.urljoin(app.config.public_url, location.lstrip("/")),
                                       [self._session_cookie(raw, max_age), self._oauth_cookie("", 0)])
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
                if (
                    not self._raw_session()
                    and self.headers.get("X-Treer-Identity-Token") is None
                ):
                    self._static("/", head=False)
                    return
                try:
                    record = self._session()
                except Error as exc:
                    if exc.status != 401 or "X-Treer-Identity-Token" in self.headers or "Authorization" in self.headers:
                        raise
                    self._static("/", head=False)
                    return
                # Recheck current member/contact binding on navigation as on APIs.
                with app.db() as db:
                    t = db.execute("SELECT * FROM threads WHERE id=?", (path.split("/")[2],)).fetchone()
                try:
                    app.check_thread(t, record)
                except Error as exc:
                    if exc.status != 403: raise
                    self._bytes(403, "text/html; charset=utf-8", b'<!doctype html><meta name="viewport" content="width=device-width"><h1>Access denied</h1><p>You are not the current recipient of this thread.</p>')
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
        [entry.strip() for entry in os.environ.get("EMAIL_ENDPOINT_ALLOWLIST", os.environ.get("TREER_EMAIL_ALLOWLIST", "")).split(",") if entry.strip()]
        if "EMAIL_ENDPOINT_ALLOWLIST" in os.environ or "TREER_EMAIL_ALLOWLIST" in os.environ
        else value.get("allowlist", []),
        value.get("wake_agents", []),
        value.get("mode", "outbox"),
        int(value.get("agent_limit", 10)),
        int(value.get("recipient_limit", 5)),
        float(value.get("retry_seconds", 30)),
        int(value.get("max_attempts", 5)),
    )
    settings.browser_origins = tuple(value.get("browser_origins", []))
    settings.contacts = value.get("contacts", [])
    settings.runtime_id = value.get("runtime_agent_id") or os.environ.get("TREER_AGENT_ID", "")
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
