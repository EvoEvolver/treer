#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$root"

revision=${1:-HEAD}
commit=$(git rev-parse "$revision^{commit}")
repository=${TREER_GITHUB_REPOSITORY:-EvoEvolver/treer}
workflow=${TREER_ARTIFACT_WORKFLOW:-build-release-artifacts.yml}
run_id=${TREER_ARTIFACT_RUN_ID:-}

command -v gh >/dev/null 2>&1 || { echo "GitHub CLI is required" >&2; exit 1; }
command -v jq >/dev/null 2>&1 || { echo "jq is required" >&2; exit 1; }

if [ -z "$run_id" ]; then
    run_id=$(gh run list \
        --repo "$repository" \
        --workflow "$workflow" \
        --commit "$commit" \
        --status success \
        --limit 20 \
        --json databaseId,headSha,createdAt \
        | jq -er --arg commit "$commit" \
            'map(select(.headSha == $commit)) | sort_by(.createdAt) | last | .databaseId') || {
        echo "no successful $workflow run found for $commit" >&2
        exit 1
    }
fi

temporary=$(mktemp -d "${TMPDIR:-/tmp}/treer-artifacts.XXXXXX")
cleanup() {
    rm -rf "$temporary"
}
trap cleanup EXIT HUP INT TERM

gh run download "$run_id" --repo "$repository" --dir "$temporary"

verify_checksums() {
    directory=$1
    if command -v sha256sum >/dev/null 2>&1; then
        (cd "$directory" && sha256sum -c SHA256SUMS)
    else
        (cd "$directory" && shasum -a 256 -c SHA256SUMS)
    fi
}

platforms="linux-x86_64 linux-aarch64 darwin-x86_64 darwin-aarch64"

for platform in $platforms; do
    source="$temporary/treer-$platform"
    metadata="$source/build-metadata.json"
    [ -d "$source" ] && [ -f "$metadata" ] || {
        echo "workflow run $run_id is missing treer-$platform" >&2
        exit 1
    }
    jq -e --arg commit "$commit" --arg platform "$platform" \
        '.schema_version == 1 and .git_commit == $commit and .platform == $platform' \
        "$metadata" >/dev/null || {
        echo "treer-$platform metadata does not match $commit" >&2
        exit 1
    }
    chmod 755 "$source/treer" "$source/treer-agent-host" "$source/treer-agent-server"
    verify_checksums "$source"

    destination="$root/dist/$platform"
    [ ! -e "$destination" ] || {
        echo "$destination already exists; remove it before collecting a different build" >&2
        exit 1
    }
done

mkdir -p "$root/dist"
for platform in $platforms; do
    mv "$temporary/treer-$platform" "$root/dist/$platform"
done

echo "collected release artifacts for $commit from workflow run $run_id"
