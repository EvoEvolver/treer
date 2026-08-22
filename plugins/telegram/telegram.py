#!/usr/bin/env python3
"""Telegram Bot API bridge for Treer Core Message using only nested CLI calls."""

import sys
from pathlib import Path


PLUGIN_ROOT = str(Path(__file__).resolve().parent)
if PLUGIN_ROOT not in sys.path:
    sys.path.insert(0, PLUGIN_ROOT)

from telegram_plugin.bridge import TelegramBridge
from telegram_plugin.clients import BotApi, TreerCli
from telegram_plugin.common import Binding, Config
from telegram_plugin.runtime import main
from telegram_plugin.state import StateStore

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
