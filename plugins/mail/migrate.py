#!/usr/bin/env python3
"""Export and restartably import legacy Treer Mail data into Core Message."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import sqlite3
import subprocess
import sys
from collections import defaultdict
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Iterable
from urllib.parse import unquote, urlsplit


MAX_BATCH_SIZE = 1_000


class MigrationError(Exception):
    pass


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Migrate legacy Mail Messages into Treer Core through the CLI"
    )
    parser.add_argument("--source", required=True, help="SQLite path/URL or PostgreSQL URL")
    parser.add_argument(
        "--source-kind", choices=("auto", "sqlite", "postgres"), default="auto"
    )
    parser.add_argument("--workspace", required=True)
    parser.add_argument("--treer", default=os.environ.get("TREER_CLI", "treer"))
    parser.add_argument("--url", help="optional local Controller URL for the operator CLI")
    parser.add_argument("--psql", default="psql")
    parser.add_argument("--batch-size", type=int, default=250)
    parser.add_argument("--report", type=Path, default=Path("mail-migration-report.json"))
    parser.add_argument("--export-file", type=Path)
    parser.add_argument("--dry-run", action="store_true")
    return parser.parse_args()


def source_kind(source: str, requested: str) -> str:
    if requested != "auto":
        return requested
    scheme = urlsplit(source).scheme.lower()
    if scheme in {"postgres", "postgresql"}:
        return "postgres"
    if scheme in {"", "file", "sqlite"}:
        return "sqlite"
    raise MigrationError("cannot infer source kind; pass --source-kind")


def sqlite_path(source: str) -> Path:
    parsed = urlsplit(source)
    if parsed.scheme == "":
        return Path(source)
    if parsed.scheme == "file":
        return Path(unquote(parsed.path))
    if parsed.scheme != "sqlite":
        raise MigrationError("SQLite source must be a path, file: URL, or sqlite: URL")
    if parsed.netloc and parsed.path:
        value = f"/{parsed.netloc}{parsed.path}"
    elif parsed.netloc:
        value = parsed.netloc
    else:
        value = parsed.path
    if value.startswith("//"):
        value = value[1:]
    return Path(unquote(value))


def load_sqlite(source: str, workspace: str) -> tuple[list[dict[str, Any]], dict[str, int]]:
    path = sqlite_path(source)
    if not path.is_file():
        raise MigrationError(f"legacy SQLite database does not exist: {path}")
    connection = sqlite3.connect(f"file:{path.resolve()}?mode=ro", uri=True)
    connection.row_factory = sqlite3.Row
    try:
        message_rows = connection.execute(
            """SELECT message_id, workspace_id, sender_kind, sender_id, sender_name,
                      sender_role, body, created_at
               FROM messages WHERE workspace_id = ? ORDER BY created_at, message_id""",
            (workspace,),
        ).fetchall()
        recipients: dict[str, list[sqlite3.Row]] = defaultdict(list)
        for row in connection.execute(
            """SELECT message_id, recipient_kind, recipient_id, recipient_name,
                      recipient_role, position, read_at
               FROM recipients WHERE workspace_id = ? ORDER BY message_id, position""",
            (workspace,),
        ):
            recipients[str(row["message_id"])].append(row)
        message_ids = {str(row["message_id"]) for row in message_rows}
        contexts: dict[str, list[str]] = defaultdict(list)
        for row in connection.execute(
            "SELECT message_id, context_message_id FROM contexts ORDER BY message_id, position"
        ):
            if str(row["message_id"]) in message_ids:
                contexts[str(row["message_id"])].append(str(row["context_message_id"]))
        sessions = {"active": 0, "expired": 0}
        now = datetime.now(timezone.utc).timestamp()
        try:
            rows = connection.execute("SELECT expires_at FROM human_sessions").fetchall()
        except sqlite3.OperationalError:
            rows = []
        for row in rows:
            key = "active" if parse_timestamp(str(row["expires_at"])) > now else "expired"
            sessions[key] += 1
        messages = [
            legacy_message(row, recipients[str(row["message_id"])], contexts[str(row["message_id"])])
            for row in message_rows
        ]
        return messages, sessions
    except sqlite3.DatabaseError as error:
        raise MigrationError(f"failed to read legacy SQLite schema: {error}") from error
    finally:
        connection.close()


POSTGRES_MESSAGES_SQL = r"""
SELECT json_build_object(
    'message_id', m.message_id,
    'workspace_id', m.workspace_id,
    'sender', json_build_object(
        'kind', m.sender_kind,
        'id', m.sender_id,
        'name', m.sender_name,
        'role', m.sender_role
    ),
    'recipients', COALESCE((
        SELECT json_agg(json_build_object(
            'principal', json_build_object(
                'kind', r.recipient_kind,
                'id', r.recipient_id,
                'name', r.recipient_name,
                'role', r.recipient_role
            ),
            'position', r.position,
            'read_at', r.read_at
        ) ORDER BY r.position)
        FROM recipients r
        WHERE r.workspace_id = m.workspace_id AND r.message_id = m.message_id
    ), '[]'::json),
    'context_ids', COALESCE((
        SELECT json_agg(c.context_message_id ORDER BY c.position)
        FROM contexts c
        WHERE c.message_id = m.message_id
    ), '[]'::json),
    'body', m.body,
    'created_at', m.created_at
)::text
FROM messages m
WHERE m.workspace_id = :'workspace'
ORDER BY m.created_at, m.message_id;
"""

POSTGRES_SESSIONS_SQL = r"""
SELECT json_build_object(
    'active', COUNT(*) FILTER (WHERE expires_at::timestamptz > now()),
    'expired', COUNT(*) FILTER (WHERE expires_at::timestamptz <= now())
)::text
FROM human_sessions;
"""


def psql_lines(psql: str, source: str, workspace: str, sql: str) -> list[str]:
    try:
        completed = subprocess.run(
            [
                psql,
                source,
                "--no-psqlrc",
                "--set",
                "ON_ERROR_STOP=1",
                "--set",
                f"workspace={workspace}",
                "--tuples-only",
                "--no-align",
            ],
            input=sql,
            capture_output=True,
            text=True,
            timeout=120,
            check=False,
        )
    except (OSError, subprocess.TimeoutExpired) as error:
        raise MigrationError("psql is unavailable or timed out") from error
    if completed.returncode != 0:
        detail = completed.stderr.strip().splitlines()[-1:] or ["unknown psql error"]
        raise MigrationError(f"psql failed: {detail[0]}")
    return [line for line in completed.stdout.splitlines() if line.strip()]


def load_postgres(
    source: str, workspace: str, psql: str
) -> tuple[list[dict[str, Any]], dict[str, int]]:
    try:
        messages = [json.loads(line) for line in psql_lines(psql, source, workspace, POSTGRES_MESSAGES_SQL)]
        session_lines = psql_lines(psql, source, workspace, POSTGRES_SESSIONS_SQL)
        sessions = json.loads(session_lines[0]) if session_lines else {"active": 0, "expired": 0}
    except (IndexError, TypeError, ValueError) as error:
        raise MigrationError("psql returned invalid legacy Mail JSON") from error
    if not all(isinstance(message, dict) for message in messages) or not isinstance(sessions, dict):
        raise MigrationError("psql returned invalid legacy Mail records")
    return messages, {
        "active": int(sessions.get("active", 0)),
        "expired": int(sessions.get("expired", 0)),
    }


def legacy_message(
    row: sqlite3.Row, recipients: list[sqlite3.Row], contexts: list[str]
) -> dict[str, Any]:
    return {
        "message_id": str(row["message_id"]),
        "workspace_id": str(row["workspace_id"]),
        "sender": {
            "kind": str(row["sender_kind"]),
            "id": str(row["sender_id"]),
            "name": str(row["sender_name"]),
            "role": row["sender_role"],
        },
        "recipients": [
            {
                "principal": {
                    "kind": str(recipient["recipient_kind"]),
                    "id": str(recipient["recipient_id"]),
                    "name": str(recipient["recipient_name"]),
                    "role": recipient["recipient_role"],
                },
                "position": int(recipient["position"]),
                "read_at": recipient["read_at"],
            }
            for recipient in recipients
        ],
        "context_ids": contexts,
        "body": str(row["body"]),
        "created_at": str(row["created_at"]),
    }


def validate_and_order(messages: list[dict[str, Any]], workspace: str) -> list[dict[str, Any]]:
    by_id: dict[str, dict[str, Any]] = {}
    for message in messages:
        message_id = required_string(message.get("message_id"), "message ID", 256)
        if message_id in by_id:
            raise MigrationError(f"duplicate legacy Message ID: {message_id}")
        if message.get("workspace_id") != workspace:
            raise MigrationError(f"Message {message_id} belongs to another workspace")
        required_string(message.get("body"), f"Message {message_id} body", 32 * 1024)
        parse_timestamp(required_string(message.get("created_at"), "created_at", 128))
        principal(message.get("sender"), f"Message {message_id} sender")
        recipients = message.get("recipients")
        contexts = message.get("context_ids", [])
        if not isinstance(recipients, list) or not 1 <= len(recipients) <= 32:
            raise MigrationError(f"Message {message_id} must have 1-32 recipients")
        if not isinstance(contexts, list) or len(contexts) > 32:
            raise MigrationError(f"Message {message_id} has invalid contexts")
        recipient_keys: set[tuple[str, str]] = set()
        positions: set[int] = set()
        for value in recipients:
            if not isinstance(value, dict):
                raise MigrationError(f"Message {message_id} has an invalid recipient")
            recipient_value = principal(value.get("principal"), f"Message {message_id} recipient")
            position = value.get("position")
            if not isinstance(position, int) or isinstance(position, bool) or position < 0:
                raise MigrationError(f"Message {message_id} has an invalid recipient position")
            key = (recipient_value["kind"], recipient_value["id"])
            if key in recipient_keys or position in positions:
                raise MigrationError(f"Message {message_id} has duplicate recipients or positions")
            recipient_keys.add(key)
            positions.add(position)
            if value.get("read_at") is not None:
                parse_timestamp(required_string(value.get("read_at"), "read_at", 128))
        normalized_contexts = [required_string(value, "context ID", 256) for value in contexts]
        if len(set(normalized_contexts)) != len(normalized_contexts):
            raise MigrationError(f"Message {message_id} has duplicate contexts")
        by_id[message_id] = message

    indegree = {message_id: 0 for message_id in by_id}
    children: dict[str, list[str]] = defaultdict(list)
    for message_id, message in by_id.items():
        for parent_id in message.get("context_ids", []):
            if parent_id not in by_id:
                raise MigrationError(
                    f"Message {message_id} references missing or cross-workspace context {parent_id}"
                )
            indegree[message_id] += 1
            children[parent_id].append(message_id)
    ready = sorted(
        (message_id for message_id, count in indegree.items() if count == 0),
        key=lambda message_id: (str(by_id[message_id]["created_at"]), message_id),
    )
    ordered: list[dict[str, Any]] = []
    while ready:
        message_id = ready.pop(0)
        ordered.append(by_id[message_id])
        for child in sorted(children[message_id]):
            indegree[child] -= 1
            if indegree[child] == 0:
                ready.append(child)
        ready.sort(key=lambda item: (str(by_id[item]["created_at"]), item))
    if len(ordered) != len(messages):
        raise MigrationError("legacy Message contexts contain a cycle")
    return ordered


def principal(value: Any, label: str) -> dict[str, str | None]:
    if not isinstance(value, dict):
        raise MigrationError(f"{label} is invalid")
    kind = required_string(value.get("kind"), f"{label} kind", 32)
    if kind not in {"agent", "human"}:
        raise MigrationError(f"{label} has unsupported kind {kind}")
    required_string(value.get("id"), f"{label} ID", 256)
    required_string(value.get("name"), f"{label} name", 256)
    role = value.get("role")
    if role is not None:
        required_string(role, f"{label} role", 256)
    return value


def required_string(value: Any, label: str, maximum: int) -> str:
    if not isinstance(value, str) or not value or len(value.encode("utf-8")) > maximum:
        raise MigrationError(f"{label} is empty or too long")
    return value


def parse_timestamp(value: str) -> float:
    try:
        return datetime.fromisoformat(value.replace("Z", "+00:00")).timestamp()
    except ValueError as error:
        raise MigrationError(f"invalid RFC3339 timestamp: {value}") from error


def migration_fingerprint(messages: list[dict[str, Any]]) -> str:
    structural = [
        {
            "message_id": message["message_id"],
            "contexts": message.get("context_ids", []),
            "recipients": [
                {
                    "kind": recipient["principal"]["kind"],
                    "id": recipient["principal"]["id"],
                    "position": recipient["position"],
                    "read": recipient.get("read_at") is not None,
                }
                for recipient in message["recipients"]
            ],
        }
        for message in messages
    ]
    encoded = json.dumps(structural, sort_keys=True, separators=(",", ":")).encode("utf-8")
    return hashlib.sha256(encoded).hexdigest()


def batches(values: list[dict[str, Any]], size: int) -> Iterable[list[dict[str, Any]]]:
    for offset in range(0, len(values), size):
        yield values[offset : offset + size]


def run_import(
    treer: str,
    workspace: str,
    url: str | None,
    operation_id: str,
    messages: list[dict[str, Any]],
) -> dict[str, Any]:
    command = [treer]
    if url:
        command.extend(["--url", url])
    command.extend(
        [
            "--workspace",
            workspace,
            "message",
            "import",
            "--format",
            "legacy-mail-v1",
            "--operation-id",
            operation_id,
            "--body-file",
            "-",
        ]
    )
    try:
        completed = subprocess.run(
            command,
            input=json.dumps(messages, separators=(",", ":")),
            capture_output=True,
            text=True,
            timeout=180,
            check=False,
        )
    except (OSError, subprocess.TimeoutExpired) as error:
        raise MigrationError("treer message import is unavailable or timed out") from error
    if completed.returncode != 0:
        try:
            failure = json.loads(completed.stderr)
            detail = failure.get("error", {}).get("message", "Treer rejected the import")
        except (TypeError, ValueError):
            detail = "Treer rejected the import"
        raise MigrationError(str(detail))
    try:
        response = json.loads(completed.stdout)
    except ValueError as error:
        raise MigrationError("treer message import returned invalid JSON") from error
    if not isinstance(response, dict):
        raise MigrationError("treer message import returned an invalid response")
    expected_ids = [message["message_id"] for message in messages]
    if response.get("message_ids") != expected_ids:
        raise MigrationError("Core import did not return the expected Message IDs in order")
    if int(response.get("imported", -1)) + int(response.get("existing", -1)) != len(messages):
        raise MigrationError("Core import counts do not match the source batch")
    return response


def atomic_json(path: Path, value: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_name(path.name + ".tmp")
    temporary.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    os.replace(temporary, path)


def write_export(path: Path, messages: list[dict[str, Any]]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_name(path.name + ".tmp")
    with temporary.open("w", encoding="utf-8") as handle:
        for message in messages:
            handle.write(json.dumps(message, separators=(",", ":"), ensure_ascii=False) + "\n")
    os.replace(temporary, path)


def main() -> int:
    args = parse_args()
    try:
        if not 1 <= args.batch_size <= MAX_BATCH_SIZE:
            raise MigrationError(f"batch size must be between 1 and {MAX_BATCH_SIZE}")
        if not args.workspace or len(args.workspace) > 256:
            raise MigrationError("workspace ID is empty or too long")
        kind = source_kind(args.source, args.source_kind)
        if kind == "sqlite":
            messages, sessions = load_sqlite(args.source, args.workspace)
        else:
            messages, sessions = load_postgres(args.source, args.workspace, args.psql)
        ordered = validate_and_order(messages, args.workspace)
        checksum = migration_fingerprint(ordered)
        if args.export_file:
            write_export(args.export_file, ordered)
        report: dict[str, Any] = {
            "schema_version": 1,
            "workspace_id": args.workspace,
            "source_kind": kind,
            "structural_sha256": checksum,
            "message_count": len(ordered),
            "context_edge_count": sum(len(message.get("context_ids", [])) for message in ordered),
            "delivery_count": sum(len(message["recipients"]) for message in ordered),
            "read_delivery_count": sum(
                1
                for message in ordered
                for recipient in message["recipients"]
                if recipient.get("read_at") is not None
            ),
            "legacy_sessions": sessions,
            "requires_human_relogin": sessions["active"] > 0,
            "dry_run": bool(args.dry_run),
            "batches": [],
            "completed": bool(args.dry_run or not ordered),
        }
        atomic_json(args.report, report)
        if args.dry_run:
            print(json.dumps(report, indent=2, sort_keys=True))
            return 0
        if os.environ.get("TREER_PLUGIN_BROKER_SOCKET"):
            raise MigrationError("migration must use an operator CLI outside a plugin broker")
        if os.environ.get("TREER_AGENT_ID") or os.environ.get("TREER_WORKLOAD_CREDENTIAL"):
            raise MigrationError("migration requires a local operator identity, not an Agent identity")
        for index, batch in enumerate(batches(ordered, args.batch_size)):
            operation_id = f"mailmig-{checksum[:20]}-{index:06d}"
            response = run_import(
                args.treer, args.workspace, args.url, operation_id, batch
            )
            report["batches"].append(
                {
                    "index": index,
                    "operation_id": operation_id,
                    "message_count": len(batch),
                    "imported": response["imported"],
                    "existing": response["existing"],
                    "first_message_id": batch[0]["message_id"],
                    "last_message_id": batch[-1]["message_id"],
                }
            )
            atomic_json(args.report, report)
        report["completed"] = True
        atomic_json(args.report, report)
        print(json.dumps(report, indent=2, sort_keys=True))
        return 0
    except MigrationError as error:
        print(json.dumps({"error": {"code": "mail_migration_failed", "message": str(error)}}), file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
