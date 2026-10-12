"""Self-only TUI -> ACP handoff, detached from the process being replaced."""
import argparse
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import sys
import time
import uuid
from urllib.request import Request, build_opener, ProxyHandler

from native_session import discover, summary


def treer(*args):
    result = subprocess.run(["treer", *args], check=True, text=True, capture_output=True, timeout=30)
    return json.loads(result.stdout)


def detached_restart(plan, state):
    # Double fork + setsid works on both Linux and macOS; nohup alone would
    # remain in the PTY process group that Host stops during restart.
    pid = os.fork()
    if pid:
        os.waitpid(pid, 0)
        return
    os.setsid()
    if os.fork():
        os._exit(0)
    os.chdir("/")
    with open(os.devnull, "rb") as null, open(state / "restart.log", "ab", buffering=0) as log:
        os.dup2(null.fileno(), 0)
        os.dup2(log.fileno(), 1)
        os.dup2(log.fileno(), 2)
    try:
        treer("agent", "restart", "self")
        deadline = time.monotonic() + 120
        journal = Path(plan["cwd"]) / ".treer/agents" / plan["agent_id"] / "journal.sqlite"
        opener = build_opener(ProxyHandler({}))
        while time.monotonic() < deadline:
            try:
                agent = treer("agent", "show", "self")
                agent = agent.get("agent", agent)
                descriptor = treer("interface", "show")
                if agent.get("run_generation", 0) <= plan["generation"] or not descriptor:
                    time.sleep(1)
                    continue
                with sqlite3.connect(f"file:{journal}?mode=ro", uri=True) as connection:
                    row = connection.execute("SELECT value FROM kv WHERE key = 'session_key'").fetchone()
                actual = row[0].split("::", 1)[-1] if row else None
                if actual is None:
                    time.sleep(1)
                    continue
                # Reject stale descriptors: query this Agent's current AIS and
                # require a healthy process and the advertised instance ID.
                port = descriptor["port"]
                base = f"http://127.0.0.1:{int(port)}"
                with opener.open(base + "/v1/manifest", timeout=5) as response:
                    manifest = json.load(response)
                if manifest["instance_id"] != descriptor["instance_id"]:
                    time.sleep(1)
                    continue
                if actual != plan["session_id"]:
                    # Future cold recovery must resume the replacement session,
                    # rather than repeatedly retrying the rejected native ID.
                    command = plan["command"][:-1] + [actual]
                    relative = Path(plan["cwd"]).relative_to(Path(plan["machine_root"]))
                    subprocess.run(["treer", "agent", "startup", "set", "--cwd", str(relative),
                                    "--", *command], check=True, timeout=30)
                    data = json.dumps({"operation_id": plan["operation_id"], "text": plan["summary"]}).encode()
                    with opener.open(Request(base + "/v1/prompts", data=data,
                                             headers={"Content-Type": "application/json"}), timeout=10):
                        pass
                (state / "result.json").write_text(json.dumps({"session_loaded": actual == plan["session_id"],
                    "session_id": actual, "summary_submitted": actual != plan["session_id"]}) + "\n")
                os._exit(0)
            except (OSError, ValueError, KeyError, sqlite3.Error, subprocess.SubprocessError):
                time.sleep(1)
        raise RuntimeError("replacement AIS did not become ready within 120 seconds; see plan.json and retry the summary manually")
    except Exception as error:
        (state / "error.txt").write_text(str(error) + "\n")
        os._exit(1)


def main():
    parser = argparse.ArgumentParser(description="Run inside the Agent to replace its own TUI with ACP")
    parser.add_argument("--agent", required=True, choices=["codex", "claude"])
    parser.add_argument("--session-id", help="Override the most recently modified matching native session")
    parser.add_argument("--summary-file", type=Path, help="Use your own handoff summary on load failure")
    parser.add_argument("--ui", choices=["builtin", "remote-codex"])
    parser.add_argument("--dry-run", action="store_true", help="Discover and print the plan without changing startup or restarting")
    options = parser.parse_args()
    context = treer("whoami")
    root = Path(context["machine"]["root"]).resolve()
    cwd = Path.cwd().resolve()
    relative = cwd.relative_to(root)
    # Resolve current before saving startup: this handoff keeps the selected
    # version even if another install changes current during restart.
    version = (root / ".treer/launchers/acp/current").resolve(strict=True)
    launcher = version / "launchers/acp/scripts/treer-agent.sh"
    profiles = json.loads((version / "launchers/acp/profiles.json").read_text())["profiles"]
    presentation = {None: "headless", "builtin": "builtin-ui", "remote-codex": "remote-codex-ui"}[options.ui]
    profile = next((p for p in profiles if p["provider"] == options.agent and p["presentation"] == presentation), None)
    if profile is None:
        raise RuntimeError(f"installed version has no {options.agent} {presentation} profile; rerun install.sh")
    import shutil
    for command in profile.get("requires", []):
        if shutil.which(command) is None:
            raise RuntimeError(f"{command} is not installed; startup was not changed")
    runtime = version / "launchers/acp/.build" / (
        "runtime-remote-codex" if options.ui == "remote-codex" else "runtime-headless") / "release/treer-acp"
    if not os.access(launcher, os.X_OK) or not os.access(runtime, os.X_OK):
        raise RuntimeError("installed launcher/runtime is missing; startup was not changed")
    if options.ui == "remote-codex" and not (version / "launchers/acp/.build/remote-codex-ui/source/apps/agent-ui-web/dist/index.html").is_file():
        raise RuntimeError("Remote Codex UI assets are missing; startup was not changed")
    sid, transcript = discover(options.agent, cwd, options.session_id)
    text = options.summary_file.read_text() if options.summary_file else summary(options.agent, sid, transcript, cwd)
    command = [str(launcher), *profile["run"]["args"], "--session-id", sid]
    plan = {"agent_id": context["agent"]["agent_id"], "generation": context["agent"].get("run_generation", 0), "machine_root": str(root),
            "cwd": str(cwd), "session_id": sid, "command": command,
            "operation_id": "acp-switch-" + uuid.uuid4().hex, "summary": text}
    print(json.dumps({k: v for k, v in plan.items() if k != "summary"}, indent=2), flush=True)
    if options.dry_run:
        return
    if not all(os.environ.get(key) for key in ("TREER_AGENT_ID", "TREER_WORKLOAD_CREDENTIAL", "TREER_AGENT_SERVER_URL")):
        raise RuntimeError("switch.sh must run inside the target managed Agent")
    if context["agent"]["agent_id"] != os.environ["TREER_AGENT_ID"]:
        raise RuntimeError("treer whoami does not match the injected Agent identity")
    os.umask(0o077)
    state = root / ".treer/launchers/acp/switches" / plan["agent_id"] / plan["operation_id"]
    state.mkdir(parents=True)
    (state / "plan.json").write_text(json.dumps(plan) + "\n")
    subprocess.run(["treer", "agent", "startup", "set", "--cwd", str(relative), "--", *command], check=True)
    print(f"Startup saved. Detached self restart queued; status files: {state}", flush=True)
    detached_restart(plan, state)


if __name__ == "__main__":
    try:
        main()
    except (RuntimeError, OSError, ValueError, subprocess.SubprocessError) as error:
        sys.exit(str(error))
