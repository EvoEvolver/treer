#!/bin/sh
set -eu
LAUNCHER_ROOT="$(CDPATH= cd -- "$(dirname "$0")/.." && pwd -P)"
REPO_ROOT="$(CDPATH= cd -- "$LAUNCHER_ROOT/../.." && pwd -P)"
# Save profiles only; creation/restart is always a separate explicit operation.
exec node "$LAUNCHER_ROOT/scripts/install.mjs" --source "$REPO_ROOT" "$@" </dev/null
