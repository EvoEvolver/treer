#!/bin/sh
set -eu

SCRIPT_ROOT="$(CDPATH= cd -- "$(dirname "$0")" && pwd)"
REPO_ROOT="$(CDPATH= cd -- "$SCRIPT_ROOT/../../.." && pwd)"
AGENTS=""
UI=""
LAUNCH=1
LIST=0
BUILD_ONLY=0
SKIP_BUILD=0
PROFILE_ROOT=""

while [ "$#" -gt 0 ]; do
  case "$1" in
    --list)
      LIST=1
      shift
      ;;
    --dir)
      REPO_ROOT="$(CDPATH= cd -- "$2" && pwd)"
      shift 2
      ;;
    --agent)
      AGENTS="$AGENTS $2"
      shift 2
      ;;
    --ui)
      UI="$2"
      shift 2
      ;;
    --no-launch)
      LAUNCH=0
      shift
      ;;
    --build-only) BUILD_ONLY=1; LAUNCH=0; shift ;;
    --skip-build) SKIP_BUILD=1; shift ;;
    --profile-root) PROFILE_ROOT="$2"; shift 2 ;;
    *)
      echo "unknown argument: $1" >&2
      exit 2
      ;;
  esac
done

LAUNCHER_ROOT="$REPO_ROOT/launchers/acp"
MANIFEST="$LAUNCHER_ROOT/profiles.json"
PROFILE_TOOL="$LAUNCHER_ROOT/scripts/install_profiles.py"
if [ ! -f "$MANIFEST" ] || [ ! -f "$PROFILE_TOOL" ]; then
  echo "$REPO_ROOT is not a Treer checkout containing launchers/acp" >&2
  exit 1
fi

if [ "$LIST" -eq 1 ]; then
  exec python3 "$PROFILE_TOOL" "$MANIFEST" --list
fi
if [ -z "${AGENTS# }" ]; then
  echo "at least one --agent is required; available launchers:" >&2
  python3 "$PROFILE_TOOL" "$MANIFEST" --list >&2
  exit 2
fi

for command in python3; do
  if ! command -v "$command" >/dev/null 2>&1; then
    echo "$command is not on PATH" >&2
    exit 1
  fi
done

case "$UI" in
  "")
    PRESENTATION="headless"
    ;;
  builtin) PRESENTATION="builtin-ui" ;;
  remote-codex)
    PRESENTATION="remote-codex-ui"
    ;;
  *)
    echo "unsupported --ui value: $UI" >&2
    exit 2
    ;;
esac

for agent in $AGENTS; do
  python3 "$PROFILE_TOOL" "$MANIFEST" \
    --agent "$agent" \
    --presentation "$PRESENTATION" \
    --repo-cwd . \
    --check
done

if [ "$BUILD_ONLY" -eq 0 ]; then
WHOAMI="$(treer whoami)"
MACHINE_SERVER_ID="$(python3 -c 'import json,sys
server_id = json.loads(sys.argv[1])["machine"].get("server_id", "").strip()
if not server_id:
    raise SystemExit("treer whoami did not return machine.server_id")
print(server_id)
' "$WHOAMI")"
REPO_CWD="$(python3 -c 'import json,os,sys
repo = os.path.realpath(sys.argv[1])
machine_root = os.path.realpath(json.loads(sys.argv[2])["machine"]["root"])
relative = os.path.relpath(repo, machine_root)
if relative == ".." or relative.startswith("../") or os.path.isabs(relative):
    raise SystemExit(f"Treer checkout {repo} is outside machine root {machine_root}")
print(relative)
' "$REPO_ROOT" "$WHOAMI")"
fi

if [ "$SKIP_BUILD" -eq 0 ]; then
command -v cargo >/dev/null 2>&1 || { echo 'cargo is not on PATH' >&2; exit 1; }
case "$UI" in
  ""|builtin)
    echo "building headless ACP runtime"
    cargo build \
      --locked \
      --release \
      --manifest-path "$LAUNCHER_ROOT/runtime/Cargo.toml" \
      --target-dir "$LAUNCHER_ROOT/.build/runtime-headless"
    ;;
  remote-codex)
    for command in git node corepack; do
      if ! command -v "$command" >/dev/null 2>&1; then
        echo "$command is required for the explicitly selected Remote Codex UI" >&2
        exit 1
      fi
    done
    LOCK="$LAUNCHER_ROOT/optional-ui/remote-codex.lock.json"
    UI_REPOSITORY="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["repository"])' "$LOCK")"
    UI_COMMIT="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["commit"])' "$LOCK")"
    UI_PACKAGE_MANAGER="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["package_manager"])' "$LOCK")"
    UI_SOURCE="$LAUNCHER_ROOT/.build/remote-codex-ui/source"
    mkdir -p "$(dirname "$UI_SOURCE")"
    if [ ! -d "$UI_SOURCE/.git" ]; then
      git clone --filter=blob:none --no-checkout "$UI_REPOSITORY" "$UI_SOURCE"
    fi
    git -C "$UI_SOURCE" remote set-url origin "$UI_REPOSITORY"
    git -C "$UI_SOURCE" fetch --depth 1 origin "$UI_COMMIT"
    git -C "$UI_SOURCE" checkout --detach "$UI_COMMIT"
    test "$(git -C "$UI_SOURCE" rev-parse HEAD)" = "$UI_COMMIT"
    corepack "$UI_PACKAGE_MANAGER" --dir "$UI_SOURCE" install --frozen-lockfile
    corepack "$UI_PACKAGE_MANAGER" --dir "$UI_SOURCE" run agent-ui:build
    python3 "$LAUNCHER_ROOT/scripts/prepare_remote_codex.py" "$LOCK" "$UI_SOURCE"
    echo "building ACP runtime with the optional Remote Codex adapter"
    cargo build \
      --locked \
      --release \
      --features remote-codex-ui \
      --manifest-path "$LAUNCHER_ROOT/runtime/Cargo.toml" \
      --target-dir "$LAUNCHER_ROOT/.build/runtime-remote-codex"
    ;;
  *)
    echo "unsupported --ui value: $UI" >&2
    exit 2
    ;;
esac
fi

case "$UI" in
  ""|builtin) RUNTIME="$LAUNCHER_ROOT/.build/runtime-headless/release/treer-acp" ;;
  remote-codex) RUNTIME="$LAUNCHER_ROOT/.build/runtime-remote-codex/release/treer-acp" ;;
esac
[ -x "$RUNTIME" ] || { echo "ACP runtime is missing: $RUNTIME" >&2; exit 1; }

[ "$BUILD_ONLY" -eq 0 ] || exit 0

# Installed profiles use a stable absolute command through current. Preserve
# the machine-root-relative checkout cwd for source-only apply.sh users.
TEMP_MANIFEST=""
if [ -n "$PROFILE_ROOT" ]; then
  case "$PROFILE_ROOT" in /*) ;; *) echo '--profile-root must be absolute' >&2; exit 2 ;; esac
  TEMP_MANIFEST="$(mktemp)"
  trap 'rm -f -- "$TEMP_MANIFEST"' EXIT
  python3 - "$MANIFEST" "$TEMP_MANIFEST" "$PROFILE_ROOT" <<'PY'
import json,sys
source,dest,root=sys.argv[1:]
with open(source) as handle: data=json.load(handle)
for profile in data["profiles"]:
    profile["run"]["command"]=root+"/launchers/acp/scripts/treer-agent.sh"
with open(dest,"w") as handle: json.dump(data,handle)
PY
  MANIFEST="$TEMP_MANIFEST"
  REPO_CWD=.
fi

for agent in $AGENTS; do
  set -- "$PROFILE_TOOL" "$MANIFEST" \
    --agent "$agent" \
    --presentation "$PRESENTATION" \
    --repo-cwd "$REPO_CWD" \
    --machine "$MACHINE_SERVER_ID"
  if [ "$LAUNCH" -eq 1 ]; then
    set -- "$@" --launch
  fi
  python3 "$@"
done

# Do not treat successful upsert as proof that the saved launch is correct.
python3 - "$MANIFEST" "$PRESENTATION" $AGENTS <<'PY'
import json,subprocess,sys
manifest,presentation,*agents=sys.argv[1:]
with open(manifest) as handle: profiles=json.load(handle)["profiles"]
for profile in profiles:
    if profile["provider"] not in agents or profile["presentation"] != presentation: continue
    result=subprocess.run(["treer","agent","admin","profile","show",profile["name"]],check=True,capture_output=True,text=True)
    saved=json.loads(result.stdout)
    saved=saved.get("profile",saved)
    if saved["command"] != profile["run"]["command"] or saved["args"] != profile["run"].get("args",[]):
        raise SystemExit("saved profile command/args did not match: "+profile["name"])
    print(result.stdout.strip())
PY
