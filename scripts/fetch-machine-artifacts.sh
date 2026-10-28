#!/bin/sh
# Put verified native machine artifacts for one exact commit into ./dist/<platform>/.
#   fetch-machine-artifacts.sh [revision]
# An existing dist/ is verified, never overwritten. Otherwise the newest successful
# `Build release artifacts` run for the commit is downloaded (TREER_ARTIFACT_RUN_ID
# selects one explicitly); with no run at all, the workflow is dispatched for the
# commit and awaited. Requires authenticated gh and jq.
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$root"
commit=$(git rev-parse "${1:-HEAD}^{commit}")
repository=${TREER_GITHUB_REPOSITORY:-EvoEvolver/treer}
workflow=${TREER_ARTIFACT_WORKFLOW:-build-release-artifacts.yml}
workflow_ref=${TREER_ARTIFACT_WORKFLOW_REF:-main}
timeout=${TREER_ARTIFACT_BUILD_TIMEOUT:-2400}
run_id=${TREER_ARTIFACT_RUN_ID:-}
platforms="linux-x86_64 linux-aarch64 darwin-x86_64 darwin-aarch64 windows-x86_64"

if [ -d dist ]; then
    sh scripts/verify-machine-artifacts.sh "$commit" "$root/dist"
    exit 0
fi
command -v gh >/dev/null 2>&1 || { echo "GitHub CLI is required" >&2; exit 1; }
command -v jq >/dev/null 2>&1 || { echo "jq is required" >&2; exit 1; }

# A manual dispatch on ref C may build revision B; its title then names the revision.
find_run() {
    gh run list --repo "$repository" --workflow "$workflow" --limit 100 \
        --json databaseId,headSha,displayTitle,status,conclusion,createdAt \
        | jq -c --arg commit "$commit" \
        'map(select(.displayTitle == ("Machine artifacts " + $commit)
            or (.headSha == $commit and (.displayTitle | startswith("Machine artifacts ") | not))))
         | sort_by(.createdAt) | last // empty'
}

if [ -z "$run_id" ]; then
    deadline=$(( $(date +%s) + timeout ))
    dispatched=false
    while :; do
        run=$(find_run)
        if [ -n "$run" ] && [ "$(printf '%s' "$run" | jq -r '.status')" = completed ]; then
            [ "$(printf '%s' "$run" | jq -r '.conclusion')" = success ] || {
                echo "native artifact build failed: $run" >&2; exit 1;
            }
            run_id=$(printf '%s' "$run" | jq -r '.databaseId')
            break
        fi
        if [ -z "$run" ] && [ "$dispatched" = false ]; then
            gh workflow run "$workflow" --repo "$repository" --ref "$workflow_ref" -f revision="$commit"
            echo "building native artifacts for $commit"
            dispatched=true
        fi
        [ "$(date +%s)" -lt "$deadline" ] || {
            echo "timed out waiting for native artifacts for $commit" >&2; exit 1;
        }
        sleep 10
    done
fi

# An explicit run ID must not bypass a failed or incomplete run.
receipt=$(gh run view "$run_id" --repo "$repository" --json status,conclusion,headSha,displayTitle,jobs)
printf '%s' "$receipt" | jq -e --arg commit "$commit" '
    .status == "completed" and .conclusion == "success" and
    (.headSha == $commit or .displayTitle == ("Machine artifacts " + $commit))
' >/dev/null || { echo "artifact run $run_id is incomplete, failed or for another commit" >&2; exit 1; }

temporary=$(mktemp -d "${TMPDIR:-/tmp}/treer-artifacts.XXXXXX")
trap 'rm -rf "$temporary"' EXIT HUP INT TERM
gh run download "$run_id" --repo "$repository" --dir "$temporary"
mkdir "$temporary/dist"
for platform in $platforms; do
    [ -d "$temporary/treer-$platform" ] || { echo "run $run_id is missing treer-$platform" >&2; exit 1; }
    chmod 755 "$temporary/treer-$platform/treer" "$temporary/treer-$platform/treer-agent-host" \
        "$temporary/treer-$platform/treer-agent-server"
    mv "$temporary/treer-$platform" "$temporary/dist/$platform"
done
sh scripts/verify-machine-artifacts.sh "$commit" "$temporary/dist"
mv "$temporary/dist" "$root/dist"
echo "collected machine artifacts for $commit from workflow run $run_id"
