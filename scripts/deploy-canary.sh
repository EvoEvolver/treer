#!/bin/sh
set -eu

project_id=${TREER_RAILWAY_PROJECT_ID:-09d4eeeb-cc5a-49e5-bcfe-f43c1c1b112b}
environment=${TREER_CANARY_ENVIRONMENT:-canary}
proxy_service=${TREER_CANARY_PROXY_SERVICE:-c7f70740-49fa-4ab9-bb41-97b82f99dcce}
app_service=${TREER_CANARY_APP_SERVICE:-dfbdd809-c55b-41d6-b537-885f06ebb1cb}
domain_id=${TREER_CANARY_DOMAIN_ID:-aabf784e-48e1-4ffb-b1ea-645f83ebf713}
proxy_url=${TREER_CANARY_PROXY_URL:-https://treer-proxy-canary.up.railway.app/}
app_url=${TREER_CANARY_APP_URL:-https://treer-app-canary.up.railway.app/}
ingress_url=${TREER_CANARY_INGRESS_URL:-https://canary.apps.treer.ai/}
proxy_url=${proxy_url%/}
app_url=${app_url%/}
timeout=${TREER_CANARY_DEPLOY_TIMEOUT:-900}

command -v railway >/dev/null 2>&1 || { echo "railway CLI is required" >&2; exit 1; }
command -v jq >/dev/null 2>&1 || { echo "jq is required" >&2; exit 1; }
command -v curl >/dev/null 2>&1 || { echo "curl is required" >&2; exit 1; }

wait_for_deployment() {
    service=$1
    deadline=$(( $(date +%s) + timeout ))
    while [ "$(date +%s)" -lt "$deadline" ]; do
        status=$(railway deployment list \
            --project "$project_id" \
            --environment "$environment" \
            --service "$service" \
            --json | jq -r '.[0].status // "UNKNOWN"')
        case "$status" in
            SUCCESS) return 0 ;;
            FAILED|CRASHED|REMOVED|CANCELLED)
                echo "deployment for $service ended with $status" >&2
                return 1
                ;;
        esac
        sleep 5
    done
    echo "timed out waiting for deployment of $service" >&2
    return 1
}

revision=$(git rev-parse --short HEAD 2>/dev/null || printf unknown)
if ! git diff --quiet 2>/dev/null || ! git diff --cached --quiet 2>/dev/null || \
    [ -n "$(git ls-files --others --exclude-standard 2>/dev/null)" ]; then
    revision="$revision-dirty"
fi

railway variable set \
    "TREER_PROXY_PUBLIC_URL=$proxy_url" \
    "TREER_APP_PUBLIC_URL=$app_url" \
    "TREER_INGRESS_PUBLIC_URL=$ingress_url" \
    --project "$project_id" \
    --environment "$environment" \
    --service "$proxy_service" \
    --skip-deploys --json >/dev/null

railway variable set \
    "TREER_PROXY_PUBLIC_URL=$proxy_url" \
    --project "$project_id" \
    --environment "$environment" \
    --service "$app_service" \
    --skip-deploys --json >/dev/null

proxy_deploy=$(railway up --detach --json \
    --project "$project_id" \
    --environment "$environment" \
    --service "$proxy_service" \
    --message "canary $revision")
app_deploy=$(railway up web --path-as-root --detach --json \
    --project "$project_id" \
    --environment "$environment" \
    --service "$app_service" \
    --message "canary app $revision")

printf 'Proxy deployment: %s\n' "$(printf '%s' "$proxy_deploy" | jq -r '.deploymentId // .id')"
printf 'App deployment: %s\n' "$(printf '%s' "$app_deploy" | jq -r '.deploymentId // .id')"

wait_for_deployment "$proxy_service"
wait_for_deployment "$app_service"
curl -fsS --retry 12 --retry-all-errors "$proxy_url/api/health" >/dev/null
curl -fsS --retry 12 --retry-all-errors "$app_url/health" >/dev/null
echo "Canary control plane is healthy at $revision"

domain=$(railway domain status "$domain_id" --project "$project_id" \
    --environment "$environment" --service "$proxy_service" --json)
if [ "$(printf '%s' "$domain" | jq -r '.domain.certificate.status')" = \
    CERTIFICATE_STATUS_TYPE_ISSUED ]; then
    echo "Canary wildcard certificate is ready"
else
    echo "Canary wildcard DNS or certificate is not ready:" >&2
    printf '%s' "$domain" | jq -r \
        '.domain.dnsRecords[] | "  \(.recordType) \(.fqdn) -> \(.requiredValue) [\(.status)]"' >&2
    printf '%s' "$domain" | jq -r \
        'select(.domain.verification.verified == false) | "  TXT \(.domain.verification.dnsHost).\(.domain.dnsRecords[0].zone) -> \(.domain.verification.token) [unverified]"' >&2
fi
