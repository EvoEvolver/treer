#!/usr/bin/env python3
from __future__ import annotations

import argparse
import hashlib
import hmac
import json
import os
import re
import sqlite3
import threading
from datetime import datetime, timezone
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import parse_qs, urlsplit


APP_ROOT = Path(__file__).resolve().parent
MAX_BODY_BYTES = 4 * 1024 * 1024
MAX_EVENTS_PER_BATCH = 256
MAX_PAGE_SIZE = 500
ID_PATTERN = re.compile(r"[A-Za-z0-9_-]{1,160}")
HASH_PATTERN = re.compile(r"[0-9a-f]{64}")


class TowerError(Exception):
    def __init__(self, status: int, code: str, message: str) -> None:
        super().__init__(message)
        self.status = status
        self.code = code
        self.message = message


def utc_now() -> str:
    return datetime.now(timezone.utc).isoformat().replace("+00:00", "Z")


def canonical_json(value: object) -> bytes:
    return json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode("utf-8")


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def prefix_node_id(parent_id: str | None, direction: str, payload_hash: str) -> str:
    return sha256(f"{parent_id or ''}\n{direction}\n{payload_hash}".encode())


def root_representation(accept: str, user_agent: str) -> str:
    choices: list[tuple[float, int, str]] = []
    for order, raw_entry in enumerate(accept.split(",")):
        parts = [part.strip().lower() for part in raw_entry.split(";")]
        if not parts or parts[0] not in {"text/html", "text/markdown"}:
            continue
        quality = 1.0
        for parameter in parts[1:]:
            if parameter.startswith("q="):
                try:
                    quality = float(parameter[2:])
                except ValueError:
                    quality = 0.0
        if 0 < quality <= 1:
            choices.append((-quality, order, parts[0]))
    if choices:
        return min(choices)[2]
    return "text/html" if "mozilla/" in user_agent.lower() else "text/markdown"


def require_id(value: object, field: str) -> str:
    if not isinstance(value, str) or not ID_PATTERN.fullmatch(value):
        raise TowerError(400, "invalid_request", f"{field} is invalid")
    return value


def optional_hash(value: object, field: str) -> str | None:
    if value is None:
        return None
    if not isinstance(value, str) or not HASH_PATTERN.fullmatch(value):
        raise TowerError(400, "invalid_request", f"{field} must be a SHA-256 digest")
    return value


class TowerStore:
    def __init__(self, path: Path) -> None:
        path.parent.mkdir(parents=True, exist_ok=True)
        self.lock = threading.Lock()
        self.connection = sqlite3.connect(path, check_same_thread=False)
        self.connection.row_factory = sqlite3.Row
        self.connection.execute("PRAGMA journal_mode=WAL")
        self.connection.execute("PRAGMA foreign_keys=ON")
        self.connection.executescript(
            """
            CREATE TABLE IF NOT EXISTS blobs (
                digest TEXT PRIMARY KEY,
                media_type TEXT NOT NULL,
                size INTEGER NOT NULL,
                body BLOB NOT NULL,
                created_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS prefix_nodes (
                node_id TEXT PRIMARY KEY,
                parent_id TEXT REFERENCES prefix_nodes(node_id),
                direction TEXT NOT NULL,
                payload_hash TEXT NOT NULL REFERENCES blobs(digest),
                depth INTEGER NOT NULL,
                created_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS streams (
                stream_id TEXT PRIMARY KEY,
                collector_id TEXT NOT NULL,
                workspace_id TEXT,
                agent_id TEXT NOT NULL,
                session_id TEXT,
                head_node_id TEXT REFERENCES prefix_nodes(node_id),
                last_sequence INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS events (
                event_id TEXT PRIMARY KEY,
                stream_id TEXT NOT NULL REFERENCES streams(stream_id),
                sequence INTEGER NOT NULL,
                node_id TEXT NOT NULL REFERENCES prefix_nodes(node_id),
                occurred_at TEXT NOT NULL,
                direction TEXT NOT NULL,
                method TEXT,
                rpc_id TEXT,
                issuer TEXT NOT NULL,
                evidence_class TEXT NOT NULL,
                UNIQUE(stream_id, sequence)
            );
            CREATE INDEX IF NOT EXISTS events_stream_sequence
                ON events(stream_id, sequence);
            CREATE TABLE IF NOT EXISTS findings (
                finding_id TEXT PRIMARY KEY,
                workspace_id TEXT,
                kind TEXT NOT NULL,
                verdict TEXT NOT NULL,
                severity TEXT NOT NULL,
                uncertainty REAL NOT NULL,
                summary TEXT NOT NULL,
                reviewer_id TEXT NOT NULL,
                reviewer_version TEXT,
                source_set_root TEXT NOT NULL,
                sources_json TEXT NOT NULL,
                created_at TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS findings_created_at ON findings(created_at DESC);
            """
        )

    def ingest(self, value: object) -> dict[str, object]:
        if not isinstance(value, dict) or value.get("schema_version") != 1:
            raise TowerError(400, "invalid_request", "schema_version must be 1")
        reject_unknown(value, {"schema_version", "stream", "events"})
        stream_value = value.get("stream")
        events_value = value.get("events")
        if not isinstance(stream_value, dict) or not isinstance(events_value, list):
            raise TowerError(400, "invalid_request", "stream and events are required")
        reject_unknown(stream_value, {"stream_id", "collector_id", "workspace_id", "agent_id", "session_id"})
        if not events_value or len(events_value) > MAX_EVENTS_PER_BATCH:
            raise TowerError(400, "invalid_request", f"events must contain 1-{MAX_EVENTS_PER_BATCH} records")

        stream_id = require_id(stream_value.get("stream_id"), "stream_id")
        collector_id = require_id(stream_value.get("collector_id"), "collector_id")
        agent_id = require_id(stream_value.get("agent_id"), "agent_id")
        workspace_id = stream_value.get("workspace_id")
        session_id = stream_value.get("session_id")
        if workspace_id is not None:
            workspace_id = require_id(workspace_id, "workspace_id")
        if session_id is not None and (not isinstance(session_id, str) or len(session_id) > 240):
            raise TowerError(400, "invalid_request", "session_id is invalid")

        prepared: list[dict[str, object]] = []
        for raw in events_value:
            if not isinstance(raw, dict):
                raise TowerError(400, "invalid_request", "each event must be an object")
            reject_unknown(
                raw,
                {"event_id", "sequence", "node_id", "parent_id", "payload_hash", "payload", "direction", "method", "rpc_id", "occurred_at"},
            )
            event_id = require_id(raw.get("event_id"), "event_id")
            sequence = raw.get("sequence")
            if not isinstance(sequence, int) or sequence < 1:
                raise TowerError(400, "invalid_request", "sequence must be a positive integer")
            direction = raw.get("direction")
            if direction not in {"client_to_agent", "agent_to_client", "lifecycle"}:
                raise TowerError(400, "invalid_request", "direction is invalid")
            payload = raw.get("payload")
            payload_bytes = canonical_json(payload)
            payload_hash = optional_hash(raw.get("payload_hash"), "payload_hash")
            if payload_hash != sha256(payload_bytes):
                raise TowerError(400, "payload_hash_mismatch", "payload hash does not match canonical payload")
            parent_id = optional_hash(raw.get("parent_id"), "parent_id")
            node_id = optional_hash(raw.get("node_id"), "node_id")
            if node_id != prefix_node_id(parent_id, direction, payload_hash):
                raise TowerError(400, "node_hash_mismatch", "prefix node hash is invalid")
            occurred_at = raw.get("occurred_at")
            if not isinstance(occurred_at, str) or len(occurred_at) > 64:
                raise TowerError(400, "invalid_request", "occurred_at is invalid")
            method = raw.get("method")
            rpc_id = raw.get("rpc_id")
            if method is not None and (not isinstance(method, str) or len(method) > 240):
                raise TowerError(400, "invalid_request", "method is invalid")
            if rpc_id is not None and (not isinstance(rpc_id, str) or len(rpc_id) > 240):
                raise TowerError(400, "invalid_request", "rpc_id is invalid")
            prepared.append(
                {
                    "event_id": event_id,
                    "sequence": sequence,
                    "direction": direction,
                    "payload": payload_bytes,
                    "payload_hash": payload_hash,
                    "parent_id": parent_id,
                    "node_id": node_id,
                    "occurred_at": occurred_at,
                    "method": method,
                    "rpc_id": rpc_id,
                    "issuer": "agent" if direction == "agent_to_client" else "gateway",
                    "evidence_class": "claim" if direction == "agent_to_client" else "observation",
                }
            )
        prepared.sort(key=lambda item: int(item["sequence"]))

        now = utc_now()
        inserted = 0
        deduplicated = 0
        with self.lock, self.connection:
            current = self.connection.execute(
                "SELECT * FROM streams WHERE stream_id = ?", (stream_id,)
            ).fetchone()
            if current is None:
                self.connection.execute(
                    "INSERT INTO streams(stream_id, collector_id, workspace_id, agent_id, session_id, created_at, updated_at) "
                    "VALUES(?, ?, ?, ?, ?, ?, ?)",
                    (stream_id, collector_id, workspace_id, agent_id, session_id, now, now),
                )
                last_sequence = 0
                head_node_id = None
            else:
                if current["collector_id"] != collector_id or current["agent_id"] != agent_id:
                    raise TowerError(409, "stream_identity_conflict", "stream identity does not match")
                last_sequence = int(current["last_sequence"])
                head_node_id = current["head_node_id"]

            for event in prepared:
                existing = self.connection.execute(
                    "SELECT stream_id, sequence, node_id FROM events WHERE event_id = ?",
                    (event["event_id"],),
                ).fetchone()
                if existing is not None:
                    if (
                        existing["stream_id"] != stream_id
                        or int(existing["sequence"]) != event["sequence"]
                        or existing["node_id"] != event["node_id"]
                    ):
                        raise TowerError(409, "event_conflict", "event id already has different content")
                    deduplicated += 1
                    continue
                if event["sequence"] != last_sequence + 1:
                    raise TowerError(409, "sequence_gap", f"expected sequence {last_sequence + 1}")
                if event["parent_id"] != head_node_id:
                    raise TowerError(409, "prefix_conflict", "event parent is not the current stream head")
                parent_depth = 0
                if event["parent_id"] is not None:
                    parent = self.connection.execute(
                        "SELECT depth FROM prefix_nodes WHERE node_id = ?", (event["parent_id"],)
                    ).fetchone()
                    if parent is None:
                        raise TowerError(409, "missing_parent", "prefix parent does not exist")
                    parent_depth = int(parent["depth"])
                self.connection.execute(
                    "INSERT OR IGNORE INTO blobs(digest, media_type, size, body, created_at) VALUES(?, ?, ?, ?, ?)",
                    (event["payload_hash"], "application/json", len(event["payload"]), event["payload"], now),
                )
                self.connection.execute(
                    "INSERT OR IGNORE INTO prefix_nodes(node_id, parent_id, direction, payload_hash, depth, created_at) "
                    "VALUES(?, ?, ?, ?, ?, ?)",
                    (
                        event["node_id"],
                        event["parent_id"],
                        event["direction"],
                        event["payload_hash"],
                        parent_depth + 1,
                        event["occurred_at"],
                    ),
                )
                self.connection.execute(
                    "INSERT INTO events(event_id, stream_id, sequence, node_id, occurred_at, direction, method, rpc_id, issuer, evidence_class) "
                    "VALUES(?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                    (
                        event["event_id"], stream_id, event["sequence"], event["node_id"],
                        event["occurred_at"], event["direction"], event["method"], event["rpc_id"],
                        event["issuer"], event["evidence_class"],
                    ),
                )
                last_sequence = int(event["sequence"])
                head_node_id = str(event["node_id"])
                inserted += 1
            self.connection.execute(
                "UPDATE streams SET workspace_id = COALESCE(?, workspace_id), session_id = COALESCE(?, session_id), "
                "head_node_id = ?, last_sequence = ?, updated_at = ? WHERE stream_id = ?",
                (workspace_id, session_id, head_node_id, last_sequence, now, stream_id),
            )
        return {"ok": True, "inserted": inserted, "deduplicated": deduplicated, "head_node_id": head_node_id}

    def list_streams(self, limit: int) -> list[dict[str, object]]:
        with self.lock:
            rows = self.connection.execute(
                "SELECT stream_id, collector_id, workspace_id, agent_id, session_id, head_node_id, "
                "last_sequence, created_at, updated_at FROM streams ORDER BY updated_at DESC LIMIT ?",
                (limit,),
            ).fetchall()
        return [dict(row) for row in rows]

    def stream_events(self, stream_id: str, after: int, limit: int) -> list[dict[str, object]]:
        with self.lock:
            rows = self.connection.execute(
                "SELECT e.event_id, e.stream_id, e.sequence, e.node_id, n.parent_id, e.occurred_at, "
                "e.direction, e.method, e.rpc_id, e.issuer, e.evidence_class, n.payload_hash, b.body "
                "FROM events e JOIN prefix_nodes n ON n.node_id = e.node_id "
                "JOIN blobs b ON b.digest = n.payload_hash "
                "WHERE e.stream_id = ? AND e.sequence > ? ORDER BY e.sequence LIMIT ?",
                (stream_id, after, limit),
            ).fetchall()
        return [self._event(row) for row in rows]

    def get_event(self, event_id: str) -> dict[str, object]:
        with self.lock:
            row = self.connection.execute(
                "SELECT e.event_id, e.stream_id, e.sequence, e.node_id, n.parent_id, e.occurred_at, "
                "e.direction, e.method, e.rpc_id, e.issuer, e.evidence_class, n.payload_hash, b.body "
                "FROM events e JOIN prefix_nodes n ON n.node_id = e.node_id "
                "JOIN blobs b ON b.digest = n.payload_hash WHERE e.event_id = ?",
                (event_id,),
            ).fetchone()
        if row is None:
            raise TowerError(404, "event_not_found", "event not found")
        return self._event(row)

    @staticmethod
    def _event(row: sqlite3.Row) -> dict[str, object]:
        value = dict(row)
        body = value.pop("body")
        value["payload"] = json.loads(bytes(body).decode("utf-8"))
        return value

    def create_finding(self, value: object) -> dict[str, object]:
        if not isinstance(value, dict) or value.get("schema_version") != 1:
            raise TowerError(400, "invalid_request", "schema_version must be 1")
        reject_unknown(
            value,
            {"schema_version", "workspace_id", "kind", "verdict", "severity", "uncertainty", "summary", "reviewer_id", "reviewer_version", "sources"},
        )
        kind = require_id(value.get("kind"), "kind")
        verdict = require_id(value.get("verdict"), "verdict")
        severity = require_id(value.get("severity"), "severity")
        reviewer_id = require_id(value.get("reviewer_id"), "reviewer_id")
        summary = value.get("summary")
        uncertainty = value.get("uncertainty")
        sources = value.get("sources")
        workspace_id = value.get("workspace_id")
        if workspace_id is not None:
            workspace_id = require_id(workspace_id, "workspace_id")
        if not isinstance(summary, str) or not summary.strip() or len(summary) > 20_000:
            raise TowerError(400, "invalid_request", "summary is invalid")
        if not isinstance(uncertainty, (int, float)) or not 0 <= float(uncertainty) <= 1:
            raise TowerError(400, "invalid_request", "uncertainty must be between 0 and 1")
        if not isinstance(sources, list) or not sources or len(sources) > MAX_PAGE_SIZE:
            raise TowerError(400, "invalid_request", f"sources must contain 1-{MAX_PAGE_SIZE} event ids")
        clean_sources = sorted(set(require_id(item, "source event id") for item in sources))
        with self.lock, self.connection:
            placeholders = ",".join("?" for _ in clean_sources)
            count = self.connection.execute(
                f"SELECT COUNT(*) FROM events WHERE event_id IN ({placeholders})", clean_sources
            ).fetchone()[0]
            if count != len(clean_sources):
                raise TowerError(400, "source_not_found", "one or more source events do not exist")
            source_workspaces = {
                row[0]
                for row in self.connection.execute(
                    f"SELECT DISTINCT s.workspace_id FROM events e JOIN streams s ON s.stream_id = e.stream_id "
                    f"WHERE e.event_id IN ({placeholders})",
                    clean_sources,
                ).fetchall()
            }
            if len(source_workspaces) != 1:
                raise TowerError(400, "mixed_workspace_sources", "all finding sources must belong to one workspace")
            source_workspace = next(iter(source_workspaces))
            if workspace_id is not None and workspace_id != source_workspace:
                raise TowerError(400, "workspace_mismatch", "finding workspace does not match its sources")
            workspace_id = source_workspace
            sources_json = canonical_json(clean_sources).decode()
            source_set_root = sha256(sources_json.encode())
            finding_id = f"finding_{sha256(canonical_json(value) + os.urandom(16))[:32]}"
            created_at = utc_now()
            self.connection.execute(
                "INSERT INTO findings(finding_id, workspace_id, kind, verdict, severity, uncertainty, summary, "
                "reviewer_id, reviewer_version, source_set_root, sources_json, created_at) "
                "VALUES(?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                (
                    finding_id, workspace_id, kind, verdict, severity, float(uncertainty),
                    summary.strip(), reviewer_id, value.get("reviewer_version"), source_set_root,
                    sources_json, created_at,
                ),
            )
        return {
            "finding_id": finding_id,
            "kind": kind,
            "verdict": verdict,
            "severity": severity,
            "uncertainty": float(uncertainty),
            "summary": summary.strip(),
            "reviewer_id": reviewer_id,
            "reviewer_version": value.get("reviewer_version"),
            "source_set_root": source_set_root,
            "sources": clean_sources,
            "created_at": created_at,
        }

    def list_findings(self, limit: int) -> list[dict[str, object]]:
        with self.lock:
            rows = self.connection.execute(
                "SELECT * FROM findings ORDER BY created_at DESC LIMIT ?", (limit,)
            ).fetchall()
        result = []
        for row in rows:
            item = dict(row)
            item["sources"] = json.loads(item.pop("sources_json"))
            result.append(item)
        return result

    def stats(self) -> dict[str, int]:
        with self.lock:
            counts = {}
            for table in ("streams", "events", "prefix_nodes", "blobs", "findings"):
                counts[table] = int(self.connection.execute(f"SELECT COUNT(*) FROM {table}").fetchone()[0])
            counts["blob_bytes"] = int(self.connection.execute("SELECT COALESCE(SUM(size), 0) FROM blobs").fetchone()[0])
        return counts


class TowerHandler(BaseHTTPRequestHandler):
    server: "TowerServer"

    def do_GET(self) -> None:
        try:
            self._get()
        except TowerError as error:
            self.send_error_json(error)
        except Exception as error:
            self.log_error("GET failed: %s", error)
            self.send_error_json(TowerError(500, "internal_error", "request failed"))

    def do_POST(self) -> None:
        try:
            self._post()
        except TowerError as error:
            self.send_error_json(error)
        except Exception as error:
            self.log_error("POST failed: %s", error)
            self.send_error_json(TowerError(500, "internal_error", "request failed"))

    def _get(self) -> None:
        parsed = urlsplit(self.path)
        path = parsed.path
        query = parse_qs(parsed.query)
        if path in {"", "/"}:
            representation = root_representation(self.headers.get("Accept", ""), self.headers.get("User-Agent", ""))
            if representation == "text/html":
                self.send_file(APP_ROOT / "web" / "index.html", "text/html; charset=utf-8", vary=True)
            else:
                self.send_file(APP_ROOT / "AGENT.md", "text/markdown; charset=utf-8", vary=True)
            return
        if path in {"/app.js", "/app.css"}:
            media = "text/javascript; charset=utf-8" if path.endswith(".js") else "text/css; charset=utf-8"
            self.send_file(APP_ROOT / "web" / path[1:], media)
            return
        if path == "/health":
            self.send_json(200, {"ok": True, "service": "tower", "schema_version": 1})
            return
        limit = self.page_limit(query)
        if path == "/v1/stats":
            self.send_json(200, {"stats": self.server.store.stats()})
        elif path == "/v1/streams":
            self.send_json(200, {"streams": self.server.store.list_streams(limit)})
        elif path == "/v1/findings":
            self.send_json(200, {"findings": self.server.store.list_findings(limit)})
        elif path.startswith("/v1/streams/") and path.endswith("/events"):
            stream_id = require_id(path[len("/v1/streams/") : -len("/events")].strip("/"), "stream_id")
            after = self.nonnegative_int(query.get("after", ["0"])[0], "after")
            events = self.server.store.stream_events(stream_id, after, limit)
            self.send_json(200, {"events": events, "next_after": events[-1]["sequence"] if events else after})
        elif path.startswith("/v1/events/"):
            event_id = require_id(path[len("/v1/events/") :], "event_id")
            self.send_json(200, {"event": self.server.store.get_event(event_id)})
        else:
            raise TowerError(404, "not_found", "route not found")

    def _post(self) -> None:
        path = urlsplit(self.path).path
        self.require_auth()
        value = self.read_json()
        if path == "/v1/ingest":
            self.send_json(200, self.server.store.ingest(value))
        elif path == "/v1/findings":
            self.send_json(201, {"finding": self.server.store.create_finding(value)})
        else:
            raise TowerError(404, "not_found", "route not found")

    def require_auth(self) -> None:
        token = self.server.token
        if token is None:
            return
        supplied = self.headers.get("Authorization", "")
        if not hmac.compare_digest(supplied, f"Bearer {token}"):
            raise TowerError(401, "unauthorized", "a valid bearer token is required")

    def read_json(self) -> object:
        try:
            length = int(self.headers.get("Content-Length", "0"))
        except ValueError as error:
            raise TowerError(400, "invalid_request", "Content-Length is invalid") from error
        if length < 1 or length > MAX_BODY_BYTES:
            raise TowerError(413, "body_too_large", f"request body must be 1-{MAX_BODY_BYTES} bytes")
        try:
            return json.loads(self.rfile.read(length).decode("utf-8"))
        except (UnicodeDecodeError, json.JSONDecodeError) as error:
            raise TowerError(400, "invalid_json", "request body is not valid JSON") from error

    @staticmethod
    def page_limit(query: dict[str, list[str]]) -> int:
        return min(MAX_PAGE_SIZE, max(1, TowerHandler.nonnegative_int(query.get("limit", ["100"])[0], "limit")))

    @staticmethod
    def nonnegative_int(value: str, field: str) -> int:
        try:
            parsed = int(value)
        except ValueError as error:
            raise TowerError(400, "invalid_request", f"{field} must be an integer") from error
        if parsed < 0:
            raise TowerError(400, "invalid_request", f"{field} must not be negative")
        return parsed

    def send_file(self, path: Path, media_type: str, vary: bool = False) -> None:
        try:
            body = path.read_bytes()
        except FileNotFoundError as error:
            raise TowerError(404, "not_found", "asset not found") from error
        self.send_response(200)
        self.send_header("Content-Type", media_type)
        self.send_header("Content-Length", str(len(body)))
        self.security_headers()
        if vary:
            self.send_header("Vary", "Accept, User-Agent")
        self.end_headers()
        self.wfile.write(body)

    def send_json(self, status: int, value: object) -> None:
        body = canonical_json(value)
        self.send_response(status)
        self.send_header("Content-Type", "application/json; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.security_headers()
        self.end_headers()
        self.wfile.write(body)

    def send_error_json(self, error: TowerError) -> None:
        self.send_json(error.status, {"error": {"code": error.code, "message": error.message}})

    def security_headers(self) -> None:
        self.send_header("X-Content-Type-Options", "nosniff")
        self.send_header("Content-Security-Policy", "default-src 'self'; connect-src 'self'; style-src 'self'; script-src 'self'")
        self.send_header("Referrer-Policy", "no-referrer")

    def log_message(self, format: str, *args: object) -> None:
        print(f"tower: {format % args}")


class TowerServer(ThreadingHTTPServer):
    def __init__(self, address: tuple[str, int], store: TowerStore, token: str | None) -> None:
        super().__init__(address, TowerHandler)
        self.store = store
        self.token = token


def parse_listen(value: str) -> tuple[str, int]:
    host, separator, port = value.rpartition(":")
    if not separator or not host:
        raise ValueError("TOWER_LISTEN must use HOST:PORT")
    return host, int(port)


def reject_unknown(value: dict[str, object], allowed: set[str]) -> None:
    unknown = sorted(set(value) - allowed)
    if unknown:
        raise TowerError(400, "unknown_field", f"unknown field: {unknown[0]}")


def main() -> None:
    parser = argparse.ArgumentParser(description="TOWER evidence archive")
    parser.add_argument("--listen", default=os.environ.get("TOWER_LISTEN", "127.0.0.1:9460"))
    parser.add_argument("--data", default=os.environ.get("TOWER_DATA_DIR", ".treer/apps/tower"))
    args = parser.parse_args()
    token = os.environ.get("TOWER_TOKEN") or None
    server = TowerServer(parse_listen(args.listen), TowerStore(Path(args.data) / "tower.sqlite"), token)
    print(f"TOWER listening on http://{args.listen}")
    server.serve_forever()


if __name__ == "__main__":
    main()
