from __future__ import annotations

import argparse
import hashlib
import json
import os
import signal
import stat
import sys
import threading
import time
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
    _list,
    _object,
    _string,
)
from .state import StateStore
from .tokenstore import TokenStore

def read_token_file(path: str) -> str:
    """The bot token from a private file, so it never appears in argv, the environment or the configuration.

    The path must be absolute and name a regular file (not a symlink) owned by this user that no group or other
    user can access; messages never include the file's content."""
    if not os.path.isabs(path):
        raise BridgeError("bot_token_file must be an absolute path")
    try:
        info = os.lstat(path)
    except OSError as error:
        raise BridgeError("bot_token_file cannot be read") from error
    if not stat.S_ISREG(info.st_mode):
        raise BridgeError("bot_token_file must be a regular file, not a symlink or directory")
    if info.st_uid != os.getuid() or info.st_mode & 0o077:
        raise BridgeError("bot_token_file must be owned by this user with mode 0600 (no group/other access)")
    if info.st_size == 0 or info.st_size > 512:
        raise BridgeError("bot_token_file must hold one token of at most 512 bytes")
    try:
        with open(path, "rb") as handle:
            raw = handle.read(513)
        return raw.decode("ascii").strip()
    except (OSError, UnicodeDecodeError) as error:
        raise BridgeError("bot_token_file cannot be read as an ASCII token") from error


def resolve_token(token_file: Any, state_dir: Path) -> str | None:
    """The bot token, in order: TELEGRAM_BOT_TOKEN, the file the settings App maintains in the state directory,
    the `bot_token_file` named in the configuration. None when none is configured yet (the bridge then waits)."""
    token = os.environ.get("TELEGRAM_BOT_TOKEN")
    if token is None:
        token = TokenStore(state_dir).read()
    if token is None and token_file is not None:
        if not isinstance(token_file, str) or not token_file:
            raise BridgeError("bot_token_file must be a non-empty path")
        token = read_token_file(token_file)
    return token


def load_config() -> tuple[Config, Path, str, str | None]:
    config_path = os.environ.get("TREER_APP_CONFIG")
    state_value = os.environ.get("TREER_APP_STATE_DIR")
    cli = os.environ.get("TREER_CLI", "treer")
    if not config_path or not state_value or not cli:
        raise BridgeError("TREER_APP_CONFIG and TREER_APP_STATE_DIR are required")
    with open(config_path, "rb") as handle:
        value = json.load(handle)
    if not isinstance(value, dict):
        raise BridgeError("Telegram config must contain a JSON object")
    state_dir = Path(state_value)
    state_dir.mkdir(parents=True, exist_ok=True)
    token = resolve_token(value.get("bot_token_file"), state_dir)
    if token is not None and (not token or len(token) > 256 or any(character.isspace() for character in token)):
        raise BridgeError("the bot token is invalid")
    allowed_keys = {
        "bot_token_file",
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
        "users",
        "limits",
        "control",
        "settings",
    }
    unknown = set(value) - allowed_keys
    if unknown:
        raise BridgeError(f"Telegram config has unknown fields: {', '.join(sorted(unknown))}")
    users = value.get("allowed_user_ids", [])
    if not isinstance(users, list) or len(users) > 1_000:
        raise BridgeError("allowed_user_ids must contain at most 1000 numeric user IDs")
    allowed_ids = [_integer(user, "allowed Telegram user ID") for user in users]
    labels: dict[int, str] = {}
    labelled = value.get("users", [])
    if not isinstance(labelled, list) or len(labelled) > 1_000:
        raise BridgeError("users must be a list of at most 1000 {id, label} objects")
    for raw in labelled:
        item = _object(raw, "user")
        if set(item) - {"id", "label"}:
            raise BridgeError("user has unknown fields")
        user_id = _integer(item.get("id"), "Telegram user ID")
        labels[user_id] = " ".join(_string(item.get("label"), "user label", 64).split())
        allowed_ids.append(user_id)
    allowed_users = frozenset(allowed_ids)
    if len(allowed_users) != len(allowed_ids):
        raise BridgeError("Telegram user IDs must be unique")
    binding_values = value.get("bindings", [])
    if not isinstance(binding_values, list) or len(binding_values) > 1_000:
        raise BridgeError("bindings must contain at most 1000 chat/topic bindings (more can be set later with /poc)")
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
    limits = _object(value.get("limits", {}), "limits")
    if set(limits) - {"per_user_per_hour", "per_conversation_per_minute", "max_unreplied"}:
        raise BridgeError("limits has unknown fields")
    control = _object(value.get("control", {}), "control")
    if set(control) - {"user_admin_agent_ids"}:
        raise BridgeError("control has unknown fields")
    # Absent: any sender the bridge accepts may admit IM users (like POC changes). A list restricts it; [] means nobody.
    admins = _list(control["user_admin_agent_ids"], "user_admin_agent_ids") if "user_admin_agent_ids" in control else None
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
            user_labels=tuple(sorted(labels.items())),
            per_user_per_hour=_bounded_int(limits.get("per_user_per_hour", 60), "per_user_per_hour", 1, 10_000),
            per_conversation_per_minute=_bounded_int(limits.get("per_conversation_per_minute", 20), "per_conversation_per_minute", 1, 1_000),
            max_unreplied=_bounded_int(limits.get("max_unreplied", 50), "max_unreplied", 1, 10_000),
            user_admin_agent_ids=None if admins is None else frozenset(_string(item, "user admin Agent ID", 256) for item in admins),
        ),
        state_dir,
        cli,
        token,
    )


STATUS_CODES = ("needs_token", "starting", "running", "paused", "error")


def record_status(state: StateStore, status: str, error: str = "") -> None:
    """The bridge's state as the settings App shows it. Only fixed words are stored, never an exception text."""
    state.set_setting("bridge_state", status)
    state.set_setting("bridge_error", error)
    state.set_setting("bridge_heartbeat", repr(time.time()))


def build_bridge(config: Config, state_dir: Path, cli_path: str, token: str) -> TelegramBridge:
    bot = BotApi(config.api_base_url, token)
    me = bot.get_me()
    bot_id = str(_integer(me.get("id"), "bot ID"))
    state = StateStore(state_dir / "telegram-state.sqlite3")
    state.bind_bot(bot_id)
    state.recover_sending(config.ambiguous_retry_seconds)
    username = me.get("username")
    state.set_setting("bot_id", bot_id)
    if isinstance(username, str) and username.replace("_", "").isalnum() and len(username) <= 64:
        state.set_setting("bot_username", username)
    return TelegramBridge(config, state, TreerCli(cli_path), bot, bot_id, state_dir)


def token_fingerprint(token: str | None) -> str | None:
    return None if token is None else hashlib.sha256(token.encode()).hexdigest()


def run_supervised(stopped: threading.Event, load: Any = None, poll: float = 2.0) -> None:
    """Runs until stopped. With no token the bridge waits (status needs_token) instead of exiting; when the token appears,
    changes or is cleared (by the settings App, a file or the environment) the bridge is rebuilt within a few seconds."""
    load = load or load_config
    while not stopped.is_set():
        config, state_dir, cli_path, token = load()
        status_state = StateStore(state_dir / "telegram-state.sqlite3")
        if token is None:
            record_status(status_state, "needs_token")
            stopped.wait(poll)
            continue
        record_status(status_state, "starting")
        try:
            bridge = build_bridge(config, state_dir, cli_path, token)
        except BridgeError as error:
            code = getattr(error, "code", "") or ("different_bot" if "different bot" in str(error) else "bot_unavailable")
            record_status(status_state, "error", code if code in ("different_bot", "bot_unavailable", "telegram_unavailable", "telegram_request_rejected") else "bot_unavailable")
            print(f"Treer Telegram could not start the bridge ({code}); retrying", file=sys.stderr, flush=True)
            stopped.wait(5)
            continue
        run_workers(bridge, stopped, token_fingerprint(token), config, state_dir, poll)


def run_workers(bridge: TelegramBridge, stopped: threading.Event, fingerprint: str | None, config: Config, state_dir: Path, poll: float) -> None:
    halt = threading.Event()

    def paused() -> bool:
        return bridge.state.get_setting("paused") == "1"

    def loop(label: str, action: Any) -> None:
        while not stopped.is_set() and not halt.is_set():
            if paused():
                halt.wait(1)
                continue
            try:
                action()
            except BridgeError as error:
                code = getattr(error, "code", type(error).__name__)
                print(f"Treer Telegram {label} retrying after {code}", file=sys.stderr, flush=True)
                halt.wait(1)

    threads = [threading.Thread(target=loop, args=("inbound", bridge.run_inbound_once), daemon=True),
               threading.Thread(target=loop, args=("outbound", bridge.run_outbound_once), daemon=True)]
    for thread in threads:
        thread.start()
    token_file = _token_file_hint(state_dir)
    try:
        while not stopped.wait(poll):
            record_status(bridge.state, "paused" if paused() else "running")
            try:
                current = token_fingerprint(resolve_token(token_file, state_dir))
            except BridgeError:
                current = None
            if current != fingerprint:
                print("Treer Telegram: the bot token changed; restarting the bridge", file=sys.stderr, flush=True)
                return
            if not all(thread.is_alive() for thread in threads):
                raise BridgeError("Telegram worker thread exited unexpectedly")
    finally:
        halt.set()      # the old workers wind down after their current call; they do nothing further


def _token_file_hint(state_dir: Path) -> Any:
    try:
        with open(os.environ["TREER_APP_CONFIG"], "rb") as handle:
            return json.load(handle).get("bot_token_file")
    except (OSError, ValueError, KeyError, AttributeError):
        return None


def main() -> int:
    parser = argparse.ArgumentParser(description="Treer Telegram Message App")
    parser.add_argument("--once", action="store_true", help=argparse.SUPPRESS)
    args = parser.parse_args()
    try:
        if args.once:
            config, state_dir, cli_path, token = load_config()
            if token is None:
                raise BridgeError("no bot token configured")
            bridge = build_bridge(config, state_dir, cli_path, token)
            bridge.run_inbound_once(timeout=0)
            bridge.run_outbound_once(wait_milliseconds=0)
            return 0
        stopped = threading.Event()
        signal.signal(signal.SIGTERM, lambda *_: stopped.set())
        signal.signal(signal.SIGINT, lambda *_: stopped.set())
        run_supervised(stopped)
        return 0
    except KeyboardInterrupt:
        return 0
    except Exception as error:
        code = getattr(error, "code", type(error).__name__)
        print(f"Treer Telegram stopped: {code}", file=sys.stderr)
        return 1
