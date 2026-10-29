"""Private gateway state: rules config (JSON) and the append-only audit log (JSON Lines)."""

from __future__ import annotations

import json
import os
import secrets
import threading
import time
import uuid
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

from gw_credentials import write_private
from gw_rules import DEFAULT_CONFIG, validate_config


def utc_now() -> str:
    return datetime.now(timezone.utc).isoformat(timespec="milliseconds").replace("+00:00", "Z")


class State:
    CONFIG = "config.json"
    AUDIT = "audit.jsonl"
    AUDIT_ROTATED = "audit.1.jsonl"

    def __init__(self, root: Path, seed: dict[str, Any] | None = None, audit_max_bytes: int = 8 * 1024 * 1024) -> None:
        os.umask(0o077)
        root.mkdir(parents=True, exist_ok=True, mode=0o700)
        root.chmod(0o700)
        self.root = root
        self.lock = threading.Lock()
        self.audit_max_bytes = audit_max_bytes
        self.manifest_states: dict[str, tuple[str, float]] = {}
        path = root / self.CONFIG
        if path.exists():
            self._config = validate_config(json.loads(path.read_text(encoding="utf-8")))
        else:
            self._config = validate_config(seed or {}, DEFAULT_CONFIG)
            write_private(path, json.dumps(self._config, indent=2).encode())

    # config ------------------------------------------------------------------
    def config(self) -> dict[str, Any]:
        with self.lock:
            return json.loads(json.dumps(self._config))

    def update_config(self, change: dict[str, Any]) -> dict[str, Any]:
        with self.lock:
            config = validate_config(change, self._config)
            write_private(self.root / self.CONFIG, json.dumps(config, indent=2).encode())
            self._config = config
            return json.loads(json.dumps(config))

    # audit -------------------------------------------------------------------
    def audit(self, entry: dict[str, Any]) -> dict[str, Any]:
        record = {"id": f"aud_{uuid.uuid4().hex[:16]}", "at": utc_now(), **entry}
        line = json.dumps(record, separators=(",", ":"), ensure_ascii=False) + "\n"
        with self.lock:
            path = self.root / self.AUDIT
            try:
                if path.stat().st_size + len(line) > self.audit_max_bytes:
                    os.replace(path, self.root / self.AUDIT_ROTATED)
            except FileNotFoundError:
                pass
            descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_APPEND, 0o600)
            with os.fdopen(descriptor, "a", encoding="utf-8") as handle:
                handle.write(line)
        return record

    def read_audit(self, limit: int = 100, filters: dict[str, str] | None = None) -> list[dict[str, Any]]:
        filters = {key: value for key, value in (filters or {}).items() if value}
        entries: list[dict[str, Any]] = []
        with self.lock:
            files = [self.root / self.AUDIT, self.root / self.AUDIT_ROTATED]
            for path in files:
                try:
                    lines = path.read_text(encoding="utf-8").splitlines()
                except FileNotFoundError:
                    continue
                for raw in reversed(lines):
                    try:
                        entry = json.loads(raw)
                    except ValueError:
                        continue
                    if self._matches(entry, filters):
                        entries.append(entry)
                        if len(entries) >= limit:
                            return entries
        return entries

    @staticmethod
    def _matches(entry: dict[str, Any], filters: dict[str, str]) -> bool:
        caller = entry.get("caller") or {}
        for key, value in filters.items():
            if key == "repo" and str(entry.get("repo", "")).lower() != value.lower():
                return False
            if key == "op" and entry.get("op") != value:
                return False
            if key == "result" and entry.get("result") != value:
                return False
            if key == "caller" and value not in (caller.get("id"), caller.get("name")):
                return False
            if key == "kind" and caller.get("kind") != value:
                return False
        return True

    # GitHub App manifest flow -------------------------------------------------
    def new_manifest_state(self, subject: str) -> str:
        state = secrets.token_urlsafe(32)
        now = time.time()
        with self.lock:
            self.manifest_states = {k: v for k, v in self.manifest_states.items() if v[1] > now}
            self.manifest_states[state] = (subject, now + 1800)
        return state

    def take_manifest_state(self, state: str, subject: str) -> bool:
        """Single use, 30 minutes, bound to the human who started the flow."""
        with self.lock:
            entry = self.manifest_states.pop(state, None)
        return entry is not None and entry[1] > time.time() and secrets.compare_digest(entry[0], subject)
