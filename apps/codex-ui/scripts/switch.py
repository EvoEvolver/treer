"""Replace only this managed Agent with Codex UI, keeping its native thread."""
import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--thread-id", required=True)
    parser.add_argument("--dry-run", action="store_true")
    args = parser.parse_args()
    if not args.thread_id.strip():
        parser.error("--thread-id must not be empty")
    for key in ("TREER_AGENT_ID", "TREER_WORKLOAD_CREDENTIAL", "TREER_AGENT_SERVER_URL"):
        if not os.environ.get(key):
            raise RuntimeError("Run switch.sh inside the target Treer-managed Agent")
    context = json.loads(subprocess.check_output(["treer", "whoami"], text=True, timeout=30))
    agent_id = context["agent"]["agent_id"]
    if agent_id != os.environ["TREER_AGENT_ID"]:
        raise RuntimeError("treer whoami does not match the injected Agent identity")
    launcher = Path(__file__).resolve().with_name("treer-agent.sh")
    app = launcher.parent.parent
    if not os.access(launcher, os.X_OK) or not os.access(app / "node_modules/.bin/tsx", os.X_OK):
        raise RuntimeError("Codex UI launcher/dependencies missing; run npm ci in apps/codex-ui")
    for command in ("node", "codex", "curl"):
        if not shutil.which(command):
            raise RuntimeError(f"{command} is not on PATH")
    # startup --cwd is relative to the machine root, not to this Agent.
    # Keep the requested startup command rooted at '.', but pin the Codex cwd
    # explicitly so migrating an Agent in a repository does not move its thread.
    command = ["treer", "agent", "startup", "set", "--cwd", ".", "--", str(launcher), "--thread-id", args.thread_id,
               "--cwd", str(Path.cwd())]
    print(json.dumps({"agent_id": agent_id, "cwd": str(Path.cwd()), "thread_id": args.thread_id,
                      "startup_command": command, "restart_command": ["treer", "agent", "restart", "self"]}, indent=2), flush=True)
    if args.dry_run:
        return
    os.umask(0o077)
    directory = Path.cwd() / ".treer/agents" / agent_id / "codex-ui-switches"
    directory.mkdir(parents=True, exist_ok=True, mode=0o700)
    directory.chmod(0o700)
    state = Path(tempfile.mkdtemp(prefix="switch-", dir=directory))
    (state / "plan.json").write_text(json.dumps({"thread_id": args.thread_id, "startup_command": command}) + "\n")
    subprocess.run(command, check=True, timeout=30)
    print(f"Startup saved; detached self restart queued. Log: {state / 'restart.log'}", flush=True)
    pid = os.fork()
    if pid:
        os.waitpid(pid, 0)
        return
    os.setsid()
    if os.fork():
        os._exit(0)
    with open(os.devnull, "rb") as stdin, open(state / "restart.log", "ab", buffering=0) as log:
        os.dup2(stdin.fileno(), 0)
        os.dup2(log.fileno(), 1)
        os.dup2(log.fileno(), 2)
    try:
        result = subprocess.run(["treer", "agent", "restart", "self"], timeout=60)
        (state / "result.json").write_text(json.dumps({"restart_exit_code": result.returncode}) + "\n")
        os._exit(0 if result.returncode == 0 else 1)
    except Exception as error:
        (state / "error.txt").write_text(str(error) + "\n")
        os._exit(1)


if __name__ == "__main__":
    try:
        main()
    except (RuntimeError, OSError, ValueError, subprocess.SubprocessError) as error:
        sys.exit(str(error))
