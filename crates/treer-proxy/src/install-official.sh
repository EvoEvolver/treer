#!/bin/sh
set -eu
umask 077
install_dir=${TREER_INSTALL_DIR:-"${HOME:?HOME is required}/.local/bin"}
server_dir=${TREER_AGENT_SERVER_INSTALL_DIR:-"${HOME}/.local/libexec/treer"}

echo "treer: security notice: Controller/Host run with your user permissions; use a dedicated account or sandbox" >&2
case "$(uname -s)-$(uname -m)" in
  Linux-x86_64|Linux-amd64) platform=linux-x86_64 ;;
  Linux-aarch64|Linux-arm64) platform=linux-aarch64 ;;
  Darwin-x86_64|Darwin-amd64) platform=darwin-x86_64 ;;
  Darwin-arm64|Darwin-aarch64) platform=darwin-aarch64 ;;
  *) echo "treer: unsupported platform" >&2; exit 1 ;;
esac
command -v node >/dev/null 2>&1 || { echo "treer: official signed bootstrap requires Node.js 22+" >&2; exit 1; }
node -e 'if (Number(process.versions.node.split(".")[0]) < 22) process.exit(1)' || { echo "treer: Node.js 22+ is required" >&2; exit 1; }
tmp_dir=$(mktemp -d "${TMPDIR:-/tmp}/treer-install.XXXXXX")
trap 'rm -rf "$tmp_dir"' EXIT HUP INT TERM
node --input-type=module - "$platform" "$tmp_dir" <<'TREER_SIGNED_BOOTSTRAP_JS'
__TREER_OFFICIAL_DOWNLOAD_JS__
TREER_SIGNED_BOOTSTRAP_JS

mkdir -p "$install_dir" "$server_dir"
install_dir=$(CDPATH= cd -- "$install_dir" && pwd)
server_dir=$(CDPATH= cd -- "$server_dir" && pwd)
chmod 755 "$tmp_dir/treer" "$tmp_dir/treer-launch" "$tmp_dir/treer-agent-host" "$tmp_dir/treer-agent-server"
mv "$tmp_dir/treer" "$install_dir/treer"
mv "$tmp_dir/treer-launch" "$install_dir/treer-launch"
# Deprecated alias: profiles saved before the rename call treer-agent. Replace an
# earlier alias or the Node-era shim, never an unrelated command of that name.
if [ -L "$install_dir/treer-agent" ] || [ ! -e "$install_dir/treer-agent" ] ||
    grep -q 'treer-agent-shim/v1' "$install_dir/treer-agent" 2>/dev/null; then
    rm -f "$install_dir/treer-agent" "$install_dir/treer-agent.json"
    ln -s treer-launch "$install_dir/treer-agent"
fi
mv "$tmp_dir/treer-agent-host" "$server_dir/treer-agent-host"
mv "$tmp_dir/treer-agent-server" "$server_dir/treer-agent-server"
ln -sf "$server_dir/treer-agent-server" "$install_dir/treer-agent-server"
echo "treer: verified binaries installed; no services were started or restarted"
__TREER_PATH_SETUP__
echo "treer: run the workspace connection command from the Proxy UI next"
