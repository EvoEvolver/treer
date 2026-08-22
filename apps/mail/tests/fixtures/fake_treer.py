#!/usr/bin/env python3
"""Stateful fake operator CLI used by Mail migration tests."""

from __future__ import annotations

import fcntl
import json
import os
import sys
from pathlib import Path


def fail(code: str, message: str) -> None:
    print(json.dumps({"error": {"code": code, "message": message}}), file=sys.stderr)
    raise SystemExit(1)


def option(arguments: list[str], name: str, default: str | None = None) -> str | None:
    try:
        return arguments[arguments.index(name) + 1]
    except (ValueError, IndexError):
        return default


def main() -> int:
    state_path = Path(os.environ["FAKE_TREER_STATE"])
    state_path.parent.mkdir(parents=True, exist_ok=True)
    with state_path.open("a+", encoding="utf-8") as handle:
        fcntl.flock(handle, fcntl.LOCK_EX)
        handle.seek(0)
        content = handle.read()
        state = json.loads(content) if content else {}
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
            }
        )
        stdin = state["calls"][-1]["stdin"]

        if arguments[:2] == ["message", "import"]:
            operation_id = option(arguments, "--operation-id")
            records = json.loads(stdin)
            state["import_attempts"] = int(state.get("import_attempts", 0)) + 1
            fail_once_at = int(os.environ.get("FAKE_TREER_FAIL_IMPORT_ONCE_AT", "0"))
            if (
                fail_once_at == state["import_attempts"]
                and not state.get("import_failure_injected")
            ):
                state["import_failure_injected"] = True
                handle.seek(0)
                handle.truncate()
                json.dump(state, handle)
                handle.flush()
                print(
                    json.dumps(
                        {
                            "error": {
                                "code": "fake_import_interrupted",
                                "message": "injected import interruption",
                            }
                        }
                    ),
                    file=sys.stderr,
                )
                return 1
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


if __name__ == "__main__":
    raise SystemExit(main())
