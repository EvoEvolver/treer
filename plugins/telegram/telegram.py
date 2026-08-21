#!/usr/bin/env python3
"""Telegram Bot API bridge for Treer Core Message using only nested CLI calls."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import signal
import sqlite3
import subprocess
import sys
import tempfile
import threading
import time
import urllib.error
import urllib.parse
import urllib.request
from dataclasses import dataclass
from pathlib import Path
from typing import Any


MAX_TELEGRAM_TEXT_UNITS = 4096
MAX_CORE_BODY_BYTES = 32 * 1024
CLI_TIMEOUT_SECONDS = 125
BOT_API_RESPONSE_LIMIT = 2 * 1024 * 1024


class BridgeError(Exception):
    pass


class CliError(BridgeError):
    def __init__(self, code: str, message: str) -> None:
        super().__init__(message)
        self.code = code
        self.message = message


class TelegramError(BridgeError):
    def __init__(self, code: str, message: str) -> None:
        super().__init__(message)
        self.code = code
        self.message = message


class TelegramRateLimited(TelegramError):
    def __init__(self, retry_after: float) -> None:
        super().__init__("telegram_rate_limited", "Telegram rate limit reached")
        self.retry_after = max(0.05, min(retry_after, 3600.0))


class TelegramAmbiguous(TelegramError):
    pass


@dataclass(frozen=True)
class Binding:
    chat_id: str
    thread_id: int | None
    target_agent_id: str
    wake_agent: bool

    @property
    def key(self) -> tuple[str, int]:
        return self.chat_id, _thread_key(self.thread_id)


@dataclass(frozen=True)
class Config:
    api_base_url: str
    allowed_user_ids: frozenset[int]
    bindings: tuple[Binding, ...]
    poll_timeout_seconds: int
    receive_wait_milliseconds: int
    batch_size: int
    retry_initial_seconds: float
    retry_max_seconds: float
    ambiguous_retry_seconds: float
    respond_to_denied: bool

    def binding(self, chat_id: int, thread_id: int | None) -> Binding | None:
        key = (str(chat_id), _thread_key(thread_id))
        return next((binding for binding in self.bindings if binding.key == key), None)


class TreerCli:
    def __init__(self, executable: str) -> None:
        self.executable = executable

    def run(self, arguments: list[str], *, stdin: str | None = None) -> dict[str, Any]:
        try:
            completed = subprocess.run(
                [self.executable, *arguments],
                input=stdin,
                capture_output=True,
                text=True,
                timeout=CLI_TIMEOUT_SECONDS,
                check=False,
                env=os.environ.copy(),
            )
        except (OSError, subprocess.TimeoutExpired) as error:
            raise CliError("plugin_cli_unavailable", "Treer CLI is unavailable") from error
        if completed.returncode != 0:
            code = "plugin_cli_failed"
            message = "Treer rejected the request"
            try:
                value = json.loads(completed.stderr.strip())
                failure = value.get("error", {})
                code = str(failure.get("code") or code)
                message = str(failure.get("message") or message)
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


class BotApi:
    def __init__(self, base_url: str, token: str, timeout_seconds: float = 65.0) -> None:
        self.base_url = base_url.rstrip("/")
        self.token = token
        self.timeout_seconds = timeout_seconds

    def call(self, method: str, payload: dict[str, Any], *, sending: bool = False) -> Any:
        url = f"{self.base_url}/bot{urllib.parse.quote(self.token, safe=':')}/{method}"
        request = urllib.request.Request(
            url,
            data=json.dumps(payload, separators=(",", ":")).encode("utf-8"),
            headers={"Content-Type": "application/json", "Accept": "application/json"},
            method="POST",
        )
        try:
            with urllib.request.urlopen(request, timeout=self.timeout_seconds) as response:
                raw = response.read(BOT_API_RESPONSE_LIMIT + 1)
        except urllib.error.HTTPError as error:
            raw = error.read(BOT_API_RESPONSE_LIMIT + 1)
            return self._decode(method, raw, sending=sending, http_status=error.code)
        except (OSError, urllib.error.URLError, TimeoutError) as error:
            if sending:
                raise TelegramAmbiguous(
                    "telegram_send_ambiguous",
                    "Telegram send may have succeeded but its response was unavailable",
                ) from error
            raise TelegramError("telegram_unavailable", "Telegram Bot API is unavailable") from error
        if len(raw) > BOT_API_RESPONSE_LIMIT:
            if sending:
                raise TelegramAmbiguous(
                    "telegram_send_ambiguous", "Telegram returned an oversized send response"
                )
            raise TelegramError("telegram_response_invalid", "Telegram returned an oversized response")
        return self._decode(method, raw, sending=sending, http_status=200)

    def _decode(self, method: str, raw: bytes, *, sending: bool, http_status: int) -> Any:
        try:
            value = json.loads(raw)
        except (UnicodeDecodeError, ValueError) as error:
            if sending and 200 <= http_status < 300:
                raise TelegramAmbiguous(
                    "telegram_send_ambiguous", "Telegram returned an invalid send response"
                ) from error
            raise TelegramError("telegram_response_invalid", "Telegram returned invalid JSON") from error
        if not isinstance(value, dict):
            raise TelegramError("telegram_response_invalid", "Telegram returned an invalid object")
        if value.get("ok") is True:
            return value.get("result")
        error_code = value.get("error_code", http_status)
        parameters = value.get("parameters") if isinstance(value.get("parameters"), dict) else {}
        if error_code == 429:
            retry_after = parameters.get("retry_after", 1)
            try:
                retry_seconds = float(retry_after)
            except (TypeError, ValueError):
                retry_seconds = 1.0
            raise TelegramRateLimited(retry_seconds)
        if isinstance(error_code, int) and error_code >= 500:
            raise TelegramError("telegram_temporary_failure", "Telegram temporarily rejected the request")
        raise TelegramError(
            "telegram_request_rejected",
            f"Telegram rejected {method} with error {error_code}",
        )

    def get_me(self) -> dict[str, Any]:
        return _object(self.call("getMe", {}), "getMe result")

    def get_updates(self, offset: int, timeout: int) -> list[dict[str, Any]]:
        result = self.call(
            "getUpdates",
            {"offset": offset, "timeout": timeout, "allowed_updates": ["message"]},
        )
        return [_object(item, "Telegram update") for item in _list(result, "getUpdates result")]

    def send_message(
        self,
        chat_id: str,
        thread_id: int | None,
        text: str,
        reply_to_message_id: int | None,
    ) -> dict[str, Any]:
        payload: dict[str, Any] = {"chat_id": int(chat_id), "text": text}
        if thread_id is not None:
            payload["message_thread_id"] = thread_id
        if reply_to_message_id is not None:
            payload["reply_parameters"] = {
                "message_id": reply_to_message_id,
                "allow_sending_without_reply": True,
            }
        return _object(self.call("sendMessage", payload, sending=True), "sendMessage result")


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


class TelegramBridge:
    def __init__(
        self,
        config: Config,
        state: StateStore,
        cli: TreerCli,
        bot: BotApi,
        bot_id: str,
        state_dir: Path,
    ) -> None:
        self.config = config
        self.state = state
        self.cli = cli
        self.bot = bot
        self.bot_id = bot_id
        self.state_dir = state_dir

    def run_inbound_once(self, timeout: int | None = None) -> int:
        offset = self.state.last_update_id() + 1
        updates = self.bot.get_updates(
            offset, self.config.poll_timeout_seconds if timeout is None else timeout
        )
        processed = 0
        for update in sorted(updates, key=lambda item: _integer(item.get("update_id"), "update_id")):
            self.process_update(update)
            processed += 1
        self.process_pending_wakes()
        return processed

    def process_update(self, update: dict[str, Any]) -> None:
        update_id = _integer(update.get("update_id"), "update_id")
        if self.state.update_completed(update_id):
            return
        message = update.get("message")
        if not isinstance(message, dict):
            self.state.complete_update(update_id, bot_id=self.bot_id, error="unsupported update")
            return
        chat = message.get("chat")
        sender = message.get("from")
        telegram_message_id = message.get("message_id")
        if not isinstance(chat, dict) or not isinstance(sender, dict) or not _is_integer(telegram_message_id):
            self.state.complete_update(update_id, bot_id=self.bot_id, error="malformed message")
            return
        chat_id = _integer(chat.get("id"), "chat ID")
        user_id = _integer(sender.get("id"), "user ID")
        thread_id = message.get("message_thread_id")
        if thread_id is not None:
            thread_id = _integer(thread_id, "message thread ID")
        binding = self.config.binding(chat_id, thread_id)
        if sender.get("is_bot") is True:
            self.state.complete_update(update_id, bot_id=self.bot_id, error="bot sender ignored")
            return
        if user_id not in self.config.allowed_user_ids or binding is None:
            if self.config.respond_to_denied:
                self._safe_notice(str(chat_id), thread_id, "This Telegram identity or chat is not allowed.")
            self.state.complete_update(update_id, bot_id=self.bot_id, error="Telegram identity denied")
            return
        text = message.get("text")
        if not isinstance(text, str) or not text:
            self._safe_notice(str(chat_id), thread_id, "Treer Telegram currently accepts text messages only.")
            self.state.complete_update(update_id, bot_id=self.bot_id, error="non-text message ignored")
            return
        command = text.split(maxsplit=1)[0].split("@", 1)[0].lower()
        if command in {"/start", "/help", "/target", "/status"}:
            self._process_command(update_id, command, binding, str(chat_id), thread_id)
            return
        if len(text.encode("utf-8")) > MAX_CORE_BODY_BYTES:
            self._safe_notice(str(chat_id), thread_id, "Message is too large for Treer Core.")
            self.state.complete_update(update_id, bot_id=self.bot_id, error="message too large")
            return

        context_ids: list[str] = []
        reply = message.get("reply_to_message")
        if isinstance(reply, dict) and _is_integer(reply.get("message_id")):
            parent = self.state.mapping_for_telegram(
                self.bot_id,
                str(chat_id),
                thread_id,
                int(reply["message_id"]),
            )
            if parent:
                context_ids.append(parent)
        source = {
            "channel": "telegram",
            "account_id": self.bot_id,
            "conversation_id": f"{chat_id}:{thread_id if thread_id is not None else 0}",
            "message_id": str(telegram_message_id),
            "metadata": {"user_id": str(user_id), "update_id": str(update_id)},
        }
        source_path = self._write_source(update_id, source)
        arguments = ["message", "send", "--to", binding.target_agent_id]
        for context_id in context_ids:
            arguments.extend(["--context", context_id])
        arguments.extend(
            [
                "--idempotency-key",
                f"telegram-{self.bot_id}-{update_id}",
                "--external-source-file",
                str(source_path),
                "--body-file",
                "-",
            ]
        )
        try:
            response = self.cli.run(arguments, stdin=text)
        finally:
            try:
                source_path.unlink()
            except FileNotFoundError:
                pass
        core_message = _object(response.get("message"), "Core Message")
        core_message_id = _string(core_message.get("message_id"), "Core Message ID", 256)
        self.state.complete_update(
            update_id,
            bot_id=self.bot_id,
            chat_id=str(chat_id),
            thread_id=thread_id,
            telegram_message_id=int(telegram_message_id),
            core_message_id=core_message_id,
            target_agent_id=binding.target_agent_id,
            wake_requested=binding.wake_agent,
        )
        self.process_pending_wakes()

    def _process_command(
        self,
        update_id: int,
        command: str,
        binding: Binding,
        chat_id: str,
        thread_id: int | None,
    ) -> None:
        if command == "/status":
            try:
                response = self.cli.run(["agent", "get", binding.target_agent_id])
                name = str(response.get("name") or binding.target_agent_id)
                status = str(response.get("status") or "unknown")
                notice = f"Treer target {name} is {status}."
            except CliError as error:
                notice = f"Treer target status is unavailable ({error.code})."
        elif command == "/target":
            notice = f"Treer target: {binding.target_agent_id}"
        else:
            notice = (
                f"Treer bridge ready. Target: {binding.target_agent_id}. "
                "Send text or reply to a bridged message."
            )
        self.bot.send_message(chat_id, thread_id, notice, None)
        self.state.complete_update(update_id, bot_id=self.bot_id)

    def _safe_notice(self, chat_id: str, thread_id: int | None, text: str) -> None:
        try:
            self.bot.send_message(chat_id, thread_id, text, None)
        except TelegramError:
            pass

    def _write_source(self, update_id: int, value: dict[str, Any]) -> Path:
        descriptor, name = tempfile.mkstemp(
            prefix=f"telegram-source-{update_id}-", suffix=".json", dir=self.state_dir
        )
        path = Path(name)
        try:
            os.fchmod(descriptor, 0o600)
            with os.fdopen(descriptor, "w", encoding="utf-8") as handle:
                json.dump(value, handle, separators=(",", ":"))
        except Exception:
            try:
                os.close(descriptor)
            except OSError:
                pass
            path.unlink(missing_ok=True)
            raise
        return path

    def process_pending_wakes(self) -> None:
        for wake in self.state.pending_wakes():
            message_id = str(wake["core_message_id"])
            target = str(wake["target_agent_id"])
            prompt = (
                f"New Treer Message {message_id}. Read it with `treer message get {message_id}` "
                "and reply through `treer message reply`."
            )
            try:
                self.cli.run(["agent", "prompt", target, prompt])
                self.state.finish_wake(int(wake["update_id"]), "sent")
            except CliError as error:
                status = "failed" if error.code in {"policy_denied", "plugin_command_denied"} else "pending"
                self.state.finish_wake(int(wake["update_id"]), status, error.code)

    def run_outbound_once(self, wait_milliseconds: int | None = None) -> int:
        wait = self.config.receive_wait_milliseconds if wait_milliseconds is None else wait_milliseconds
        response = self.cli.run(
            [
                "message",
                "receive",
                "--wait",
                str(wait),
                "--limit",
                str(self.config.batch_size),
            ]
        )
        deliveries = [_object(value, "Core delivery") for value in _list(response.get("deliveries"), "Core deliveries")]
        for delivery in deliveries:
            try:
                self.process_delivery(delivery)
            except BridgeError as error:
                code = getattr(error, "code", type(error).__name__)
                print(f"Treer Telegram delivery deferred after {code}", file=sys.stderr, flush=True)
        return len(deliveries)

    def process_delivery(self, delivery: dict[str, Any]) -> None:
        delivery_id = _string(delivery.get("delivery_id"), "delivery ID", 256)
        message = _object(delivery.get("message"), "Core Message")
        core_message_id = _string(message.get("message_id"), "Core Message ID", 256)
        body = _string(message.get("body"), "Core Message body", MAX_CORE_BODY_BYTES)
        contexts = [_string(value, "context ID", 256) for value in _list(message.get("context_ids", []), "contexts")]
        route = self.state.first_mapping_for_contexts(self.bot_id, contexts)
        if route:
            chat_id = str(route["chat_id"])
            thread_id = _thread_value(int(route["thread_key"]))
            native_reply_to = int(route["telegram_message_id"])
        else:
            sender = _object(message.get("sender"), "Core sender")
            sender_id = _string(sender.get("id"), "Core sender ID", 256)
            candidates = [binding for binding in self.config.bindings if binding.target_agent_id == sender_id]
            if len(candidates) != 1:
                raise BridgeError(
                    f"delivery {delivery_id} has no unambiguous Telegram route; it remains unacknowledged"
                )
            binding = candidates[0]
            chat_id = binding.chat_id
            thread_id = binding.thread_id
            native_reply_to = None
        rendered = body
        if len(contexts) > 1:
            rendered += f"\n\n[Treer: {len(contexts)} context nodes; one native reply is shown.]"
        chunks = split_telegram_text(rendered)
        fingerprint = _sha256(
            json.dumps(
                {
                    "message_id": core_message_id,
                    "body": body,
                    "contexts": contexts,
                    "chat_id": chat_id,
                    "thread_id": thread_id,
                    "reply_to": native_reply_to,
                },
                sort_keys=True,
                separators=(",", ":"),
            )
        )
        self.state.ensure_outbound(
            delivery_id,
            core_message_id,
            fingerprint,
            chat_id,
            thread_id,
            native_reply_to,
            chunks,
        )
        self._deliver_outbound(delivery_id, chunks)

    def _deliver_outbound(self, delivery_id: str, bodies: list[str]) -> None:
        outbound, chunks = self.state.outbound(delivery_id)
        if len(bodies) != len(chunks) or any(
            _sha256(bodies[index]) != chunk["body_hash"] for index, chunk in enumerate(chunks)
        ):
            raise BridgeError("Core delivery chunks changed after Telegram intent was persisted")
        if outbound["status"] == "acked":
            return
        if outbound["status"] == "sent":
            self._ack_delivery(delivery_id)
            return
        previous_message_id = outbound["native_reply_to"]
        for chunk in chunks:
            if chunk["status"] == "sent":
                previous_message_id = int(chunk["telegram_message_id"])
                continue
            if chunk["status"] == "error" or float(chunk["next_attempt_at"]) > time.time():
                return
            index = int(chunk["chunk_index"])
            self.state.mark_chunk_sending(delivery_id, index)
            try:
                result = self.bot.send_message(
                    str(outbound["chat_id"]),
                    _thread_value(int(outbound["thread_key"])),
                    bodies[index],
                    int(previous_message_id) if previous_message_id is not None else None,
                )
                telegram_message_id = _integer(result.get("message_id"), "Telegram Message ID")
            except TelegramRateLimited as error:
                self.state.mark_chunk_retry(
                    delivery_id,
                    index,
                    error.code,
                    time.time() + error.retry_after,
                    ambiguous=False,
                )
                return
            except TelegramAmbiguous as error:
                self.state.mark_chunk_retry(
                    delivery_id,
                    index,
                    error.code,
                    time.time() + self.config.ambiguous_retry_seconds,
                    ambiguous=True,
                )
                return
            except TelegramError as error:
                if error.code == "telegram_temporary_failure":
                    delay = min(
                        self.config.retry_max_seconds,
                        self.config.retry_initial_seconds
                        * (2 ** min(int(chunk["attempt_count"]), 8)),
                    )
                    self.state.mark_chunk_retry(
                        delivery_id,
                        index,
                        error.code,
                        time.time() + delay,
                        ambiguous=False,
                    )
                else:
                    self.state.mark_chunk_error(delivery_id, index, error.code)
                return
            self.state.mark_chunk_sent(
                self.bot_id,
                delivery_id,
                str(outbound["core_message_id"]),
                str(outbound["chat_id"]),
                _thread_value(int(outbound["thread_key"])),
                index,
                telegram_message_id,
            )
            previous_message_id = telegram_message_id
        refreshed, _ = self.state.outbound(delivery_id)
        if refreshed["status"] == "sent":
            self._ack_delivery(delivery_id)

    def _ack_delivery(self, delivery_id: str) -> None:
        self.cli.run(
            [
                "message",
                "ack",
                delivery_id,
                "--operation-id",
                f"telegram-{self.bot_id}-{delivery_id}",
            ]
        )
        self.state.mark_acked(delivery_id)


def split_telegram_text(value: str) -> list[str]:
    if not value:
        raise BridgeError("Telegram cannot send an empty Message")
    chunks: list[str] = []
    current: list[str] = []
    units = 0
    for character in value:
        width = 2 if ord(character) > 0xFFFF else 1
        if current and units + width > MAX_TELEGRAM_TEXT_UNITS:
            chunks.append("".join(current))
            current = []
            units = 0
        current.append(character)
        units += width
    if current:
        chunks.append("".join(current))
    return chunks


def load_config() -> tuple[Config, Path, str, str]:
    config_path = os.environ.get("TREER_PLUGIN_CONFIG")
    state_value = os.environ.get("TREER_PLUGIN_STATE_DIR")
    cli = os.environ.get("TREER_CLI")
    token = os.environ.get("TELEGRAM_BOT_TOKEN")
    if not config_path or not state_value or not cli or not token:
        raise BridgeError("Telegram plugin must run through `treer plugin run` with its bot token")
    if len(token) > 256 or any(character.isspace() for character in token):
        raise BridgeError("TELEGRAM_BOT_TOKEN is invalid")
    with open(config_path, "rb") as handle:
        value = json.load(handle)
    if not isinstance(value, dict):
        raise BridgeError("Telegram config must contain a JSON object")
    allowed_keys = {
        "api_base_url",
        "allowed_user_ids",
        "bindings",
        "poll_timeout_seconds",
        "receive_wait_milliseconds",
        "batch_size",
        "retry_initial_seconds",
        "retry_max_seconds",
        "ambiguous_retry_seconds",
        "respond_to_denied",
    }
    unknown = set(value) - allowed_keys
    if unknown:
        raise BridgeError(f"Telegram config has unknown fields: {', '.join(sorted(unknown))}")
    users = value.get("allowed_user_ids")
    if not isinstance(users, list) or not users or len(users) > 1_000:
        raise BridgeError("allowed_user_ids must contain 1-1000 numeric user IDs")
    allowed_users = frozenset(_integer(user, "allowed Telegram user ID") for user in users)
    if len(allowed_users) != len(users):
        raise BridgeError("allowed_user_ids must be unique")
    binding_values = value.get("bindings")
    if not isinstance(binding_values, list) or not binding_values or len(binding_values) > 1_000:
        raise BridgeError("bindings must contain 1-1000 chat/topic bindings")
    bindings: list[Binding] = []
    for raw in binding_values:
        item = _object(raw, "binding")
        if set(item) - {"chat_id", "message_thread_id", "target_agent_id", "wake_agent"}:
            raise BridgeError("Telegram binding has unknown fields")
        thread_id = item.get("message_thread_id")
        if thread_id is not None:
            thread_id = _integer(thread_id, "message thread ID")
        wake = item.get("wake_agent", False)
        if not isinstance(wake, bool):
            raise BridgeError("wake_agent must be boolean")
        bindings.append(
            Binding(
                chat_id=str(_integer(item.get("chat_id"), "chat ID")),
                thread_id=thread_id,
                target_agent_id=_string(item.get("target_agent_id"), "target Agent ID", 256),
                wake_agent=wake,
            )
        )
    if len({binding.key for binding in bindings}) != len(bindings):
        raise BridgeError("Telegram chat/topic bindings must be unique")
    base_url = _http_url(value.get("api_base_url", "https://api.telegram.org"))
    poll = _bounded_int(value.get("poll_timeout_seconds", 25), "poll timeout", 1, 50)
    receive = _bounded_int(
        value.get("receive_wait_milliseconds", 10_000), "receive wait", 0, 30_000
    )
    batch = _bounded_int(value.get("batch_size", 20), "batch size", 1, 100)
    initial = _bounded_float(value.get("retry_initial_seconds", 1), "initial retry", 0.01, 300)
    maximum = _bounded_float(value.get("retry_max_seconds", 60), "maximum retry", initial, 3600)
    ambiguous = _bounded_float(
        value.get("ambiguous_retry_seconds", 5), "ambiguous retry", 0.01, 3600
    )
    respond = value.get("respond_to_denied", True)
    if not isinstance(respond, bool):
        raise BridgeError("respond_to_denied must be boolean")
    state_dir = Path(state_value)
    state_dir.mkdir(parents=True, exist_ok=True)
    return (
        Config(
            api_base_url=base_url,
            allowed_user_ids=allowed_users,
            bindings=tuple(bindings),
            poll_timeout_seconds=poll,
            receive_wait_milliseconds=receive,
            batch_size=batch,
            retry_initial_seconds=initial,
            retry_max_seconds=maximum,
            ambiguous_retry_seconds=ambiguous,
            respond_to_denied=respond,
        ),
        state_dir,
        cli,
        token,
    )


def build_bridge() -> TelegramBridge:
    config, state_dir, cli_path, token = load_config()
    bot = BotApi(config.api_base_url, token)
    me = bot.get_me()
    bot_id = str(_integer(me.get("id"), "bot ID"))
    state = StateStore(state_dir / "telegram-state.sqlite3")
    state.bind_bot(bot_id)
    state.recover_sending(config.ambiguous_retry_seconds)
    return TelegramBridge(config, state, TreerCli(cli_path), bot, bot_id, state_dir)


def run_forever(bridge: TelegramBridge) -> None:
    stopped = threading.Event()

    def stop(_signum: int, _frame: object) -> None:
        stopped.set()

    signal.signal(signal.SIGTERM, stop)
    signal.signal(signal.SIGINT, stop)

    def loop(label: str, action: Any) -> None:
        while not stopped.is_set():
            try:
                action()
            except BridgeError as error:
                code = getattr(error, "code", type(error).__name__)
                print(f"Treer Telegram {label} retrying after {code}", file=sys.stderr, flush=True)
                stopped.wait(1)

    inbound = threading.Thread(target=loop, args=("inbound", bridge.run_inbound_once), daemon=True)
    outbound = threading.Thread(target=loop, args=("outbound", bridge.run_outbound_once), daemon=True)
    inbound.start()
    outbound.start()
    while not stopped.wait(0.25):
        if not inbound.is_alive() or not outbound.is_alive():
            raise BridgeError("Telegram worker thread exited unexpectedly")
    inbound.join(timeout=2)
    outbound.join(timeout=2)


def _thread_key(value: int | None) -> int:
    return value if value is not None else -1


def _thread_value(value: int) -> int | None:
    return None if value == -1 else value


def _sha256(value: str) -> str:
    return hashlib.sha256(value.encode("utf-8")).hexdigest()


def _bounded_error(value: str | None) -> str | None:
    if value is None:
        return None
    return value[:512]


def _is_integer(value: Any) -> bool:
    return isinstance(value, int) and not isinstance(value, bool)


def _integer(value: Any, label: str) -> int:
    if not _is_integer(value):
        raise BridgeError(f"{label} must be an integer")
    return int(value)


def _bounded_int(value: Any, label: str, minimum: int, maximum: int) -> int:
    number = _integer(value, label)
    if number < minimum or number > maximum:
        raise BridgeError(f"{label} must be between {minimum} and {maximum}")
    return number


def _bounded_float(value: Any, label: str, minimum: float, maximum: float) -> float:
    if not isinstance(value, (int, float)) or isinstance(value, bool):
        raise BridgeError(f"{label} must be numeric")
    number = float(value)
    if number < minimum or number > maximum:
        raise BridgeError(f"{label} must be between {minimum} and {maximum}")
    return number


def _string(value: Any, label: str, maximum: int) -> str:
    if not isinstance(value, str) or not value or len(value.encode("utf-8")) > maximum:
        raise BridgeError(f"{label} is empty or too long")
    return value


def _object(value: Any, label: str) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise BridgeError(f"{label} must be an object")
    return value


def _list(value: Any, label: str) -> list[Any]:
    if not isinstance(value, list):
        raise BridgeError(f"{label} must be an array")
    return value


def _http_url(value: Any) -> str:
    text = _string(value, "Telegram API base URL", 4096)
    parsed = urllib.parse.urlsplit(text)
    if parsed.scheme not in {"http", "https"} or not parsed.netloc or parsed.username or parsed.password:
        raise BridgeError("api_base_url must be an absolute HTTP or HTTPS URL without credentials")
    if parsed.scheme == "http" and parsed.hostname not in {"127.0.0.1", "localhost", "::1"}:
        raise BridgeError("non-loopback Telegram API URLs must use HTTPS")
    return urllib.parse.urlunsplit((parsed.scheme, parsed.netloc, parsed.path.rstrip("/"), "", ""))


def main() -> int:
    parser = argparse.ArgumentParser(description="Treer Telegram CLI-only Message bridge")
    parser.add_argument("--once", action="store_true", help=argparse.SUPPRESS)
    args = parser.parse_args()
    try:
        bridge = build_bridge()
        if args.once:
            bridge.run_inbound_once(timeout=0)
            bridge.run_outbound_once(wait_milliseconds=0)
        else:
            run_forever(bridge)
        return 0
    except KeyboardInterrupt:
        return 0
    except Exception as error:
        code = getattr(error, "code", type(error).__name__)
        print(f"Treer Telegram stopped: {code}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
