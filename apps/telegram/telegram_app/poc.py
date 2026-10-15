"""POC (point of contact) control: resolving targets, changing bindings, and the Core-Message control protocol.

Anyone who can reach the bridge Agent with a Core Message (Message Policy decides) may read or change POC bindings -
including a POC handing a conversation over to another Agent - because the authority is the verified Core sender,
which is recorded in the audit trail. There is deliberately no human confirmation gate. Admitting new IM users is a
wider than choosing a POC; by default nobody may do it through control messages, `control.user_admin_agent_ids` lists the Agents
that may, and the literal "any" opens it to any sender the bridge accepts (a deployment decision, audited like POC changes).
Control messages are JSON objects with an `im` key, e.g. {"im":"poc.set","conversation":"-10042:9","to":"/team/builder"}.
They are never relayed to the chat; the result is sent back to the sender as a Core Message in the same context.
"""
from __future__ import annotations

import json
import sys
import uuid
from typing import Any

from .clients import TreerCli
from .common import BridgeError, CliError, _thread_key
from .registry import PocBinding
from .state import StateStore

CHANNEL = "telegram"
MAX_TARGET = 256
MAX_LABEL = 64
OPS = ("poc.get", "poc.list", "poc.set", "poc.handoff", "poc.enable", "poc.disable", "audit", "users.list", "users.set")


def conversation_key(chat_id: str | int, thread_id: int | None) -> str:
    return f"{chat_id}:{thread_id if thread_id is not None else 0}"


def parse_conversation(value: Any) -> str:
    """`CHAT` or `CHAT:THREAD` (thread 0 = the main chat) -> canonical key."""
    if not isinstance(value, str) or not value.strip():
        raise ValueError("conversation must be CHAT or CHAT:THREAD")
    chat, _, thread = value.strip().rpartition(":")
    if not chat:
        chat, thread = thread, "0"
    try:
        int(chat)
        int(thread)
    except ValueError as error:
        raise ValueError("conversation must be CHAT or CHAT:THREAD with numeric parts") from error
    return f"{int(chat)}:{int(thread)}"


def parse_control(body: str) -> dict[str, Any] | None:
    text = body.strip()
    if not text.startswith("{") or len(text.encode("utf-8")) > 16 * 1024:
        return None
    try:
        value = json.loads(text)
    except ValueError:
        return None
    return value if isinstance(value, dict) and isinstance(value.get("im"), str) else None


def describe(binding: PocBinding | None) -> dict[str, Any] | None:
    if binding is None:
        return None
    return {"conversation": binding.conversation, "target_agent_id": binding.target_agent_id, "enabled": binding.enabled,
            "wake_agent": binding.wake_agent, "updated_by": binding.updated_by}


class PocError(Exception):
    def __init__(self, code: str, message: str) -> None:
        super().__init__(message)
        self.code, self.message = code, message


class PocService:
    def __init__(self, state: StateStore, cli: TreerCli, account_id: str, user_admin_agent_ids: frozenset[str] | None = frozenset()) -> None:
        self.state, self.cli, self.account_id = state, cli, account_id
        self.user_admin_agent_ids = user_admin_agent_ids

    def resolve_agent(self, target: str) -> str:
        """Stable ID of an Agent given as ID or absolute tree path (never an option-looking string); the bridge's own Policy decides visibility."""
        if (not isinstance(target, str) or not target.strip() or len(target) > MAX_TARGET or any(c.isspace() for c in target.strip())
                or not target.strip().startswith(("ag_", "agent-", "/"))):
            raise PocError("invalid_target", "target must be a stable Agent ID (ag_...) or an absolute tree path (/team/agent)")
        try:
            info = self.cli.run(["agent", "show", target.strip()])
        except CliError as error:
            raise PocError("target_unavailable", f"Agent not found or not visible ({error.code})") from error
        agent_id = info.get("agent_id")
        if not isinstance(agent_id, str) or not agent_id.startswith(("ag_", "agent-")) or len(agent_id) > MAX_TARGET:
            raise PocError("target_unavailable", "target is not an Agent")
        return agent_id

    def set_poc(self, conversation: str, target: str | None, *, actor: str, via: str, op: str, enabled: bool | None = None,
                wake: bool | None = None, delivery_id: str | None = None, notify_exclude: str = "") -> tuple[PocBinding | None, PocBinding]:
        agent_id = self.resolve_agent(target) if target is not None else None
        try:
            old, new = self.state.set_binding(CHANNEL, self.account_id, conversation, target=agent_id, enabled=enabled, wake=wake,
                                              actor=actor, via=via, op=op, delivery_id=delivery_id)
        except KeyError as error:
            raise PocError("no_binding", "this conversation has no POC yet; set one with a target") from error
        self.announce(op, conversation, old, new, actor, delivery_id, notify_exclude)
        return old, new

    def announce(self, op: str, conversation: str, old: PocBinding | None, new: PocBinding, actor: str, delivery_id: str | None, exclude: str) -> None:
        """Makes a change visible without gating it: a durable log line, and a fixed short Message to the Agents that
        gained or lost the conversation (never the actor). Best effort; the audit row is the record."""
        print(f"Treer IM poc change: actor={json.dumps(actor)} op={op} conversation={conversation} "
              f"old={old.target_agent_id if old else None} new={new.target_agent_id}{'' if new.enabled else ' (off)'}", file=sys.stderr, flush=True)
        if old is None or (old.target_agent_id == new.target_agent_id and old.enabled == new.enabled):
            return
        # The chat actor's label is admin-set free text: the notice names only the numeric ID (the audit keeps the label).
        text = (f"IM point of contact changed for conversation {conversation}: {old.target_agent_id} -> {new.target_agent_id}"
                f"{'' if new.enabled else ' (forwarding off)'}, by {actor.split('(')[0]}.")
        for recipient in sorted({old.target_agent_id, new.target_agent_id} - {exclude}):
            if not self.state.claim_notice(conversation, recipient):
                continue          # at most one notice per conversation and recipient per five minutes
            try:
                key = f"im-poc-change-{delivery_id or uuid.uuid4().hex}-{conversation}-{recipient}"
                self.cli.run(["message", "send", "--to", recipient, "--idempotency-key", key[:200], "--body-file", "-"], stdin=text)
            except BridgeError as error:
                print(f"Treer IM poc change notice to {recipient} failed ({getattr(error, 'code', type(error).__name__)})", file=sys.stderr, flush=True)

    # --- Core control messages ---------------------------------------------------------------

    def handle_control(self, delivery_id: str, message: dict[str, Any], control: dict[str, Any]) -> None:
        """Applies one control message and sends the result to its sender. Idempotent per delivery."""
        sender = message.get("sender") if isinstance(message.get("sender"), dict) else {}
        sender_id = str(sender.get("id") or "")
        if not sender_id:
            raise BridgeError("control message without a sender")
        actor = f"{sender.get('kind', 'principal')}:{sender_id}"
        op = control["im"]
        replay = self.state.delivery_audited(delivery_id)
        try:
            if op not in OPS:
                raise PocError("unknown_op", f"unknown op; supported: {', '.join(OPS)}")
            result = self._apply(op, control, actor, sender_id, delivery_id, replay)
            outcome = {"ok": True, "op": op, **result}
        except PocError as error:
            self.state.audit_reject(actor, op, error.code)
            outcome = {"ok": False, "op": op, "error": {"code": error.code, "message": error.message}}
        if replay:
            outcome["replayed"] = True
        self.cli.run(["message", "send", "--to", sender_id, "--context", str(message["message_id"]),
                      "--idempotency-key", f"im-control-{delivery_id}", "--body-file", "-"],
                     stdin=json.dumps({"im": "result", **outcome}, separators=(",", ":")))

    def _conversation(self, control: dict[str, Any]) -> str:
        try:
            return parse_conversation(control.get("conversation"))
        except ValueError as error:
            raise PocError("invalid_conversation", str(error)) from error

    def _apply(self, op: str, control: dict[str, Any], actor: str, sender_id: str, delivery_id: str, replay: bool) -> dict[str, Any]:
        state, account = self.state, self.account_id
        if op == "poc.list":
            return {"bindings": [describe(b) for b in state.list_bindings(CHANNEL, account)]}
        if op == "audit":
            limit = control.get("limit", 20)
            return {"events": state.audit_list(CHANNEL, account, limit if isinstance(limit, int) and 1 <= limit <= 200 else 20)}
        if op == "users.list":
            return {"users": state.list_users(CHANNEL, account), "user_admission": ("open" if self.user_admin_agent_ids is None else "restricted" if self.user_admin_agent_ids else "closed")}
        if op == "users.set":
            if self.user_admin_agent_ids is not None and sender_id not in self.user_admin_agent_ids:
                raise PocError("forbidden", "only agents listed in control.user_admin_agent_ids may manage IM users")
            user_id, label, enabled = control.get("user_id"), control.get("label"), control.get("enabled", True)
            if not isinstance(user_id, (str, int)) or isinstance(user_id, bool) or not str(user_id).lstrip("-").isdigit():
                raise PocError("invalid_user", "user_id must be a numeric Telegram user ID")
            if not isinstance(label, str) or not label.strip() or len(label) > MAX_LABEL or not isinstance(enabled, bool):
                raise PocError("invalid_user", f"label must be 1-{MAX_LABEL} characters and enabled a boolean")
            if not replay:
                state.set_user(CHANNEL, account, str(int(user_id)), " ".join(label.split()), enabled, actor=actor, delivery_id=delivery_id)
            return {"user": {"user_id": str(int(user_id)), "label": " ".join(label.split()), "enabled": enabled}}
        conversation = None
        if op != "poc.handoff" or control.get("conversation") is not None:
            conversation = self._conversation(control)
        if op == "poc.get":
            return {"binding": describe(state.get_binding(CHANNEL, account, conversation))}
        wake = control.get("wake_agent")
        if wake is not None and not isinstance(wake, bool):
            raise PocError("invalid_field", "wake_agent must be a boolean")
        if op == "poc.handoff":
            return self._handoff(control, conversation, actor, sender_id, delivery_id, replay)
        if replay:
            return {"binding": describe(state.get_binding(CHANNEL, account, conversation))}
        if op == "poc.set":
            _, new = self.set_poc(conversation, control.get("to"), actor=actor, via="control", op=op, enabled=True, wake=wake,
                                  delivery_id=delivery_id, notify_exclude=sender_id)
        else:
            _, new = self.set_poc(conversation, None, actor=actor, via="control", op=op, enabled=op == "poc.enable",
                                  delivery_id=delivery_id, notify_exclude=sender_id)
        return {"binding": describe(new)}

    def _handoff(self, control: dict[str, Any], conversation: str | None, actor: str, sender_id: str, delivery_id: str, replay: bool) -> dict[str, Any]:
        """The current POC passes one conversation (or every conversation it serves) to another Agent."""
        mine = [b for b in self.state.list_bindings(CHANNEL, self.account_id) if b.enabled and b.target_agent_id == sender_id]
        if conversation is not None:
            mine = [b for b in mine if b.conversation == conversation]
        if not mine:
            raise PocError("not_poc", "the sender is not the POC of that conversation")
        if replay:
            return {"handed_over": [b.conversation for b in mine]}
        target = self.resolve_agent(control.get("to"))
        done = []
        for index, binding in enumerate(mine):
            old, new = self.state.set_binding(CHANNEL, self.account_id, binding.conversation, target=target, enabled=True, wake=None, actor=actor,
                                              via="control", op="poc.handoff", delivery_id=delivery_id if index == 0 else None)
            self.announce("poc.handoff", binding.conversation, old, new, actor, f"{delivery_id}-{index}", sender_id)
            done.append(binding.conversation)
        return {"handed_over": done, "to": target}
