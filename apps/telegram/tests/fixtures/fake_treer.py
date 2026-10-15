#!/usr/bin/env python3
"""Stateful fake CLI for Telegram bridge tests."""

from __future__ import annotations

import fcntl
import json
import os
import sqlite3
import sys
from datetime import datetime, timezone
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[3]))
import cli_contract  # noqa: E402  (apps/cli_contract.py: the real CLI's argument contract)


def fail(code: str, message: str) -> None:
    print(json.dumps({"error": {"code": code, "message": message}}), file=sys.stderr)
    raise SystemExit(1)


def option(arguments: list[str], name: str, default: str | None = None) -> str | None:
    try:
        return arguments[arguments.index(name) + 1]
    except (ValueError, IndexError):
        return default


def options(arguments: list[str], name: str) -> list[str]:
    return [arguments[index + 1] for index, value in enumerate(arguments[:-1]) if value == name]


def main() -> int:
    path = Path(os.environ["FAKE_TREER_STATE"])
    with path.open("a+", encoding="utf-8") as handle:
        fcntl.flock(handle, fcntl.LOCK_EX)
        handle.seek(0)
        content = handle.read()
        state = json.loads(content) if content else {}
        state.setdefault("messages", [])
        state.setdefault("deliveries", [])
        state.setdefault("idempotency", {})
        state.setdefault("calls", [])
        state.setdefault("prompts", [])
        arguments = sys.argv[1:]
        has_envelope = not os.environ.get("FAKE_TREER_NO_ENVELOPE")  # an older Controller has no --stdin-envelope
        if arguments == ["message", "send", "--help"]:
            print("Send a durable Message\n\nOptions:\n      --to <RECIPIENTS>\n      --body <BODY>")
            if has_envelope:
                print("      --stdin-envelope  Read one JSON object {body, external_source} from stdin")
            return 0
        if "--stdin-envelope" in arguments and not has_envelope:
            print(json.dumps({"error": {"code": "cli_failed", "message": "unknown option --stdin-envelope"}}), file=sys.stderr)
            return 2
        problem = cli_contract.violation(arguments)
        if problem:
            print(json.dumps({"error": {"code": "cli_failed", "message": problem}}), file=sys.stderr)
            return 2
        files = options(arguments, "--external-source-file") + options(arguments, "--body-file")
        envelope = "--stdin-envelope" in arguments
        if envelope and (files or option(arguments, "--body") or any(a.startswith("--body=") for a in arguments)):
            print(json.dumps({"error": {"code": "cli_failed", "message": "--stdin-envelope excludes --body, --body-file and --external-source-file"}}), file=sys.stderr)
            return 2
        stdin = sys.stdin.read() if "-" in files or envelope else ""
        if any(value != "-" for value in files):
            print(json.dumps({"error": {"code": "cli_failed", "message": "client-local files cannot be read by the Controller; pipe the contents to stdin and use '-' instead"}}), file=sys.stderr)
            return 1
        state["calls"].append({"argv": arguments, "stdin": stdin})

        if arguments[:2] == ["message", "send"]:
            key = option(arguments, "--idempotency-key")
            if key in state["idempotency"]:
                message_id = state["idempotency"][key]
                message = next(item for item in state["messages"] if item["message_id"] == message_id)
                response = {"message": message, "delivery_ids": [], "idempotent_replay": True}
            else:
                message_id = f"msg_{len(state['messages']) + 1}"
                source_file = option(arguments, "--external-source-file")
                external_source = None
                if envelope:
                    parsed = json.loads(stdin)
                    external_source = parsed.get("external_source")
                    stdin = parsed["body"]
                elif source_file:
                    external_source = json.loads(stdin)
                message = {
                    "schema_version": 1,
                    "message_id": message_id,
                    "workspace_id": "workspace-a",
                    "sender": {"kind": "agent", "id": "bridge-agent", "name": "Telegram"},
                    "recipients": [
                        {"kind": "agent", "id": target, "name": "Builder"}
                        for target in options(arguments, "--to")
                    ],
                    "context_ids": options(arguments, "--context"),
                    "body": (next((v[7:] for v in arguments if v.startswith("--body=")), None) or stdin),
                    "created_at": datetime.now(timezone.utc).isoformat(),
                    "external_source": external_source,
                }
                state["messages"].append(message)
                state["idempotency"][key] = message_id
                response = {"message": message, "delivery_ids": [], "idempotent_replay": False}
        elif arguments[:2] == ["message", "receive"]:
            pending = [delivery for delivery in state["deliveries"] if not delivery.get("acked")]
            limit = int(option(arguments, "--limit", "50"))
            response = {
                "deliveries": pending[:limit],
                "remaining_unacknowledged": len(pending),
            }
        elif arguments[:2] == ["message", "ack"]:
            delivery_id = arguments[2]
            if os.environ.get("FAKE_TREER_FAIL_ACK_DELIVERY") == delivery_id:
                fail("fake_ack_failure", "injected acknowledgement failure")
            guard = os.environ.get("FAKE_TELEGRAM_ACK_GUARD_DB")
            if guard:
                connection = sqlite3.connect(guard)
                mapped = connection.execute(
                    "SELECT COUNT(*) FROM message_mappings WHERE delivery_id = ?", (delivery_id,)
                ).fetchone()[0]
                unsent = connection.execute(
                    "SELECT COUNT(*) FROM outbound_chunks WHERE delivery_id = ? AND status != 'sent'",
                    (delivery_id,),
                ).fetchone()[0]
                connection.close()
                if mapped < 1 or unsent != 0:
                    fail("ack_before_mapping", "Core ack happened before Telegram mapping commit")
            for delivery in state["deliveries"]:
                if delivery["delivery_id"] == delivery_id:
                    delivery["acked"] = True
            response = {
                "acknowledged_delivery_ids": [delivery_id],
                "already_acknowledged_delivery_ids": [],
            }
        elif arguments[:2] in (["agent", "get"], ["agent", "show"]):
            paths = json.loads(os.environ.get("FAKE_TREER_PATHS", "{}"))
            target = arguments[2]
            if target.startswith("/") or target in os.environ.get("FAKE_TREER_UNKNOWN_AGENTS", "").split(","):
                if target not in paths:
                    fail("agent_not_found", "no such Agent")
                target = paths[target]
            response = {
                "agent_id": target,
                "workspace_id": "workspace-a",
                "server_id": "server-a",
                "kind": "codex",
                "name": "Builder",
                "cwd": "/workspace",
                "status": "idle",
                "started_at": "2026-08-20T00:00:00Z",
                "updated_at": "2026-08-20T00:00:00Z",
                "output_revision": 0,
            }
        elif arguments[:2] == ["agent", "prompt"]:
            state["prompts"].append({"target": arguments[2], "text": arguments[3]})
            response = {"agent_id": arguments[2], "status": "working"}
        else:
            fail("fake_command_unknown", "fake Treer received an unsupported command")

        handle.seek(0)
        handle.truncate()
        json.dump(state, handle)
        handle.flush()
        print(json.dumps(response))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
