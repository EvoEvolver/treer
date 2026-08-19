#!/bin/sh
set -eu

: "${TREER_PROXY_URL:?TREER_PROXY_URL is required}"
: "${TREER_ENROLLMENT_KEY:?TREER_ENROLLMENT_KEY is required}"
: "${TREER_MACHINE_NAME:?TREER_MACHINE_NAME is required}"

proxy_url=${TREER_PROXY_URL%/}
bin_dir=/opt/treer/bin
state_dir=/opt/treer/state
root_dir=/workspace
mkdir -p "$bin_dir" "$state_dir" "$root_dir"

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

workspace_id=$(printf '%s' "$claim" | jq -er '.workspace_id')
server_id=$(printf '%s' "$claim" | jq -er '.server_id')
machine_token=$(printf '%s' "$claim" | jq -er '.machine_token')
host_socket="$state_dir/host.sock"

jq -n \
    --arg proxy "$proxy_url/" \
    --arg workspace "$workspace_id" \
    --arg server_id "$server_id" \
    --arg machine_token "$machine_token" \
    --arg root "$root_dir" \
    --arg host_socket "$host_socket" \
    '{
        proxy: $proxy,
        workspace: $workspace,
        server_id: $server_id,
        machine_token: $machine_token,
        root: $root,
        listen: "127.0.0.1:8790",
        host_socket: $host_socket
    }' > "$state_dir/controller.json"

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
    }' > "$state_dir/host.json"

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
