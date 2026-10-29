#!/usr/bin/env python3
"""Manual smoke test against real GitHub in gh mode (not part of the unit suite).

Runs the gateway in-process with a fake Treer identity verifier, then through it:
ls-remote, shallow clone, push a throwaway ``agent/gateway-smoke-<ts>`` branch,
confirm it via the REST passthrough, verify that pushing main is refused, delete
the branch and confirm it is gone. Needs a logged-in ``gh`` with push access.

    python3 apps/git-gateway/tests/smoke_real.py [--repo EvoEvolver/treer]
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import tempfile
import threading
import time
import urllib.error
import urllib.request
from pathlib import Path

import fixture


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--repo", default="EvoEvolver/treer")
    args = parser.parse_args()
    temporary = tempfile.TemporaryDirectory()
    root = Path(temporary.name)
    proxy = fixture.serve(fixture.FakeProxy, {"verifications": 0})
    module = fixture.load_gateway()
    settings = module.Settings(
        listen=("127.0.0.1", 0), data_dir=root / "state", proxy_url=f"http://127.0.0.1:{proxy.server_port}",
        service_id="svc_gateway", workspace_id="ws_test", public_url=None, agent_url="http://git-gateway.treer",
        github_url="https://github.com", github_api_url="https://api.github.com", gh_bin="gh",
        gh_refresh_seconds=600, treer_bin="treer", seed={"repos": [args.repo]},
    )
    gateway = module.Gateway(settings)
    server = module.GatewayServer(settings.listen, gateway, quiet=True)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    base = f"http://127.0.0.1:{server.server_port}"
    remote = f"{base}/github/{args.repo}.git"
    branch = f"agent/gateway-smoke-{int(time.time())}"
    environment = {**os.environ, "GIT_TERMINAL_PROMPT": "0"}
    identity = ["-c", "http.extraHeader=X-Treer-Identity-Token: agent-token", "-c", "credential.helper="]

    def git(*command: str, cwd: Path | None = None, check: bool = True) -> subprocess.CompletedProcess[str]:
        started = time.time()
        result = subprocess.run(["git", *identity, *command], cwd=cwd, capture_output=True, text=True,
                                env=environment, timeout=600)
        print(f"$ git {' '.join(command)}  -> {result.returncode} ({time.time() - started:.1f}s)")
        if check and result.returncode != 0:
            raise SystemExit(result.stderr)
        return result

    def api(path: str) -> tuple[int, object]:
        request = urllib.request.Request(f"{base}/github-api{path}", headers={"X-Treer-Identity-Token": "agent-token"})
        try:
            with urllib.request.build_opener(urllib.request.ProxyHandler({})).open(request, timeout=30) as response:
                return response.status, json.loads(response.read())
        except urllib.error.HTTPError as error:
            return error.code, json.loads(error.read() or b"{}")

    pushed = False
    try:
        refs = git("ls-remote", "--heads", remote, "main").stdout.split()
        print("main =", refs[0])
        work = root / "clone"
        git("clone", "-q", "--depth", "1", remote, str(work))
        git("-c", "user.name=Gateway Smoke", "-c", "user.email=gateway-smoke@example.invalid",
            "commit", "-q", "--allow-empty", "-m", "gateway smoke test (throwaway)", cwd=work)
        head = git("rev-parse", "HEAD", cwd=work).stdout.strip()
        git("push", "-q", "origin", f"HEAD:{branch}", cwd=work)
        pushed = True
        status, value = api(f"/repos/{args.repo}/branches/{branch}")
        print("REST branch after push:", status, value.get("commit", {}).get("sha") if isinstance(value, dict) else value)
        assert status == 200 and value["commit"]["sha"] == head, (status, value)
        status, value = api(f"/repos/{args.repo}/commits/{head}/status")
        print("REST combined status:", status, value.get("state"))
        denied = git("push", "origin", "HEAD:main", cwd=work, check=False)
        assert denied.returncode != 0 and "Agents may push only" in denied.stderr, denied.stderr
        print("main push refused:", denied.stderr.strip().splitlines()[-2:])
        git("push", "-q", "origin", "--delete", branch, cwd=work)
        pushed = False
        status, value = api(f"/repos/{args.repo}/branches/{branch}")
        print("REST branch after delete:", status)
        assert status == 404, (status, value)
        print("status:", json.dumps({k: gateway.status()[k] for k in ("credential", "rate_limit")}, default=str)[:600])
        print("audit:")
        for entry in reversed(gateway.state.read_audit(50)):
            print("  ", entry["op"], entry.get("repo") or entry.get("path"), entry["result"], entry.get("status"),
                  [f"{r['ref']} {r['old'][:7]}->{r['new'][:7]} {r.get('result')}" for r in entry.get("refs", [])])
        print("SMOKE OK")
        return 0
    finally:
        if pushed:  # leave nothing behind even if an assertion failed
            subprocess.run(["gh", "api", "-X", "DELETE", f"repos/{args.repo}/git/refs/heads/{branch}"], check=False)
        server.shutdown()
        proxy.shutdown()
        temporary.cleanup()


if __name__ == "__main__":
    sys.exit(main())
