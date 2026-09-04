#!/bin/sh
set -eu
root="$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)"
src="${CODEX_AGENT_UI_DIST:-$HOME/dev/codex-agent-ui/apps/web/dist}"
dest="$root/mobile/agent-ui"
if [ ! -f "$src/index.html" ]; then
  echo "missing $src/index.html; build codex-agent-ui web first" >&2
  exit 1
fi
mkdir -p "$dest"
rm -rf "$dest"/*
cp -R "$src"/. "$dest"/
echo "copied agent UI bundle to $dest"
