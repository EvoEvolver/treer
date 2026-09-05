#!/bin/sh
set -eu

SCRIPT_ROOT="$(CDPATH= cd -- "$(dirname "$0")" && pwd)"
REPO_ROOT="$(CDPATH= cd -- "$SCRIPT_ROOT/../../.." && pwd)"
AGENTS=""
UI=""
LAUNCH=1
LIST=0

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

for command in cargo python3 treer; do
  if ! command -v "$command" >/dev/null 2>&1; then
    echo "$command is not on PATH" >&2
    exit 1
  fi
done

case "$UI" in
  "")
    PRESENTATION="headless"
    ;;
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

WHOAMI="$(treer whoami)"
REPO_CWD="$(python3 -c 'import json,os,sys
repo = os.path.realpath(sys.argv[1])
machine_root = os.path.realpath(json.loads(sys.argv[2])["machine"]["root"])
relative = os.path.relpath(repo, machine_root)
if relative == ".." or relative.startswith("../") or os.path.isabs(relative):
    raise SystemExit(f"Treer checkout {repo} is outside machine root {machine_root}")
print(relative)
' "$REPO_ROOT" "$WHOAMI")"

case "$UI" in
  "")
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

for agent in $AGENTS; do
  set -- "$PROFILE_TOOL" "$MANIFEST" \
    --agent "$agent" \
    --presentation "$PRESENTATION" \
    --repo-cwd "$REPO_CWD" \
    --machine self
  if [ "$LAUNCH" -eq 1 ]; then
    set -- "$@" --launch
  fi
  python3 "$@"
done
