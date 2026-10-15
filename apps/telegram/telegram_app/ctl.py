"""`im-poc`: send a POC control message to the bridge Agent (see poc.py for the protocol).

The bridge answers with a Core Message to the caller in the same context (the launcher notifies the caller as
for any Message); this tool only submits the request and prints the Core response for the request itself.
"""
from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import uuid
from typing import Any


def build(arguments: argparse.Namespace) -> dict[str, Any]:
    command = arguments.command
    if command == "list":
        return {"im": "poc.list"}
    if command == "audit":
        return {"im": "audit", "limit": arguments.limit}
    if command == "users":
        return {"im": "users.list"}
    if command == "user-set":
        return {"im": "users.set", "user_id": arguments.user_id, "label": arguments.label, "enabled": not arguments.disabled}
    if command == "handoff":
        payload: dict[str, Any] = {"im": "poc.handoff", "to": arguments.to}
        if arguments.conversation:
            payload["conversation"] = arguments.conversation
        return payload
    payload = {"im": f"poc.{command}", "conversation": arguments.conversation}
    if command == "set":
        payload["to"] = arguments.to
        if arguments.wake is not None:
            payload["wake_agent"] = arguments.wake
    return payload


def parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(description="Read or change IM point-of-contact bindings through the bridge Agent")
    p.add_argument("--bridge", default=os.environ.get("IM_BRIDGE"), help="bridge Agent ID or tree path (or IM_BRIDGE)")
    sub = p.add_subparsers(dest="command", required=True)
    # Group chat IDs are negative, so the conversation is an option; write --conversation=-10042:9 (with the equals sign).
    conversation = "CHAT or CHAT:THREAD (a Telegram chat ID, optionally with the topic/thread ID), e.g. --conversation=-10042:9"
    for name in ("get", "enable", "disable"):
        sub.add_parser(name).add_argument("--conversation", required=True, help=conversation)
    s = sub.add_parser("set", help="make AGENT the POC of a conversation (creates the binding if needed)")
    s.add_argument("--conversation", required=True, help=conversation)
    s.add_argument("to", metavar="AGENT", help="Agent ID or tree path")
    s.add_argument("--wake", dest="wake", action="store_true", default=None, help="also prompt the POC when a message arrives")
    s.add_argument("--no-wake", dest="wake", action="store_false")
    h = sub.add_parser("handoff", help="as the current POC, hand over a conversation (default: all you serve) to AGENT")
    h.add_argument("to", metavar="AGENT")
    h.add_argument("--conversation", help=conversation)
    sub.add_parser("list")
    a = sub.add_parser("audit")
    a.add_argument("--limit", type=int, default=20)
    sub.add_parser("users")
    u = sub.add_parser("user-set", help="admit/rename/disable an IM user (any accepted sender unless control.user_admin_agent_ids restricts it)")
    u.add_argument("user_id", type=int)
    u.add_argument("label")
    u.add_argument("--disabled", action="store_true")
    return p


def main(argv: list[str] | None = None) -> int:
    arguments = parser().parse_args(argv)
    if not arguments.bridge:
        print(json.dumps({"error": {"code": "bridge_required", "message": "pass --bridge or set IM_BRIDGE"}}), file=sys.stderr)
        return 2
    payload = json.dumps(build(arguments), separators=(",", ":"))
    completed = subprocess.run(
        [os.environ.get("TREER_CLI", "treer"), "message", "send", "--to", arguments.bridge,
         "--idempotency-key", f"im-poc-{uuid.uuid4().hex}", "--body-file", "-"],
        input=payload, text=True, capture_output=True, timeout=60, check=False)
    sys.stdout.write(completed.stdout)
    sys.stderr.write(completed.stderr)
    return completed.returncode
