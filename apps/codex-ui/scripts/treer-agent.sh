#!/bin/sh
# Compatibility entry point for Agents saved before Codex UI became the shared
# Agent UI. It always selects the Codex harness.
set -eu
exec "$(CDPATH= cd -- "$(dirname "$0")/../../agent-ui/scripts" && pwd)/treer-agent.sh" --harness codex "$@"
