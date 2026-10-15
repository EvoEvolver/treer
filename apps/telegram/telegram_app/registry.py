"""Channel-agnostic POC registry: bindings, admitted IM users, audit trail and rate limits.

The tables are keyed by (channel, account, conversation) so a second IM adapter can reuse them unchanged; the
Telegram-specific delivery state in state.py is untouched. The configuration file only *seeds* the registry
(INSERT OR IGNORE): afterwards the registry is authoritative and changes made through IM commands or Core control
messages survive restarts and later config edits.
"""
from __future__ import annotations

import hashlib
import hmac
import secrets
import sqlite3
import time
from dataclasses import dataclass

PAIR_TTL = 600.0               # a pairing code lives ten minutes
PAIR_ALPHABET = "ABCDEFGHJKMNPQRSTVWXYZ23456789"   # 30 symbols, no look-alikes: 10 characters ~ 49 bits
PAIR_LENGTH = 10
PAIR_USER_FAILURES = 5          # failed attempts per Telegram user per window before that user is blocked
PAIR_GLOBAL_FAILURES = 20       # failed attempts overall per window before every open code is invalidated
PAIR_WINDOW = 600.0
AUDIT_KEEP = 5000      # changes only; rejected requests go to their own small ring so they cannot flush this
REJECT_KEEP = 200
RATE_KEEP_SECONDS = 3700

REGISTRY_SCHEMA = """
CREATE TABLE IF NOT EXISTS poc_bindings (
    channel TEXT NOT NULL,
    account_id TEXT NOT NULL,
    conversation TEXT NOT NULL,
    target_agent_id TEXT NOT NULL,
    wake_agent INTEGER NOT NULL DEFAULT 0,
    enabled INTEGER NOT NULL DEFAULT 1,
    updated_at REAL NOT NULL,
    updated_by TEXT NOT NULL,
    PRIMARY KEY(channel, account_id, conversation)
);
CREATE TABLE IF NOT EXISTS im_users (
    channel TEXT NOT NULL,
    account_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    label TEXT NOT NULL,
    enabled INTEGER NOT NULL DEFAULT 1,
    source TEXT NOT NULL DEFAULT 'config',
    PRIMARY KEY(channel, account_id, user_id)
);
CREATE TABLE IF NOT EXISTS poc_notices (
    conversation TEXT NOT NULL,
    recipient TEXT NOT NULL,
    at REAL NOT NULL,
    PRIMARY KEY(conversation, recipient)
);
CREATE TABLE IF NOT EXISTS settings (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL,
    updated_at REAL NOT NULL
);
CREATE TABLE IF NOT EXISTS pairings (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    salt TEXT NOT NULL,
    code_hash TEXT NOT NULL,
    created_by TEXT NOT NULL,
    poc_agent_id TEXT NOT NULL,
    created_at REAL NOT NULL,
    expires_at REAL NOT NULL,
    used_at REAL,
    used_by TEXT,
    invalidated_at REAL
);
CREATE TABLE IF NOT EXISTS pair_attempts (
    user_id TEXT NOT NULL,
    at REAL NOT NULL,
    ok INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS pair_attempts_at ON pair_attempts(at);
CREATE TABLE IF NOT EXISTS poc_rejects (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    at REAL NOT NULL,
    actor TEXT NOT NULL,
    op TEXT NOT NULL,
    code TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS poc_audit (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    at REAL NOT NULL,
    delivery_id TEXT UNIQUE,
    actor TEXT NOT NULL,
    via TEXT NOT NULL,
    op TEXT NOT NULL,
    channel TEXT NOT NULL,
    account_id TEXT NOT NULL,
    conversation TEXT,
    old_target TEXT,
    new_target TEXT,
    outcome TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS rate_events (
    channel TEXT NOT NULL,
    account_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    conversation TEXT NOT NULL,
    at REAL NOT NULL
);
CREATE INDEX IF NOT EXISTS rate_events_user ON rate_events(channel, account_id, user_id, at);
CREATE INDEX IF NOT EXISTS rate_events_conversation ON rate_events(channel, account_id, conversation, at);
"""


@dataclass(frozen=True)
class PocBinding:
    conversation: str
    target_agent_id: str
    wake_agent: bool
    enabled: bool
    updated_at: float
    updated_by: str


def _binding(row: sqlite3.Row) -> PocBinding:
    return PocBinding(row["conversation"], row["target_agent_id"], bool(row["wake_agent"]), bool(row["enabled"]),
                      row["updated_at"], row["updated_by"])


class RegistryMixin:
    """Mixed into StateStore: uses its lock and connections."""

    def reconcile_registry(self, channel: str, account: str, bindings: list[tuple[str, str, bool]], users: list[tuple[str, str]]) -> list[str]:
        """Aligns the config-owned rows with the configuration, and returns a log line per change.

        Rows the configuration created and nobody has touched since (users with source 'config', bindings with
        updated_by 'config') follow the file: new entries are inserted, changed ones updated, removed ones are
        DISABLED (and audited) so deleting an entry and restarting revokes it. Anything changed at runtime - by /poc,
        a control message or users.set - is runtime-owned and is never overwritten by the file."""
        now = time.time()
        changes: list[str] = []
        with self._lock, self._connect() as db:
            db.execute("BEGIN IMMEDIATE")
            wanted_users = dict(users)
            for user_id, label in wanted_users.items():
                row = db.execute("SELECT label, enabled, source FROM im_users WHERE channel=? AND account_id=? AND user_id=?", (channel, account, user_id)).fetchone()
                if row is None:
                    db.execute("INSERT INTO im_users VALUES (?,?,?,?,1,'config')", (channel, account, user_id, label))
                elif row["source"] == "config" and (row["label"] != label or not row["enabled"]):
                    db.execute("UPDATE im_users SET label=?, enabled=1 WHERE channel=? AND account_id=? AND user_id=?", (label, channel, account, user_id))
                    self._audit(db, now, None, "config", "startup", "config.user", channel, account, None, None, f"user:{user_id}:on", "ok")
                    changes.append(f"user {user_id} enabled from config")
            for row in db.execute("SELECT user_id FROM im_users WHERE channel=? AND account_id=? AND source='config' AND enabled=1", (channel, account)).fetchall():
                if row["user_id"] not in wanted_users:
                    db.execute("UPDATE im_users SET enabled=0 WHERE channel=? AND account_id=? AND user_id=?", (channel, account, row["user_id"]))
                    self._audit(db, now, None, "config", "startup", "config.revoke_user", channel, account, None, None, f"user:{row['user_id']}:off", "ok")
                    changes.append(f"user {row['user_id']} removed from config: disabled")
            wanted = {conversation: (target, wake) for conversation, target, wake in bindings}
            for conversation, (target, wake) in wanted.items():
                row = db.execute("SELECT * FROM poc_bindings WHERE channel=? AND account_id=? AND conversation=?", (channel, account, conversation)).fetchone()
                if row is None:
                    db.execute("INSERT INTO poc_bindings VALUES (?,?,?,?,?,1,?,'config')", (channel, account, conversation, target, int(wake), now))
                elif row["updated_by"] == "config" and (row["target_agent_id"], bool(row["wake_agent"]), bool(row["enabled"])) != (target, wake, True):
                    db.execute("UPDATE poc_bindings SET target_agent_id=?, wake_agent=?, enabled=1, updated_at=? WHERE channel=? AND account_id=? AND conversation=?",
                               (target, int(wake), now, channel, account, conversation))
                    self._audit(db, now, None, "config", "startup", "config.binding", channel, account, conversation, row["target_agent_id"], target, "ok")
                    changes.append(f"binding {conversation} follows config")
            for row in db.execute("SELECT conversation, target_agent_id FROM poc_bindings WHERE channel=? AND account_id=? AND updated_by='config' AND enabled=1",
                                  (channel, account)).fetchall():
                if row["conversation"] not in wanted:
                    db.execute("UPDATE poc_bindings SET enabled=0, updated_at=? WHERE channel=? AND account_id=? AND conversation=?", (now, channel, account, row["conversation"]))
                    self._audit(db, now, None, "config", "startup", "config.revoke_binding", channel, account, row["conversation"], row["target_agent_id"], None, "ok")
                    changes.append(f"binding {row['conversation']} removed from config: disabled")
            db.commit()
        return changes

    def get_binding(self, channel: str, account: str, conversation: str) -> PocBinding | None:
        with self._lock, self._connect() as db:
            row = db.execute("SELECT * FROM poc_bindings WHERE channel=? AND account_id=? AND conversation=?",
                             (channel, account, conversation)).fetchone()
        return _binding(row) if row else None

    def list_bindings(self, channel: str, account: str) -> list[PocBinding]:
        with self._lock, self._connect() as db:
            rows = db.execute("SELECT * FROM poc_bindings WHERE channel=? AND account_id=? ORDER BY conversation",
                              (channel, account)).fetchall()
        return [_binding(row) for row in rows]

    def bindings_for_agent(self, channel: str, account: str, agent_id: str) -> list[PocBinding]:
        return [b for b in self.list_bindings(channel, account) if b.target_agent_id == agent_id and b.enabled]

    def set_binding(self, channel: str, account: str, conversation: str, *, target: str | None, enabled: bool | None,
                    wake: bool | None, actor: str, via: str, op: str, delivery_id: str | None = None,
                    outcome: str = "ok") -> tuple[PocBinding | None, PocBinding | None]:
        """Atomically change a binding and write the audit row; returns (old, new). Creating a binding needs a target."""
        now = time.time()
        with self._lock, self._connect() as db:
            db.execute("BEGIN IMMEDIATE")
            row = db.execute("SELECT * FROM poc_bindings WHERE channel=? AND account_id=? AND conversation=?",
                             (channel, account, conversation)).fetchone()
            old = _binding(row) if row else None
            if old is None and not target:
                raise KeyError("no such binding")
            new_target = target or old.target_agent_id
            new_enabled = (old.enabled if old else True) if enabled is None else enabled
            new_wake = (old.wake_agent if old else False) if wake is None else wake
            db.execute("""INSERT INTO poc_bindings VALUES (?,?,?,?,?,?,?,?)
                          ON CONFLICT(channel, account_id, conversation) DO UPDATE SET
                          target_agent_id=excluded.target_agent_id, wake_agent=excluded.wake_agent, enabled=excluded.enabled,
                          updated_at=excluded.updated_at, updated_by=excluded.updated_by""",
                       (channel, account, conversation, new_target, int(new_wake), int(new_enabled), now, actor))
            self._audit(db, now, delivery_id, actor, via, op, channel, account, conversation,
                        old.target_agent_id if old else None, new_target if new_enabled else None, outcome)
            db.commit()
        return old, PocBinding(conversation, new_target, new_wake, new_enabled, now, actor)

    def audit(self, actor: str, via: str, op: str, channel: str, account: str, conversation: str | None, *,
              old_target: str | None = None, new_target: str | None = None, outcome: str = "ok",
              delivery_id: str | None = None) -> bool:
        """Records an event; False when this delivery was already recorded (a Core redelivery)."""
        with self._lock, self._connect() as db:
            try:
                db.execute("BEGIN IMMEDIATE")
                self._audit(db, time.time(), delivery_id, actor, via, op, channel, account, conversation, old_target, new_target, outcome)
                db.commit()
            except sqlite3.IntegrityError:
                db.rollback()
                return False
        return True

    def delivery_audited(self, delivery_id: str) -> bool:
        with self._lock, self._connect() as db:
            return db.execute("SELECT 1 FROM poc_audit WHERE delivery_id=?", (delivery_id,)).fetchone() is not None

    @staticmethod
    def _audit(db: sqlite3.Connection, now: float, delivery_id, actor, via, op, channel, account, conversation, old, new, outcome) -> None:
        db.execute("""INSERT INTO poc_audit(at, delivery_id, actor, via, op, channel, account_id, conversation, old_target, new_target, outcome)
                      VALUES (?,?,?,?,?,?,?,?,?,?,?)""", (now, delivery_id, actor[:256], via, op, channel, account, conversation, old, new, outcome[:256]))
        db.execute("DELETE FROM poc_audit WHERE id <= (SELECT MAX(id) FROM poc_audit) - ?", (AUDIT_KEEP,))

    def audit_list(self, channel: str, account: str, limit: int = 20) -> list[dict]:
        with self._lock, self._connect() as db:
            rows = db.execute("""SELECT at, actor, via, op, conversation, old_target, new_target, outcome FROM poc_audit
                                 WHERE channel=? AND account_id=? ORDER BY id DESC LIMIT ?""", (channel, account, limit)).fetchall()
        return [dict(row) for row in rows]

    # --- admitted IM users -------------------------------------------------------------------------

    def get_user(self, channel: str, account: str, user_id: str) -> tuple[str, bool] | None:
        with self._lock, self._connect() as db:
            row = db.execute("SELECT label, enabled FROM im_users WHERE channel=? AND account_id=? AND user_id=?",
                             (channel, account, user_id)).fetchone()
        return (row["label"], bool(row["enabled"])) if row else None

    def set_user(self, channel: str, account: str, user_id: str, label: str, enabled: bool, *, actor: str, delivery_id: str | None = None,
                 via: str = "control", op: str = "users.set") -> None:
        """Runtime-owned from now on (source 'control'): the config no longer overrides it. Audited in the same transaction."""
        with self._lock, self._connect() as db:
            db.execute("BEGIN IMMEDIATE")
            db.execute("""INSERT INTO im_users VALUES (?,?,?,?,?,'control') ON CONFLICT(channel, account_id, user_id)
                          DO UPDATE SET label=excluded.label, enabled=excluded.enabled, source='control'""", (channel, account, user_id, label, int(enabled)))
            self._audit(db, time.time(), delivery_id, actor, via, op, channel, account, None, None, f"user:{user_id}:{'on' if enabled else 'off'}", "ok")
            db.commit()

    def claim_notice(self, conversation: str, recipient: str, window: float = 300.0, now: float | None = None) -> bool:
        """True at most once per (conversation, recipient) per window: flipping a POC back and forth cannot spam either side."""
        now = time.time() if now is None else now
        with self._lock, self._connect() as db:
            db.execute("BEGIN IMMEDIATE")
            row = db.execute("SELECT at FROM poc_notices WHERE conversation=? AND recipient=?", (conversation, recipient)).fetchone()
            if row and now - row["at"] < window:
                db.commit()
                return False
            db.execute("INSERT OR REPLACE INTO poc_notices VALUES (?,?,?)", (conversation, recipient, now))
            db.execute("DELETE FROM poc_notices WHERE at < ?", (now - 24 * 3600,))
            db.commit()
        return True

    def audit_reject(self, actor: str, op: str, code: str) -> None:
        """Rejected or invalid requests: a small ring of their own, so cheap bad requests cannot push change records out."""
        with self._lock, self._connect() as db:
            db.execute("INSERT INTO poc_rejects(at, actor, op, code) VALUES (?,?,?,?)", (time.time(), actor[:256], op[:64], code[:64]))
            db.execute("DELETE FROM poc_rejects WHERE id <= (SELECT MAX(id) FROM poc_rejects) - ?", (REJECT_KEEP,))

    def rejects_list(self, limit: int = 20) -> list[dict]:
        with self._lock, self._connect() as db:
            rows = db.execute("SELECT at, actor, op, code FROM poc_rejects ORDER BY id DESC LIMIT ?", (limit,)).fetchall()
        return [dict(row) for row in rows]

    def list_users(self, channel: str, account: str) -> list[dict]:
        with self._lock, self._connect() as db:
            rows = db.execute("SELECT user_id, label, enabled FROM im_users WHERE channel=? AND account_id=? ORDER BY user_id",
                              (channel, account)).fetchall()
        return [{"user_id": r["user_id"], "label": r["label"], "enabled": bool(r["enabled"])} for r in rows]

    # --- abuse limits -------------------------------------------------------------------------------

    def record_inbound(self, channel: str, account: str, user_id: str, conversation: str, now: float | None = None) -> None:
        """Counts an inbound message against the limits - called once Core accepted it, so retries are never charged."""
        with self._lock, self._connect() as db:
            db.execute("INSERT INTO rate_events VALUES (?,?,?,?,?)", (channel, account, user_id, conversation, time.time() if now is None else now))

    def admit_inbound(self, channel: str, account: str, user_id: str, conversation: str, *, per_user_per_hour: int,
                      per_conversation_per_minute: int, max_unreplied: int, now: float | None = None) -> str | None:
        """None when the message may pass, otherwise the name of the limit that stopped it. Limits: messages per
        user per hour, per conversation per minute, and unanswered messages per conversation (inbound messages
        since the POC last wrote to it). A check never records; see record_inbound."""
        now = time.time() if now is None else now
        with self._lock, self._connect() as db:
            db.execute("BEGIN IMMEDIATE")
            db.execute("DELETE FROM rate_events WHERE at < ?", (now - RATE_KEEP_SECONDS,))
            hour = db.execute("SELECT COUNT(*) FROM rate_events WHERE channel=? AND account_id=? AND user_id=? AND at > ?",
                              (channel, account, user_id, now - 3600)).fetchone()[0]
            if hour >= per_user_per_hour:
                db.commit()
                return "per_user_per_hour"
            minute = db.execute("SELECT COUNT(*) FROM rate_events WHERE channel=? AND account_id=? AND conversation=? AND at > ?",
                                (channel, account, conversation, now - 60)).fetchone()[0]
            if minute >= per_conversation_per_minute:
                db.commit()
                return "per_conversation_per_minute"
            if self._unreplied(db, conversation) >= max_unreplied:
                db.commit()
                return "max_unreplied"
            db.commit()
        return None

    @staticmethod
    def _unreplied(db: sqlite3.Connection, conversation: str) -> int:
        chat, _, thread = conversation.rpartition(":")
        thread_key = -1 if thread == "0" else int(thread)
        last_out = db.execute("""SELECT COALESCE(MAX(created_at), 0) FROM message_mappings
                                 WHERE chat_id=? AND thread_key=? AND direction='outbound'""", (chat, thread_key)).fetchone()[0]
        return db.execute("""SELECT COUNT(*) FROM message_mappings WHERE chat_id=? AND thread_key=? AND direction='inbound'
                             AND created_at > ?""", (chat, thread_key, last_out)).fetchone()[0]

    # --- shared settings (written by the settings App and the bridge process) ------------------------

    def get_setting(self, key: str, default: str | None = None) -> str | None:
        with self._lock, self._connect() as db:
            row = db.execute("SELECT value FROM settings WHERE key=?", (key,)).fetchone()
        return row["value"] if row else default

    def set_setting(self, key: str, value: str) -> None:
        with self._lock, self._connect() as db:
            db.execute("INSERT INTO settings VALUES (?,?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value, updated_at=excluded.updated_at",
                       (key, value[:512], time.time()))

    def delete_settings(self, *keys: str) -> None:
        with self._lock, self._connect() as db:
            for key in keys:
                db.execute("DELETE FROM settings WHERE key=?", (key,))

    def settings_with_age(self, prefix: str = "") -> dict[str, tuple[str, float]]:
        with self._lock, self._connect() as db:
            rows = db.execute("SELECT key, value, updated_at FROM settings WHERE key LIKE ?", (prefix + "%",)).fetchall()
        return {row["key"]: (row["value"], row["updated_at"]) for row in rows}

    # --- pairing codes ---------------------------------------------------------------------------

    @staticmethod
    def _normalise_code(value: str) -> str:
        return "".join(ch for ch in value.upper() if ch not in " -_\t\n")

    @staticmethod
    def _code_hash(salt: str, code: str) -> str:
        return hashlib.sha256((salt + ":" + code).encode()).hexdigest()

    def create_pairing(self, created_by: str, poc_agent_id: str, ttl: float = PAIR_TTL, now: float | None = None) -> tuple[str, float]:
        """A fresh one-time code bound to its creator and target POC. Every earlier open code dies at once. The code
        is returned exactly once; only a salted hash is stored."""
        now = time.time() if now is None else now
        code = "".join(secrets.choice(PAIR_ALPHABET) for _ in range(PAIR_LENGTH))
        salt = secrets.token_hex(8)
        with self._lock, self._connect() as db:
            db.execute("BEGIN IMMEDIATE")
            db.execute("UPDATE pairings SET invalidated_at=? WHERE used_at IS NULL AND invalidated_at IS NULL", (now,))
            db.execute("INSERT INTO pairings(salt, code_hash, created_by, poc_agent_id, created_at, expires_at) VALUES (?,?,?,?,?,?)",
                       (salt, self._code_hash(salt, code), created_by[:256], poc_agent_id, now, now + ttl))
            db.commit()
        return f"{code[:5]}-{code[5:]}", now + ttl

    def pairing_status(self, now: float | None = None) -> dict | None:
        now = time.time() if now is None else now
        with self._lock, self._connect() as db:
            row = db.execute("""SELECT created_by, poc_agent_id, created_at, expires_at FROM pairings
                                WHERE used_at IS NULL AND invalidated_at IS NULL AND expires_at > ? ORDER BY id DESC LIMIT 1""", (now,)).fetchone()
        return dict(row) if row else None

    def invalidate_pairings(self, now: float | None = None) -> int:
        now = time.time() if now is None else now
        with self._lock, self._connect() as db:
            return db.execute("UPDATE pairings SET invalidated_at=? WHERE used_at IS NULL AND invalidated_at IS NULL", (now,)).rowcount

    def redeem_pairing(self, code: str, user_id: str, now: float | None = None) -> tuple[str, dict | None]:
        """Checks and spends a code in ONE transaction (so a replay cannot succeed). Returns ('ok', row),
        ('invalid', None), ('blocked', None) - this user failed too often - or ('locked', None) - too many failures
        overall, every open code has been invalidated. A blocked user is refused even with the right code."""
        now = time.time() if now is None else now
        wanted = self._normalise_code(code)
        with self._lock, self._connect() as db:
            db.execute("BEGIN IMMEDIATE")
            db.execute("DELETE FROM pair_attempts WHERE at < ?", (now - 3600,))
            window = now - PAIR_WINDOW
            if db.execute("SELECT COUNT(*) FROM pair_attempts WHERE ok=0 AND at>?", (window,)).fetchone()[0] >= PAIR_GLOBAL_FAILURES:
                db.execute("UPDATE pairings SET invalidated_at=? WHERE used_at IS NULL AND invalidated_at IS NULL", (now,))
                db.commit()
                return "locked", None
            if db.execute("SELECT COUNT(*) FROM pair_attempts WHERE ok=0 AND user_id=? AND at>?", (user_id, window)).fetchone()[0] >= PAIR_USER_FAILURES:
                db.commit()
                return "blocked", None
            match = None
            for row in db.execute("""SELECT * FROM pairings WHERE used_at IS NULL AND invalidated_at IS NULL AND expires_at > ?""", (now,)).fetchall():
                # compare every open code in constant time; there is at most one in practice
                if hmac.compare_digest(row["code_hash"], self._code_hash(row["salt"], wanted)) and len(wanted) == PAIR_LENGTH and match is None:
                    match = row
            if match is None:
                db.execute("INSERT INTO pair_attempts VALUES (?,?,0)", (user_id, now))
                db.commit()
                return "invalid", None
            db.execute("UPDATE pairings SET used_at=?, used_by=? WHERE id=?", (now, user_id, match["id"]))
            db.execute("INSERT INTO pair_attempts VALUES (?,?,1)", (user_id, now))
            db.commit()
        return "ok", dict(match)
