#!/usr/bin/env python3
"""Stateful fake Treer CLI used only by the Mail plugin contract tests."""

from __future__ import annotations

import fcntl
import json
import os
import sys
from datetime import datetime, timedelta, timezone
from pathlib import Path
from urllib.parse import urlencode


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


def chrono_timestamp(delta: timedelta) -> str:
    value = datetime.now(timezone.utc) + delta
    return value.isoformat(timespec="microseconds").replace("+00:00", "123Z")


def main() -> int:
    state_path = Path(os.environ["FAKE_TREER_STATE"])
    state_path.parent.mkdir(parents=True, exist_ok=True)
    with state_path.open("a+", encoding="utf-8") as handle:
        fcntl.flock(handle, fcntl.LOCK_EX)
        handle.seek(0)
        content = handle.read()
        state = json.loads(content) if content else {}
        state.setdefault("workspace_id", "workspace-a")
        state.setdefault("service_id", "svc_mail")
        state.setdefault("capability", "phs_fake." + "a" * 64)
        state.setdefault("revoked", False)
        state.setdefault("messages", [])
        state.setdefault("deliveries", [])
        state.setdefault("idempotency", {})
        state.setdefault("imports", {})
        state.setdefault("calls", [])

        arguments = sys.argv[1:]
        while arguments[:1] in (["--workspace"], ["--url"]):
            arguments = arguments[2:]
        if arguments[:1] == ["--workspace"]:
            arguments = arguments[2:]
        state["calls"].append(
            {
                "argv": arguments,
                "stdin": sys.stdin.read() if "--body-file" in arguments else "",
                "human_session": bool(os.environ.get("TREER_PLUGIN_HUMAN_SESSION")),
            }
        )
        stdin = state["calls"][-1]["stdin"]

        if arguments[:3] == ["plugin", "auth", "start"]:
            redirect_uri = option(arguments, "--redirect-uri")
            service = option(arguments, "--service")
            state_value = "pos_fake_state"
            response = {
                "authorize_url": "https://proxy.example/api/apps/oauth/authorize?"
                + urlencode(
                    {
                        "client_id": service,
                        "redirect_uri": redirect_uri,
                        "state": state_value,
                    }
                ),
                "expires_at": chrono_timestamp(timedelta(minutes=10)),
            }
        elif arguments[:3] == ["plugin", "auth", "exchange"]:
            state["revoked"] = False
            response = {
                "session_capability": state["capability"],
                "session": {
                    "plugin_id": "mail",
                    "workspace_id": state["workspace_id"],
                    "service_id": state["service_id"],
                    "principal": {
                        "kind": "human",
                        "id": "user-a",
                        "name": "Owner",
                        "role": "owner",
                    },
                    "expires_at": chrono_timestamp(timedelta(hours=12)),
                },
            }
        elif arguments[:3] == ["plugin", "auth", "revoke"]:
            state["revoked"] = True
            response = {"revoked": 1}
        elif arguments[:2] == ["human", "list"]:
            require_human(state)
            response = {
                "humans": [
                    {"user_id": "user-a", "preferred_name": "Owner", "role": "owner"},
                    {"user_id": "user-b", "preferred_name": "Reviewer", "role": "member"},
                ]
            }
        elif arguments[:2] == ["agent", "list"]:
            require_human(state)
            response = {
                "agents": [
                    {
                        "agent_id": "agent-a",
                        "workspace_id": state["workspace_id"],
                        "server_id": "server-a",
                        "kind": "codex",
                        "name": "Builder",
                        "cwd": "/workspace",
                        "status": "idle",
                        "started_at": "2026-08-20T00:00:00Z",
                        "updated_at": "2026-08-20T00:00:00Z",
                        "output_revision": 0,
                    }
                ]
            }
        elif arguments[:2] == ["message", "send"]:
            require_human(state)
            key = option(arguments, "--idempotency-key")
            if key in state["idempotency"]:
                message_id = state["idempotency"][key]
                message = next(item for item in state["messages"] if item["message_id"] == message_id)
                response = {"message": message, "delivery_ids": [], "idempotent_replay": True}
            else:
                recipients = options(arguments, "--to")
                principals = [
                    {
                        "kind": "agent" if value.startswith("agent-") else "human",
                        "id": value,
                        "name": "Builder" if value == "agent-a" else "Reviewer",
                    }
                    for value in recipients
                ]
                message_id = f"msg_sent_{len(state['messages']) + 1}"
                message = {
                    "schema_version": 1,
                    "message_id": message_id,
                    "workspace_id": state["workspace_id"],
                    "sender": {
                        "kind": "human",
                        "id": "user-a",
                        "name": "Owner",
                        "role": "owner",
                    },
                    "recipients": principals,
                    "context_ids": options(arguments, "--context"),
                    "body": stdin,
                    "created_at": datetime.now(timezone.utc).isoformat(),
                }
                state["messages"].append(message)
                state["idempotency"][key] = message_id
                response = {"message": message, "delivery_ids": [], "idempotent_replay": False}
        elif arguments[:2] == ["message", "list"]:
            require_human(state)
            limit = int(option(arguments, "--limit", "50"))
            response = {
                "messages": list(reversed(state["messages"]))[:limit],
                "remaining_unacknowledged": sum(
                    1 for delivery in state["deliveries"] if not delivery.get("acked")
                ),
            }
        elif arguments[:2] == ["message", "receive"]:
            require_human(state)
            limit = int(option(arguments, "--limit", "50"))
            pending = [item for item in state["deliveries"] if not item.get("acked")]
            response = {
                "deliveries": [
                    {
                        "delivery_id": delivery["delivery_id"],
                        "message": next(
                            message
                            for message in state["messages"]
                            if message["message_id"] == delivery["message_id"]
                        ),
                        "recipient": {
                            "kind": "human",
                            "id": "user-a",
                            "name": "Owner",
                            "role": "owner",
                        },
                        "created_at": "2026-08-20T00:01:00Z",
                    }
                    for delivery in pending[:limit]
                ],
                "remaining_unacknowledged": len(pending),
            }
        elif arguments[:2] == ["message", "ack"]:
            require_human(state)
            delivery_ids = []
            for value in arguments[2:]:
                if value == "--operation-id":
                    break
                delivery_ids.append(value)
            for delivery in state["deliveries"]:
                if delivery["delivery_id"] in delivery_ids:
                    delivery["acked"] = True
            response = {
                "acknowledged_delivery_ids": delivery_ids,
                "already_acknowledged_delivery_ids": [],
            }
        elif arguments[:2] == ["message", "import"]:
            operation_id = option(arguments, "--operation-id")
            records = json.loads(stdin)
            if operation_id in state["imports"]:
                response = state["imports"][operation_id]
            else:
                response = {
                    "imported": len(records),
                    "existing": 0,
                    "message_ids": [record["message_id"] for record in records],
                }
                state["imports"][operation_id] = response
                state.setdefault("imported_messages", []).extend(records)
        else:
            fail("fake_command_unknown", "fake Treer received an unsupported command")

        handle.seek(0)
        handle.truncate()
        json.dump(state, handle)
        handle.flush()
        print(json.dumps(response))
    return 0


def require_human(state: dict[str, object]) -> None:
    if state.get("revoked") or os.environ.get("TREER_PLUGIN_HUMAN_SESSION") != state["capability"]:
        fail("plugin_session_invalid", "plugin human session is invalid")


if __name__ == "__main__":
    raise SystemExit(main())
