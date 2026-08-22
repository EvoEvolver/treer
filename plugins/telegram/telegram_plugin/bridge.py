from __future__ import annotations

import json
import os
import sys
import tempfile
import time
from pathlib import Path
from typing import Any

from .clients import BotApi, TreerCli
from .common import (
    MAX_CORE_BODY_BYTES,
    BridgeError,
    CliError,
    Config,
    TelegramAmbiguous,
    TelegramError,
    TelegramRateLimited,
    _integer,
    _is_integer,
    _list,
    _object,
    _sha256,
    _string,
    _thread_value,
    split_telegram_text,
)
from .state import StateStore

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
