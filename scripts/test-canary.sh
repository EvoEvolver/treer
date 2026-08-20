#!/bin/sh
set -eu

project_id=${TREER_RAILWAY_PROJECT_ID:-09d4eeeb-cc5a-49e5-bcfe-f43c1c1b112b}
environment=${TREER_CANARY_ENVIRONMENT:-canary}
proxy_service=${TREER_CANARY_PROXY_SERVICE:-c7f70740-49fa-4ab9-bb41-97b82f99dcce}
proxy_url=${TREER_CANARY_PROXY_URL:-https://proxy.canary.treer.ai/}
proxy_url=${proxy_url%/}
timeout=${TREER_CANARY_TEST_TIMEOUT:-900}
keep_resources=${TREER_CANARY_KEEP_RESOURCES:-0}
skip_public=${TREER_CANARY_SKIP_PUBLIC:-0}
provision_machines=${TREER_CANARY_PROVISION_MACHINES:-0}
enroll_machines=${TREER_CANARY_ENROLL_MACHINES:-0}
run_id=$(date -u +%Y%m%d%H%M%S)-$$
machine_a_service=${TREER_CANARY_MACHINE_A_SERVICE:-461c12b4-0333-46e3-b757-b0221db80470}
machine_b_service=${TREER_CANARY_MACHINE_B_SERVICE:-1a3d759e-02db-45a8-b68c-a2d2c45e1800}
machine_a=${TREER_CANARY_MACHINE_A_NAME:-canary-a-20260820022530-70983}
machine_b=${TREER_CANARY_MACHINE_B_NAME:-canary-b-20260820022530-70983}
hostname="service-$run_id.internal"
slug="canary-$run_id"
workspace_id=canary-e2e
test_email=canary-tester@treer.invalid
tmp_dir=$(mktemp -d "${TMPDIR:-/tmp}/treer-canary.XXXXXX")
admin_cookies="$tmp_dir/admin.cookies"
user_cookies="$tmp_dir/user.cookies"
agent_ids=
treer_service_id=
ingress_id=
virtual_host_created=0

command -v railway >/dev/null 2>&1 || { echo "railway CLI is required" >&2; exit 1; }
command -v jq >/dev/null 2>&1 || { echo "jq is required" >&2; exit 1; }
command -v curl >/dev/null 2>&1 || { echo "curl is required" >&2; exit 1; }

api() {
    method=$1
    api_path=$2
    data=${3:-}
    if [ -n "$data" ]; then
        if ! api_response=$(curl -sS --fail-with-body -b "$user_cookies" -c "$user_cookies" \
            -X "$method" -H 'Content-Type: application/json' --data "$data" \
            "$proxy_url$api_path"); then
            printf 'Treer API %s %s failed: %s\n' "$method" "$api_path" "$api_response" >&2
            return 1
        fi
    elif [ "$method" = GET ]; then
        if ! api_response=$(curl -sS --fail-with-body --retry 5 --retry-delay 1 \
            --retry-all-errors -b "$user_cookies" -c "$user_cookies" \
            "$proxy_url$api_path"); then
            printf 'Treer API GET %s failed: %s\n' "$api_path" "$api_response" >&2
            return 1
        fi
    else
        if ! api_response=$(curl -sS --fail-with-body -b "$user_cookies" -c "$user_cookies" \
            -X "$method" "$proxy_url$api_path"); then
            printf 'Treer API %s %s failed: %s\n' "$method" "$api_path" "$api_response" >&2
            return 1
        fi
    fi
    printf '%s' "$api_response"
}

cleanup() {
    trap - EXIT HUP INT TERM
    set +e
    if [ "$keep_resources" = 1 ]; then
        echo "Keeping Canary logical resources for inspection"
        return
    fi
    if [ -s "$user_cookies" ]; then
        for agent_id in $agent_ids; do
            api DELETE "/api/workspaces/$workspace_id/agents/$agent_id" >/dev/null 2>&1
        done
        if [ -n "$ingress_id" ]; then
            api DELETE "/api/workspaces/$workspace_id/ingresses/$ingress_id" >/dev/null 2>&1
        fi
        if [ "$virtual_host_created" = 1 ]; then
            api DELETE "/api/workspaces/$workspace_id/virtual-hosts/$hostname" >/dev/null 2>&1
        fi
        if [ -n "$treer_service_id" ]; then
            api DELETE "/api/workspaces/$workspace_id/services/$treer_service_id" >/dev/null 2>&1
        fi
    fi
    rm -rf "$tmp_dir"
}
trap cleanup EXIT HUP INT TERM

wait_for_deployment() {
    service_id=$1
    deadline=$(( $(date +%s) + timeout ))
    while [ "$(date +%s)" -lt "$deadline" ]; do
        status=$(railway deployment list --project "$project_id" \
            --environment "$environment" --service "$service_id" --json \
            | jq -r '.[0].status // "UNKNOWN"')
        case "$status" in
            SUCCESS) return 0 ;;
            FAILED|CRASHED|REMOVED|CANCELLED)
                echo "machine deployment $service_id ended with $status" >&2
                railway logs --project "$project_id" --environment "$environment" \
                    --service "$service_id" --lines 100 >&2 || true
                return 1
                ;;
        esac
        sleep 5
    done
    echo "timed out waiting for machine deployment $service_id" >&2
    return 1
}

wait_for_machine() {
    machine_name=$1
    deadline=$(( $(date +%s) + timeout ))
    while [ "$(date +%s)" -lt "$deadline" ]; do
        if ! snapshot=$(api GET "/api/workspaces/$workspace_id/snapshot"); then
            sleep 3
            continue
        fi
        server_id=$(printf '%s' "$snapshot" | jq -r \
            --arg name "$machine_name" '.servers[]? | select(.name == $name and .status == "online") | .server_id' \
            | head -n 1)
        if [ -n "$server_id" ]; then
            printf '%s' "$server_id"
            return 0
        fi
        sleep 3
    done
    echo "timed out waiting for machine $machine_name" >&2
    return 1
}

provision_machine() {
    machine_name=$1
    enrollment_key=$2
    service_id=$3
    railway variable set \
        "TREER_PROXY_URL=$proxy_url/" \
        "TREER_MACHINE_NAME=$machine_name" \
        --project "$project_id" --environment "$environment" \
        --service "$service_id" --skip-deploys --json >/dev/null
    if [ -n "$enrollment_key" ]; then
        printf '%s' "$enrollment_key" | railway variable set TREER_ENROLLMENT_KEY --stdin \
            --project "$project_id" --environment "$environment" \
            --service "$service_id" --skip-deploys --json >/dev/null
    fi
    railway up canary/machine --path-as-root --detach --json \
        --project "$project_id" --environment "$environment" \
        --service "$service_id" --message "canary machine $run_id" >/dev/null
    wait_for_deployment "$service_id"
}

refresh_machine() {
    machine_name=$1
    service_id=$2
    old_connection=$(api GET "/api/workspaces/$workspace_id/snapshot" \
        | jq -r --arg name "$machine_name" \
            '.servers[]? | select(.name == $name) | .connected_at' \
        | head -n 1)
    railway service restart --yes --json \
        --project "$project_id" --environment "$environment" \
        --service "$service_id" >/dev/null
    deadline=$(( $(date +%s) + timeout ))
    while [ "$(date +%s)" -lt "$deadline" ]; do
        if snapshot=$(api GET "/api/workspaces/$workspace_id/snapshot"); then
            connection=$(printf '%s' "$snapshot" | jq -r --arg name "$machine_name" \
                '.servers[]? |
                 select(.name == $name and .status == "online") |
                 .connected_at' | head -n 1)
            if [ -n "$connection" ] && [ "$connection" != "$old_connection" ]; then
                return 0
            fi
        fi
        sleep 3
    done
    echo "timed out waiting for machine reconnect $machine_name" >&2
    railway logs --project "$project_id" --environment "$environment" \
        --service "$service_id" --lines 100 >&2 || true
    return 1
}

echo "Checking Canary control plane"
curl -fsS --retry 5 --retry-all-errors "$proxy_url/api/health" >/dev/null
admin_password=$(railway variable list --project "$project_id" \
    --environment "$environment" --service "$proxy_service" --json \
    | jq -er '.ADMIN_PASSWORD')

curl -fsS -c "$admin_cookies" -H 'Content-Type: application/json' \
    --data "$(jq -n --arg password "$admin_password" '{password: $password}')" \
    "$proxy_url/api/admin/login" >/dev/null

login_payload=$(jq -n --arg email "$test_email" --arg password "$admin_password" \
    '{email: $email, password: $password}')
if ! curl -fsS -c "$user_cookies" -H 'Content-Type: application/json' \
    --data "$login_payload" "$proxy_url/api/auth/login" >/dev/null 2>&1; then
    invitation=$(curl -fsS -b "$admin_cookies" -H 'Content-Type: application/json' \
        -X POST "$proxy_url/api/admin/invitations")
    invite=$(printf '%s' "$invitation" | jq -er '.token')
    register_payload=$(jq -n --arg invite "$invite" --arg email "$test_email" \
        --arg password "$admin_password" \
        '{invite: $invite, email: $email, preferred_name: "Canary tester", password: $password}')
    curl -fsS -c "$user_cookies" -H 'Content-Type: application/json' \
        --data "$register_payload" "$proxy_url/api/auth/register" >/dev/null
fi

organization_id=$(api GET /api/organizations | jq -er '.organizations[0].organization_id')
workspaces=$(api GET "/api/workspaces?organization_id=$organization_id")
if ! printf '%s' "$workspaces" | jq -e --arg id "$workspace_id" \
    '.workspaces[]? | select(.workspace_id == $id)' >/dev/null; then
    workspace_payload=$(jq -n --arg organization_id "$organization_id" \
        --arg workspace_id "$workspace_id" \
        '{organization_id: $organization_id, workspace_id: $workspace_id, name: "Canary E2E"}')
    api POST /api/workspaces "$workspace_payload" >/dev/null
fi

if [ "$provision_machines" = 1 ]; then
    enrollment_a=
    enrollment_b=
    if [ "$enroll_machines" = 1 ]; then
        enrollment_a=$(api POST "/api/workspaces/$workspace_id/bootstrap" | jq -er '.enrollment_key')
        enrollment_b=$(api POST "/api/workspaces/$workspace_id/bootstrap" | jq -er '.enrollment_key')
    fi
    echo "Provisioning two dedicated Canary Railway machines"
    provision_machine "$machine_a" "$enrollment_a" "$machine_a_service"
    provision_machine "$machine_b" "$enrollment_b" "$machine_b_service"
else
    [ "$enroll_machines" = 0 ] || {
        echo "TREER_CANARY_ENROLL_MACHINES requires TREER_CANARY_PROVISION_MACHINES=1" >&2
        exit 1
    }
    echo "Restarting the persistent Canary Railway machines"
    refresh_machine "$machine_a" "$machine_a_service"
    refresh_machine "$machine_b" "$machine_b_service"
fi
echo "Waiting for $machine_a to connect"
server_a=$(wait_for_machine "$machine_a")
echo "Waiting for $machine_b to connect"
server_b=$(wait_for_machine "$machine_b")
echo "Both Canary machines are online"

service_payload=$(jq -n --arg name "http-$run_id" --arg server_id "$server_b" \
    '{name: $name, server_id: $server_id, target_host: "127.0.0.1", target_port: 8081, protocol: "http"}')
service_id=$(api POST "/api/workspaces/$workspace_id/services" "$service_payload" \
    | jq -er '.service.service_id')
treer_service_id=$service_id
host_payload=$(jq -n --arg hostname "$hostname" --arg service_id "$service_id" \
    '{hostname: $hostname, service_id: $service_id}')
api POST "/api/workspaces/$workspace_id/virtual-hosts" "$host_payload" >/dev/null
virtual_host_created=1

probe=$(api POST "/api/workspaces/$workspace_id/services/$service_id/probe")
printf '%s' "$probe" | jq -e '.health.healthy == true' >/dev/null
sleep 5

virtual_success=0
virtual_output=
for attempt in 1 2 3; do
    agent_payload=$(jq -n --arg server_id "$server_a" --arg name "network-$run_id-$attempt" \
        --arg url "http://$hostname/" \
        '{server_id: $server_id, kind: "command", name: $name, cwd: "", args: ["curl", "-fsS", "--max-time", "30", $url]}')
    agent_id=$(api POST "/api/workspaces/$workspace_id/agents" "$agent_payload" | jq -er '.agent_id')
    agent_ids="$agent_ids $agent_id"
    deadline=$(( $(date +%s) + 45 ))
    while [ "$(date +%s)" -lt "$deadline" ]; do
        virtual_output=$(api GET "/api/workspaces/$workspace_id/agents/$agent_id/output?lines=100" \
            | jq -r '.text')
        if printf '%s' "$virtual_output" | jq -e --arg machine "$machine_b" \
            'select(.machine == $machine and .service == "treer-canary")' >/dev/null 2>&1; then
            virtual_success=1
            break
        fi
        sleep 2
    done
    [ "$virtual_success" = 1 ] && break
    sleep 5
done
if [ "$virtual_success" != 1 ]; then
    printf 'Virtual network output did not match the target machine: %s\n' "$virtual_output" >&2
    exit 1
fi
echo "Virtual network: passed ($machine_a -> $machine_b)"

sleep 12
traffic=$(api GET "/api/workspaces/$workspace_id/traffic?hours=1")
printf '%s' "$traffic" | jq -e --arg source "$server_a" --arg destination "$server_b" \
    '.traffic[]? | select(.source_server_id == $source and .destination_server_id == $destination and .payload_bytes > 0)' >/dev/null
echo "Directional traffic accounting: passed"

if [ "$skip_public" = 1 ]; then
    echo "Public ingress: skipped by TREER_CANARY_SKIP_PUBLIC=1"
    echo "Canary internal E2E passed; this result is not eligible for Production promotion"
    exit 0
fi

ingress_payload=$(jq -n --arg service_id "$service_id" --arg slug "$slug" \
    '{service_id: $service_id, slug: $slug, access: "public"}')
ingress=$(api POST "/api/workspaces/$workspace_id/ingresses" "$ingress_payload")
ingress_id=$(printf '%s' "$ingress" | jq -er '.ingress.ingress_id')
ingress_url=$(printf '%s' "$ingress" | jq -er '.ingress.url')
public_output=$(curl -fsS --retry 20 --retry-delay 3 --retry-all-errors "$ingress_url")
printf '%s' "$public_output" | jq -e --arg machine "$machine_b" \
    'select(.machine == $machine and .service == "treer-canary")' >/dev/null
echo "Public ingress: passed ($ingress_url)"
echo "Canary E2E passed"
