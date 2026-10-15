#!/usr/bin/env python3
"""Settings App for the Treer Telegram bridge (see README, "Settings App")."""
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from settings_app.server import main

if __name__ == "__main__":
    raise SystemExit(main())
