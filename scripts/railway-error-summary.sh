#!/bin/sh
# Reads Railway CLI --json output on stdin and prints one bounded, sanitized line
# for a failed deployment upload. Railway may report a rejection as JSON on stdout,
# which command substitution would otherwise swallow. Never prints the raw body:
# only scalar error fields, with exact RAILWAY_*TOKEN* environment values, UUID-shaped
# and long token-like strings replaced, control characters removed and 300 characters kept.
set -eu
jq -r '[$ENV | to_entries[] | select(.key | test("^RAILWAY_.*TOKEN")) | .value | select(type == "string" and length > 0)] as $secrets
       | [.error?, .message?, .status?, .code?]
       | map(select(type == "string" or type == "number") | tostring) | join(" ")
       | reduce $secrets[] as $secret (.; split($secret) | join("[redacted]"))' 2>/dev/null \
    | tr -d '\000-\037\177' \
    | sed -E 's#[0-9A-Fa-f]{8}-([0-9A-Fa-f]{4}-){3}[0-9A-Fa-f]{12}#[redacted]#g; s#[A-Za-z0-9+/=_-]{32,}#[redacted]#g' \
    | cut -c1-300
