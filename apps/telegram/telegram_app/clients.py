from __future__ import annotations

import json
import os
import subprocess
import urllib.error
import urllib.parse
import urllib.request
from typing import Any

from .common import (
    BOT_API_RESPONSE_LIMIT,
    CLI_TIMEOUT_SECONDS,
    CliError,
    TelegramAmbiguous,
    TelegramError,
    TelegramRateLimited,
    _list,
    _object,
)

class TreerCli:
    def __init__(self, executable: str) -> None:
        self.executable = executable

    def run(self, arguments: list[str], *, stdin: str | None = None) -> dict[str, Any]:
        try:
            completed = subprocess.run(
                [self.executable, *arguments],
                input=stdin,
                capture_output=True,
                text=True,
                timeout=CLI_TIMEOUT_SECONDS,
                check=False,
                env=os.environ.copy(),
            )
        except (OSError, subprocess.TimeoutExpired) as error:
            raise CliError("app_cli_unavailable", "Treer CLI is unavailable") from error
        if completed.returncode != 0:
            code = "app_cli_failed"
            message = "Treer rejected the request"
            try:
                value = json.loads(completed.stderr.strip())
                failure = value.get("error", {})
                code = str(failure.get("code") or code)
                message = str(failure.get("message") or message)
            except (TypeError, ValueError):
                pass
            raise CliError(code, message)
        try:
            value = json.loads(completed.stdout)
        except ValueError as error:
            raise CliError("app_cli_invalid_response", "Treer CLI returned invalid JSON") from error
        if not isinstance(value, dict):
            raise CliError("app_cli_invalid_response", "Treer CLI returned an invalid object")
        return value


class BotApi:
    def __init__(self, base_url: str, token: str, timeout_seconds: float = 65.0) -> None:
        self.base_url = base_url.rstrip("/")
        self.token = token
        self.timeout_seconds = timeout_seconds

    def call(self, method: str, payload: dict[str, Any], *, sending: bool = False) -> Any:
        url = f"{self.base_url}/bot{urllib.parse.quote(self.token, safe=':')}/{method}"
        request = urllib.request.Request(
            url,
            data=json.dumps(payload, separators=(",", ":")).encode("utf-8"),
            headers={"Content-Type": "application/json", "Accept": "application/json"},
            method="POST",
        )
        try:
            with urllib.request.urlopen(request, timeout=self.timeout_seconds) as response:
                raw = response.read(BOT_API_RESPONSE_LIMIT + 1)
        except urllib.error.HTTPError as error:
            raw = error.read(BOT_API_RESPONSE_LIMIT + 1)
            return self._decode(method, raw, sending=sending, http_status=error.code)
        except (OSError, urllib.error.URLError, TimeoutError) as error:
            if sending:
                raise TelegramAmbiguous(
                    "telegram_send_ambiguous",
                    "Telegram send may have succeeded but its response was unavailable",
                ) from error
            raise TelegramError("telegram_unavailable", "Telegram Bot API is unavailable") from error
        if len(raw) > BOT_API_RESPONSE_LIMIT:
            if sending:
                raise TelegramAmbiguous(
                    "telegram_send_ambiguous", "Telegram returned an oversized send response"
                )
            raise TelegramError("telegram_response_invalid", "Telegram returned an oversized response")
        return self._decode(method, raw, sending=sending, http_status=200)

    def _decode(self, method: str, raw: bytes, *, sending: bool, http_status: int) -> Any:
        try:
            value = json.loads(raw)
        except (UnicodeDecodeError, ValueError) as error:
            if sending and 200 <= http_status < 300:
                raise TelegramAmbiguous(
                    "telegram_send_ambiguous", "Telegram returned an invalid send response"
                ) from error
            raise TelegramError("telegram_response_invalid", "Telegram returned invalid JSON") from error
        if not isinstance(value, dict):
            raise TelegramError("telegram_response_invalid", "Telegram returned an invalid object")
        if value.get("ok") is True:
            return value.get("result")
        error_code = value.get("error_code", http_status)
        parameters = value.get("parameters") if isinstance(value.get("parameters"), dict) else {}
        if error_code == 429:
            retry_after = parameters.get("retry_after", 1)
            try:
                retry_seconds = float(retry_after)
            except (TypeError, ValueError):
                retry_seconds = 1.0
            raise TelegramRateLimited(retry_seconds)
        if isinstance(error_code, int) and error_code >= 500:
            raise TelegramError("telegram_temporary_failure", "Telegram temporarily rejected the request")
        raise TelegramError(
            "telegram_request_rejected",
            f"Telegram rejected {method} with error {error_code}",
        )

    def get_me(self) -> dict[str, Any]:
        return _object(self.call("getMe", {}), "getMe result")

    def get_updates(self, offset: int, timeout: int) -> list[dict[str, Any]]:
        result = self.call(
            "getUpdates",
            {"offset": offset, "timeout": timeout, "allowed_updates": ["message"]},
        )
        return [_object(item, "Telegram update") for item in _list(result, "getUpdates result")]

    def send_message(
        self,
        chat_id: str,
        thread_id: int | None,
        text: str,
        reply_to_message_id: int | None,
    ) -> dict[str, Any]:
        payload: dict[str, Any] = {"chat_id": int(chat_id), "text": text}
        if thread_id is not None:
            payload["message_thread_id"] = thread_id
        if reply_to_message_id is not None:
            payload["reply_parameters"] = {
                "message_id": reply_to_message_id,
                "allow_sending_without_reply": True,
            }
        return _object(self.call("sendMessage", payload, sending=True), "sendMessage result")
