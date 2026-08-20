#!/bin/sh
set -eu
umask 077

: "${TREER_PROXY_URL:?TREER_PROXY_URL is required}"
: "${TREER_MACHINE_NAME:?TREER_MACHINE_NAME is required}"

proxy_url=${TREER_PROXY_URL%/}
bin_dir=/opt/treer/bin
root_dir=/workspace
persistent_dir=$root_dir/.treer-canary
state_dir=$persistent_dir/state
identity_path=$persistent_dir/identity.json
mkdir -p "$bin_dir" "$state_dir" "$root_dir/treer"

git config --global --add safe.directory "$root_dir/treer"
if [ ! -d "$root_dir/treer/.git" ]; then
    if ! git clone https://github.com/EvoEvolver/treer.git "$root_dir/treer"; then
        echo "treer-canary-machine: initial source clone failed; retry it after connecting" >&2
    fi
fi

case "$(uname -m)" in
    x86_64|amd64) platform=linux-x86_64 ;;
    aarch64|arm64) platform=linux-aarch64 ;;
    *) echo "unsupported machine architecture: $(uname -m)" >&2; exit 1 ;;
esac

for binary in treer-agent-host treer-agent-server treer; do
    curl -fsSL --retry 5 --retry-all-errors \
        "$proxy_url/artifacts/$platform/$binary" -o "$bin_dir/$binary"
    chmod 755 "$bin_dir/$binary"
done

if [ ! -s "$identity_path" ]; then
    : "${TREER_ENROLLMENT_KEY:?TREER_ENROLLMENT_KEY is required for first provisioning}"
    raw_id=${RAILWAY_SERVICE_ID:-$(cat /proc/sys/kernel/random/uuid)}
    installation_id="mid_$(printf '%s' "$raw_id" | tr -d '-')"
    enrollment=$(jq -n \
        --arg installation_id "$installation_id" \
        --arg name "$TREER_MACHINE_NAME" \
        '{installation_id: $installation_id, name: $name}')
    claim=$(curl -fsSL --retry 5 --retry-all-errors \
        -H "Authorization: Bearer $TREER_ENROLLMENT_KEY" \
        -H 'Content-Type: application/json' \
        --data "$enrollment" \
        "$proxy_url/api/machines/enroll")
    identity_tmp=$identity_path.tmp
    printf '%s\n' "$claim" | jq -e \
        '{workspace_id, server_id, machine_token}' > "$identity_tmp"
    mv "$identity_tmp" "$identity_path"
fi

workspace_id=$(jq -er '.workspace_id' "$identity_path")
server_id=$(jq -er '.server_id' "$identity_path")
machine_token=$(jq -er '.machine_token' "$identity_path")
host_socket="$state_dir/host.sock"
install_hostname=$(hostname)

jq -n \
    --arg proxy "$proxy_url/" \
    --arg workspace "$workspace_id" \
    --arg server_id "$server_id" \
    --arg machine_token "$machine_token" \
    --arg root "$root_dir" \
    --arg host_socket "$host_socket" \
    --arg install_hostname "$install_hostname" \
    '{
        proxy: $proxy,
        workspace: $workspace,
        server_id: $server_id,
        machine_token: $machine_token,
        root: $root,
        listen: "127.0.0.1:8790",
        host_socket: $host_socket,
        install_hostname: $install_hostname
    }' > "$state_dir/controller.json.tmp"
mv "$state_dir/controller.json.tmp" "$state_dir/controller.json"

jq -n \
    --arg socket_path "$host_socket" \
    --arg controller_path "$bin_dir/treer-agent-server" \
    --arg controller_config_path "$state_dir/controller.json" \
    --arg root "$root_dir" \
    '{
        socket_path: $socket_path,
        controller_path: $controller_path,
        controller_config_path: $controller_config_path,
        root: $root
    }' > "$state_dir/host.json.tmp"
mv "$state_dir/host.json.tmp" "$state_dir/host.json"

TREER_MACHINE_NAME="$TREER_MACHINE_NAME" python3 - <<'PY' &
import http.server
import json
import os

payload = json.dumps({
    "machine": os.environ["TREER_MACHINE_NAME"],
    "service": "treer-canary",
}).encode()

class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def log_message(self, _format, *_args):
        pass

http.server.ThreadingHTTPServer(("127.0.0.1", 8081), Handler).serve_forever()
PY

export TREER_NETWORK_MODE=${TREER_NETWORK_MODE:-proxy-env}
exec "$bin_dir/treer-agent-host" run --config "$state_dir/host.json"
