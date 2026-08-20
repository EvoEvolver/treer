#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$root"

manifest_arg=${1:-}
[ -n "$manifest_arg" ] || {
    echo "usage: scripts/promote-production.sh <release-manifest>" >&2
    exit 2
}
case "$manifest_arg" in
    /*) manifest=$manifest_arg ;;
    *) manifest="$root/$manifest_arg" ;;
esac
[ -f "$manifest" ] || { echo "release manifest not found: $manifest" >&2; exit 1; }

sha256_file() {
    if command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$1" | awk '{print $1}'
    else
        sha256sum "$1" | awk '{print $1}'
    fi
}

schema_version=$(jq -er '.schema_version' "$manifest")
status=$(jq -er '.status' "$manifest")
commit=$(jq -er '.commit' "$manifest")
[ "$schema_version" = 1 ] && [ "$status" = canary_passed ] || {
    echo "manifest is not eligible for production promotion" >&2
    exit 1
}
[ "$(git rev-parse HEAD)" = "$commit" ] || {
    echo "check out release commit $commit before promotion" >&2
    exit 1
}
git diff --quiet && git diff --cached --quiet && \
    [ -z "$(git ls-files --others --exclude-standard)" ] || {
    echo "promotion requires a clean worktree" >&2
    exit 1
}
release_ref=${TREER_RELEASE_REMOTE_REF:-origin/main}
git merge-base --is-ancestor "$commit" "$release_ref" || {
    echo "release commit $commit is not contained in $release_ref" >&2
    exit 1
}

artifact_rel=$(jq -er '.app_artifact.path' "$manifest")
artifact="$root/$artifact_rel"
expected_sha=$(jq -er '.app_artifact.sha256' "$manifest")
[ -f "$artifact" ] || { echo "release artifact not found: $artifact" >&2; exit 1; }
actual_sha=$(sha256_file "$artifact")
[ "$actual_sha" = "$expected_sha" ] || {
    echo "release artifact checksum does not match the Canary manifest" >&2
    exit 1
}

project_id=${TREER_RAILWAY_PROJECT_ID:-09d4eeeb-cc5a-49e5-bcfe-f43c1c1b112b}
environment=${TREER_PRODUCTION_ENVIRONMENT:-production}
proxy_service=${TREER_PRODUCTION_PROXY_SERVICE:-c7f70740-49fa-4ab9-bb41-97b82f99dcce}
proxy_url=${TREER_PRODUCTION_PROXY_URL:-https://proxy.treer.ai/}
app_url=${TREER_PRODUCTION_APP_URL:-https://app.treer.ai/}
ingress_url=${TREER_PRODUCTION_INGRESS_URL:-https://apps.treer.ai/}
worker_environment=${TREER_PRODUCTION_WORKER_ENVIRONMENT:-production}
timeout=${TREER_PRODUCTION_DEPLOY_TIMEOUT:-900}
short_commit=$(git rev-parse --short "$commit")
proxy_url=${proxy_url%/}
app_url=${app_url%/}

command -v railway >/dev/null 2>&1 || { echo "railway CLI is required" >&2; exit 1; }
command -v jq >/dev/null 2>&1 || { echo "jq is required" >&2; exit 1; }
command -v curl >/dev/null 2>&1 || { echo "curl is required" >&2; exit 1; }
command -v pnpm >/dev/null 2>&1 || { echo "pnpm is required" >&2; exit 1; }

lock="$root/.treer/release.lock"
mkdir "$lock" 2>/dev/null || {
    echo "another Treer release is active: $lock" >&2
    exit 1
}
trap 'rmdir "$lock" 2>/dev/null || true' EXIT HUP INT TERM

mkdir -p web/dist
cp "$artifact" web/dist/index.html

railway variable set \
    "TREER_PROXY_PUBLIC_URL=$proxy_url" \
    "TREER_APP_PUBLIC_URL=$app_url" \
    "TREER_INGRESS_PUBLIC_URL=${ingress_url%/}" \
    "TREER_BUILD_COMMIT=$commit" \
    --project "$project_id" --environment "$environment" \
    --service "$proxy_service" --skip-deploys --json >/dev/null

proxy_deploy=$(railway up --detach --json \
    --project "$project_id" --environment "$environment" \
    --service "$proxy_service" --message "production $short_commit")
proxy_deployment_id=$(printf '%s' "$proxy_deploy" | jq -er '.deploymentId // .id')

(cd web && pnpm exec wrangler deploy --env "$worker_environment" \
    --message "production $commit" --tag "$short_commit")

deadline=$(( $(date +%s) + timeout ))
deployment_status=UNKNOWN
while [ "$(date +%s)" -lt "$deadline" ]; do
    deployment_status=$(railway deployment list --project "$project_id" \
        --environment "$environment" --service "$proxy_service" --json \
        | jq -r --arg id "$proxy_deployment_id" \
            '.[] | select(.id == $id) | .status // "UNKNOWN"')
    case "$deployment_status" in
        SUCCESS) break ;;
        FAILED|CRASHED|REMOVED|CANCELLED)
            echo "production deployment ended with $deployment_status" >&2
            exit 1
            ;;
    esac
    sleep 5
done
[ "$deployment_status" = SUCCESS ] || {
    echo "timed out waiting for production deployment" >&2
    exit 1
}

curl -fsS --retry 12 --retry-all-errors "$proxy_url/api/health" >/dev/null
curl -fsS --retry 12 --retry-all-errors "$app_url/health" \
    | jq -e '.service == "treer-app" and .status == "ok" and .environment == "production"' >/dev/null
curl -fsS --retry 12 --retry-all-errors "$app_url/config.json" \
    | jq -e --arg proxy "$proxy_url/" '.proxy_url == $proxy' >/dev/null
wildcard_status=$(curl -sS -o /dev/null -w '%{http_code}:%{ssl_verify_result}' \
    "https://release-check.apps.treer.ai/")
[ "$wildcard_status" = 404:0 ] || {
    echo "production wildcard check failed: $wildcard_status" >&2
    exit 1
}

worker_message="production $commit"
worker_version_id=
attempt=0
while [ "$attempt" -lt 12 ]; do
    worker_version_id=$(cd web && pnpm exec wrangler versions list \
        --env "$worker_environment" --json | jq -er --arg message "$worker_message" \
        'map(select(.annotations["workers/message"] == $message))
         | sort_by(.number) | last | .id' 2>/dev/null) && break
    attempt=$(( attempt + 1 ))
    sleep 2
done
[ -n "$worker_version_id" ] || {
    echo "could not resolve the Production Worker version for $commit" >&2
    exit 1
}
promoted_at=$(date -u +%Y-%m-%dT%H:%M:%SZ)
updated="$manifest.tmp.$$"
jq \
    --arg promoted_at "$promoted_at" \
    --arg proxy_deployment_id "$proxy_deployment_id" \
    --arg worker_name "treer-app" \
    --arg worker_version_id "$worker_version_id" \
    '.status = "production_deployed"
     | .production = {
         promoted_at: $promoted_at,
         proxy_deployment_id: $proxy_deployment_id,
         worker_name: $worker_name,
         worker_version_id: $worker_version_id
       }' "$manifest" > "$updated"
mv "$updated" "$manifest"

echo "Production promotion passed: $manifest"
