#!/bin/sh
# Print {"<platform>/<file>": "<sha256>"} for every binary, SHA256SUMS and build-metadata.json in a verified dist.
# All 25 entries or a non-zero exit with no output: never a partial result.
set -eu
directory=${1:?artifact directory is required}
hash_file() {
    if command -v shasum >/dev/null 2>&1; then shasum -a 256 -- "$1"; else sha256sum -- "$1"; fi
}
lines=
for platform in linux-x86_64 linux-aarch64 darwin-x86_64 darwin-aarch64 windows-x86_64; do
    for file in treer treer-agent-host treer-agent-server SHA256SUMS build-metadata.json; do
        path="$directory/$platform/$file"
        [ -f "$path" ] && [ ! -L "$path" ] || { echo "missing $platform/$file" >&2; exit 1; }
        output=$(hash_file "$path") || { echo "cannot hash $platform/$file" >&2; exit 1; }
        digest=${output%% *}
        printf '%s' "$digest" | grep -Eq '^[0-9a-f]{64}$' || { echo "invalid hash for $platform/$file" >&2; exit 1; }
        lines="$lines$platform/$file	$digest
"
    done
done
printf '%s' "$lines" | jq -Rn '[inputs | select(length > 0) | split("\t") | {(.[0]): .[1]}] | add
    | if length == 25 then . else error("expected 25 artifact hashes") end'
