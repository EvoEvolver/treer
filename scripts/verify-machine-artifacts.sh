#!/bin/sh
set -eu
commit=${1:?source commit is required}
directory=${2:?artifact directory is required}
version=
built_commit=$commit
# Artifacts built for an earlier commit are acceptable only with explicit
# provenance, and only when git proves the candidate differs from that commit
# exclusively in paths the native binaries are not built from.
if [ -e "$directory/artifact-provenance.json" ]; then
    root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
    jq -e --arg commit "$commit" \
        '.schema_version == 1 and .candidate_commit == $commit
         and (.source_commit | type == "string" and test("^[0-9a-f]{40}$"))' \
        "$directory/artifact-provenance.json" >/dev/null || {
        echo "invalid artifact provenance for $commit" >&2; exit 1;
    }
    built_commit=$(jq -r '.source_commit' "$directory/artifact-provenance.json")
    git -C "$root" merge-base --is-ancestor "$built_commit" "$commit" || {
        echo "artifact source $built_commit is not an ancestor of $commit" >&2; exit 1;
    }
    changed=$(git -C "$root" diff --no-renames --no-ext-diff --name-only "$built_commit" "$commit")
    if [ -n "$changed" ] && printf '%s\n' "$changed" | grep -Evq '^(web|apps|docs)/'; then
        echo "native inputs changed since artifact source $built_commit" >&2; exit 1
    fi
fi
for platform in linux-x86_64 linux-aarch64 darwin-x86_64 darwin-aarch64 windows-x86_64; do
    source="$directory/$platform"
    jq -e --arg commit "$built_commit" --arg platform "$platform" \
        '.schema_version == 1 and .git_commit == $commit and .platform == $platform
         and (.version | type == "string" and length > 0)' \
        "$source/build-metadata.json" >/dev/null || {
        echo "missing or mismatched $platform artifacts for $built_commit" >&2; exit 1;
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
