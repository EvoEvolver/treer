#!/usr/bin/env python3
"""Fake psql output for exercising the PostgreSQL legacy export parser."""

import json
import os
import sys


def main() -> int:
    command = sys.stdin.read()
    if "FROM human_sessions" in command:
        print(json.dumps({"active": 1, "expired": 1}, separators=(",", ":")))
    else:
        with open(os.environ["FAKE_PSQL_MESSAGES"], encoding="utf-8") as handle:
            for line in handle:
                if line.strip():
                    print(line.strip())
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
