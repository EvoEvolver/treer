#!/bin/sh
# Deploy the checked-out commit to the hosted SaaS (Railway Proxy + Cloudflare App Worker).
#   deploy-hosted.sh canary|production
# Requires a clean worktree, railway, jq, curl, pnpm and (unless dist/ is present) gh.
# Production additionally requires the commit to be contained in origin/main.
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$root"
target=${1:?usage: scripts/deploy-hosted.sh canary|production}
project_id=${TREER_RAILWAY_PROJECT_ID:-09d4eeeb-cc5a-49e5-bcfe-f43c1c1b112b}
proxy_service=${TREER_PROXY_SERVICE:-c7f70740-49fa-4ab9-bb41-97b82f99dcce}
timeout=${TREER_DEPLOY_TIMEOUT:-900}
case "$target" in
    canary)
        environment=${TREER_RAILWAY_ENVIRONMENT:-canary}
        proxy_url=${TREER_PROXY_URL:-https://proxy.canary.treer.ai/}
        app_url=${TREER_APP_URL:-https://app.canary.treer.ai/}
        ingress_url=${TREER_INGRESS_URL:-https://canary.apps.treer.ai/}
        domain_id=${TREER_CANARY_DOMAIN_ID:-aabf784e-48e1-4ffb-b1ea-645f83ebf713} ;;
    production)
        environment=${TREER_RAILWAY_ENVIRONMENT:-production}
        proxy_url=${TREER_PROXY_URL:-https://proxy.treer.ai/}
        app_url=${TREER_APP_URL:-https://app.treer.ai/}
        ingress_url=${TREER_INGRESS_URL:-https://apps.treer.ai/} ;;
    *) echo "unknown target $target (canary or production)" >&2; exit 2 ;;
esac
worker_environment=${TREER_WORKER_ENVIRONMENT:-$target}
proxy_url=${proxy_url%/}
app_url=${app_url%/}
commit=$(git rev-parse HEAD)
short_commit=$(git rev-parse --short "$commit")

for tool in railway jq curl pnpm; do
    command -v "$tool" >/dev/null 2>&1 || { echo "$tool is required" >&2; exit 1; }
done
git diff --quiet && git diff --cached --quiet && \
    [ -z "$(git ls-files --others --exclude-standard)" ] || {
    echo "deployment requires a clean worktree" >&2; exit 1;
}
if [ "$target" = production ]; then
    git fetch origin main
    git merge-base --is-ancestor "$commit" origin/main || {
        echo "commit $commit is not contained in origin/main" >&2; exit 1;
    }
fi
lock="$root/.treer/deploy.lock"
mkdir -p "$root/.treer"
mkdir "$lock" 2>/dev/null || { echo "another Treer deployment is active: $lock" >&2; exit 1; }
upload=$(mktemp -d "${TMPDIR:-/tmp}/treer-proxy-upload.XXXXXX")
trap 'rm -rf "$upload"; rmdir "$lock" 2>/dev/null || true' EXIT HUP INT TERM

# Upload = committed source + verified native artifacts; Dockerfile.hosted bundles them.
sh scripts/fetch-machine-artifacts.sh "$commit"
git archive "$commit" | tar -x -C "$upload"
cp -R dist "$upload/dist"
jq '.build.dockerfilePath = "Dockerfile.hosted"' railway.json > "$upload/railway.json"
sh scripts/verify-machine-artifacts.sh "$commit" "$upload/dist"
pnpm --dir web install --frozen-lockfile
pnpm --dir web build

railway_scope="--project $project_id --environment $environment --service $proxy_service"
# Record the serving deployment so a confirmed failure names the rollback target.
previous=$(railway deployment list $railway_scope --json \
    | jq -r 'map(select(.status == "SUCCESS")) | sort_by(.createdAt) | last | .id // "none"')
echo "previous $target Proxy deployment: $previous"

railway variable set \
    "TREER_PROXY_PUBLIC_URL=$proxy_url" \
    "TREER_APP_PUBLIC_URL=$app_url" \
    "TREER_INGRESS_PUBLIC_URL=${ingress_url%/}" \
    "TREER_ENABLE_CORE_MESSAGES=true" \
    "TREER_BUILD_COMMIT=$commit" \
    $railway_scope --skip-deploys --json >/dev/null
deploy=$(sh scripts/railway-retry.sh 1 0 -- railway up "$upload" --path-as-root --no-gitignore --detach --json \
    $railway_scope --message "$target $short_commit")
deployment_id=$(printf '%s' "$deploy" | jq -er '.deploymentId // .id') || {
    echo "railway up returned no deployment id: $(printf '%s' "$deploy" | sh scripts/railway-error-summary.sh)" >&2
    exit 1
}
echo "Proxy deployment $deployment_id started"
proxy_failed() {
    echo "$1" >&2
    echo "The Console Worker was not deployed. A deployment that never became healthy leaves $previous serving;" >&2
    echo "if $deployment_id did go live, roll back to $previous (Railway dashboard: Deployments > Rollback)." >&2
    exit 1
}

# The Proxy must serve this commit before the Console that depends on it ships.
deadline=$(( $(date +%s) + timeout ))
status=UNKNOWN
while [ "$status" != SUCCESS ]; do
    [ "$(date +%s)" -lt "$deadline" ] || proxy_failed "timed out waiting for deployment $deployment_id (last status $status)"
    sleep 5
    status=$(railway deployment list $railway_scope --json \
        | jq -r --arg id "$deployment_id" '.[] | select(.id == $id) | .status // "UNKNOWN"') || status=UNKNOWN
    case "$status" in
        FAILED|CRASHED|REMOVED|CANCELLED) proxy_failed "deployment $deployment_id ended with $status" ;;
    esac
done
health=$(curl -fsS --retry 12 --retry-all-errors "$proxy_url/api/health") \
    || proxy_failed "$proxy_url/api/health is not healthy after deployment $deployment_id"
printf '%s' "$health" | jq -e --arg commit "$commit" '.status == "ok" and .commit == $commit' >/dev/null \
    || proxy_failed "$proxy_url/api/health does not report $commit: $health"
sh scripts/verify-served-artifacts.sh "$proxy_url" "$upload/dist" \
    || proxy_failed "$proxy_url serves artifacts that differ from $commit"

(cd web && pnpm exec wrangler deploy --env "$worker_environment" \
    --message "$target $commit" --tag "$short_commit")
curl -fsS --retry 12 --retry-all-errors "$app_url/health" \
    | jq -e --arg env "$target" '.service == "treer-app" and .status == "ok" and .environment == $env' >/dev/null
curl -fsS --retry 12 --retry-all-errors "$app_url/config.json" \
    | jq -e --arg proxy "$proxy_url/" '.proxy_url == $proxy' >/dev/null
python3 scripts/verify-served-frontend.py web/dist/index.html "$app_url/"
if [ "$target" = canary ]; then
    certificate=$(railway domain status "$domain_id" --project "$project_id" \
        --environment "$environment" --service "$proxy_service" --json \
        | jq -r '.domain.certificate.status')
    case "$certificate" in
        CERTIFICATE_STATUS_TYPE_VALID|CERTIFICATE_STATUS_TYPE_ISSUED) ;;
        *) echo "Canary wildcard DNS or certificate is not ready: $certificate" >&2; exit 1 ;;
    esac
else
    wildcard=$(curl -sS -o /dev/null -w '%{http_code}:%{ssl_verify_result}' "https://release-check.${ingress_url#https://}")
    [ "$wildcard" = 404:0 ] || { echo "production wildcard check failed: $wildcard" >&2; exit 1; }
fi
echo "$target is serving $short_commit (Railway deployment $deployment_id)"
