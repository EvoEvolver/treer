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
SERVER = APP_ROOT / "tower.py"
sys.path.insert(0, str(APP_ROOT))
from tower import canonical_json, prefix_node_id, sha256  # noqa: E402


class TowerTest(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.port = free_port()
        environment = os.environ.copy()
        environment.update({"TOWER_LISTEN": f"127.0.0.1:{self.port}", "TOWER_TOKEN": "test-token"})
        self.process = subprocess.Popen(
            [sys.executable, str(SERVER), "--data", self.temporary.name],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            env=environment,
        )
        wait_for_health(self.port, self.process)

    def tearDown(self) -> None:
        self.process.terminate()
        self.process.communicate(timeout=5)
        self.temporary.cleanup()

    def request(self, method: str, path: str, value: object | None = None, token: bool = True, headers=None):
        body = None if value is None else canonical_json(value)
        request_headers = dict(headers or {})
        if body is not None:
            request_headers["Content-Type"] = "application/json"
        if token:
            request_headers["Authorization"] = "Bearer test-token"
        connection = http.client.HTTPConnection("127.0.0.1", self.port, timeout=3)
        connection.request(method, path, body=body, headers=request_headers)
        response = connection.getresponse()
        data = response.read()
        result_headers = dict(response.getheaders())
        connection.close()
        parsed = json.loads(data) if result_headers.get("Content-Type", "").startswith("application/json") else data.decode()
        return response.status, result_headers, parsed

    def batch(self):
        payload1 = {"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}}
        hash1 = sha256(canonical_json(payload1))
        node1 = prefix_node_id(None, "client_to_agent", hash1)
        payload2 = {"jsonrpc": "2.0", "id": 1, "result": {"protocolVersion": 1}}
        hash2 = sha256(canonical_json(payload2))
        node2 = prefix_node_id(node1, "agent_to_client", hash2)
        return {
            "schema_version": 1,
            "stream": {"stream_id": "stream_test", "collector_id": "collector_test", "agent_id": "agent_test"},
            "events": [
                {"event_id": "event_1", "sequence": 1, "node_id": node1, "parent_id": None,
                 "payload_hash": hash1, "payload": payload1, "direction": "client_to_agent",
                 "method": "initialize", "rpc_id": "1", "occurred_at": "2026-09-05T00:00:00Z"},
                {"event_id": "event_2", "sequence": 2, "node_id": node2, "parent_id": node1,
                 "payload_hash": hash2, "payload": payload2, "direction": "agent_to_client",
                 "method": None, "rpc_id": "1", "occurred_at": "2026-09-05T00:00:01Z"},
            ],
        }

    def test_root_negotiation_and_relative_assets(self):
        status, headers, body = self.request("GET", "/", token=False)
        self.assertEqual(status, 200)
        self.assertTrue(headers["Content-Type"].startswith("text/markdown"))
        self.assertIn("# TOWER", body)
        status, _, body = self.request("GET", "/", token=False, headers={"Accept": "text/html"})
        self.assertEqual(status, 200)
        self.assertIn('./app.js', body)
        self.assertEqual(self.request("GET", "/app.js", token=False)[0], 200)

    def test_hash_contract_matches_the_acp_collector(self):
        payload = {"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}}
        payload_hash = sha256(canonical_json(payload))
        self.assertEqual(payload_hash, "2228a4133e56a504b4e3654d1120b4f4adc9a266ec4af9d10cc9958e087ae348")
        self.assertEqual(
            prefix_node_id(None, "client_to_agent", payload_hash),
            "b47d02ad89b92da0dba86c22957d656a6982825085e60265ca02f2231b9f44d7",
        )

    def test_ingest_is_idempotent_and_pages_events(self):
        status, _, result = self.request("POST", "/v1/ingest", self.batch())
        self.assertEqual(status, 200)
        self.assertEqual(result["inserted"], 2)
        status, _, result = self.request("POST", "/v1/ingest", self.batch())
        self.assertEqual(status, 200)
        self.assertEqual(result["deduplicated"], 2)
        status, _, result = self.request("GET", "/v1/streams/stream_test/events?limit=10")
        self.assertEqual(status, 200)
        self.assertEqual([event["event_id"] for event in result["events"]], ["event_1", "event_2"])
        self.assertEqual(result["events"][1]["issuer"], "agent")

    def test_rejects_hash_tampering_and_requires_auth(self):
        batch = self.batch()
        batch["events"][0]["payload"]["method"] = "tampered"
        status, _, result = self.request("POST", "/v1/ingest", batch)
        self.assertEqual(status, 400)
        self.assertEqual(result["error"]["code"], "payload_hash_mismatch")
        self.assertEqual(self.request("POST", "/v1/ingest", self.batch(), token=False)[0], 401)

    def test_finding_commits_to_existing_source_set(self):
        self.request("POST", "/v1/ingest", self.batch())
        finding = {
            "schema_version": 1,
            "kind": "source_check",
            "verdict": "confirmed",
            "severity": "high",
            "uncertainty": 0.2,
            "summary": "The response is linked to the initialization request.",
            "reviewer_id": "reviewer_test",
            "reviewer_version": "v1",
            "sources": ["event_2", "event_1", "event_1"],
        }
        status, _, result = self.request("POST", "/v1/findings", finding)
        self.assertEqual(status, 201)
        self.assertEqual(result["finding"]["sources"], ["event_1", "event_2"])
        self.assertEqual(len(result["finding"]["source_set_root"]), 64)


def free_port() -> int:
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        return listener.getsockname()[1]


def wait_for_health(port: int, process: subprocess.Popen[str]) -> None:
    deadline = time.monotonic() + 10
    while time.monotonic() < deadline:
        if process.poll() is not None:
            stdout, stderr = process.communicate()
            raise RuntimeError(f"TOWER exited early\nstdout: {stdout}\nstderr: {stderr}")
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
    raise RuntimeError("TOWER did not become healthy")


if __name__ == "__main__":
    unittest.main()
