#!/bin/sh
# Print {"<platform>/<file>": "<sha256>"} for every binary, SHA256SUMS and build-metadata.json in a verified dist.
set -eu
directory=${1:?artifact directory is required}
hash_file() {
    if command -v shasum >/dev/null 2>&1; then shasum -a 256 "$1" | awk '{print $1}'; else sha256sum "$1" | awk '{print $1}'; fi
}
for platform in linux-x86_64 linux-aarch64 darwin-x86_64 darwin-aarch64 windows-x86_64; do
    for file in treer treer-agent-host treer-agent-server SHA256SUMS build-metadata.json; do
        path="$directory/$platform/$file"
        [ -f "$path" ] && [ ! -L "$path" ] || { echo "missing $platform/$file" >&2; exit 1; }
        printf '%s\t%s\n' "$platform/$file" "$(hash_file "$path")"
    done
done | jq -Rn '[inputs | split("\t") | {(.[0]): .[1]}] | add'
