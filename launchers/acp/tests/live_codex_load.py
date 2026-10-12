"""Opt-in native Codex -> ACP regression, with disposable CODEX_HOME and Agent.

Run with --adapter /absolute/path/to/maintained/codex-acp. No existing native
session is opened. Existing auth/config are read and copied privately, never
printed. The owned temporary Agent is removed even on test failure.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import tempfile
import time
from urllib.request import build_opener, ProxyHandler


def treer(*args):
    return json.loads(subprocess.check_output(["treer", *args], text=True, timeout=30))


def wait_for(callback, label, seconds=180):
    end = time.monotonic() + seconds
    while time.monotonic() < end:
        result = callback()
        if result:
            return result
        time.sleep(0.5)
    raise RuntimeError("Timed out: " + label)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--adapter", required=True, type=Path)
    parser.add_argument("--old-adapter", type=Path)
    options = parser.parse_args()
    for key in ("TREER_AGENT_ID", "TREER_WORKLOAD_CREDENTIAL", "TREER_AGENT_SERVER_URL"):
        if not os.environ.get(key):
            raise RuntimeError("Run this opt-in check inside a managed Treer Agent")
    context = treer("whoami")
    repo = Path(__file__).resolve().parents[3]
    runtime = repo / "launchers/acp/runtime/target/debug/treer-acp"
    codex = Path(shutil.which("codex")).resolve()
    os.umask(0o077)
    parent = Path(context["machine"]["root"]) / ".treer/launcher-validation"
    parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    state = Path(tempfile.mkdtemp(prefix="acp-load-check-", dir=parent))
    home = state / "codex-home"
    home.mkdir(mode=0o700)
    source = Path(os.environ.get("CODEX_HOME", Path.home() / ".codex"))
    for name in ("auth.json", "config.toml"):
        if (source / name).is_file():
            shutil.copyfile(source / name, home / name)
            (home / name).chmod(0o600)
    cwd = state / "cwd"
    cwd.mkdir()
    outside = state / "outside.txt"
    token = state.name
    env = {**os.environ, "CODEX_HOME": str(home)}
    with (state / "native.log").open("w") as log:
        subprocess.run([str(codex), "--no-daemon", "exec", "--dangerously-bypass-approvals-and-sandbox",
            "--skip-git-repo-check", "-C", str(cwd), "Reply NATIVE_READY only. Do not use tools."],
            env=env, stdout=log, stderr=log, check=True, timeout=180)
    rollouts = list((home / "sessions").rglob("*.jsonl"))
    assert len(rollouts) == 1, rollouts
    rollout = rollouts[0]
    meta = json.loads(rollout.read_text().splitlines()[0])["payload"]
    sid = meta.get("id", meta.get("session_id"))
    before = hashlib.sha256(rollout.read_bytes()).hexdigest()
    base = [str(runtime), "--check-session-load", "--harness", "codex", "--base-command", str(codex), "--session-id", sid]
    if options.old_adapter:
        result = subprocess.run([*base, "--server-command", str(options.old_adapter)], env=env, capture_output=True, text=True)
        assert result.returncode != 0 and "Refusing native Codex" in result.stderr, result.stderr
        assert hashlib.sha256(rollout.read_bytes()).hexdigest() == before
    verified = subprocess.run([*base, "--server-command", str(options.adapter)], env=env, capture_output=True, text=True, check=True)
    compatibility = json.loads(verified.stdout)
    wrapper = state / "start.sh"
    wrapper.write_text("#!/bin/sh\nexport CODEX_HOME=" + shlex.quote(str(home)) + "\nexec " + shlex.join([
        str(runtime), "--cwd", str(cwd), "--harness", "codex", "--base-command", str(codex),
        "--server-command", str(options.adapter), "--session-id", sid]) + "\n")
    wrapper.chmod(0o700)
    agent = None
    opener = build_opener(ProxyHandler({}))
    try:
        result = treer("agent", "admin", "create", "--machine", context["machine"]["server_id"],
            "--kind", "command", "--name", token, "--cwd", str(cwd.relative_to(context["machine"]["root"])), "--", str(wrapper))
        agent = result.get("agent_id") or result["agent"]["agent_id"]

        def ready():
            info = treer("agent", "show", agent)
            if info["status"] in ("exited", "failed"):
                raise RuntimeError(str(treer("agent", "read", agent, "--lines", "80")))
            interface = info.get("interface")
            return info if interface and interface.get("port") else None

        info = wait_for(ready, "ACP interface")
        port = info["interface"]["port"]
        treer("agent", "prompt", agent,
            "This is an authorized regression test. Use the shell tool to execute exactly: "
            f"treer whoami > {shlex.quote(str(state / 'whoami.json'))} && printf '%s' {shlex.quote(token)} > {shlex.quote(str(outside))}. "
            "The second path is intentionally outside cwd. Do not use file API to write it. Then reply LOAD_POLICY_OK. Do not do anything else.")

        def finished():
            with opener.open(f"http://127.0.0.1:{port}/v1/status", timeout=10) as response:
                status = json.load(response)
            if status.get("error"):
                raise RuntimeError(str(status))
            if not status["busy"] and outside.is_file() and (state / "whoami.json").is_file():
                return True
            return None

        wait_for(finished, "treer whoami and write outside cwd")
        assert outside.read_text() == token
        identity = json.loads((state / "whoami.json").read_text())
        assert identity["agent"]["agent_id"] == agent
        actual = treer("agent", "transcript", agent, "--page", "0", "--limit", "10")
        (state / "transcript.json").write_text(json.dumps(actual))
        assert "LOAD_POLICY_OK" in json.dumps(actual)
    finally:
        if agent:
            treer("agent", "admin", "delete", agent)
    # Re-open with native 0.162 after the ACP turn. A malformed ordinal causes
    # this RPC itself to fail, even before a model request could be dispatched.
    process = subprocess.Popen([str(codex), "app-server", "--listen", "stdio://"], env=env,
        stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True)
    try:
        for number, method, params in [(1, "initialize", {"clientInfo":{"name":"acp-load-check","version":"1"}}),
            (2, "thread/resume", {"threadId":sid,"cwd":str(cwd),"approvalPolicy":"never","sandbox":"danger-full-access"})]:
            process.stdin.write(json.dumps({"id": number, "method": method, "params": params}) + "\n")
            process.stdin.flush()
            while True:
                line = process.stdout.readline()
                if not line:
                    raise RuntimeError("Native app-server exited before resume")
                response = json.loads(line)
                if response.get("id") == number:
                    assert "error" not in response, response
                    break
        assert response["result"]["thread"]["id"] == sid
    finally:
        process.terminate()
        process.wait(timeout=10)
    print(json.dumps({"ok":True, "compatibility":compatibility, "treer_whoami":True,
        "write_outside_cwd":True, "native_resume_after_acp":True, "temporary_agent_deleted":True, "private_artifacts":str(state)}, indent=2))


if __name__ == "__main__":
    main()
