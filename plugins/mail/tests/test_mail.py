from __future__ import annotations

import http.client
import hashlib
import json
import os
import socket
import sqlite3
import subprocess
import sys
import tempfile
import time
import unittest
import urllib.parse
from pathlib import Path


PLUGIN_ROOT = Path(__file__).resolve().parents[1]
MAIL = PLUGIN_ROOT / "mail.py"
MIGRATE = PLUGIN_ROOT / "migrate.py"
FAKE_TREER = PLUGIN_ROOT / "tests/fixtures/fake_treer.py"
FAKE_PSQL = PLUGIN_ROOT / "tests/fixtures/fake_psql.py"
SQLITE_FIXTURE = PLUGIN_ROOT / "tests/fixtures/legacy-mail-v1.sqlite.sql"


def core_message(message_id: str = "msg_inbound") -> dict[str, object]:
    return {
        "schema_version": 1,
        "message_id": message_id,
        "workspace_id": "workspace-a",
        "sender": {"kind": "agent", "id": "agent-a", "name": "Builder"},
        "recipients": [
            {"kind": "human", "id": "user-a", "name": "Owner", "role": "owner"}
        ],
        "context_ids": [],
        "body": "Ready for review",
        "created_at": "2026-08-20T12:00:00Z",
    }


class MailServerTest(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        self.state = self.root / "fake-treer.json"
        self.state.write_text(
            json.dumps(
                {
                    "messages": [core_message()],
                    "deliveries": [
                        {"delivery_id": "dlv_inbound", "message_id": "msg_inbound", "acked": False}
                    ],
                }
            ),
            encoding="utf-8",
        )
        web = self.root / "web"
        web.mkdir()
        (web / "index.html").write_text("<!doctype html><title>Treer Mail</title>", encoding="utf-8")
        self.port = free_port()
        config = self.root / "config.json"
        config.write_text(
            json.dumps(
                {
                    "listen": f"127.0.0.1:{self.port}",
                    "service_id": "svc_mail",
                    "public_url": f"http://127.0.0.1:{self.port}/",
                    "proxy_public_url": "https://proxy.example/",
                    "web_dir": str(web),
                }
            ),
            encoding="utf-8",
        )
        FAKE_TREER.chmod(0o755)
        environment = os.environ.copy()
        environment.update(
            {
                "TREER_PLUGIN_CONFIG": str(config),
                "TREER_PLUGIN_STATE_DIR": str(self.root / "plugin-state"),
                "TREER_CLI": str(FAKE_TREER),
                "FAKE_TREER_STATE": str(self.state),
            }
        )
        self.process = subprocess.Popen(
            [sys.executable, str(MAIL)],
            env=environment,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        wait_for_health(self.port, self.process)
        self.cookie = ""

    def tearDown(self) -> None:
        self.process.terminate()
        try:
            self.process.communicate(timeout=5)
        except subprocess.TimeoutExpired:
            self.process.kill()
            self.process.communicate(timeout=5)
        self.temporary.cleanup()

    def request(
        self, method: str, path: str, body: dict[str, object] | None = None
    ) -> tuple[int, dict[str, str], object | None]:
        connection = http.client.HTTPConnection("127.0.0.1", self.port, timeout=5)
        headers = {"Cookie": self.cookie} if self.cookie else {}
        encoded = None
        if body is not None:
            encoded = json.dumps(body)
            headers["Content-Type"] = "application/json"
        connection.request(method, path, body=encoded, headers=headers)
        response = connection.getresponse()
        raw = response.read()
        response_headers = {key.lower(): value for key, value in response.getheaders()}
        connection.close()
        value = json.loads(raw) if raw else None
        return response.status, response_headers, value

    def login(self) -> None:
        status, headers, _ = self.request("GET", "/api/auth/start?return_to=%2Finbox")
        self.assertEqual(status, 302)
        authorize = urllib.parse.urlsplit(headers["location"])
        state = urllib.parse.parse_qs(authorize.query)["state"][0]
        status, headers, _ = self.request(
            "GET", f"/api/auth/callback?code=fake-code&state={urllib.parse.quote(state)}"
        )
        self.assertEqual(status, 302)
        self.assertEqual(headers["location"], "/inbox")
        self.assertIn("HttpOnly", headers["set-cookie"])
        self.cookie = headers["set-cookie"].split(";", 1)[0]

    def test_browser_contract_uses_only_cli_and_preserves_mail_shapes(self) -> None:
        status, _, value = self.request("GET", "/api/health")
        self.assertEqual((status, value), (200, {"status": "ok", "service": "treer-mail"}))
        status, _, _ = self.request("GET", "/api/auth/session")
        self.assertEqual(status, 401)

        self.login()
        status, _, session = self.request("GET", "/api/auth/session")
        self.assertEqual(status, 200)
        self.assertEqual(session["workspace_id"], "workspace-a")
        self.assertEqual(session["user"]["id"], "user-a")

        status, _, directory = self.request("GET", "/api/directory")
        self.assertEqual(status, 200)
        self.assertEqual(
            {(item["kind"], item["id"]) for item in directory["principals"]},
            {("agent", "agent-a"), ("human", "user-a"), ("human", "user-b")},
        )

        status, _, mailbox = self.request("GET", "/api/messages?limit=100")
        self.assertEqual(status, 200)
        self.assertEqual(mailbox["deliveries"][0]["message"]["message_id"], "msg_inbound")
        self.assertTrue(mailbox["deliveries"][0]["unread"])
        self.assertEqual(mailbox["remaining_unread"], 0)

        body = "Review complete\nNo secrets in argv."
        status, _, sent = self.request(
            "POST",
            "/api/messages",
            {"recipients": ["agent-a"], "context_ids": ["msg_inbound"], "body": body},
        )
        self.assertEqual(status, 200)
        self.assertEqual(sent["message"]["context_ids"], ["msg_inbound"])
        self.assertEqual(sent["message"]["body"], body)
        state = json.loads(self.state.read_text(encoding="utf-8"))
        send_call = next(call for call in reversed(state["calls"]) if call["argv"][:2] == ["message", "send"])
        self.assertNotIn(body, send_call["argv"])
        self.assertEqual(send_call["stdin"], body)
        self.assertTrue(send_call["human_session"])

        state["messages"].append(core_message("msg_second"))
        state["deliveries"].append(
            {"delivery_id": "dlv_second", "message_id": "msg_second", "acked": False}
        )
        self.state.write_text(json.dumps(state), encoding="utf-8")
        status, _, inbox = self.request("POST", "/api/inbox", {"limit": 50})
        self.assertEqual(status, 200)
        self.assertEqual([item["message"]["message_id"] for item in inbox["deliveries"]], ["msg_second"])
        self.assertEqual(inbox["remaining_unread"], 0)

        status, _, _ = self.request("POST", "/api/auth/logout", {})
        self.assertEqual(status, 204)
        status, _, _ = self.request("GET", "/api/auth/session")
        self.assertEqual(status, 401)


class MigrationTest(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        self.database = self.root / "legacy.sqlite3"
        connection = sqlite3.connect(self.database)
        connection.executescript(SQLITE_FIXTURE.read_text(encoding="utf-8"))
        connection.close()
        self.fake_state = self.root / "fake-treer.json"
        self.fake_state.write_text("{}", encoding="utf-8")
        FAKE_TREER.chmod(0o755)
        FAKE_PSQL.chmod(0o755)

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def run_migration(self, *arguments: str, environment: dict[str, str] | None = None) -> subprocess.CompletedProcess[str]:
        merged = os.environ.copy()
        merged["FAKE_TREER_STATE"] = str(self.fake_state)
        if environment:
            merged.update(environment)
        return subprocess.run(
            [sys.executable, str(MIGRATE), *arguments],
            capture_output=True,
            text=True,
            env=merged,
            timeout=30,
            check=False,
        )

    def test_sqlite_migration_is_topological_restartable_and_complete(self) -> None:
        report = self.root / "report.json"
        export = self.root / "export.jsonl"
        common = (
            "--source",
            str(self.database),
            "--workspace",
            "workspace-a",
            "--actor",
            "test-owner",
            "--treer",
            str(FAKE_TREER),
            "--batch-size",
            "2",
            "--report",
            str(report),
            "--export-file",
            str(export),
        )
        completed = self.run_migration(*common)
        self.assertEqual(completed.returncode, 0, completed.stderr)
        value = json.loads(report.read_text(encoding="utf-8"))
        self.assertTrue(value["completed"])
        self.assertEqual(value["schema_version"], 2)
        self.assertEqual(value["actor"], "test-owner")
        self.assertEqual(value["source_sha256_scope"], "database_file")
        self.assertEqual(
            value["source_sha256"], hashlib.sha256(self.database.read_bytes()).hexdigest()
        )
        self.assertEqual(value["message_count"], 4)
        self.assertEqual(value["context_edge_count"], 4)
        self.assertEqual(value["delivery_count"], 5)
        self.assertEqual(value["read_delivery_count"], 2)
        self.assertTrue(value["requires_human_relogin"])
        self.assertEqual(value["completed_batch_count"], 2)
        self.assertEqual(
            value["target_counts"],
            {
                "processed_messages": 4,
                "imported_messages": 4,
                "existing_messages": 0,
            },
        )
        self.assertIn("started_at", value)
        self.assertIn("completed_at", value)
        self.assertTrue(
            all("started_at" in batch and "completed_at" in batch for batch in value["batches"])
        )
        records = [json.loads(line) for line in export.read_text(encoding="utf-8").splitlines()]
        self.assertEqual(
            [record["message_id"] for record in records],
            ["legacy_root", "legacy_branch_a", "legacy_branch_b", "legacy_merge"],
        )
        state = json.loads(self.fake_state.read_text(encoding="utf-8"))
        self.assertEqual(len(state["imports"]), 2)
        self.assertEqual(len(state["imported_messages"]), 4)

        repeated = self.run_migration(*common)
        self.assertEqual(repeated.returncode, 0, repeated.stderr)
        repeated_state = json.loads(self.fake_state.read_text(encoding="utf-8"))
        self.assertEqual(len(repeated_state["imports"]), 2)
        self.assertEqual(len(repeated_state["imported_messages"]), 4)
        repeated_report = json.loads(report.read_text(encoding="utf-8"))
        self.assertEqual(repeated_report["resume_count"], 1)

    def test_interrupted_migration_resumes_from_body_free_checkpoint(self) -> None:
        report = self.root / "interrupted-report.json"
        arguments = (
            "--source",
            str(self.database),
            "--workspace",
            "workspace-a",
            "--actor",
            "change-ticket-42",
            "--treer",
            str(FAKE_TREER),
            "--batch-size",
            "2",
            "--report",
            str(report),
        )
        interrupted = self.run_migration(
            *arguments, environment={"FAKE_TREER_FAIL_IMPORT_ONCE_AT": "2"}
        )
        self.assertNotEqual(interrupted.returncode, 0)
        checkpoint = json.loads(report.read_text(encoding="utf-8"))
        self.assertFalse(checkpoint["completed"])
        self.assertEqual(checkpoint["completed_batch_count"], 1)
        self.assertEqual(
            checkpoint["failure"],
            {
                "code": "mail_migration_failed",
                "stage": "import_batch",
                "batch_index": 1,
            },
        )
        encoded_checkpoint = json.dumps(checkpoint)
        for body in ("Root", "Branch A", "Branch B", "Merge"):
            self.assertNotIn(body, encoded_checkpoint)

        resumed = self.run_migration(*arguments)
        self.assertEqual(resumed.returncode, 0, resumed.stderr)
        completed = json.loads(report.read_text(encoding="utf-8"))
        self.assertTrue(completed["completed"])
        self.assertEqual(completed["resume_count"], 1)
        self.assertEqual(completed["completed_batch_count"], 2)
        self.assertNotIn("failure", completed)
        state = json.loads(self.fake_state.read_text(encoding="utf-8"))
        self.assertEqual(state["import_attempts"], 3)
        self.assertEqual(len(state["imports"]), 2)
        self.assertEqual(len(state["imported_messages"]), 4)

    def test_postgres_export_path_uses_structured_psql_json(self) -> None:
        export = self.root / "sqlite-export.jsonl"
        sqlite_result = self.run_migration(
            "--source",
            str(self.database),
            "--workspace",
            "workspace-a",
            "--actor",
            "test-owner",
            "--dry-run",
            "--report",
            str(self.root / "sqlite-report.json"),
            "--export-file",
            str(export),
        )
        self.assertEqual(sqlite_result.returncode, 0, sqlite_result.stderr)
        postgres_result = self.run_migration(
            "--source",
            "postgresql://legacy.invalid/mail",
            "--source-kind",
            "postgres",
            "--workspace",
            "workspace-a",
            "--actor",
            "test-owner",
            "--psql",
            str(FAKE_PSQL),
            "--dry-run",
            "--report",
            str(self.root / "postgres-report.json"),
            environment={"FAKE_PSQL_MESSAGES": str(export)},
        )
        self.assertEqual(postgres_result.returncode, 0, postgres_result.stderr)
        value = json.loads((self.root / "postgres-report.json").read_text(encoding="utf-8"))
        self.assertEqual(value["source_kind"], "postgres")
        self.assertEqual(value["source_sha256_scope"], "workspace_export")
        self.assertEqual(value["message_count"], 4)
        self.assertEqual(value["legacy_sessions"], {"active": 1, "expired": 1})


def free_port() -> int:
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        return int(listener.getsockname()[1])


def wait_for_health(port: int, process: subprocess.Popen[str]) -> None:
    deadline = time.monotonic() + 10
    while time.monotonic() < deadline:
        if process.poll() is not None:
            _stdout, stderr = process.communicate(timeout=1)
            raise AssertionError(f"Mail server exited during startup: {stderr}")
        try:
            connection = http.client.HTTPConnection("127.0.0.1", port, timeout=0.25)
            connection.request("GET", "/api/health")
            response = connection.getresponse()
            response.read()
            connection.close()
            if response.status == 200:
                return
        except OSError:
            pass
        time.sleep(0.05)
    raise AssertionError("Mail server did not become healthy")


if __name__ == "__main__":
    unittest.main()
