#!/bin/sh
# Run one Railway CLI command with a bounded number of attempts.
#   railway-retry.sh ATTEMPTS BACKOFF_SECONDS -- command [args...]
# Only the command itself is retried. A failed attempt prints one sanitized summary
# line (see railway-error-summary.sh) on stderr; the last exit status is returned.
# Never use this around assertions or around commands that are not safe to repeat.
set -eu
attempts=${1:?attempt count is required}
backoff=${2:?backoff seconds are required}
[ "${3:-}" = -- ] || { echo "usage: railway-retry.sh ATTEMPTS BACKOFF -- command..." >&2; exit 2; }
shift 3
case "$attempts" in ''|*[!0-9]*|0) echo "attempts must be a positive integer" >&2; exit 2 ;; esac
case "$backoff" in ''|*[!0-9]*) echo "backoff must be a whole number of seconds" >&2; exit 2 ;; esac
root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
attempt=1
while :; do
    status=0
    output=$("$@" 2>/dev/null) || status=$?
    if [ "$status" -eq 0 ]; then
        [ -z "$output" ] || printf '%s\n' "$output"
        exit 0
    fi
    summary=$(printf '%s' "$output" | sh "$root/scripts/railway-error-summary.sh" 2>/dev/null || true)
    echo "railway attempt $attempt/$attempts failed (exit $status)${summary:+: $summary}" >&2
    [ "$attempt" -lt "$attempts" ] || exit "$status"
    sleep $(( backoff * attempt ))
    attempt=$(( attempt + 1 ))
done
