"""End-to-end gateway tests against a fake GitHub that runs real `git http-backend`."""

from __future__ import annotations

import http.client
import json
import os
import subprocess
import unittest
import urllib.parse
from pathlib import Path
from typing import Any

from fixture import Environment

ZERO = "0" * 40


class GatewayCase(unittest.TestCase):
    seed: dict[str, Any] = {"trusted_agents": ["ag_leader"]}

    def setUp(self) -> None:
        self.env = Environment(seed=self.seed)
        self.addCleanup(self.env.close)
        self.github = self.env.github_state
        self.upstream = self.github.create_repo("EvoEvolver", "treer")
        self.seed_upstream()

    def seed_upstream(self) -> None:
        work = self.env.root / "seed"
        subprocess.run(["git", "init", "-q", "-b", "main", str(work)], check=True)
        (work / "README.md").write_text("hello\n")
        self.git("add", "README.md", cwd=work)
        self.git("commit", "-qm", "initial", cwd=work)
        self.git("push", "-q", str(self.upstream), "main", cwd=work)

    def git(self, *args: str, cwd: Path | None = None, token: str | None = None, check: bool = True) -> subprocess.CompletedProcess[str]:
        command = ["git", "-c", "user.name=Agent", "-c", "user.email=agent@example.invalid",
                   "-c", "protocol.version=2", "-c", "credential.helper="]
        if token:
            command += ["-c", f"http.extraHeader=X-Treer-Identity-Token: {token}"]
        environment = {**os.environ, "GIT_TERMINAL_PROMPT": "0", "GIT_ASKPASS": "/bin/false"}
        completed = subprocess.run([*command, *args], cwd=cwd, capture_output=True, text=True, env=environment, timeout=120)
        if check and completed.returncode != 0:
            self.fail(f"git {' '.join(args)} failed:\n{completed.stderr}")
        return completed

    def request(self, method: str, path: str, token: str | None = "agent-token", body: Any = None,
                headers: dict[str, str] | None = None) -> tuple[int, dict[str, str], Any]:
        connection = http.client.HTTPConnection("127.0.0.1", self.env.server.server_port, timeout=30)
        request_headers = dict(headers or {})
        if token:
            request_headers["X-Treer-Identity-Token"] = token
        raw = None
        if body is not None:
            raw = json.dumps(body).encode()
            request_headers["Content-Type"] = "application/json"
        connection.request(method, path, body=raw, headers=request_headers)
        response = connection.getresponse()
        data = response.read()
        connection.close()
        response_headers = {k.lower(): v for k, v in response.getheaders()}
        value: Any = data.decode(errors="replace")
        if "json" in response_headers.get("content-type", "") and data:
            value = json.loads(data)
        return response.status, response_headers, value

    @property
    def remote(self) -> str:
        return f"{self.env.url}/github/EvoEvolver/treer.git"

    def clone(self, token: str = "agent-token", name: str = "clone") -> Path:
        target = self.env.root / name
        self.git("clone", "-q", self.remote, str(target), token=token)
        return target

    def commit(self, work: Path, name: str, size: int = 0) -> str:
        (work / name).write_bytes(os.urandom(size) if size else f"{name}\n".encode())
        self.git("add", name, cwd=work)
        self.git("commit", "-qm", f"add {name}", cwd=work)
        return self.git("rev-parse", "HEAD", cwd=work).stdout.strip()

    def upstream_ref(self, ref: str) -> str | None:
        result = subprocess.run(["git", "--git-dir", str(self.upstream), "rev-parse", "-q", "--verify", ref],
                                capture_output=True, text=True)
        return result.stdout.strip() or None

    def audit(self, **filters: str) -> list[dict[str, Any]]:
        status, _, value = self.request("GET", "/api/audit?" + urllib.parse.urlencode({"limit": 50, **filters}))
        self.assertEqual(status, 200)
        return value["entries"]


class GitTests(GatewayCase):
    def test_clone_fetch_and_credential_injection(self) -> None:
        work = self.clone()
        self.assertEqual((work / "README.md").read_text(), "hello\n")
        git_requests = [r for r in self.github.requests if ".git/" in r["path"]]
        self.assertTrue(git_requests)
        self.assertTrue(all(r["credential"] == "tok1" for r in git_requests))
        # The caller's identity token is never forwarded to GitHub.
        self.assertFalse(any("x-treer-identity-token" in r["headers"] for r in git_requests))
        self.git("fetch", "-q", "origin", cwd=work, token="agent-token")
        fetches = self.audit(op="fetch")
        self.assertTrue(fetches)
        self.assertEqual(fetches[0]["caller"]["id"], "ag_worker")
        self.assertEqual(fetches[0]["caller"]["machine_id"], "mach_remote")
        self.assertEqual(fetches[0]["caller"]["name"], "Worker")  # from the Core directory
        self.assertEqual(fetches[0]["repo"], "EvoEvolver/treer")
        self.assertEqual(fetches[0]["result"], "ok")

    def test_agent_may_push_agent_refs_but_not_main(self) -> None:
        work = self.clone()
        head = self.commit(work, "feature.txt")
        self.git("push", "-q", "origin", "HEAD:agent/job-1", cwd=work, token="agent-token")
        self.assertEqual(self.upstream_ref("refs/heads/agent/job-1"), head)
        before = self.upstream_ref("refs/heads/main")
        denied = self.git("push", "origin", "HEAD:main", cwd=work, token="agent-token", check=False)
        self.assertNotEqual(denied.returncode, 0)
        self.assertIn("remote rejected", denied.stderr)
        self.assertIn("Agents may push only refs/heads/agent/**", denied.stderr)
        self.assertEqual(self.upstream_ref("refs/heads/main"), before)
        pushes = self.audit(op="push")
        self.assertEqual(pushes[0]["result"], "denied")
        self.assertEqual(pushes[0]["refs"][0]["ref"], "refs/heads/main")
        self.assertEqual(pushes[1]["result"], "ok")
        self.assertEqual(pushes[1]["refs"], [{"ref": "refs/heads/agent/job-1", "old": ZERO, "new": head, "result": "ok"}])
        # A mixed push is rejected as a whole; nothing reaches GitHub.
        self.commit(work, "second.txt")
        mixed = self.git("push", "origin", "HEAD:agent/job-1", "HEAD:main", cwd=work, token="agent-token", check=False)
        self.assertNotEqual(mixed.returncode, 0)
        self.assertEqual(self.upstream_ref("refs/heads/agent/job-1"), head)

    def test_delete_agent_ref_allowed_and_protected_delete_denied(self) -> None:
        work = self.clone()
        self.commit(work, "x.txt")
        self.git("push", "-q", "origin", "HEAD:agent/tmp", cwd=work, token="agent-token")
        self.git("push", "-q", "origin", "--delete", "agent/tmp", cwd=work, token="agent-token")
        self.assertIsNone(self.upstream_ref("refs/heads/agent/tmp"))
        # Even a human may not delete main through the gateway.
        denied = self.git("push", "origin", "--delete", "main", cwd=work, token="human-token", check=False)
        self.assertNotEqual(denied.returncode, 0)
        self.assertIn("protected ref", denied.stderr)
        self.assertIsNotNone(self.upstream_ref("refs/heads/main"))

    def test_human_and_trusted_agent_may_push_main(self) -> None:
        work = self.clone(token="trusted-token")
        head = self.commit(work, "leader.txt")
        self.git("push", "-q", "origin", "HEAD:main", cwd=work, token="trusted-token")
        self.assertEqual(self.upstream_ref("refs/heads/main"), head)
        head = self.commit(work, "human.txt")
        self.git("push", "-q", "origin", "HEAD:main", cwd=work, token="human-token")
        self.assertEqual(self.upstream_ref("refs/heads/main"), head)

    def test_large_chunked_push_streams_and_size_cap(self) -> None:
        work = self.clone()
        head = self.commit(work, "blob.bin", size=3 * 1024 * 1024)  # > http.postBuffer: chunked request
        self.git("push", "-q", "origin", "HEAD:agent/big", cwd=work, token="agent-token")
        self.assertEqual(self.upstream_ref("refs/heads/agent/big"), head)
        receive = [r for r in self.github.requests if r["path"].endswith("git-receive-pack")]
        self.assertEqual(receive[-1]["headers"].get("transfer-encoding"), "chunked")
        self.assertGreater(receive[-1]["body_bytes"], 3 * 1024 * 1024)
        status, _, _ = self.request("PUT", "/api/config", token="trusted-token", body={"max_push_bytes": 1024 * 1024})
        self.assertEqual(status, 200)
        self.commit(work, "blob2.bin", size=2 * 1024 * 1024)
        too_big = self.git("push", "origin", "HEAD:agent/big2", cwd=work, token="agent-token", check=False)
        self.assertNotEqual(too_big.returncode, 0)
        self.assertIsNone(self.upstream_ref("refs/heads/agent/big2"))

    def test_allowlist_and_identity_required(self) -> None:
        self.github.create_repo("someone", "private")
        denied = self.git("ls-remote", f"{self.env.url}/github/someone/private.git", token="agent-token", check=False)
        self.assertNotEqual(denied.returncode, 0)
        self.assertIn("allowlist", denied.stderr)
        self.assertEqual(self.audit(result="denied")[0]["reason"], "repo_not_allowed")
        for token in (None, "bogus", "wrong-audience"):
            anonymous = self.git("ls-remote", self.remote, token=token, check=False)
            self.assertNotEqual(anonymous.returncode, 0)
        # No upstream traffic for rejected callers, and never a credential prompt (no 401).
        self.assertFalse(any("someone" in r["path"] for r in self.github.requests))
        status, headers, _ = self.request("GET", "/github/EvoEvolver/treer.git/info/refs?service=git-upload-pack", token=None)
        self.assertEqual(status, 403)
        self.assertNotIn("www-authenticate", headers)

    def test_gh_token_refresh_on_401(self) -> None:
        self.git("ls-remote", self.remote, token="agent-token")
        self.assertEqual(self.env.gh_calls(), 1)
        self.github.tokens = {"tok2"}  # GitHub revokes tok1; gh now has tok2
        self.env.token_file.write_text("tok2")
        self.git("ls-remote", self.remote, token="agent-token")
        self.assertEqual(self.env.gh_calls(), 2)
        self.assertEqual(self.github.requests[-1]["credential"], "tok2")


class ApiTests(GatewayCase):
    def test_rest_passthrough_reads_and_rewrites_links(self) -> None:
        status, headers, value = self.request("GET", "/github-api/repos/EvoEvolver/treer/branches/main",
                                              headers={"Authorization": "token stolen", "Cookie": "a=b"})
        self.assertEqual(status, 200, value)
        self.assertEqual(value["commit"]["sha"], self.upstream_ref("refs/heads/main"))
        last = self.github.requests[-1]
        self.assertEqual(last["credential"], "tok1")
        self.assertNotIn("cookie", last["headers"])
        status, headers, value = self.request("GET", "/github-api/repos/EvoEvolver/treer/commits/abc/status")
        self.assertEqual(value["state"], "success")
        self.assertIn("http://git-gateway.treer/github-api/repos/x/y/commits?page=2", headers["link"])
        self.assertEqual(headers["x-ratelimit-remaining"], "4999")
        entry = self.audit(op="api")[0]
        self.assertEqual((entry["method"], entry["path"], entry["status"], entry["result"]),
                         ("GET", "/repos/EvoEvolver/treer/commits/abc/status", 200, "ok"))

    def test_rest_rules(self) -> None:
        status, _, value = self.request("POST", "/github-api/repos/EvoEvolver/treer/issues", body={"title": "t"})
        self.assertEqual(status, 201, value)
        self.assertEqual(value["echo"], {"title": "t"})
        status, _, _ = self.request("DELETE", "/github-api/repos/EvoEvolver/treer/git/refs/heads/agent/x")
        self.assertEqual(status, 204)
        for method, path in (("DELETE", "/github-api/repos/EvoEvolver/treer"),
                             ("PATCH", "/github-api/repos/EvoEvolver/treer/git/refs/heads/main"),
                             ("GET", "/github-api/repos/other/repo"),
                             ("POST", "/github-api/graphql"),
                             ("GET", "/github-api/repos/EvoEvolver/treer/../../user")):
            status, _, value = self.request(method, path, body={} if method != "GET" else None)
            self.assertEqual(status, 403, (method, path, value))
        status, _, _ = self.request("PATCH", "/github-api/repos/EvoEvolver/treer/git/refs/heads/main",
                                    token="trusted-token", body={"sha": "x"})
        self.assertEqual(status, 404)  # forwarded (fake has no such route)
        reasons = [entry["reason"] for entry in self.audit(result="denied")]
        self.assertEqual(reasons[:5], ["invalid_path", "path_not_allowed", "repo_not_allowed",
                                       "agent_write_not_allowed", "agent_write_not_allowed"])


class AppTests(GatewayCase):
    def test_manual_status_and_ui(self) -> None:
        status, headers, manual = self.request("GET", "/", token=None)
        self.assertEqual(status, 200)
        self.assertTrue(headers["content-type"].startswith("text/markdown"))
        self.assertIn("git clone http://git-gateway.treer/github/EvoEvolver/treer.git", manual)
        status, headers, page = self.request("GET", "/", token=None, headers={"Accept": "text/html"})
        self.assertIn("<title>Git Gateway</title>", page)
        self.assertIn("form-action 'self' http://127.0.0.1", headers["content-security-policy"])
        status, _, value = self.request("GET", "/api/status")
        self.assertEqual(status, 200)
        self.assertEqual(value["credential"]["active"], "gh")
        self.assertTrue(value["credential"]["gh"]["available"])
        self.assertEqual(value["rate_limit"]["remaining"], 4998)
        self.assertEqual(value["caller"]["id"], "ag_worker")
        self.assertNotIn("tok1", json.dumps(value))
        status, _, _ = self.request("GET", "/api/status", token=None)
        self.assertEqual(status, 401)

    def test_config_api(self) -> None:
        status, _, value = self.request("GET", "/api/config")
        self.assertEqual(value["config"]["agent_push_refs"], ["refs/heads/agent/**"])
        status, _, value = self.request("PUT", "/api/config", body={"repos": ["x/y"]})
        self.assertEqual(status, 403)  # ordinary Agent
        status, _, value = self.request("PUT", "/api/config", token="trusted-token", body={"repos": ["EvoEvolver/*"]})
        self.assertEqual((status, value["config"]["repos"]), (200, ["EvoEvolver/*"]))
        status, _, value = self.request("PUT", "/api/config", token="trusted-token", body={"nope": 1})
        self.assertEqual(status, 400)
        status, _, value = self.request("PUT", "/api/config", token="trusted-token", body={"agent_push_refs": ["main"]})
        self.assertEqual(status, 400)
        # Humans need the browser mutation guard.
        status, _, _ = self.request("PUT", "/api/config", token="human-token", body={"repos": ["a/b"]})
        self.assertEqual(status, 403)
        status, _, _ = self.request("PUT", "/api/config", token="human-token", body={"repos": ["a/b"]},
                                    headers={"X-Gateway-Request": "1", "Origin": "https://evil.example"})
        self.assertEqual(status, 403)
        status, _, _ = self.request("PUT", "/api/config", token="human-token", body={"repos": ["a/b"]},
                                    headers={"X-Gateway-Request": "1", "Origin": "http://gateway.example",
                                             "Sec-Fetch-Site": "same-origin"})
        self.assertEqual(status, 200)
        self.assertEqual(self.audit(op="config")[0]["caller"]["id"], "u_owner")
        self.assertEqual(oct(os.stat(self.env.root / "state" / "config.json").st_mode & 0o777), "0o600")

    def test_github_app_manifest_flow_and_installation_tokens(self) -> None:
        from test_units import test_key

        pem, key = test_key()
        self.github.app_key = key
        self.github.conversion = {"id": self.github.app_id, "slug": "treer-gw", "name": "Treer GW", "pem": pem,
                                  "html_url": "https://github.com/apps/treer-gw", "client_secret": "secret",
                                  "owner": {"login": "EvoEvolver", "type": "Organization"}}
        guard = {"X-Gateway-Request": "1"}
        status, _, value = self.request("POST", "/api/github-app/manifest", token="agent-token", body={})
        self.assertEqual(status, 403)
        status, _, value = self.request("POST", "/api/github-app/manifest", token="human-token", body={"org": "EvoEvolver"},
                                        headers=guard)
        self.assertEqual(status, 200, value)
        self.assertTrue(value["post_url"].endswith("/organizations/EvoEvolver/settings/apps/new?state=" + value["post_url"].split("state=")[1]))
        manifest = json.loads(value["manifest"])
        self.assertEqual(manifest["redirect_url"], "http://gateway.example/api/github-app/callback")
        self.assertEqual(manifest["default_permissions"]["contents"], "write")
        state = value["post_url"].split("state=")[1]
        callback = f"/api/github-app/callback?code=manifest-code&state={state}"
        status, _, _ = self.request("GET", callback, token="human2-token")
        self.assertEqual(status, 403)  # another human cannot complete (and burns) the state
        status, _, value = self.request("POST", "/api/github-app/manifest", token="human-token", body={}, headers=guard)
        state = value["post_url"].split("state=")[1]
        status, headers, _ = self.request("GET", f"/api/github-app/callback?code=manifest-code&state={state}", token="human-token")
        self.assertEqual((status, headers["location"]), (303, "../../?github_app=created"))
        status, _, _ = self.request("GET", f"/api/github-app/callback?code=manifest-code&state={state}", token="human-token")
        self.assertEqual(status, 403)  # single use
        state_dir = self.env.root / "state"
        self.assertEqual(oct(os.stat(state_dir / "github-app.pem").st_mode & 0o777), "0o600")
        self.assertNotIn("secret", (state_dir / "github-app.json").read_text())
        # auto mode now prefers the installation token for EvoEvolver.
        self.github.tokens = {"ghs_install_token"}
        self.git("ls-remote", self.remote, token="agent-token")
        self.assertEqual(self.github.requests[-1]["credential"], "ghs_install_token")
        status, _, value = self.request("GET", "/api/status")
        app = value["credential"]["github_app"]
        self.assertEqual((value["credential"]["active"], app["app"]["id"], app["installations"][0]["id"]),
                         ("github_app", 4242, 99))
        self.assertEqual(app["install_url"], "https://github.com/apps/treer-gw/installations/new")
        self.assertIsNotNone(app["token_expires_at"])
        self.assertEqual(self.audit(op="github_app")[0]["action"], "create")


if __name__ == "__main__":
    unittest.main()
