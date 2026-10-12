#!/bin/sh
set -eu
LAUNCHER_ROOT="$(CDPATH= cd -- "$(dirname "$0")/.." && pwd -P)"
exec "$LAUNCHER_ROOT/node_modules/.bin/tsx" "$LAUNCHER_ROOT/scripts/session.ts" "$@"
