from __future__ import annotations

import json
import os
import re
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
from .poc import CHANNEL, PocError, PocService, conversation_key, parse_control
from .state import StateStore

LIMIT_NOTICES = {
    "per_user_per_hour": "You are sending too many messages; please try again later.",
    "per_conversation_per_minute": "This chat is receiving messages too quickly; please wait a minute.",
    "max_unreplied": "The point of contact has not answered your earlier messages yet; please wait for a reply.",
}


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
        # The configuration only seeds the registry; afterwards bindings and users live in SQLite.
        for change in state.reconcile_registry(
            CHANNEL,
            bot_id,
            [(conversation_key(b.chat_id, b.thread_id), b.target_agent_id, b.wake_agent) for b in config.bindings],
            [(str(user), config.user_label(user)) for user in sorted(config.allowed_user_ids)],
        ):
            print(f"Treer Telegram registry: {change}", file=sys.stderr, flush=True)
        self.poc = PocService(state, cli, bot_id, config.user_admin_agent_ids)
        if config.user_admin_agent_ids is None:
            print("Treer Telegram: IM user admission is OPEN to any sender the bridge accepts (control.user_admin_agent_ids = \"any\")", file=sys.stderr, flush=True)

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
        conversation = conversation_key(chat_id, thread_id)
        binding = self.state.get_binding(CHANNEL, self.bot_id, conversation)
        user = self.state.get_user(CHANNEL, self.bot_id, str(user_id))
        text = message.get("text")
        command = text.split(maxsplit=1)[0].split("@", 1)[0].lower() if isinstance(text, str) and text.strip() else ""
        if sender.get("is_bot") is True:
            self.state.complete_update(update_id, bot_id=self.bot_id, error="bot sender ignored")
            return
        if command == "/pair":
            self._pair(update_id, chat, sender, text, chat_id, thread_id, user_id)
            return
        admitted = user is not None and user[1]
        if not admitted or (binding is None and command != "/poc"):
            if self.config.respond_to_denied:
                self._safe_notice(str(chat_id), thread_id, "This Telegram identity or chat is not allowed.")
            self.state.complete_update(update_id, bot_id=self.bot_id, error="Telegram identity denied")
            return
        label = user[0] if user else f"user-{user_id}"
        if not isinstance(text, str) or not text:
            self._safe_notice(str(chat_id), thread_id, "Treer Telegram currently accepts text messages only.")
            self.state.complete_update(update_id, bot_id=self.bot_id, error="non-text message ignored")
            return
        if command in {"/start", "/help", "/target", "/status", "/poc"}:
            self._process_command(update_id, command, binding, str(chat_id), thread_id, text=text, conversation=conversation,
                                  actor=f"telegram:{user_id}({label})")
            return
        if binding is not None and not binding.enabled:
            self._safe_notice(str(chat_id), thread_id, "This chat's POC is switched off. Send /poc on to turn it back on.")
            self.state.complete_update(update_id, bot_id=self.bot_id, error="POC disabled")
            return
        if len(text.encode("utf-8")) > MAX_CORE_BODY_BYTES:
            self._safe_notice(str(chat_id), thread_id, "Message is too large for Treer Core.")
            self.state.complete_update(update_id, bot_id=self.bot_id, error="message too large")
            return
        limited = self.state.admit_inbound(
            CHANNEL, self.bot_id, str(user_id), conversation,
            per_user_per_hour=self.config.per_user_per_hour,
            per_conversation_per_minute=self.config.per_conversation_per_minute,
            max_unreplied=self.config.max_unreplied,
        )
        if limited:
            self._safe_notice(str(chat_id), thread_id, LIMIT_NOTICES[limited])
            self.state.complete_update(update_id, bot_id=self.bot_id, error=f"rate limited: {limited}")
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
            "metadata": {"user_id": str(user_id), "user_label": label, "update_id": str(update_id)},
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
        self.state.record_inbound(CHANNEL, self.bot_id, str(user_id), conversation)
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
        binding: Any,
        chat_id: str,
        thread_id: int | None,
        *,
        text: str = "",
        conversation: str = "",
        actor: str = "",
    ) -> None:
        if command == "/poc":
            notice = self._poc_command(text, binding, conversation, actor)
        elif command == "/status":
            try:
                response = self.cli.run(["agent", "show", binding.target_agent_id])
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
                "Send text or reply to a bridged message. /poc shows or changes the point of contact."
            )
        self.bot.send_message(chat_id, thread_id, notice, None)
        self.state.complete_update(update_id, bot_id=self.bot_id)

    def _pair(self, update_id: int, chat: dict[str, Any], sender: dict[str, Any], text: str, chat_id: int, thread_id: int | None, user_id: int) -> None:
        """`/pair CODE`: redeems a one-time code made in the settings App, admits this Telegram user and binds this private chat
        to the code's POC. Works for users who are not admitted yet - that is its purpose - but only in a private chat."""
        if chat.get("type") != "private":
            notice, outcome = "Pairing works only in a private chat with this bot. Open one and send /pair CODE there.", "pair refused: not private"
        else:
            parts = text.split(maxsplit=1)
            status, row = self.state.redeem_pairing(parts[1] if len(parts) > 1 else "", str(user_id))
            if status == "ok" and row is not None:
                notice, outcome = self._complete_pairing(row, sender, chat_id, user_id), "paired"
            elif status in ("blocked", "locked"):
                if status == "locked":
                    self.state.audit_reject(f"telegram:{user_id}", "pair", "pair_locked")
                notice, outcome = "Too many failed attempts. Ask for a new pairing code later.", f"pair {status}"
            else:
                notice, outcome = "That pairing code is not valid or has expired.", "pair invalid"
        self._safe_notice(str(chat_id), thread_id, notice)
        self.state.complete_update(update_id, bot_id=self.bot_id, error=outcome)

    @staticmethod
    def _pair_label(sender: dict[str, Any], user_id: int) -> str:
        username = sender.get("username")
        return username if isinstance(username, str) and re.fullmatch(r"[A-Za-z0-9_]{1,32}", username) else f"user-{user_id}"

    def _complete_pairing(self, row: dict[str, Any], sender: dict[str, Any], chat_id: int, user_id: int) -> str:
        label = self._pair_label(sender, user_id)
        actor = f"telegram:{user_id}({label})"
        self.state.set_user(CHANNEL, self.bot_id, str(user_id), label, True, actor=actor, via="pair", op="pair.user")
        try:
            _, new = self.poc.set_poc(conversation_key(chat_id, None), row["poc_agent_id"], actor=actor, via="pair", op="poc.pair", enabled=True)
        except PocError as error:
            return f"You are paired, but the point of contact could not be set: {error.message}"
        creator = re.sub(r"[^A-Za-z0-9:_-]", "?", str(row["created_by"]))[:128]
        text = (f"Telegram user {label} (id {user_id}) was paired to this point of contact for chat {chat_id}; "
                f"the pairing code was created by {creator}.")
        for recipient in sorted({new.target_agent_id, creator.split(":", 1)[-1]} - {""}):
            try:
                self.cli.run(["message", "send", "--to", recipient, "--idempotency-key", f"im-pair-{row['id']}-{recipient}"[:200], "--body-file", "-"], stdin=text)
            except BridgeError as error:
                print(f"Treer Telegram pairing notice to {recipient} failed ({getattr(error, 'code', type(error).__name__)})", file=sys.stderr, flush=True)
        return f"Paired. Your messages in this chat now go to {new.target_agent_id}."

    def _poc_command(self, text: str, binding: Any, conversation: str, actor: str) -> str:
        """`/poc` shows, `/poc AGENT` (ID or tree path) sets, `/poc off|on` pauses/resumes this chat's POC."""
        parts = text.split(maxsplit=1)
        argument = parts[1].strip() if len(parts) > 1 else ""
        usage = "Change it with /poc <agent ID or tree path>; pause or resume with /poc off or /poc on."
        try:
            if not argument:
                if binding is None:
                    return f"This chat has no point of contact yet. Set one with /poc <agent ID or tree path>."
                state = "on" if binding.enabled else "off"
                return f"POC for this chat: {binding.target_agent_id} ({state}). {usage}"
            if argument.lower() in {"on", "off"}:
                if binding is None:
                    return "This chat has no point of contact yet. Set one with /poc <agent ID or tree path>."
                _, new = self.poc.set_poc(conversation, None, actor=actor, via="telegram", op=f"poc.{argument.lower()}",
                                          enabled=argument.lower() == "on")
                return f"POC {new.target_agent_id} is now {argument.lower()} for this chat."
            _, new = self.poc.set_poc(conversation, argument, actor=actor, via="telegram", op="poc.set", enabled=True)
            return f"POC for this chat is now {new.target_agent_id}."
        except PocError as error:
            return f"Could not change the POC: {error.message}"

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
                status = "failed" if error.code == "policy_denied" else "pending"
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
        control = parse_control(body)
        if control is not None:
            # A Core control message (POC read/change): never relayed to the chat; the result goes back to its sender.
            self.poc.handle_control(delivery_id, message, control)
            self._ack_delivery(delivery_id)
            return
        route = self.state.first_mapping_for_contexts(self.bot_id, contexts)
        if route:
            chat_id = str(route["chat_id"])
            thread_id = _thread_value(int(route["thread_key"]))
            native_reply_to = int(route["telegram_message_id"])
        else:
            sender = _object(message.get("sender"), "Core sender")
            sender_id = _string(sender.get("id"), "Core sender ID", 256)
            candidates = self.state.bindings_for_agent(CHANNEL, self.bot_id, sender_id)
            if len(candidates) != 1:
                raise BridgeError(
                    f"delivery {delivery_id} has no unambiguous Telegram route; it remains unacknowledged"
                )
            chat_id, _, thread = candidates[0].conversation.rpartition(":")
            thread_id = None if thread == "0" else int(thread)
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
