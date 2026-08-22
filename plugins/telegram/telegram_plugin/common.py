from __future__ import annotations

import hashlib
import json
import urllib.parse
from dataclasses import dataclass
from typing import Any

MAX_TELEGRAM_TEXT_UNITS = 4096
MAX_CORE_BODY_BYTES = 32 * 1024
CLI_TIMEOUT_SECONDS = 125
BOT_API_RESPONSE_LIMIT = 2 * 1024 * 1024


class BridgeError(Exception):
    pass


class CliError(BridgeError):
    def __init__(self, code: str, message: str) -> None:
        super().__init__(message)
        self.code = code
        self.message = message


class TelegramError(BridgeError):
    def __init__(self, code: str, message: str) -> None:
        super().__init__(message)
        self.code = code
        self.message = message


class TelegramRateLimited(TelegramError):
    def __init__(self, retry_after: float) -> None:
        super().__init__("telegram_rate_limited", "Telegram rate limit reached")
        self.retry_after = max(0.05, min(retry_after, 3600.0))


class TelegramAmbiguous(TelegramError):
    pass


@dataclass(frozen=True)
class Binding:
    chat_id: str
    thread_id: int | None
    target_agent_id: str
    wake_agent: bool

    @property
    def key(self) -> tuple[str, int]:
        return self.chat_id, _thread_key(self.thread_id)


@dataclass(frozen=True)
class Config:
    api_base_url: str
    allowed_user_ids: frozenset[int]
    bindings: tuple[Binding, ...]
    poll_timeout_seconds: int
    receive_wait_milliseconds: int
    batch_size: int
    retry_initial_seconds: float
    retry_max_seconds: float
    ambiguous_retry_seconds: float
    respond_to_denied: bool

    def binding(self, chat_id: int, thread_id: int | None) -> Binding | None:
        key = (str(chat_id), _thread_key(thread_id))
        return next((binding for binding in self.bindings if binding.key == key), None)

def split_telegram_text(value: str) -> list[str]:
    if not value:
        raise BridgeError("Telegram cannot send an empty Message")
    chunks: list[str] = []
    current: list[str] = []
    units = 0
    for character in value:
        width = 2 if ord(character) > 0xFFFF else 1
        if current and units + width > MAX_TELEGRAM_TEXT_UNITS:
            chunks.append("".join(current))
            current = []
            units = 0
        current.append(character)
        units += width
    if current:
        chunks.append("".join(current))
    return chunks
def _thread_key(value: int | None) -> int:
    return value if value is not None else -1


def _thread_value(value: int) -> int | None:
    return None if value == -1 else value


def _sha256(value: str) -> str:
    return hashlib.sha256(value.encode("utf-8")).hexdigest()


def _bounded_error(value: str | None) -> str | None:
    if value is None:
        return None
    return value[:512]


def _is_integer(value: Any) -> bool:
    return isinstance(value, int) and not isinstance(value, bool)


def _integer(value: Any, label: str) -> int:
    if not _is_integer(value):
        raise BridgeError(f"{label} must be an integer")
    return int(value)


def _bounded_int(value: Any, label: str, minimum: int, maximum: int) -> int:
    number = _integer(value, label)
    if number < minimum or number > maximum:
        raise BridgeError(f"{label} must be between {minimum} and {maximum}")
    return number


def _bounded_float(value: Any, label: str, minimum: float, maximum: float) -> float:
    if not isinstance(value, (int, float)) or isinstance(value, bool):
        raise BridgeError(f"{label} must be numeric")
    number = float(value)
    if number < minimum or number > maximum:
        raise BridgeError(f"{label} must be between {minimum} and {maximum}")
    return number


def _string(value: Any, label: str, maximum: int) -> str:
    if not isinstance(value, str) or not value or len(value.encode("utf-8")) > maximum:
        raise BridgeError(f"{label} is empty or too long")
    return value


def _object(value: Any, label: str) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise BridgeError(f"{label} must be an object")
    return value


def _list(value: Any, label: str) -> list[Any]:
    if not isinstance(value, list):
        raise BridgeError(f"{label} must be an array")
    return value


def _http_url(value: Any) -> str:
    text = _string(value, "Telegram API base URL", 4096)
    parsed = urllib.parse.urlsplit(text)
    if parsed.scheme not in {"http", "https"} or not parsed.netloc or parsed.username or parsed.password:
        raise BridgeError("api_base_url must be an absolute HTTP or HTTPS URL without credentials")
    if parsed.scheme == "http" and parsed.hostname not in {"127.0.0.1", "localhost", "::1"}:
        raise BridgeError("non-loopback Telegram API URLs must use HTTPS")
    return urllib.parse.urlunsplit((parsed.scheme, parsed.netloc, parsed.path.rstrip("/"), "", ""))

