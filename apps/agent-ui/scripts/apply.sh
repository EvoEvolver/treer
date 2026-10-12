#!/bin/sh
set -eu

APP_ROOT="$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)"
REPO_ROOT="$(CDPATH= cd -- "$APP_ROOT/../.." && pwd)"
NAME="${TREER_RECIPE_AGENT_NAME:-codex-ui}"
ACP_COMMAND=""
DISPLAY_NAME=""

while [ "$#" -gt 0 ]; do
  case "$1" in
    --name)
      NAME="$2"
      shift 2
      ;;
    --acp-command)
      ACP_COMMAND="$2"
      shift 2
      ;;
    --display-name)
      DISPLAY_NAME="$2"
      shift 2
      ;;
    *)
      echo "unknown argument: $1" >&2
      exit 2
      ;;
  esac
done

HARNESS_TOOL=codex
if [ -n "$ACP_COMMAND" ]; then HARNESS_TOOL="$ACP_COMMAND"; fi
for command in treer npm "$HARNESS_TOOL" curl python3; do
  if ! command -v "$command" >/dev/null 2>&1; then
    echo "$command is not on PATH" >&2
    exit 1
  fi
done

echo "installing Agent UI dependencies in $APP_ROOT"
npm --prefix "$APP_ROOT" install --install-strategy=nested --no-workspaces

WHOAMI="$(treer whoami)"
AGENT_CWD="$(python3 -c 'import json, os, sys
repo = os.path.abspath(sys.argv[1])
whoami = json.loads(sys.argv[2])
root = os.path.abspath(whoami["machine"]["root"])
relative = os.path.relpath(repo, root)
if relative.startswith("..") or os.path.isabs(relative):
    raise SystemExit(f"Treer checkout {repo} is outside machine root {root}")
print(relative)
' "$REPO_ROOT" "$WHOAMI")"

LAUNCH_ARGS="$(python3 -c 'import json, sys
acp, display = sys.argv[1], sys.argv[2]
args = ["--harness", "acp", "--acp-command", acp] if acp else []
if display:
    args += ["--name", display]
print(json.dumps(args))' "$ACP_COMMAND" "$DISPLAY_NAME")"

python3 - "$APP_ROOT/treer-agent.json" "$AGENT_CWD" "$LAUNCH_ARGS" "$ACP_COMMAND" "$DISPLAY_NAME" <<'PY'
import json, subprocess, sys

meta = json.load(open(sys.argv[1]))
cwd = sys.argv[2]
run = meta["run"]
command = run["command"]
args = (run.get("args") or []) + json.loads(sys.argv[3])
acp, display = sys.argv[4], sys.argv[5]
if acp:
    label = display or acp
    name = f"{label} + UI"
    description = f"One {label} ACP session, one Treer Agent, and the shared Agent UI."
else:
    name = meta["name"]
    description = meta.get("description") or ""

existing = subprocess.run(
    ["treer", "agent", "admin", "profile", "show", name],
    capture_output=True,
    text=True,
)
if existing.returncode == 0:
    argv = [
        "treer", "agent", "admin", "profile", "update", name,
        "--description", description, "--cwd", cwd, "--command", command,
    ]
    if args:
        for item in args:
            argv.extend(["--arg", item])
    else:
        argv.append("--clear-args")
else:
    argv = [
        "treer", "agent", "admin", "profile", "create", name,
        "--description", description, "--cwd", cwd, command,
    ]
    if args:
        argv.append("--")
        argv.extend(args)
subprocess.check_call(argv)
PY

create_agent() {
  treer agent admin create \
    --machine self \
    --kind command \
    --name "$NAME" \
    --cwd "$AGENT_CWD" \
    -- ./apps/agent-ui/scripts/treer-agent.sh "$@"
}
# Restore the JSON launch arguments as positional parameters, preserving spaces.
eval "set -- $(python3 -c 'import json, shlex, sys; print(" ".join(shlex.quote(a) for a in json.loads(sys.argv[1])))' "$LAUNCH_ARGS")"

if treer agent show "$NAME" >/dev/null 2>&1; then
  STATUS="$(treer agent show "$NAME" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("status") or "")')"
  if [ "$STATUS" = "failed" ] || [ "$STATUS" = "exited" ]; then
    treer agent admin delete "$NAME" >/dev/null
    create_agent "$@"
  else
    echo "Agent $NAME already exists ($STATUS)"
  fi
else
  create_agent "$@"
fi

python3 - "$NAME" <<'PY'
import json, subprocess, sys, time

name = sys.argv[1]
required = {"prompt.submit", "transcript.read", "state.observe", "abort"}
deadline = time.time() + 300
last = "not visible"
while time.time() < deadline:
    result = subprocess.run(
        ["treer", "agent", "show", name],
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        time.sleep(2)
        continue
    agent = json.loads(result.stdout)
    status = agent.get("status")
    if status in {"failed", "exited"}:
        raise SystemExit(f"Agent {name} entered {status}")
    interface = agent.get("interface") if isinstance(agent.get("interface"), dict) else None
    capabilities = set(interface.get("capabilities") or []) if interface else set()
    if (interface
            and interface.get("protocol") == "treer.agent-interface/v1"
            and interface.get("ui_path") == "/"
            and required.issubset(capabilities)):
        print(json.dumps({"ok": True, "agent": agent, "interface": interface}, indent=2))
        raise SystemExit(0)
    last = f"status={status}, interface={interface}"
    time.sleep(2)
raise SystemExit(f"timed out waiting for {name}: {last}")
PY
