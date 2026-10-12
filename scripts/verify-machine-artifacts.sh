#!/bin/sh
set -eu
commit=${1:?source commit is required}
directory=${2:?artifact directory is required}
version=
for platform in linux-x86_64 linux-aarch64 darwin-x86_64 darwin-aarch64 windows-x86_64; do
    source="$directory/$platform"
    jq -e --arg commit "$commit" --arg platform "$platform" \
        '.schema_version == 1 and .git_commit == $commit and .platform == $platform
         and (.version | type == "string" and length > 0)' \
        "$source/build-metadata.json" >/dev/null || {
        echo "missing or mismatched $platform artifacts for $commit" >&2; exit 1;
    }
    current_version=$(jq -r '.version' "$source/build-metadata.json")
    [ -z "$version" ] || [ "$current_version" = "$version" ] || {
        echo "machine artifacts have different versions" >&2; exit 1;
    }
    version=$current_version
    # Reject partial manifests and unexpected paths before trusting checksum tools.
    awk '
        length($1) != 64 || $1 ~ /[^0-9a-f]/ || NF != 2 { exit 1 }
        $2 != "treer" && $2 != "treer-agent-host" && $2 != "treer-agent-server" { exit 1 }
        seen[$2]++ { exit 1 }
        END { if (NR != 3) exit 1 }
    ' "$source/SHA256SUMS" || { echo "invalid $platform checksum manifest" >&2; exit 1; }
    for binary in treer treer-agent-host treer-agent-server; do
        [ -f "$source/$binary" ] && [ ! -L "$source/$binary" ] || {
            echo "missing executable $platform/$binary" >&2; exit 1;
        }
        if [ "$platform" != windows-x86_64 ]; then
            [ -x "$source/$binary" ] || { echo "missing executable bit $platform/$binary" >&2; exit 1; }
        else
            [ "$(dd if="$source/$binary" bs=1 count=2 2>/dev/null)" = MZ ] || {
                echo "invalid Windows executable $platform/$binary" >&2; exit 1;
            }
        fi
    done
    if command -v sha256sum >/dev/null 2>&1; then
        (cd "$source" && sha256sum -c SHA256SUMS)
    else
        (cd "$source" && shasum -a 256 -c SHA256SUMS)
    fi
done
