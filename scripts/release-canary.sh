#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$root"

revision=${1:-HEAD}
commit=$(git rev-parse "$revision^{commit}")
head_commit=$(git rev-parse HEAD)
[ "$commit" = "$head_commit" ] || {
    echo "release commit $commit is not checked out (HEAD is $head_commit)" >&2
    exit 1
}
git diff --quiet && git diff --cached --quiet && \
    [ -z "$(git ls-files --others --exclude-standard)" ] || {
    echo "release requires a clean worktree" >&2
    exit 1
}
# Formal releases always use origin/main, regardless of preview ref overrides.
git fetch origin main
: "${TREER_RELEASE_SOURCE_REVIEW:?set an absolute previous-live source review JSON path}"
python3 scripts/release-source-preflight.py --candidate "$commit" --review "$TREER_RELEASE_SOURCE_REVIEW"

release_root="$root/.treer/releases/$commit"
. "$root/scripts/release-timing.sh"
TREER_TIMINGS_FILE=${TREER_TIMINGS_FILE:-$release_root/timings.tsv}
export TREER_TIMINGS_FILE
lock="$root/.treer/release.lock"
mkdir -p "$root/.treer/releases"
mkdir "$lock" 2>/dev/null || {
    echo "another Treer release is active: $lock" >&2
    exit 1
}
cleanup() {
    rm -f "$release_root/canary-deploy.json"
    rmdir "$lock" 2>/dev/null || true
}
trap cleanup EXIT HUP INT TERM

sha256_file() {
    if command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$1" | awk '{print $1}'
    else
        sha256sum "$1" | awk '{print $1}'
    fi
}

mkdir -p "$release_root/app"
checkpoint="$release_root/check-passed.json"
if [ "${TREER_RELEASE_RESUME:-0}" = 1 ] && \
    python3 scripts/release-checkpoint.py verify "$checkpoint" "$commit" web/dist/index.html; then
    echo "Reusing the completed local gate for $commit"
else
    rm -f "$checkpoint"
    just test-db-up
    # The release gate takes priority over author gates for the machine-wide slot.
    TREER_GATE_ROLE=release TREER_GATE_NAME=release just check
    python3 scripts/release-checkpoint.py write "$checkpoint" "$commit" web/dist/index.html
fi
cp web/dist/index.html "$release_root/app/index.html"
artifact_sha=$(sha256_file "$release_root/app/index.html")
lap local_gate

TREER_RELEASE_RESULT_FILE="$release_root/canary-deploy.json" \
    sh scripts/deploy-canary.sh "$commit"
lap deploy_total
TREER_CANARY_KEEP_RESOURCES=0 TREER_CANARY_SKIP_PUBLIC=0 \
    sh scripts/test-canary.sh
lap canary_e2e_total

tested_at=$(date -u +%Y-%m-%dT%H:%M:%SZ)
manifest="$release_root/manifest.json"
artifact_source=$commit
if [ -f dist/artifact-provenance.json ]; then
    artifact_source=$(jq -er '.source_commit' dist/artifact-provenance.json)
fi
artifact_hashes=$(sh scripts/machine-artifact-hashes.sh dist) || {
    echo "cannot record the machine artifact hashes" >&2; exit 1;
}
printf '%s' "$artifact_hashes" | jq -e 'length == 25 and all(.[]; test("^[0-9a-f]{64}$"))' >/dev/null || {
    echo "machine artifact hash receipt is incomplete" >&2; exit 1;
}
timings=$(jq -Rn '[inputs | split("\t") | select(length == 2) | {(.[0]): (.[1] | tonumber)}] | add // {}' "$TREER_TIMINGS_FILE" 2>/dev/null || echo '{}')
jq -n \
    --arg commit "$commit" \
    --arg created_at "$tested_at" \
    --arg artifact_path ".treer/releases/$commit/app/index.html" \
    --arg artifact_sha256 "$artifact_sha" \
    --arg artifact_source "$artifact_source" \
    --argjson artifact_hashes "$artifact_hashes" \
    --arg reuse_disabled "${TREER_REUSE_NATIVE_ARTIFACTS:-1}" \
    --slurpfile canary "$release_root/canary-deploy.json" \
    --argjson timings "$timings" \
    '{schema_version: 1,
      status: "canary_passed",
      commit: $commit,
      created_at: $created_at,
      app_artifact: {path: $artifact_path, sha256: $artifact_sha256},
      machine_artifacts: {source_commit: $artifact_source, reused: ($artifact_source != $commit), files: $artifact_hashes, reuse_enabled: ($reuse_disabled == "1")},
      timings_seconds: $timings,
      canary: ($canary[0] + {tested_at: $created_at})}' > "$manifest"

echo "Canary release passed: $manifest"
