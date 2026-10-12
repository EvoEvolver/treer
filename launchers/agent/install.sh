#!/bin/sh
set -eu
SCRIPT_ROOT="$(CDPATH= cd -- "$(dirname "$0")" && pwd -P)"
exec node "$SCRIPT_ROOT/scripts/install.mjs" "$@" </dev/null
