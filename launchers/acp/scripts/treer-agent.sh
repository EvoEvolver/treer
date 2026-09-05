#!/bin/sh
set -eu

LAUNCHER_ROOT="$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)"
HARNESS=""
BASE_COMMAND=""
SERVER_COMMAND=""
SESSION_ID=""
UI=""

while [ "$#" -gt 0 ]; do
  case "$1" in
    --harness)
      HARNESS="$2"
      shift 2
      ;;
    --base-command)
      BASE_COMMAND="$2"
      shift 2
      ;;
    --server-command)
      SERVER_COMMAND="$2"
      shift 2
      ;;
    --session-id)
      SESSION_ID="$2"
      shift 2
      ;;
    --ui)
      UI="$2"
      shift 2
      ;;
    *)
      echo "unknown ACP launcher argument: $1" >&2
      exit 2
      ;;
  esac
done

if [ -z "$HARNESS" ]; then
  echo "--harness is required" >&2
  exit 2
fi
if [ -z "$BASE_COMMAND" ] || [ -z "$SERVER_COMMAND" ]; then
  echo "--base-command and --server-command are required" >&2
  exit 2
fi

case "$UI" in
  "")
    RUNTIME="$LAUNCHER_ROOT/.build/runtime-headless/release/treer-acp"
    ;;
  remote-codex)
    RUNTIME="$LAUNCHER_ROOT/.build/runtime-remote-codex/release/treer-acp"
    UI_DIST="$LAUNCHER_ROOT/.build/remote-codex-ui/source/apps/agent-ui-web/dist"
    if [ ! -f "$UI_DIST/index.html" ]; then
      echo "Remote Codex UI is not installed; rerun apply.sh with --ui remote-codex" >&2
      exit 1
    fi
    ;;
  *)
    echo "unsupported optional UI: $UI" >&2
    exit 2
    ;;
esac

if [ ! -x "$RUNTIME" ]; then
  echo "ACP runtime is not built; run launchers/acp/scripts/apply.sh first" >&2
  exit 1
fi

set -- "$RUNTIME" --cwd "$PWD" --harness "$HARNESS" \
  --base-command "$BASE_COMMAND" --server-command "$SERVER_COMMAND"
if [ -n "$SESSION_ID" ]; then
  set -- "$@" --session-id "$SESSION_ID"
fi
if [ "$UI" = "remote-codex" ]; then
  set -- "$@" --ui-dist "$UI_DIST"
fi
exec "$@"
