"""Channel-agnostic POC registry: bindings, admitted IM users, audit trail and rate limits.

The tables are keyed by (channel, account, conversation) so a second IM adapter can reuse them unchanged; the
Telegram-specific delivery state in state.py is untouched. The configuration file only *seeds* the registry
(INSERT OR IGNORE): afterwards the registry is authoritative and changes made through IM commands or Core control
messages survive restarts and later config edits.
"""
from __future__ import annotations

import sqlite3
import time
from dataclasses import dataclass

AUDIT_KEEP = 2000
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
    PRIMARY KEY(channel, account_id, user_id)
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

    def seed_registry(self, channel: str, account: str, bindings: list[tuple[str, str, bool]], users: list[tuple[str, str]]) -> None:
        now = time.time()
        with self._lock, self._connect() as db:
            for conversation, target, wake in bindings:
                db.execute("INSERT OR IGNORE INTO poc_bindings VALUES (?,?,?,?,?,1,?,?)",
                           (channel, account, conversation, target, int(wake), now, "config"))
            for user_id, label in users:
                db.execute("INSERT OR IGNORE INTO im_users VALUES (?,?,?,?,1)", (channel, account, user_id, label))

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

    def set_user(self, channel: str, account: str, user_id: str, label: str, enabled: bool) -> None:
        with self._lock, self._connect() as db:
            db.execute("""INSERT INTO im_users VALUES (?,?,?,?,?) ON CONFLICT(channel, account_id, user_id)
                          DO UPDATE SET label=excluded.label, enabled=excluded.enabled""", (channel, account, user_id, label, int(enabled)))

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
