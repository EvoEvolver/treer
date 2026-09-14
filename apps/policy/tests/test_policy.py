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
SERVER = APP_ROOT / "policy.py"


class PolicyServerTest(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.port = free_port()
        environment = os.environ.copy()
        environment.update(
            {
                "POLICY_LISTEN": f"127.0.0.1:{self.port}",
                "POLICY_DATA_FILE": str(Path(self.temporary.name) / "policy.json"),
                "TREER_WORKSPACE_ID": "ws_test",
            }
        )
        for name in ("TREER_AGENT_SERVER_URL", "TREER_AGENT_ID", "TREER_WORKLOAD_CREDENTIAL"):
            environment.pop(name, None)
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
        body: object | None = None,
        headers: dict[str, str] | None = None,
    ) -> tuple[int, dict[str, str], object]:
        connection = http.client.HTTPConnection("127.0.0.1", self.port, timeout=5)
        request_headers = dict(headers or {})
        encoded = None
        if body is not None:
            encoded = json.dumps(body)
            request_headers["Content-Type"] = "application/json"
        connection.request(method, path, encoded, request_headers)
        response = connection.getresponse()
        raw = response.read()
        response_headers = {name.lower(): value for name, value in response.getheaders()}
        connection.close()
        value = json.loads(raw) if "json" in response_headers.get("content-type", "") else raw.decode()
        return response.status, response_headers, value

    def test_root_manifest_and_bundle_contract(self) -> None:
        status, headers, manual = self.request("GET", "/")
        self.assertEqual(status, 200)
        self.assertEqual(headers["content-type"], "text/markdown; charset=utf-8")
        self.assertIn("# Treer Policy", manual)

        status, headers, page = self.request("GET", "/", headers={"Accept": "text/html"})
        self.assertEqual(status, 200)
        self.assertEqual(headers["vary"], "Accept, User-Agent")
        self.assertIn("<title>Treer Policy</title>", page)
        self.assertIn('src="./app.js"', page)

        status, _, manifest = self.request("GET", "/v1/manifest")
        self.assertEqual(status, 200)
        self.assertEqual(manifest["protocol"], "treer.policy-provider/v1")
        self.assertIn("policy.bundle.v1", manifest["capabilities"])

        status, _, bundle = self.request("GET", "/v1/policy/bundle?workspace_id=ws_test")
        self.assertEqual(status, 200)
        self.assertEqual(bundle["workspace_id"], "ws_test")
        self.assertEqual(bundle["revision"], 1)
        self.assertEqual(bundle["mode"], "monitor")

        status, _, error = self.request("GET", "/v1/policy/bundle?workspace_id=other")
        self.assertEqual(status, 404)
        self.assertEqual(error["error"]["code"], "workspace_not_found")

    def test_publish_is_optimistic_persistent_and_notifies_best_effort(self) -> None:
        document = {
            "schema_version": 1,
            "defaults": {"agent.prompt": "deny"},
            "groups": {},
            "rules": [{"id": "allow all reads", "priority": 10, "effect": "allow", "subjects": [{}], "actions": ["agent.metadata.read"], "resources": [{}]}],
        }
        status, _, result = self.request(
            "POST",
            "/v1/policy/publish",
            {"expected_revision": 1, "mode": "enforce", "document": document},
        )
        self.assertEqual(status, 200, result)
        self.assertEqual(result["bundle"]["revision"], 2)
        self.assertEqual(result["bundle"]["mode"], "enforce")
        self.assertEqual(result["proxy_sync"]["status"], "unavailable")

        status, _, conflict = self.request(
            "POST",
            "/v1/policy/publish",
            {"expected_revision": 1, "mode": "monitor", "document": document},
        )
        self.assertEqual(status, 409)
        self.assertEqual(conflict["error"]["code"], "policy_revision_conflict")

        status, _, bundle = self.request("GET", "/v1/policy/bundle?workspace_id=ws_test")
        self.assertEqual(status, 200)
        self.assertEqual(bundle["revision"], 2)
        self.assertEqual(bundle["document"], document)

    def test_simulator_observes_monitor_and_enforce_modes(self) -> None:
        request = {
            "subject": {"kind": "agent", "id": "agent_a", "machine_id": "machine_a"},
            "action": "agent.prompt",
            "resource": {"kind": "agent", "id": "agent_b"},
        }
        status, _, initial = self.request("POST", "/v1/policy/test", request)
        self.assertEqual(status, 200)
        self.assertEqual(initial["decision"], "allow")

        document = {"schema_version": 1, "defaults": {"agent.prompt": "deny"}, "groups": {}, "rules": []}
        self.request("POST", "/v1/policy/publish", {"expected_revision": 1, "mode": "enforce", "document": document})
        status, _, result = self.request("POST", "/v1/policy/test", request)
        self.assertEqual(status, 200)
        self.assertEqual(result["effect"], "deny")
        self.assertEqual(result["decision"], "deny")


def free_port() -> int:
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        return listener.getsockname()[1]


def wait_for_health(port: int, process: subprocess.Popen[str]) -> None:
    deadline = time.monotonic() + 5
    while time.monotonic() < deadline:
        if process.poll() is not None:
            stdout, stderr = process.communicate()
            raise AssertionError(f"Policy server exited:\n{stdout}\n{stderr}")
        try:
            connection = http.client.HTTPConnection("127.0.0.1", port, timeout=0.2)
            connection.request("GET", "/health")
            if connection.getresponse().status == 200:
                connection.close()
                return
        except OSError:
            time.sleep(0.05)
    raise AssertionError("Policy server did not become healthy")


if __name__ == "__main__":
    unittest.main()
