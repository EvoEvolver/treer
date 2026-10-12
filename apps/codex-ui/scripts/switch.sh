#!/bin/sh
# Compatibility entry point; see apps/agent-ui/README.md.
set -eu
exec "$(CDPATH= cd -- "$(dirname "$0")/../../agent-ui/scripts" && pwd)/switch.sh" --harness codex "$@"
