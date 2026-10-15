#!/bin/sh
set -eu
root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$root"
commit=$(git rev-parse "${1:-HEAD}^{commit}")
repository=${TREER_GITHUB_REPOSITORY:-EvoEvolver/treer}
workflow=${TREER_ARTIFACT_WORKFLOW:-build-release-artifacts.yml}
workflow_ref=${TREER_ARTIFACT_WORKFLOW_REF:-main}
timeout=${TREER_ARTIFACT_BUILD_TIMEOUT:-2400}

if [ -d dist ]; then
    # Never silently replace stale or partially collected builds.
    sh scripts/verify-machine-artifacts.sh "$commit" "$root/dist"
    exit 0
fi
command -v gh >/dev/null 2>&1 || { echo "GitHub CLI is required" >&2; exit 1; }
deadline=$(( $(date +%s) + timeout ))
dispatched=false
while [ "$(date +%s)" -lt "$deadline" ]; do
    run=$(gh run list --repo "$repository" --workflow "$workflow" --limit 100 \
        --json databaseId,headSha,displayTitle,status,conclusion,createdAt \
        | jq -c --arg commit "$commit" \
        'map(select(.displayTitle == ("Machine artifacts " + $commit)
            or (.headSha == $commit and (.displayTitle | startswith("Machine artifacts ") | not))))
         | sort_by(.createdAt) | last // empty')
    if [ -n "$run" ]; then
        status=$(printf '%s' "$run" | jq -r '.status')
        if [ "$status" = completed ]; then
            conclusion=$(printf '%s' "$run" | jq -r '.conclusion')
            [ "$conclusion" = success ] || {
                echo "native artifact build failed: $run" >&2; exit 1;
            }
            TREER_ARTIFACT_RUN_ID=$(printf '%s' "$run" | jq -r '.databaseId') \
                sh scripts/collect-build-artifacts.sh "$commit"
            sh scripts/verify-machine-artifacts.sh "$commit" "$root/dist"
            exit 0
        fi
    elif [ "$dispatched" = false ]; then
        # No native run exists for this exact commit. If only web/, apps/ or docs/
        # changed since a canary_passed commit, its verified binaries are reused;
        # any doubt falls through to the normal native build.
        if [ "${TREER_REUSE_NATIVE_ARTIFACTS:-1}" = 1 ] && sh scripts/reuse-machine-artifacts.sh "$commit"; then
            sh scripts/verify-machine-artifacts.sh "$commit" "$root/dist"
            exit 0
        fi
        gh workflow run "$workflow" --repo "$repository" --ref "$workflow_ref" -f revision="$commit"
        echo "building native artifacts for $commit"
        dispatched=true
    fi
    sleep 10
done
echo "timed out waiting for native artifacts for $commit" >&2
exit 1
