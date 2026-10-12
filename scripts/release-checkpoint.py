#!/usr/bin/env python3
"""Reuse a completed local gate only for the same commit and frontend bytes."""
import argparse
import hashlib
import json
from pathlib import Path


def receipt(commit: str, artifact: Path) -> dict:
    return {"commit": commit, "app_sha256": hashlib.sha256(artifact.read_bytes()).hexdigest()}


def verified(path: Path, commit: str, artifact: Path) -> bool:
    try:
        return json.loads(path.read_text()) == receipt(commit, artifact)
    except (OSError, ValueError):
        return False


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=("write", "verify"))
    parser.add_argument("path", type=Path)
    parser.add_argument("commit")
    parser.add_argument("artifact", type=Path)
    args = parser.parse_args()
    if args.action == "verify":
        return 0 if verified(args.path, args.commit, args.artifact) else 1
    temporary = args.path.with_suffix(".tmp")
    temporary.write_text(json.dumps(receipt(args.commit, args.artifact)) + "\n")
    temporary.replace(args.path)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
