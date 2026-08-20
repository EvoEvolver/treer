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
release_ref=${TREER_RELEASE_REMOTE_REF:-origin/main}
git merge-base --is-ancestor "$commit" "$release_ref" || {
    echo "release commit $commit is not contained in $release_ref" >&2
    exit 1
}

release_root="$root/.treer/releases/$commit"
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
just test-db-up
just check
cp web/dist/index.html "$release_root/app/index.html"
artifact_sha=$(sha256_file "$release_root/app/index.html")

TREER_RELEASE_RESULT_FILE="$release_root/canary-deploy.json" \
    sh scripts/deploy-canary.sh "$commit"
TREER_CANARY_KEEP_RESOURCES=0 TREER_CANARY_SKIP_PUBLIC=0 \
    sh scripts/test-canary.sh

tested_at=$(date -u +%Y-%m-%dT%H:%M:%SZ)
manifest="$release_root/manifest.json"
jq -n \
    --arg commit "$commit" \
    --arg created_at "$tested_at" \
    --arg artifact_path ".treer/releases/$commit/app/index.html" \
    --arg artifact_sha256 "$artifact_sha" \
    --slurpfile canary "$release_root/canary-deploy.json" \
    '{schema_version: 1,
      status: "canary_passed",
      commit: $commit,
      created_at: $created_at,
      app_artifact: {path: $artifact_path, sha256: $artifact_sha256},
      canary: ($canary[0] + {tested_at: $created_at})}' > "$manifest"

echo "Canary release passed: $manifest"
