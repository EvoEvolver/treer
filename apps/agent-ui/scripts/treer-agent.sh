#!/bin/sh
set -eu

APP_ROOT="$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)"
HARNESS=codex
ACP_ARGS='[]'
while [ "$#" -gt 0 ]; do
  case "$1" in
    --harness)
      if [ "$#" -lt 2 ] || [ -z "$2" ]; then echo "--harness requires codex or acp" >&2; exit 2; fi
      HARNESS="$2"
      shift 2 ;;
    --session-id|--thread-id)
      if [ "$#" -lt 2 ] || [ -z "$2" ]; then echo "$1 requires an ID" >&2; exit 2; fi
      export AGENT_UI_SESSION_ID="$2"
      shift 2 ;;
    --cwd)
      if [ "$#" -lt 2 ] || [ -z "$2" ]; then echo "--cwd requires a directory" >&2; exit 2; fi
      export AGENT_UI_CWD="$2"
      shift 2 ;;
    --acp-command)
      if [ "$#" -lt 2 ] || [ -z "$2" ]; then echo "--acp-command requires an executable" >&2; exit 2; fi
      export AGENT_UI_ACP_COMMAND="$2"
      shift 2 ;;
    --acp-arg)
      if [ "$#" -lt 2 ]; then echo "--acp-arg requires a value" >&2; exit 2; fi
      ACP_ARGS="$(python3 -c 'import json, sys; print(json.dumps(json.loads(sys.argv[1]) + [sys.argv[2]]))' "$ACP_ARGS" "$2")"
      shift 2 ;;
    --name)
      if [ "$#" -lt 2 ] || [ -z "$2" ]; then echo "--name requires a value" >&2; exit 2; fi
      export AGENT_UI_NAME="$2"
      shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done
case "$HARNESS" in
  codex) ;;
  acp) [ -n "${AGENT_UI_ACP_COMMAND:-}" ] || { echo "--harness acp requires --acp-command" >&2; exit 2; } ;;
  *) echo "unknown harness: $HARNESS (expected codex or acp)" >&2; exit 2 ;;
esac
export AGENT_UI_HARNESS="$HARNESS"
export AGENT_UI_ACP_ARGS="$ACP_ARGS"
INSTANCE_ID="${AGENT_UI_INSTANCE_ID:-agent-ui-${HARNESS}-${TREER_AGENT_ID:-local}-$$}"
export AGENT_UI_CWD="${AGENT_UI_CWD:-${CODEX_UI_CWD:-$(pwd)}}"
export AGENT_UI_INSTANCE_ID="$INSTANCE_ID"
export AGENT_UI_PUBLIC_DIR="${AGENT_UI_PUBLIC_DIR:-$APP_ROOT/public}"

export PATH="${HOME}/.local/bin:${HOME}/.npm-global/bin:${HOME}/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:${PATH}"
for NODE_BIN_DIR in "${HOME}"/.nvm/versions/node/*/bin; do
  if [ -x "$NODE_BIN_DIR/node" ]; then
    PATH="$NODE_BIN_DIR:$PATH"
  fi
done
export PATH
if command -v fnm >/dev/null 2>&1; then
  eval "$(fnm env --shell=sh)"
fi
if [ -z "${CODEX_BIN:-}" ] && command -v codex >/dev/null 2>&1; then
  export CODEX_BIN="$(command -v codex)"
fi

if [ ! -f "$AGENT_UI_PUBLIC_DIR/index.html" ]; then
  echo "Agent UI assets are missing from $AGENT_UI_PUBLIC_DIR" >&2
  exit 1
fi
if [ ! -x "$APP_ROOT/node_modules/.bin/tsx" ] || [ ! -f "$APP_ROOT/node_modules/ws/package.json" ]; then
  echo "Agent UI dependencies are missing; run npm --prefix $APP_ROOT install" >&2
  exit 1
fi

umask 077
PORT_FILE="$(mktemp "${TMPDIR:-/tmp}/agent-ui-port.XXXXXXXX")"
export AGENT_UI_PORT_FILE="$PORT_FILE"
"$APP_ROOT/node_modules/.bin/tsx" "$APP_ROOT/src/index.ts" &
SERVER_PID=$!
cleanup() {
  if command -v treer >/dev/null 2>&1; then
    treer interface clear >/dev/null 2>&1 || true
  fi
  kill "$SERVER_PID" 2>/dev/null || true
  rm -f "$PORT_FILE"
}
trap cleanup EXIT
trap 'exit 0' INT TERM

i=0
while [ "$i" -lt 180 ]; do
  if [ -s "$PORT_FILE" ]; then
    PORT="$(cat "$PORT_FILE")"
    HEALTH="http://127.0.0.1:${PORT}/api/health"
    if curl --noproxy '*' -sf "$HEALTH" >/dev/null 2>&1; then break; fi
  fi
  if ! kill -0 "$SERVER_PID" 2>/dev/null; then
    echo "Agent UI server exited before becoming healthy" >&2
    exit 1
  fi
  i=$((i + 1))
  sleep 0.5
done
if [ -z "${HEALTH:-}" ] || ! curl --noproxy '*' -sf "$HEALTH" >/dev/null 2>&1; then
  echo "Agent UI did not become ready on port ${PORT:-unbound}" >&2
  exit 1
fi

if ! command -v treer >/dev/null 2>&1; then
  echo "treer CLI is not on PATH; cannot register Agent Interface" >&2
  exit 1
fi

register() {
  treer interface register \
    --port "$PORT" \
    --instance-id "$INSTANCE_ID" \
    --capability prompt.submit \
    --capability transcript.read \
    --capability state.observe \
    --capability abort \
    --ui-path /
}

register
echo "registered Agent UI ($HARNESS) AIS $INSTANCE_ID on private port $PORT"

wait "$SERVER_PID"
