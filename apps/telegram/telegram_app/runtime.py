from __future__ import annotations

import argparse
import json
import os
import signal
import sys
import threading
from pathlib import Path
from typing import Any

from .bridge import TelegramBridge
from .clients import BotApi, TreerCli
from .common import (
    Binding,
    BridgeError,
    Config,
    _bounded_float,
    _bounded_int,
    _http_url,
    _integer,
    _object,
    _string,
)
from .state import StateStore

def load_config() -> tuple[Config, Path, str, str]:
    config_path = os.environ.get("TREER_APP_CONFIG")
    state_value = os.environ.get("TREER_APP_STATE_DIR")
    cli = os.environ.get("TREER_CLI", "treer")
    token = os.environ.get("TELEGRAM_BOT_TOKEN")
    if not config_path or not state_value or not cli or not token:
        raise BridgeError(
            "TREER_APP_CONFIG, TREER_APP_STATE_DIR, and TELEGRAM_BOT_TOKEN are required"
        )
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



def main() -> int:
    parser = argparse.ArgumentParser(description="Treer Telegram Message App")
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
