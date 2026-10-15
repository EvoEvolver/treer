#!/bin/sh
# Reuse the native machine artifacts of an earlier canary_passed commit when the
# candidate changes nothing the binaries are built from. Fail-closed: any doubt
# exits non-zero without touching dist/, and the caller falls back to a native build.
set -eu
root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$root"
commit=$(git rev-parse "${1:?candidate commit is required}^{commit}")
requested=${2:-}
[ ! -e dist ] || { echo "dist already exists; refusing to replace it" >&2; exit 1; }
common=$(git rev-parse --path-format=absolute --git-common-dir)
checkouts=${TREER_ARTIFACT_REUSE_ROOT:-$(dirname "$common")/.treer/canary-checkouts}
# Only these paths cannot influence the CLI, Host or Controller binaries. Anything
# else, including unknown paths, requires a native build at the exact commit.
allowed='^(web|apps|docs)/'

candidates() {
    if [ -n "$requested" ]; then
        git rev-parse "$requested^{commit}"
        return
    fi
    for manifest in "$checkouts"/*/.treer/releases/*/manifest.json; do
        [ -f "$manifest" ] || continue
        jq -r 'select(.status == "canary_passed") | "\(.created_at) \(.commit)"' "$manifest" 2>/dev/null
    done | sort -r | awk '{print $2}'
}

for source in $(candidates); do
    printf '%s' "$source" | grep -Eq '^[0-9a-f]{40}$' || continue
    [ "$source" != "$commit" ] || continue
    release="$checkouts/$source"
    manifest="$release/.treer/releases/$source/manifest.json"
    [ -f "$manifest" ] && [ -d "$release/dist" ] && [ ! -L "$release/dist" ] || continue
    jq -e --arg source "$source" '.status == "canary_passed" and .commit == $source' "$manifest" >/dev/null 2>&1 || continue
    git merge-base --is-ancestor "$source" "$commit" 2>/dev/null || continue
    changed=$(git diff --no-renames --no-ext-diff --name-only "$source" "$commit") || continue
    if [ -n "$changed" ] && printf '%s\n' "$changed" | grep -Evq "$allowed"; then
        echo "not reusing $source: native inputs changed since it" >&2
        continue
    fi
    sh scripts/verify-machine-artifacts.sh "$source" "$release/dist" >/dev/null 2>&1 || {
        echo "not reusing $source: its artifacts failed verification" >&2
        continue
    }
    temporary=$(mktemp -d "$root/.dist-reuse.XXXXXX")
    cp -R "$release/dist" "$temporary/dist"
    jq -n --arg source "$source" --arg candidate "$commit" \
        '{schema_version: 1, source_commit: $source, candidate_commit: $candidate,
          reason: "only web/, apps/ or docs/ changed since the source commit"}' \
        > "$temporary/dist/artifact-provenance.json"
    if sh scripts/verify-machine-artifacts.sh "$commit" "$temporary/dist" >/dev/null 2>&1; then
        mv "$temporary/dist" dist
        rmdir "$temporary"
        echo "reusing native artifacts of $source for $commit"
        exit 0
    fi
    rm -rf "$temporary"
done
exit 1
