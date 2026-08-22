#!/usr/bin/env python3
"""Telegram Bot API bridge for Treer Core Message using only nested CLI calls."""

import sys
from pathlib import Path


APP_ROOT = str(Path(__file__).resolve().parent)
if APP_ROOT not in sys.path:
    sys.path.insert(0, APP_ROOT)

from telegram_app.bridge import TelegramBridge
from telegram_app.clients import BotApi, TreerCli
from telegram_app.common import Binding, Config
from telegram_app.runtime import main
from telegram_app.state import StateStore

__all__ = [
    "Binding",
    "BotApi",
    "Config",
    "StateStore",
    "TelegramBridge",
    "TreerCli",
]


if __name__ == "__main__":
    raise SystemExit(main())
