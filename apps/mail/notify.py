"""Optional email notifications for unread Core Messages.

A background worker looks at each opted-in member's unacknowledged Core
deliveries using that member's own stored Mail sign-in grant (Mail cannot see
anyone's Messages otherwise). A Message still unread after the member's digest
window is batched with every other pending one into ONE email sent through the
Core App email route (`email_route.py`). Batch membership, subject and text are
frozen before the first attempt so every retry is the identical request under
the same idempotency key. Uncertain outcomes are never resent without a check.
"""

from __future__ import annotations

import hashlib
import json
import re
import secrets
import sqlite3
import threading
import time
import urllib.parse
from typing import Any, Callable

from email_route import EmailRoute, Outcome

SCOPES = ("direct", "all")
MAX_DIGEST_MINUTES = 1440
MAX_LISTED = 10
ADDRESS = re.compile(r"^[A-Za-z0-9.!#$%&'*+/=?^_`{|}~-]+@[A-Za-z0-9-]+(\.[A-Za-z0-9-]+)+$")
CONTROL = re.compile(r"[\x00-\x1f\x7f]")
UNCHECKABLE_RECHECK_SECONDS = 3600


class NotifyError(Exception):
    def __init__(self, status: int, code: str, message: str) -> None:
        super().__init__(message)
        self.status, self.code, self.message = status, code, message


def _clean(text: str, limit: int) -> str:
    text = CONTROL.sub(" ", text).strip()
    return text if len(text) <= limit else text[: max(0, limit - 1)].rstrip() + "…"


class Notifier:
    def __init__(self, db_path, route: EmailRoute, *, public_url: str, preview_chars: int,
                 unread: Callable[[str], tuple[list[dict], bool]],
                 directory_email: Callable[[str, str], str | None],
                 poll_seconds: float = 30, max_attempts: int = 5, retry_seconds: float = 60,
                 clock: Callable[[], float] = time.time) -> None:
        self.db_path = db_path
        self.route = route
        self.public_url = public_url
        self.preview_chars = preview_chars
        self.unread = unread
        self.directory_email = directory_email
        self.poll_seconds = poll_seconds
        self.max_attempts = max_attempts
        self.retry_seconds = retry_seconds
        self.clock = clock
        self.lock = threading.RLock()
        with self._db() as db:
            db.executescript("""
                CREATE TABLE IF NOT EXISTS notify_settings (
                    user_id TEXT PRIMARY KEY, enabled INTEGER NOT NULL DEFAULT 0,
                    address TEXT, directory_email TEXT,
                    digest_minutes INTEGER NOT NULL DEFAULT 0, scope TEXT NOT NULL DEFAULT 'direct',
                    baseline_done INTEGER NOT NULL DEFAULT 0, watch_error TEXT,
                    checked_at REAL, updated_at REAL NOT NULL);
                CREATE TABLE IF NOT EXISTS notify_pending (
                    user_id TEXT NOT NULL, message_id TEXT NOT NULL, first_seen REAL NOT NULL,
                    state TEXT NOT NULL, batch_id TEXT, PRIMARY KEY(user_id, message_id));
                CREATE TABLE IF NOT EXISTS notify_batches (
                    batch_id TEXT PRIMARY KEY, user_id TEXT NOT NULL, kind TEXT NOT NULL,
                    idempotency_key TEXT NOT NULL UNIQUE, address TEXT NOT NULL,
                    subject TEXT NOT NULL, text TEXT NOT NULL, message_ids TEXT NOT NULL,
                    status TEXT NOT NULL, attempts INTEGER NOT NULL DEFAULT 0,
                    next_try REAL NOT NULL, last_code TEXT, email_id TEXT,
                    created_at REAL NOT NULL, updated_at REAL NOT NULL);
                CREATE INDEX IF NOT EXISTS notify_batches_user ON notify_batches(user_id, created_at);
                CREATE TABLE IF NOT EXISTS notify_attempts (
                    batch_id TEXT NOT NULL, attempt INTEGER NOT NULL, at REAL NOT NULL,
                    result TEXT NOT NULL, code TEXT NOT NULL, PRIMARY KEY(batch_id, attempt));
            """)

    def _db(self) -> sqlite3.Connection:
        db = sqlite3.connect(self.db_path, timeout=10)
        db.row_factory = sqlite3.Row
        return db

    # Settings and status -------------------------------------------------

    def _settings_row(self, db, user_id: str) -> dict[str, Any]:
        row = db.execute("SELECT * FROM notify_settings WHERE user_id=?", (user_id,)).fetchone()
        return dict(row) if row else {"user_id": user_id, "enabled": 0, "address": None,
                                      "directory_email": None, "digest_minutes": 0, "scope": "direct",
                                      "baseline_done": 0, "watch_error": None, "checked_at": None}

    def remember_directory_email(self, user_id: str, email: str | None) -> None:
        if email and ADDRESS.match(email):
            with self.lock, self._db() as db:
                db.execute("UPDATE notify_settings SET directory_email=? WHERE user_id=?", (email, user_id))

    def update(self, user_id: str, patch: dict[str, Any], *, by_agent: bool) -> dict[str, Any]:
        unknown = set(patch) - {"enabled", "address", "digest_minutes", "scope"}
        if unknown:
            raise NotifyError(400, "notify_field_invalid", f"unknown field: {sorted(unknown)[0]}")
        if "enabled" in patch and not isinstance(patch["enabled"], bool):
            raise NotifyError(400, "notify_field_invalid", "enabled must be true or false")
        if "address" in patch:
            address = patch["address"]
            if address is not None and (not isinstance(address, str) or len(address) > 254
                                        or not ADDRESS.match(address)):
                raise NotifyError(400, "notify_address_invalid", "address must be an email address or null")
            if address is not None and by_agent:
                raise NotifyError(403, "notify_address_human_only",
                                  "only the member may set an email address; Agents may reset it to null")
        if "digest_minutes" in patch:
            value = patch["digest_minutes"]
            if not isinstance(value, int) or isinstance(value, bool) or not 0 <= value <= MAX_DIGEST_MINUTES:
                raise NotifyError(400, "notify_field_invalid", "digest_minutes must be 0-1440")
        if "scope" in patch and patch["scope"] not in SCOPES:
            raise NotifyError(400, "notify_field_invalid", "scope must be direct or all")
        with self.lock, self._db() as db:
            current = self._settings_row(db, user_id)
            merged = {**current, **{k: v for k, v in patch.items()}}
            enabled = bool(merged["enabled"])
            # Turning notifications on starts from now: Messages already unread
            # at that moment are never emailed.
            baseline = int(current["baseline_done"]) if enabled and current["enabled"] else 0
            db.execute("""INSERT OR REPLACE INTO notify_settings(user_id, enabled, address, directory_email,
                              digest_minutes, scope, baseline_done, watch_error, checked_at, updated_at)
                          VALUES(?,?,?,?,?,?,?,?,?,?)""",
                       (user_id, int(enabled), merged["address"], current["directory_email"],
                        int(merged["digest_minutes"]), merged["scope"], baseline,
                        current["watch_error"], current["checked_at"], self.clock()))
            if not enabled:
                db.execute("UPDATE notify_pending SET state='dropped' WHERE user_id=? AND state='pending'", (user_id,))
                db.execute("UPDATE notify_batches SET status='cancelled', last_code='disabled', updated_at=? "
                           "WHERE user_id=? AND kind='notification' AND status='queued'", (self.clock(), user_id))
        return self.status(user_id)

    def _grant(self, db, user_id: str) -> tuple[str, float] | None:
        row = db.execute("""SELECT access_token, expires_at FROM app_browser_sessions
                            WHERE user_id=? AND expires_at>? ORDER BY expires_at DESC LIMIT 1""",
                         (user_id, self.clock() + 60)).fetchone()
        return (row["access_token"], row["expires_at"]) if row else None

    def _state(self, db, settings: dict[str, Any], available: bool) -> tuple[str, float | None]:
        if not available:
            return "unavailable", None
        if not settings["enabled"]:
            return "disabled", None
        if not (settings["address"] or settings["directory_email"]):
            return "no_address", None
        grant = self._grant(db, settings["user_id"])
        if grant is None:
            return "login_required", None
        return "watching", grant[1]

    def status(self, user_id: str) -> dict[str, Any]:
        available = self.route.available()
        with self._db() as db:  # Read-only: never wait behind an in-flight send.
            settings = self._settings_row(db, user_id)
            state, expires = self._state(db, settings, available)
            pending = db.execute("SELECT count(*) FROM notify_pending WHERE user_id=? AND state='pending'",
                                 (user_id,)).fetchone()[0]
            batches = [self._batch_view(db, dict(row)) for row in db.execute(
                "SELECT * FROM notify_batches WHERE user_id=? ORDER BY created_at DESC LIMIT 20", (user_id,))]
        return {
            "user_id": user_id,
            "available": available,
            "unavailable_reason": None if available else "core_email_route_absent",
            "capability": self.route.capability,
            "settings": {
                "enabled": bool(settings["enabled"]),
                "address": settings["address"],
                "default_address": settings["directory_email"],
                "effective_address": settings["address"] or settings["directory_email"],
                "digest_minutes": settings["digest_minutes"],
                "scope": settings["scope"],
            },
            "watch": {"state": state, "grant_expires_at": _iso(expires),
                      "last_checked_at": _iso(settings["checked_at"]), "last_error": settings["watch_error"]},
            "pending_messages": pending,
            "preview_chars": self.preview_chars,
            "max_attempts": self.max_attempts,
            "batches": batches,
        }

    def members(self) -> list[dict[str, Any]]:
        with self._db() as db:
            users = [row[0] for row in db.execute("SELECT user_id FROM notify_settings ORDER BY user_id")]
        return [self.status(user) for user in users]

    def _batch_view(self, db, batch: dict[str, Any]) -> dict[str, Any]:
        attempts = [dict(row) | {"at": _iso(row["at"])} for row in db.execute(
            "SELECT attempt, at, result, code FROM notify_attempts WHERE batch_id=? ORDER BY attempt",
            (batch["batch_id"],))]
        return {
            "batch_id": batch["batch_id"], "kind": batch["kind"], "status": batch["status"],
            "address": batch["address"], "subject": batch["subject"],
            "message_ids": json.loads(batch["message_ids"]), "attempts": attempts,
            "next_try_at": _iso(batch["next_try"]) if batch["status"] in ("queued", "uncertain") else None,
            "last_code": batch["last_code"], "email_id": batch["email_id"],
            "created_at": _iso(batch["created_at"]), "updated_at": _iso(batch["updated_at"]),
        }

    # Sending ---------------------------------------------------------------

    def _new_batch(self, db, user_id: str, kind: str, key: str, address: str, subject: str,
                   text: str, message_ids: list[str]) -> str:
        batch_id = "nb_" + hashlib.sha256(key.encode()).hexdigest()[:24]
        now = self.clock()
        db.execute("""INSERT INTO notify_batches(batch_id, user_id, kind, idempotency_key, address, subject,
                          text, message_ids, status, next_try, created_at, updated_at)
                      VALUES(?,?,?,?,?,?,?,?, 'queued', ?, ?, ?)""",
                   (batch_id, user_id, kind, key, address, subject, text, json.dumps(message_ids), now, now, now))
        return batch_id

    def test(self, user_id: str, key: str | None) -> dict[str, Any]:
        if not self.route.available():
            raise NotifyError(503, "notify_unavailable", "Core email route is not available on this Treer")
        with self.lock, self._db() as db:
            settings = self._settings_row(db, user_id)
            address = settings["address"] or settings["directory_email"]
            if not address:
                raise NotifyError(409, "notify_address_missing", "set an email address first")
            key = "mail-test-" + hashlib.sha256(f"{user_id}\0{key or secrets.token_hex(16)}".encode()).hexdigest()[:40]
            existing = db.execute("SELECT batch_id FROM notify_batches WHERE idempotency_key=?", (key,)).fetchone()
            if existing:
                batch_id = existing[0]
            else:
                text = ("This is a test email from Treer Mail. Notifications for unread Messages will look "
                        f"like this.\n\nInbox (Treer sign-in required): {self.public_url}\n")
                batch_id = self._new_batch(db, user_id, "test", key, address, "Treer Mail test email", text, [])
            if db.execute("SELECT status FROM notify_batches WHERE batch_id=?", (batch_id,)).fetchone()[0] == "queued":
                self._deliver(db, batch_id)
            return self._batch_view(db, dict(db.execute(
                "SELECT * FROM notify_batches WHERE batch_id=?", (batch_id,)).fetchone()))

    def retry(self, user_id: str | None, batch_id: str) -> dict[str, Any]:
        """Explicit retry of a failed or uncertain batch: the identical request under
        the same idempotency key. For an uncertain batch this is the caller's
        decision that a duplicate is acceptable or that it was not delivered."""
        with self.lock, self._db() as db:
            row = db.execute("SELECT * FROM notify_batches WHERE batch_id=?", (batch_id,)).fetchone()
            if row is None or (user_id is not None and row["user_id"] != user_id):
                raise NotifyError(404, "notify_batch_not_found", "notification batch not found")
            if row["status"] not in ("failed", "uncertain"):
                raise NotifyError(409, "notify_batch_not_retryable", "only failed or uncertain batches can be retried")
            db.execute("UPDATE notify_batches SET status='queued', attempts=0, next_try=? WHERE batch_id=?",
                       (self.clock(), batch_id))
            self._deliver(db, batch_id)
            return self._batch_view(db, dict(db.execute(
                "SELECT * FROM notify_batches WHERE batch_id=?", (batch_id,)).fetchone()))

    def _deliver(self, db, batch_id: str) -> None:
        batch = dict(db.execute("SELECT * FROM notify_batches WHERE batch_id=?", (batch_id,)).fetchone())
        db.commit()  # Freeze the batch before the external call.
        outcome: Outcome = self.route.send(batch["address"], batch["subject"], batch["text"],
                                           batch["idempotency_key"])
        attempts = batch["attempts"] + 1
        now = self.clock()
        if outcome.result == "sent":
            status, next_try = "sent", now
        elif outcome.result == "permanent" or (outcome.result == "retryable" and attempts >= self.max_attempts):
            status, next_try = "failed", now
        elif outcome.result == "retryable":
            status, next_try = "queued", now + min(3600, self.retry_seconds * 2 ** (attempts - 1))
        else:
            status, next_try = "uncertain", now + self.retry_seconds
        db.execute("INSERT INTO notify_attempts SELECT ?, count(*) + 1, ?, ?, ? FROM notify_attempts "
                   "WHERE batch_id=?", (batch_id, now, outcome.result, outcome.code, batch_id))
        db.execute("""UPDATE notify_batches SET status=?, attempts=?, next_try=?, last_code=?,
                          email_id=coalesce(?, email_id), updated_at=? WHERE batch_id=?""",
                   (status, attempts, next_try, outcome.code, outcome.email_id, now, batch_id))
        db.commit()

    def _check_uncertain(self, db, batch: dict[str, Any]) -> None:
        answer = self.route.lookup(batch["idempotency_key"])
        now = self.clock()
        if answer == "sent":
            db.execute("UPDATE notify_batches SET status='sent', last_code='confirmed_sent', updated_at=? "
                       "WHERE batch_id=?", (now, batch["batch_id"]))
        elif answer == "not_sent" and batch["attempts"] < self.max_attempts:
            db.execute("UPDATE notify_batches SET status='queued', next_try=?, updated_at=? WHERE batch_id=?",
                       (now, now, batch["batch_id"]))
            self._deliver(db, batch["batch_id"])
        elif answer == "not_sent":
            db.execute("UPDATE notify_batches SET status='failed', updated_at=? WHERE batch_id=?",
                       (now, batch["batch_id"]))
        else:  # Core cannot tell us; wait for an explicit retry and look again later.
            db.execute("UPDATE notify_batches SET next_try=? WHERE batch_id=?",
                       (now + UNCHECKABLE_RECHECK_SECONDS, batch["batch_id"]))
        db.commit()

    # Watching ------------------------------------------------------------------

    def tick(self) -> None:
        available = self.route.available()
        with self.lock, self._db() as db:
            if available:
                for row in db.execute("SELECT * FROM notify_settings WHERE enabled=1").fetchall():
                    self._watch(db, dict(row))
                now = self.clock()
                for row in db.execute("SELECT * FROM notify_batches WHERE status IN ('queued','uncertain') "
                                      "AND next_try<=? ORDER BY created_at", (now,)).fetchall():
                    if row["status"] == "queued":
                        self._deliver(db, row["batch_id"])
                    else:
                        self._check_uncertain(db, dict(row))

    def _watch(self, db, settings: dict[str, Any]) -> None:
        user_id = settings["user_id"]
        grant = self._grant(db, user_id)
        if grant is None:
            return
        now = self.clock()
        try:
            messages, complete = self.unread(grant[0])
            email = self.directory_email(grant[0], user_id)
        except Exception as error:  # Keep the worker alive; report the code only.
            db.execute("UPDATE notify_settings SET watch_error=?, checked_at=? WHERE user_id=?",
                       (getattr(error, "code", type(error).__name__), now, user_id))
            db.commit()
            return
        if email and ADDRESS.match(email):
            settings["directory_email"] = email
        db.execute("UPDATE notify_settings SET watch_error=NULL, checked_at=?, directory_email=? WHERE user_id=?",
                   (now, settings["directory_email"], user_id))
        wanted = {m["message_id"]: m for m in messages if self._wanted(m, user_id, settings["scope"])}
        if not settings["baseline_done"]:
            for message_id in wanted:
                db.execute("INSERT OR IGNORE INTO notify_pending VALUES(?,?,?, 'skipped', NULL)",
                           (user_id, message_id, now))
            if complete:
                db.execute("UPDATE notify_settings SET baseline_done=1 WHERE user_id=?", (user_id,))
            db.commit()
            return
        for message_id in wanted:
            db.execute("INSERT OR IGNORE INTO notify_pending VALUES(?,?,?, 'pending', NULL)",
                       (user_id, message_id, now))
        if complete:
            # Read (acknowledged) since we last looked: drop it and any unsent batch of only-read Messages.
            for row in db.execute("SELECT message_id FROM notify_pending WHERE user_id=? AND state='pending'",
                                  (user_id,)).fetchall():
                if row[0] not in wanted:
                    db.execute("UPDATE notify_pending SET state='read' WHERE user_id=? AND message_id=?",
                               (user_id, row[0]))
            for row in db.execute("SELECT batch_id, message_ids FROM notify_batches WHERE user_id=? "
                                  "AND kind='notification' AND status='queued'", (user_id,)).fetchall():
                if not set(json.loads(row[1])) & set(wanted):
                    db.execute("UPDATE notify_batches SET status='cancelled', last_code='read_before_send', "
                               "updated_at=? WHERE batch_id=?", (now, row[0]))
        pending = db.execute("SELECT message_id, first_seen FROM notify_pending WHERE user_id=? "
                             "AND state='pending' ORDER BY first_seen", (user_id,)).fetchall()
        due = [row for row in pending if row[0] in wanted]
        address = settings["address"] or settings["directory_email"]
        if not due or not address or now - due[0][1] < settings["digest_minutes"] * 60:
            db.commit()
            return
        due = [row[0] for row in due]
        ids = sorted(due)
        key = "mail-notify-" + hashlib.sha256("\0".join([user_id, *ids]).encode()).hexdigest()[:40]
        subject, text = self._compose([wanted[i] for i in due])
        if db.execute("SELECT 1 FROM notify_batches WHERE idempotency_key=?", (key,)).fetchone() is None:
            batch_id = self._new_batch(db, user_id, "notification", key, address, subject, text, ids)
        else:
            batch_id = "nb_" + hashlib.sha256(key.encode()).hexdigest()[:24]
        db.executemany("UPDATE notify_pending SET state='batched', batch_id=? WHERE user_id=? AND message_id=?",
                       [(batch_id, user_id, i) for i in ids])
        db.commit()

    @staticmethod
    def _wanted(message: dict[str, Any], user_id: str, scope: str) -> bool:
        sender = message.get("sender") or {}
        if sender.get("kind") == "human" and sender.get("id") == user_id:
            return False
        recipients = message.get("recipients") or []
        mine = [r for r in recipients if isinstance(r, dict) and r.get("kind") == "human" and r.get("id") == user_id]
        if scope == "direct":
            return len(recipients) == 1 and len(mine) == 1
        return True

    def _link(self, message_id: str) -> str:
        return self.public_url + "?" + urllib.parse.urlencode({"message": message_id})

    def _compose(self, messages: list[dict[str, Any]]) -> tuple[str, str]:
        def line(message: dict[str, Any]) -> str:
            sender = _clean(str((message.get("sender") or {}).get("name") or "Someone"), 60)
            first = str(message.get("body") or "").strip().split("\n", 1)[0]
            preview = _clean(first, self.preview_chars) if self.preview_chars else ""
            return f"{sender}: {preview}" if preview else sender
        if len(messages) == 1:
            sender = _clean(str((messages[0].get("sender") or {}).get("name") or "Someone"), 60)
            subject = f"New Treer message from {sender}"
        else:
            subject = f"{len(messages)} unread Treer messages"
        parts = []
        for message in messages[:MAX_LISTED]:
            parts.append(f"{line(message)}\n{self._link(message['message_id'])}")
        if len(messages) > MAX_LISTED:
            parts.append(f"...and {len(messages) - MAX_LISTED} more.")
        text = ("\n\n".join(parts) + f"\n\nInbox (Treer sign-in required): {self.public_url}\n"
                "You get this because email notifications are on in Treer Mail; "
                f"change them under Notifications at {self.public_url}\n")
        return _clean(subject, 200), text.encode("utf-8")[:4096].decode("utf-8", "ignore")

    def run_forever(self, stop: threading.Event) -> None:
        while not stop.is_set():
            try:
                self.tick()
            except Exception as error:  # pragma: no cover - logged, never fatal
                import sys
                print(f"Treer Mail notifier tick failed: {type(error).__name__}", file=sys.stderr, flush=True)
            stop.wait(self.poll_seconds)


def _iso(value: float | None) -> str | None:
    if value is None:
        return None
    return time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(value))
