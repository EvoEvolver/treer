"""Replace only this managed Agent with the Agent UI, keeping its native session."""
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
    parser.add_argument("--harness", choices=("codex", "claude", "acp"), default="codex")
    parser.add_argument("--session-id", "--thread-id", dest="session_id", required=True,
                        help="native session to resume (Codex, Claude Code or ACP session ID)")
    parser.add_argument("--acp-command", help="ACP agent executable, required for --harness acp")
    parser.add_argument("--acp-arg", action="append", default=[], help="argument for the ACP agent; repeatable")
    parser.add_argument("--name", help="name shown in the UI")
    parser.add_argument("--dry-run", action="store_true")
    args = parser.parse_args()
    if not args.session_id.strip():
        parser.error("--session-id must not be empty")
    if args.harness == "acp" and not args.acp_command:
        parser.error("--harness acp requires --acp-command")
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
        raise RuntimeError(f"Agent UI launcher/dependencies missing; run npm ci in {app}")
    tool = os.environ.get("CLAUDE_BIN", "claude") if args.harness == "claude" else "codex" if args.harness == "codex" else args.acp_command
    for command in ("node", "curl", tool):
        if not shutil.which(command):
            raise RuntimeError(f"{command} is not on PATH")
    # startup --cwd is relative to the machine root, not to this Agent.
    # Keep the requested startup command rooted at '.', but pin the Codex cwd
    # explicitly so migrating an Agent in a repository does not move its session.
    command = ["treer", "agent", "startup", "set", "--cwd", ".", "--", str(launcher), "--harness", args.harness,
               "--session-id", args.session_id, "--cwd", str(Path.cwd())]
    if args.harness == "acp":
        command += ["--acp-command", args.acp_command]
        for value in args.acp_arg:
            command += ["--acp-arg", value]
    if args.name:
        command += ["--name", args.name]
    print(json.dumps({"agent_id": agent_id, "cwd": str(Path.cwd()), "harness": args.harness, "session_id": args.session_id,
                      "startup_command": command, "restart_command": ["treer", "agent", "restart", "self"]}, indent=2), flush=True)
    if args.dry_run:
        return
    os.umask(0o077)
    directory = Path.cwd() / ".treer/agents" / agent_id / "agent-ui-switches"
    directory.mkdir(parents=True, exist_ok=True, mode=0o700)
    directory.chmod(0o700)
    state = Path(tempfile.mkdtemp(prefix="switch-", dir=directory))
    (state / "plan.json").write_text(json.dumps({"harness": args.harness, "session_id": args.session_id, "startup_command": command}) + "\n")
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
