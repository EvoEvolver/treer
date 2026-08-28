from __future__ import annotations

import http.client
import json
import os
import socket
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path


APP_ROOT = Path(__file__).resolve().parents[1]
SERVER = APP_ROOT / "gits.py"


class GitsServerTest(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        self.port = free_port()
        self.url = f"http://127.0.0.1:{self.port}"
        environment = os.environ.copy()
        environment.update(
            {
                "GITS_LISTEN": f"127.0.0.1:{self.port}",
                "GITS_PUBLIC_URL": self.url,
                "GITS_DATA_DIR": str(self.root / "data"),
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
        body: dict[str, object] | None = None,
    ) -> tuple[int, dict[str, str], object | None]:
        connection = http.client.HTTPConnection("127.0.0.1", self.port, timeout=10)
        headers = {}
        encoded = None
        if body is not None:
            encoded = json.dumps(body)
            headers["Content-Type"] = "application/json"
        connection.request(method, path, body=encoded, headers=headers)
        response = connection.getresponse()
        raw = response.read()
        response_headers = {name.lower(): value for name, value in response.getheaders()}
        connection.close()
        content_type = response_headers.get("content-type", "")
        value = json.loads(raw) if raw and "json" in content_type else raw.decode() if raw else None
        return response.status, response_headers, value

    def create_repository(self, name: str = "example") -> dict[str, object]:
        status, _, value = self.request(
            "POST", "/v1/repos", {"name": name, "description": "Shared work"}
        )
        self.assertEqual(status, 201, value)
        return value["repo"]

    def git(self, *arguments: str, cwd: Path | None = None) -> subprocess.CompletedProcess[str]:
        environment = os.environ.copy()
        environment["NO_PROXY"] = "127.0.0.1,localhost"
        environment["no_proxy"] = "127.0.0.1,localhost"
        return subprocess.run(
            ["git", "-c", "http.proxy=", *arguments],
            cwd=cwd,
            env=environment,
            capture_output=True,
            text=True,
            timeout=30,
            check=False,
        )

    def test_agent_index_json_api_and_human_ui_are_separate(self) -> None:
        status, headers, manual = self.request("GET", "/")
        self.assertEqual(status, 200)
        self.assertEqual(headers["content-type"], "text/markdown; charset=utf-8")
        self.assertIn("# Gits", manual)
        self.assertIn("git clone http://gits.internal", manual)
        self.assertNotIn("<!doctype html>", manual.lower())

        status, headers, page = self.request("GET", "/_human/")
        self.assertEqual(status, 200)
        self.assertEqual(headers["content-type"], "text/html; charset=utf-8")
        self.assertIn("<title>Gits</title>", page)
        self.assertIn('src="./app.js"', page)

        status, _, script = self.request("GET", "/_human/app.js")
        self.assertEqual(status, 200)
        self.assertIn("function applicationUrl(path)", script)
        self.assertIn("fetch(applicationUrl(path)", script)

        status, headers, value = self.request("GET", "/v1/repos")
        self.assertEqual(status, 200)
        self.assertEqual(headers["content-type"], "application/json; charset=utf-8")
        self.assertEqual(value, {"repos": []})

        status, headers, value = self.request("GET", "/missing")
        self.assertEqual(status, 404)
        self.assertEqual(headers["content-type"], "application/json; charset=utf-8")
        self.assertEqual(value["error"]["code"], "not_found")

    def test_repository_creation_validates_names_and_conflicts(self) -> None:
        repository = self.create_repository("shared-tools")
        self.assertEqual(repository["name"], "shared-tools")
        self.assertEqual(repository["default_branch"], "main")
        self.assertEqual(repository["branch_count"], 0)

        status, _, value = self.request(
            "POST", "/v1/repos", {"name": "shared-tools", "description": "Again"}
        )
        self.assertEqual(status, 409)
        self.assertEqual(value["error"]["code"], "repository_exists")

        status, _, value = self.request("POST", "/v1/repos", {"name": "../escape"})
        self.assertEqual(status, 400)
        self.assertEqual(value["error"]["code"], "invalid_repository_name")
        self.assertFalse((self.root / "escape.git").exists())

    def test_git_clone_push_and_second_clone_round_trip(self) -> None:
        repository = self.create_repository()
        first = self.root / "first"
        clone = self.git("clone", repository["clone_url"], str(first))
        self.assertEqual(clone.returncode, 0, clone.stderr)

        self.assertEqual(self.git("config", "user.name", "Gits Test", cwd=first).returncode, 0)
        self.assertEqual(self.git("config", "user.email", "gits@example.test", cwd=first).returncode, 0)
        (first / "README.md").write_text("# Example\n\nShared over Gits.\n", encoding="utf-8")
        self.assertEqual(self.git("add", "README.md", cwd=first).returncode, 0)
        commit = self.git("commit", "-m", "Initial commit", cwd=first)
        self.assertEqual(commit.returncode, 0, commit.stderr)
        push = self.git("push", "origin", "HEAD:main", cwd=first)
        self.assertEqual(push.returncode, 0, push.stderr)

        second = self.root / "second"
        second_clone = self.git("clone", repository["clone_url"], str(second))
        self.assertEqual(second_clone.returncode, 0, second_clone.stderr)
        self.assertEqual((second / "README.md").read_text(encoding="utf-8"), "# Example\n\nShared over Gits.\n")

        status, _, value = self.request("GET", "/v1/repos/example")
        self.assertEqual(status, 200)
        detail = value["repo"]
        self.assertEqual(detail["branch_count"], 1)
        self.assertEqual(detail["branches"][0]["name"], "main")
        self.assertEqual(detail["recent_commits"][0]["subject"], "Initial commit")


def free_port() -> int:
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        return listener.getsockname()[1]


def wait_for_health(port: int, process: subprocess.Popen[str]) -> None:
    deadline = time.monotonic() + 10
    while time.monotonic() < deadline:
        if process.poll() is not None:
            stdout, stderr = process.communicate()
            raise RuntimeError(f"Gits exited early\nstdout: {stdout}\nstderr: {stderr}")
        try:
            connection = http.client.HTTPConnection("127.0.0.1", port, timeout=1)
            connection.request("GET", "/health")
            response = connection.getresponse()
            response.read()
            connection.close()
            if response.status == 200:
                return
        except OSError:
            time.sleep(0.05)
    raise RuntimeError("Gits did not become healthy")


if __name__ == "__main__":
    unittest.main()
