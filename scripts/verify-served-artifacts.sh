#!/bin/sh
set -eu
proxy_url=${1:?Proxy URL is required}
directory=${2:?expected artifact directory is required}
temporary=$(mktemp -d "${TMPDIR:-/tmp}/treer-served-artifacts.XXXXXX")
trap 'rm -rf "$temporary"' EXIT HUP INT TERM
for platform in linux-x86_64 linux-aarch64 darwin-x86_64 darwin-aarch64 windows-x86_64; do
    mkdir -p "$temporary/$platform"
    for binary in treer treer-agent-host treer-agent-server; do
        # No redirects: an old GitHub Release must never satisfy hosted acceptance.
        status=$(curl -sS --fail --retry 3 --retry-all-errors \
            -o "$temporary/$platform/$binary" -w '%{http_code}' \
            "${proxy_url%/}/artifacts/$platform/$binary")
        [ "$status" = 200 ] || { echo "$platform/$binary returned HTTP $status" >&2; exit 1; }
        cmp "$directory/$platform/$binary" "$temporary/$platform/$binary" || {
            echo "served $platform/$binary differs from the candidate" >&2; exit 1;
        }
        echo "verified served $platform/$binary"
    done
done
