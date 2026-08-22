from __future__ import annotations

import sqlite3
import threading
import time
from pathlib import Path

from .common import BridgeError, _bounded_error, _sha256, _thread_key, _thread_value

class StateStore:
    def __init__(self, path: Path) -> None:
        self.path = path
        self._lock = threading.RLock()
        path.parent.mkdir(parents=True, exist_ok=True)
        with self._connect() as connection:
            connection.executescript(
                """
                PRAGMA journal_mode = WAL;
                PRAGMA foreign_keys = ON;
                CREATE TABLE IF NOT EXISTS metadata (
                    key TEXT PRIMARY KEY,
                    value TEXT NOT NULL
                );
                CREATE TABLE IF NOT EXISTS processed_updates (
                    update_id INTEGER PRIMARY KEY,
                    status TEXT NOT NULL,
                    core_message_id TEXT,
                    target_agent_id TEXT,
                    wake_requested INTEGER NOT NULL DEFAULT 0,
                    wake_status TEXT,
                    last_error TEXT,
                    completed_at REAL
                );
                CREATE TABLE IF NOT EXISTS message_mappings (
                    bot_id TEXT NOT NULL,
                    chat_id TEXT NOT NULL,
                    thread_key INTEGER NOT NULL,
                    telegram_message_id INTEGER NOT NULL,
                    core_message_id TEXT NOT NULL,
                    direction TEXT NOT NULL,
                    delivery_id TEXT,
                    created_at REAL NOT NULL,
                    PRIMARY KEY(bot_id, chat_id, thread_key, telegram_message_id)
                );
                CREATE INDEX IF NOT EXISTS message_mappings_core
                    ON message_mappings(bot_id, core_message_id, created_at, telegram_message_id);
                CREATE TABLE IF NOT EXISTS outbound_deliveries (
                    delivery_id TEXT PRIMARY KEY,
                    core_message_id TEXT NOT NULL,
                    message_hash TEXT NOT NULL,
                    chat_id TEXT NOT NULL,
                    thread_key INTEGER NOT NULL,
                    native_reply_to INTEGER,
                    status TEXT NOT NULL,
                    chunk_count INTEGER NOT NULL,
                    ambiguity_count INTEGER NOT NULL DEFAULT 0,
                    last_error TEXT,
                    created_at REAL NOT NULL,
                    updated_at REAL NOT NULL
                );
                CREATE TABLE IF NOT EXISTS outbound_chunks (
                    delivery_id TEXT NOT NULL,
                    chunk_index INTEGER NOT NULL,
                    body_hash TEXT NOT NULL,
                    status TEXT NOT NULL,
                    telegram_message_id INTEGER,
                    attempt_count INTEGER NOT NULL DEFAULT 0,
                    next_attempt_at REAL NOT NULL DEFAULT 0,
                    last_error TEXT,
                    PRIMARY KEY(delivery_id, chunk_index),
                    FOREIGN KEY(delivery_id) REFERENCES outbound_deliveries(delivery_id)
                        ON DELETE CASCADE
                );
                """
            )

    def _connect(self) -> sqlite3.Connection:
        connection = sqlite3.connect(self.path, timeout=15)
        connection.row_factory = sqlite3.Row
        return connection

    def bind_bot(self, bot_id: str) -> None:
        with self._lock, self._connect() as connection:
            row = connection.execute("SELECT value FROM metadata WHERE key = 'bot_id'").fetchone()
            if row and row["value"] != bot_id:
                raise BridgeError("Telegram state belongs to a different bot identity")
            connection.execute(
                "INSERT INTO metadata(key, value) VALUES ('bot_id', ?) ON CONFLICT(key) DO NOTHING",
                (bot_id,),
            )

    def last_update_id(self) -> int:
        with self._lock, self._connect() as connection:
            row = connection.execute(
                "SELECT value FROM metadata WHERE key = 'last_update_id'"
            ).fetchone()
        return int(row["value"]) if row else -1

    def update_completed(self, update_id: int) -> bool:
        if update_id <= self.last_update_id():
            return True
        with self._lock, self._connect() as connection:
            row = connection.execute(
                "SELECT status FROM processed_updates WHERE update_id = ?", (update_id,)
            ).fetchone()
        return bool(row and row["status"] == "completed")

    def complete_update(
        self,
        update_id: int,
        *,
        bot_id: str,
        chat_id: str | None = None,
        thread_id: int | None = None,
        telegram_message_id: int | None = None,
        core_message_id: str | None = None,
        target_agent_id: str | None = None,
        wake_requested: bool = False,
        error: str | None = None,
    ) -> None:
        now = time.time()
        with self._lock, self._connect() as connection:
            connection.execute("BEGIN IMMEDIATE")
            if core_message_id is not None:
                if chat_id is None or telegram_message_id is None:
                    raise BridgeError("inbound mapping is incomplete")
                self._insert_mapping(
                    connection,
                    bot_id,
                    chat_id,
                    thread_id,
                    telegram_message_id,
                    core_message_id,
                    "inbound",
                    None,
                    now,
                )
            connection.execute(
                """INSERT INTO processed_updates(
                       update_id, status, core_message_id, target_agent_id, wake_requested,
                       wake_status, last_error, completed_at
                   ) VALUES (?, 'completed', ?, ?, ?, ?, ?, ?)
                   ON CONFLICT(update_id) DO UPDATE SET
                       status = excluded.status,
                       core_message_id = excluded.core_message_id,
                       target_agent_id = excluded.target_agent_id,
                       wake_requested = excluded.wake_requested,
                       wake_status = excluded.wake_status,
                       last_error = excluded.last_error,
                       completed_at = excluded.completed_at""",
                (
                    update_id,
                    core_message_id,
                    target_agent_id,
                    int(wake_requested),
                    "pending" if wake_requested and core_message_id else None,
                    _bounded_error(error),
                    now,
                ),
            )
            previous = connection.execute(
                "SELECT value FROM metadata WHERE key = 'last_update_id'"
            ).fetchone()
            previous_id = int(previous["value"]) if previous else -1
            if update_id > previous_id:
                connection.execute(
                    """INSERT INTO metadata(key, value) VALUES ('last_update_id', ?)
                       ON CONFLICT(key) DO UPDATE SET value = excluded.value""",
                    (str(update_id),),
                )
            connection.commit()

    def pending_wakes(self) -> list[sqlite3.Row]:
        with self._lock, self._connect() as connection:
            return connection.execute(
                """SELECT update_id, core_message_id, target_agent_id FROM processed_updates
                   WHERE wake_requested = 1 AND wake_status = 'pending'
                   ORDER BY update_id LIMIT 20"""
            ).fetchall()

    def finish_wake(self, update_id: int, status: str, error: str | None = None) -> None:
        with self._lock, self._connect() as connection:
            connection.execute(
                "UPDATE processed_updates SET wake_status = ?, last_error = ? WHERE update_id = ?",
                (status, _bounded_error(error), update_id),
            )

    def mapping_for_telegram(
        self, bot_id: str, chat_id: str, thread_id: int | None, telegram_message_id: int
    ) -> str | None:
        with self._lock, self._connect() as connection:
            row = connection.execute(
                """SELECT core_message_id FROM message_mappings
                   WHERE bot_id = ? AND chat_id = ? AND thread_key = ?
                   AND telegram_message_id = ?""",
                (bot_id, chat_id, _thread_key(thread_id), telegram_message_id),
            ).fetchone()
        return str(row["core_message_id"]) if row else None

    def first_mapping_for_contexts(
        self, bot_id: str, context_ids: list[str]
    ) -> sqlite3.Row | None:
        with self._lock, self._connect() as connection:
            for context_id in context_ids:
                row = connection.execute(
                    """SELECT chat_id, thread_key, telegram_message_id, core_message_id
                       FROM message_mappings WHERE bot_id = ? AND core_message_id = ?
                       ORDER BY created_at, telegram_message_id LIMIT 1""",
                    (bot_id, context_id),
                ).fetchone()
                if row:
                    return row
        return None

    def ensure_outbound(
        self,
        delivery_id: str,
        core_message_id: str,
        message_hash: str,
        chat_id: str,
        thread_id: int | None,
        native_reply_to: int | None,
        chunks: list[str],
    ) -> None:
        now = time.time()
        with self._lock, self._connect() as connection:
            connection.execute("BEGIN IMMEDIATE")
            row = connection.execute(
                """SELECT core_message_id, message_hash, chat_id, thread_key, native_reply_to,
                          chunk_count FROM outbound_deliveries WHERE delivery_id = ?""",
                (delivery_id,),
            ).fetchone()
            expected = (
                core_message_id,
                message_hash,
                chat_id,
                _thread_key(thread_id),
                native_reply_to,
                len(chunks),
            )
            if row:
                actual = tuple(row[key] for key in row.keys())
                if actual != expected:
                    raise BridgeError("Core delivery changed after Telegram intent was persisted")
                connection.commit()
                return
            connection.execute(
                """INSERT INTO outbound_deliveries(
                       delivery_id, core_message_id, message_hash, chat_id, thread_key,
                       native_reply_to, status, chunk_count, created_at, updated_at
                   ) VALUES (?, ?, ?, ?, ?, ?, 'pending', ?, ?, ?)""",
                (
                    delivery_id,
                    core_message_id,
                    message_hash,
                    chat_id,
                    _thread_key(thread_id),
                    native_reply_to,
                    len(chunks),
                    now,
                    now,
                ),
            )
            for index, body in enumerate(chunks):
                connection.execute(
                    """INSERT INTO outbound_chunks(
                           delivery_id, chunk_index, body_hash, status
                       ) VALUES (?, ?, ?, 'pending')""",
                    (delivery_id, index, _sha256(body)),
                )
            connection.commit()

    def outbound(self, delivery_id: str) -> tuple[sqlite3.Row, list[sqlite3.Row]]:
        with self._lock, self._connect() as connection:
            delivery = connection.execute(
                "SELECT * FROM outbound_deliveries WHERE delivery_id = ?", (delivery_id,)
            ).fetchone()
            chunks = connection.execute(
                "SELECT * FROM outbound_chunks WHERE delivery_id = ? ORDER BY chunk_index",
                (delivery_id,),
            ).fetchall()
        if not delivery or len(chunks) != delivery["chunk_count"]:
            raise BridgeError("Telegram outbound intent is incomplete")
        return delivery, chunks

    def recover_sending(self, ambiguous_retry_seconds: float) -> int:
        now = time.time()
        with self._lock, self._connect() as connection:
            rows = connection.execute(
                "SELECT delivery_id FROM outbound_chunks WHERE status = 'sending'"
            ).fetchall()
            for row in rows:
                connection.execute(
                    """UPDATE outbound_chunks SET status = 'pending',
                           next_attempt_at = ?, last_error = 'ambiguous send recovered after restart'
                       WHERE delivery_id = ? AND status = 'sending'""",
                    (now + ambiguous_retry_seconds, row["delivery_id"]),
                )
                connection.execute(
                    """UPDATE outbound_deliveries SET ambiguity_count = ambiguity_count + 1,
                           last_error = 'ambiguous send recovered after restart', updated_at = ?
                       WHERE delivery_id = ?""",
                    (now, row["delivery_id"]),
                )
        return len(rows)

    def mark_chunk_sending(self, delivery_id: str, index: int) -> None:
        with self._lock, self._connect() as connection:
            connection.execute(
                """UPDATE outbound_chunks SET status = 'sending', attempt_count = attempt_count + 1,
                       last_error = NULL WHERE delivery_id = ? AND chunk_index = ?
                       AND status = 'pending'""",
                (delivery_id, index),
            )
            connection.execute(
                "UPDATE outbound_deliveries SET status = 'sending', updated_at = ? WHERE delivery_id = ?",
                (time.time(), delivery_id),
            )

    def mark_chunk_retry(
        self,
        delivery_id: str,
        index: int,
        error: str,
        next_attempt_at: float,
        *,
        ambiguous: bool,
    ) -> None:
        with self._lock, self._connect() as connection:
            connection.execute("BEGIN IMMEDIATE")
            connection.execute(
                """UPDATE outbound_chunks SET status = 'pending', next_attempt_at = ?,
                       last_error = ? WHERE delivery_id = ? AND chunk_index = ?""",
                (next_attempt_at, _bounded_error(error), delivery_id, index),
            )
            connection.execute(
                """UPDATE outbound_deliveries SET status = 'pending', last_error = ?,
                       ambiguity_count = ambiguity_count + ?, updated_at = ? WHERE delivery_id = ?""",
                (_bounded_error(error), int(ambiguous), time.time(), delivery_id),
            )
            connection.commit()

    def mark_chunk_error(self, delivery_id: str, index: int, error: str) -> None:
        with self._lock, self._connect() as connection:
            connection.execute(
                """UPDATE outbound_chunks SET status = 'error', last_error = ?
                   WHERE delivery_id = ? AND chunk_index = ?""",
                (_bounded_error(error), delivery_id, index),
            )
            connection.execute(
                """UPDATE outbound_deliveries SET status = 'error', last_error = ?, updated_at = ?
                   WHERE delivery_id = ?""",
                (_bounded_error(error), time.time(), delivery_id),
            )

    def mark_chunk_sent(
        self,
        bot_id: str,
        delivery_id: str,
        core_message_id: str,
        chat_id: str,
        thread_id: int | None,
        index: int,
        telegram_message_id: int,
    ) -> None:
        now = time.time()
        with self._lock, self._connect() as connection:
            connection.execute("BEGIN IMMEDIATE")
            self._insert_mapping(
                connection,
                bot_id,
                chat_id,
                thread_id,
                telegram_message_id,
                core_message_id,
                "outbound",
                delivery_id,
                now + index / 1000,
            )
            connection.execute(
                """UPDATE outbound_chunks SET status = 'sent', telegram_message_id = ?,
                       next_attempt_at = 0, last_error = NULL
                   WHERE delivery_id = ? AND chunk_index = ?""",
                (telegram_message_id, delivery_id, index),
            )
            remaining = connection.execute(
                """SELECT COUNT(*) AS count FROM outbound_chunks
                   WHERE delivery_id = ? AND status != 'sent'""",
                (delivery_id,),
            ).fetchone()["count"]
            if remaining == 0:
                connection.execute(
                    """UPDATE outbound_deliveries SET status = 'sent', last_error = NULL,
                           updated_at = ? WHERE delivery_id = ?""",
                    (now, delivery_id),
                )
            connection.commit()

    def mark_acked(self, delivery_id: str) -> None:
        with self._lock, self._connect() as connection:
            connection.execute(
                """UPDATE outbound_deliveries SET status = 'acked', last_error = NULL,
                       updated_at = ? WHERE delivery_id = ?""",
                (time.time(), delivery_id),
            )

    @staticmethod
    def _insert_mapping(
        connection: sqlite3.Connection,
        bot_id: str,
        chat_id: str,
        thread_id: int | None,
        telegram_message_id: int,
        core_message_id: str,
        direction: str,
        delivery_id: str | None,
        created_at: float,
    ) -> None:
        existing = connection.execute(
            """SELECT core_message_id FROM message_mappings
               WHERE bot_id = ? AND chat_id = ? AND thread_key = ?
               AND telegram_message_id = ?""",
            (bot_id, chat_id, _thread_key(thread_id), telegram_message_id),
        ).fetchone()
        if existing:
            if existing["core_message_id"] != core_message_id:
                raise BridgeError("Telegram Message ID is already mapped to another Core Message")
            return
        connection.execute(
            """INSERT INTO message_mappings(
                   bot_id, chat_id, thread_key, telegram_message_id, core_message_id,
                   direction, delivery_id, created_at
               ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)""",
            (
                bot_id,
                chat_id,
                _thread_key(thread_id),
                telegram_message_id,
                core_message_id,
                direction,
                delivery_id,
                created_at,
            ),
        )


