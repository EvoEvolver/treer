#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$root"

project_id=${TREER_RAILWAY_PROJECT_ID:-09d4eeeb-cc5a-49e5-bcfe-f43c1c1b112b}
environment=${TREER_CANARY_ENVIRONMENT:-canary}
proxy_service=${TREER_CANARY_PROXY_SERVICE:-c7f70740-49fa-4ab9-bb41-97b82f99dcce}
domain_id=${TREER_CANARY_DOMAIN_ID:-aabf784e-48e1-4ffb-b1ea-645f83ebf713}
proxy_url=${TREER_CANARY_PROXY_URL:-https://proxy.canary.treer.ai/}
app_url=${TREER_CANARY_APP_URL:-https://app.canary.treer.ai/}
ingress_url=${TREER_CANARY_INGRESS_URL:-https://canary.apps.treer.ai/}
worker_environment=${TREER_CANARY_WORKER_ENVIRONMENT:-canary}
result_file=${TREER_RELEASE_RESULT_FILE:-}
timeout=${TREER_CANARY_DEPLOY_TIMEOUT:-900}
revision=${1:-HEAD}
commit=$(git rev-parse "$revision^{commit}")
short_commit=$(git rev-parse --short "$commit")
proxy_url=${proxy_url%/}
app_url=${app_url%/}

command -v railway >/dev/null 2>&1 || { echo "railway CLI is required" >&2; exit 1; }
command -v jq >/dev/null 2>&1 || { echo "jq is required" >&2; exit 1; }
command -v curl >/dev/null 2>&1 || { echo "curl is required" >&2; exit 1; }
command -v pnpm >/dev/null 2>&1 || { echo "pnpm is required" >&2; exit 1; }

wait_for_deployment() {
    service=$1
    deployment=$2
    deadline=$(( $(date +%s) + timeout ))
    while [ "$(date +%s)" -lt "$deadline" ]; do
        status=$(railway deployment list \
            --project "$project_id" \
            --environment "$environment" \
            --service "$service" \
            --json | jq -r --arg id "$deployment" \
                '.[] | select(.id == $id) | .status // "UNKNOWN"')
        case "$status" in
            SUCCESS) return 0 ;;
            FAILED|CRASHED|REMOVED|CANCELLED)
                echo "deployment $deployment ended with $status" >&2
                return 1
                ;;
        esac
        sleep 5
    done
    echo "timed out waiting for deployment $deployment" >&2
    return 1
}

railway variable set \
    "TREER_PROXY_PUBLIC_URL=$proxy_url" \
    "TREER_APP_PUBLIC_URL=$app_url" \
    "TREER_INGRESS_PUBLIC_URL=${ingress_url%/}" \
    "TREER_ENABLE_CORE_MESSAGES=true" \
    "TREER_BUILD_COMMIT=$commit" \
    --project "$project_id" \
    --environment "$environment" \
    --service "$proxy_service" \
    --skip-deploys --json >/dev/null

proxy_deploy=$(railway up --detach --json \
    --project "$project_id" \
    --environment "$environment" \
    --service "$proxy_service" \
    --message "canary $short_commit")
proxy_deployment_id=$(printf '%s' "$proxy_deploy" | jq -er '.deploymentId // .id')

(cd web && pnpm exec wrangler deploy --env "$worker_environment" \
    --message "canary $commit" --tag "$short_commit")

wait_for_deployment "$proxy_service" "$proxy_deployment_id"
curl -fsS --retry 12 --retry-all-errors "$proxy_url/api/health" >/dev/null
curl -fsS --retry 12 --retry-all-errors "$app_url/health" \
    | jq -e '.service == "treer-app" and .status == "ok" and .environment == "canary"' >/dev/null
curl -fsS --retry 12 --retry-all-errors "$app_url/config.json" \
    | jq -e --arg proxy "$proxy_url/" '.proxy_url == $proxy' >/dev/null

worker_message="canary $commit"
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
    echo "could not resolve the Canary Worker version for $commit" >&2
    exit 1
}

domain=$(railway domain status "$domain_id" --project "$project_id" \
    --environment "$environment" --service "$proxy_service" --json)
certificate=$(printf '%s' "$domain" | jq -r '.domain.certificate.status')
case "$certificate" in
    CERTIFICATE_STATUS_TYPE_VALID|CERTIFICATE_STATUS_TYPE_ISSUED) ;;
    *)
        echo "Canary wildcard DNS or certificate is not ready: $certificate" >&2
        exit 1
        ;;
esac

if [ -n "$result_file" ]; then
    jq -n \
        --arg proxy_deployment_id "$proxy_deployment_id" \
        --arg worker_name "treer-app-canary" \
        --arg worker_version_id "$worker_version_id" \
        --arg proxy_url "$proxy_url/" \
        --arg app_url "$app_url/" \
        '{proxy_deployment_id: $proxy_deployment_id,
          worker_name: $worker_name,
          worker_version_id: $worker_version_id,
          proxy_url: $proxy_url,
          app_url: $app_url}' > "$result_file"
fi

echo "Canary control plane is healthy at $short_commit"
