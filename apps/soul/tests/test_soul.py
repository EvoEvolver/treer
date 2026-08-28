from __future__ import annotations

import http.client
import json
import os
import subprocess
import sys
import tarfile
import tempfile
import time
import unittest
from pathlib import Path


APP_ROOT = Path(__file__).resolve().parents[1]
SERVER = APP_ROOT / "soul.py"
CLIENT = APP_ROOT / "client.py"


class SoulServerTest(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        self.port = free_port()
        self.fake_treer = self.root / "fake-treer.py"
        self.treer_argv = self.root / "treer-argv.json"
        self.fake_treer.write_text(
            """#!/usr/bin/env python3
import json, os, sys
json.dump(sys.argv[1:], open(os.environ['FAKE_TREER_ARGV'], 'w'))
print(json.dumps({'agent_id': 'ag_reborn', 'name': 'reborn', 'status': 'running'}))
""",
            encoding="utf-8",
        )
        self.fake_treer.chmod(0o755)
        environment = os.environ.copy()
        environment.update(
            {
                "SOUL_LISTEN": f"127.0.0.1:{self.port}",
                "SOUL_PUBLIC_URL": f"http://127.0.0.1:{self.port}",
                "SOUL_DATA_DIR": str(self.root / "data"),
                "TREER_BIN": str(self.fake_treer),
                "FAKE_TREER_ARGV": str(self.treer_argv),
            }
        )
        self.process = subprocess.Popen(
            [sys.executable, str(SERVER)],
            env=environment,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        wait_for_health(self.port, self.process)

    def tearDown(self) -> None:
        self.process.terminate()
        try:
            self.process.communicate(timeout=5)
        except subprocess.TimeoutExpired:
            self.process.kill()
            self.process.communicate(timeout=5)
        self.temporary.cleanup()

    def request(
        self,
        method: str,
        path: str,
        body: bytes | None = None,
        content_type: str | None = None,
    ) -> tuple[int, bytes]:
        connection = http.client.HTTPConnection("127.0.0.1", self.port, timeout=5)
        headers = {"Content-Type": content_type} if content_type else {}
        connection.request(method, path, body=body, headers=headers)
        response = connection.getresponse()
        payload = response.read()
        status = response.status
        connection.close()
        return status, payload

    def soul_archive(self, manifest: dict[str, object] | None = None) -> bytes:
        source = self.root / f"archive-{time.time_ns()}"
        source.mkdir()
        (source / "manifest.json").write_text(
            json.dumps(
                manifest
                or {
                    "schema_version": 1,
                    "name": "Test soul",
                    "environment": {"SOUL_TEST_FILE": "files/state.txt"},
                }
            ),
            encoding="utf-8",
        )
        (source / "state.txt").write_text("remember me\n", encoding="utf-8")
        archive_path = source / "soul.tar"
        with tarfile.open(archive_path, "w") as archive:
            archive.add(source / "manifest.json", arcname="manifest.json")
            archive.add(source / "state.txt", arcname="files/state.txt")
        return archive_path.read_bytes()

    def upload(self) -> dict[str, object]:
        status, body = self.request(
            "POST", "/v1/souls", self.soul_archive(), "application/x-tar"
        )
        self.assertEqual(status, 201, body)
        return json.loads(body)

    def test_upload_list_download_and_run(self) -> None:
        soul = self.upload()
        soul_id = soul["soul_id"]
        status, body = self.request("GET", "/v1/souls")
        self.assertEqual(status, 200)
        self.assertEqual(json.loads(body)["souls"][0]["soul_id"], soul_id)
        command = [
            sys.executable,
            str(CLIENT),
            "--server",
            f"http://127.0.0.1:{self.port}",
            "run",
            soul_id,
            "--",
            sys.executable,
            "-c",
            "import os; print(open(os.environ['SOUL_TEST_FILE']).read().strip())",
        ]
        result = subprocess.run(command, capture_output=True, text=True, timeout=10, check=False)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), "remember me")

    def test_installer_embeds_url_and_verified_client(self) -> None:
        status, body = self.request("GET", "/install.sh")
        self.assertEqual(status, 200)
        script = body.decode()
        self.assertIn(f"base_url=http://127.0.0.1:{self.port}", script)
        self.assertIn("checksum mismatch", script)
        self.assertIn("installed treer-soul", script)

    def test_root_is_agent_manual_and_human_ui_is_read_only(self) -> None:
        status, body = self.request("GET", "/")
        self.assertEqual(status, 200)
        manual = body.decode()
        self.assertIn("This index is for Agents", manual)
        self.assertIn("treer-soul capture-codex", manual)
        self.assertIn("/_human/", manual)
        self.assertNotIn("<!doctype html>", manual.lower())

        status, body = self.request("GET", "/_human/")
        self.assertEqual(status, 200)
        page = body.decode()
        self.assertIn("Treer Soul", page)
        self.assertIn('src="/_human/app.js"', page)
        self.assertNotIn("Upload", page)
        self.assertNotIn("Incarnate", page)

        status, script = self.request("GET", "/_human/app.js")
        self.assertEqual(status, 200)
        source = script.decode()
        self.assertIn('fetch("/v1/souls"', source)
        self.assertNotIn('method: "POST"', source)

        status, stylesheet = self.request("GET", "/_human/app.css")
        self.assertEqual(status, 200)
        self.assertIn(b".workspace", stylesheet)

    def test_codex_capture_restores_rollout_and_uses_supported_resume_command(self) -> None:
        session_id = "01234567-89ab-cdef-0123-456789abcdef"
        source_home = self.root / "source-codex"
        session_relative = Path("2026/08/24") / f"rollout-test-{session_id}.jsonl"
        source_session = source_home / "sessions" / session_relative
        source_session.parent.mkdir(parents=True)
        source_session.write_text(
            json.dumps(
                {
                    "type": "session_meta",
                    "payload": {
                        "id": session_id,
                        "cwd": str(self.root),
                        "cli_version": "0.149.0",
                    },
                }
            )
            + "\n",
            encoding="utf-8",
        )
        shell_dir = source_home / "shell_snapshots"
        shell_dir.mkdir()
        (shell_dir / f"{session_id}.123.sh").write_text("export SOUL_TEST=1\n", encoding="utf-8")
        capture_environment = os.environ.copy()
        capture_environment["CODEX_HOME"] = str(source_home)
        capture = subprocess.run(
            [
                sys.executable,
                str(CLIENT),
                "--server",
                f"http://127.0.0.1:{self.port}",
                "capture-codex",
                "--session",
                session_id,
                "--include-shell-snapshot",
            ],
            cwd=self.root,
            env=capture_environment,
            capture_output=True,
            text=True,
            timeout=10,
            check=False,
        )
        self.assertEqual(capture.returncode, 0, capture.stderr)
        soul_id = json.loads(capture.stdout)["soul_id"]

        target_home = self.root / "target-codex"
        fake_codex = self.root / "fake-codex.py"
        fake_codex.write_text(
            "#!/usr/bin/env python3\nimport json, sys\nprint(json.dumps(sys.argv[1:]))\n",
            encoding="utf-8",
        )
        fake_codex.chmod(0o755)
        resume_environment = os.environ.copy()
        resume_environment.update(
            {
                "CODEX_HOME": str(target_home),
                "CODEX_BIN": str(fake_codex),
                "TREER_SOUL_STATE_DIR": str(self.root / "incarnation-state"),
            }
        )
        resume = subprocess.run(
            [
                sys.executable,
                str(CLIENT),
                "--server",
                f"http://127.0.0.1:{self.port}",
                "run",
                soul_id,
            ],
            cwd=self.root,
            env=resume_environment,
            capture_output=True,
            text=True,
            timeout=10,
            check=False,
        )
        self.assertEqual(resume.returncode, 0, resume.stderr)
        resume_args = json.loads(resume.stdout)
        self.assertEqual(resume_args[:2], ["resume", session_id])
        self.assertIn("--dangerously-bypass-approvals-and-sandbox", resume_args)
        restored = target_home / "sessions" / session_relative
        self.assertEqual(restored.read_text(encoding="utf-8"), source_session.read_text(encoding="utf-8"))
        self.assertTrue((target_home / "shell_snapshots" / f"{session_id}.123.sh").is_file())

    def test_incarnation_invokes_treer_without_shelling_user_fields(self) -> None:
        soul = self.upload()
        result = subprocess.run(
            [
                sys.executable,
                str(CLIENT),
                "--server",
                f"http://127.0.0.1:{self.port}",
                "incarnate",
                soul["soul_id"],
                "--machine",
                "build-machine",
                "--name",
                "reborn",
                "--cwd",
                "project",
                "--",
                "sh",
                "-c",
                "cat \"$SOUL_TEST_FILE\"",
            ],
            capture_output=True,
            text=True,
            timeout=10,
            check=False,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(result.stdout)["agent"]["agent_id"], "ag_reborn")
        argv = json.loads(self.treer_argv.read_text(encoding="utf-8"))
        self.assertEqual(argv[:8], ["agent", "admin", "create", "--machine", "build-machine", "--kind", "command", "--name"])
        self.assertIn("cat \"$SOUL_TEST_FILE\"", argv)

    def test_rejects_path_traversal(self) -> None:
        archive_path = self.root / "unsafe.tar"
        payload = self.root / "payload"
        payload.write_text("bad", encoding="utf-8")
        with tarfile.open(archive_path, "w") as archive:
            archive.add(payload, arcname="../payload")
        status, body = self.request(
            "POST", "/v1/souls", archive_path.read_bytes(), "application/x-tar"
        )
        self.assertEqual(status, 400)
        self.assertEqual(json.loads(body)["error"]["code"], "invalid_path")

    def test_rejects_protected_environment_bindings(self) -> None:
        status, body = self.request(
            "POST",
            "/v1/souls",
            self.soul_archive(
                {
                    "schema_version": 1,
                    "name": "Unsafe soul",
                    "environment": {"TREER_WORKLOAD_CREDENTIAL": "files/state.txt"},
                }
            ),
            "application/x-tar",
        )
        self.assertEqual(status, 400)
        self.assertEqual(json.loads(body)["error"]["code"], "invalid_environment")


def free_port() -> int:
    import socket

    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        return listener.getsockname()[1]


def wait_for_health(port: int, process: subprocess.Popen[str]) -> None:
    deadline = time.time() + 10
    while time.time() < deadline:
        if process.poll() is not None:
            stdout, stderr = process.communicate()
            raise AssertionError(f"Soul server exited:\n{stdout}\n{stderr}")
        try:
            connection = http.client.HTTPConnection("127.0.0.1", port, timeout=1)
            connection.request("GET", "/health")
            response = connection.getresponse()
            response.read()
            connection.close()
            if response.status == 200:
                return
        except OSError:
            pass
        time.sleep(0.05)
    raise AssertionError("Soul server did not become healthy")


if __name__ == "__main__":
    unittest.main()
