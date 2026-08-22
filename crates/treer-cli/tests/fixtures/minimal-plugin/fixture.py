#!/usr/bin/env python3
"""Process-level fixture for the CLI-only plugin capability boundary."""

import hashlib
import json
import os
import subprocess
from pathlib import Path


def require(value: bool, message: str) -> None:
    if not value:
        raise RuntimeError(message)


def invoke(arguments: list[str]) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [os.environ["TREER_CLI"], *arguments],
        text=True,
        capture_output=True,
        check=False,
        timeout=10,
    )


def error_code(completed: subprocess.CompletedProcess[str]) -> str | None:
    try:
        value = json.loads(completed.stderr)
    except ValueError:
        return None
    return value.get("error", {}).get("code")


def main() -> int:
    for forbidden in (
        "TREER_AGENT_ID",
        "TREER_AGENT_SERVER_URL",
        "TREER_OPERATOR_CREDENTIAL",
        "TREER_SERVER_ID",
        "TREER_WORKLOAD_CREDENTIAL",
        "TREER_WORKSPACE_ID",
    ):
        require(forbidden not in os.environ, f"runner leaked {forbidden}")

    config = json.loads(Path(os.environ["TREER_PLUGIN_CONFIG"]).read_text(encoding="utf-8"))
    require(config == {"marker": "fixture-v1"}, "runner passed the wrong config file")
    require(os.environ.get("FIXTURE_CHANNEL") == "fixture-channel", "declared config missing")
    secret = os.environ.get("FIXTURE_SECRET")
    require(secret == "fixture-secret", "declared secret missing")

    allowed = invoke(["message", "receive", "--wait", "0", "--limit", "1"])
    require(allowed.returncode == 0, f"declared command failed: {allowed.stderr}")
    response = json.loads(allowed.stdout)
    require(isinstance(response.get("deliveries"), list), "declared command returned wrong JSON")

    undeclared = invoke(["machine", "list"])
    require(
        undeclared.returncode != 0 and error_code(undeclared) == "plugin_command_denied",
        "undeclared command escaped the broker capability check",
    )
    direct = invoke(
        [
            "--url",
            "http://127.0.0.1:1/",
            "message",
            "receive",
            "--wait",
            "0",
            "--limit",
            "1",
        ]
    )
    require(
        direct.returncode != 0 and error_code(direct) == "plugin_command_denied",
        "direct-mode connection override escaped the broker",
    )

    state_path = Path(os.environ["TREER_PLUGIN_STATE_DIR"]) / "fixture-state.json"
    previous = json.loads(state_path.read_text(encoding="utf-8")) if state_path.exists() else {}
    state = {
        "runs": int(previous.get("runs", 0)) + 1,
        "config_marker": config["marker"],
        "configuration": os.environ["FIXTURE_CHANNEL"],
        "secret_sha256": hashlib.sha256(secret.encode()).hexdigest(),
        "allowed_command": "message.receive",
        "undeclared_error": error_code(undeclared),
        "direct_override_error": error_code(direct),
    }
    temporary = state_path.with_suffix(".tmp")
    temporary.write_text(json.dumps(state, sort_keys=True) + "\n", encoding="utf-8")
    os.replace(temporary, state_path)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
