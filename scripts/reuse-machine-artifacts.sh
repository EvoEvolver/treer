#!/bin/sh
# Reuse the native machine artifacts of an earlier canary_passed commit when the
# candidate changes nothing the binaries are built from. Fail-closed: any doubt
# exits non-zero without touching dist/, and the caller falls back to a native build.
set -eu
root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$root"
commit=$(git rev-parse "${1:?candidate commit is required}^{commit}")
requested=
check_only=0
verify_dist=
shift
while [ "$#" -gt 0 ]; do
    case "$1" in
        --check-only) check_only=1 ;;
        --verify-dist) shift; verify_dist=${1:?--verify-dist needs a directory} ;;
        *) requested=$1 ;;
    esac
    shift
done
common=$(git rev-parse --path-format=absolute --git-common-dir)
checkouts=${TREER_ARTIFACT_REUSE_ROOT:-$(dirname "$common")/.treer/canary-checkouts}
if [ -n "$verify_dist" ]; then
    # Prove an existing reused dist is the one a canary_passed release recorded: its
    # provenance names the passed release, whose manifest must list identical hashes.
    provenance="$verify_dist/artifact-provenance.json"
    [ -f "$provenance" ] || { echo "dist has no reuse provenance" >&2; exit 1; }
    passed=$(jq -er '.passed_release_commit' "$provenance") || exit 1
    printf '%s' "$passed" | grep -Eq '^[0-9a-f]{40}$' || exit 1
    manifest="$checkouts/$passed/.treer/releases/$passed/manifest.json"
    [ -f "$manifest" ] || { echo "no manifest for the passed release $passed" >&2; exit 1; }
    jq -e --arg passed "$passed" '.status == "canary_passed" and .commit == $passed' "$manifest" >/dev/null || exit 1
    recorded=$(jq -cS '.machine_artifacts.files // empty' "$manifest") || exit 1
    [ -n "$recorded" ] && [ "$recorded" = "$(jq -cS '.files' "$provenance")" ] || {
        echo "reused dist hashes differ from the passed release's manifest" >&2; exit 1
    }
    sh scripts/verify-machine-artifacts.sh "$commit" "$verify_dist"
    exit 0
fi
[ ! -e dist ] || { echo "dist already exists; refusing to replace it" >&2; exit 1; }
# Only these paths cannot influence the CLI, Host or Controller binaries. Anything
# else, including unknown paths, requires a native build at the exact commit.
allowed='^((web|apps|docs)/|README\.md$)'

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
    # Anchor the bytes to what the Canary run recorded: the manifest of the passed
    # release must list all 25 file hashes and the dist must still match them.
    # Older manifests without hashes are never a reuse source.
    recorded=$(jq -cS '.machine_artifacts.files // empty' "$manifest" 2>/dev/null) || continue
    [ -n "$recorded" ] && [ "$(printf '%s' "$recorded" | jq 'length')" = 25 ] || {
        echo "not reusing $source: its manifest records no artifact hashes" >&2
        continue
    }
    actual=$(sh scripts/machine-artifact-hashes.sh "$release/dist" 2>/dev/null | jq -cS .) || continue
    [ "$actual" = "$recorded" ] || {
        echo "not reusing $source: dist bytes differ from the hashes recorded at canary_passed" >&2
        continue
    }
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
    if [ "$check_only" = 1 ]; then
        echo "reusable: native artifacts of $source can be reused for $commit"
        exit 0
    fi
    # If the passed release itself reused earlier binaries, keep the commit they
    # were really built from as the source and record the passed release apart.
    built=$source
    if [ -f "$release/dist/artifact-provenance.json" ]; then
        built=$(jq -er '.source_commit' "$release/dist/artifact-provenance.json") || continue
        printf '%s' "$built" | grep -Eq '^[0-9a-f]{40}$' || continue
    fi
    temporary=$(mktemp -d "$root/.dist-reuse.XXXXXX")
    cp -R "$release/dist" "$temporary/dist"
    rm -f "$temporary/dist/artifact-provenance.json"
    hashes=$(sh scripts/machine-artifact-hashes.sh "$temporary/dist") || { rm -rf "$temporary"; continue; }
    jq -n --arg source "$built" --arg passed "$source" --arg candidate "$commit" --argjson files "$hashes" \
        '{schema_version: 1, source_commit: $source, passed_release_commit: $passed, candidate_commit: $candidate,
          reason: "only web/, apps/ or docs/ changed since the source commit", files: $files}' \
        > "$temporary/dist/artifact-provenance.json"
    if sh scripts/verify-machine-artifacts.sh "$commit" "$temporary/dist" >/dev/null 2>&1; then
        mv "$temporary/dist" dist
        rmdir "$temporary"
        echo "reusing native artifacts of $source (built from $built) for $commit; $(printf '%s\n' "$changed" | grep -c .) changed paths, all under web/, apps/ or docs/"
        exit 0
    fi
    rm -rf "$temporary"
done
exit 1
