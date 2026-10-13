#!/bin/sh
set -eu

case "$(uname -s)-$(uname -m)" in
  Linux-x86_64|Linux-amd64) platform=linux-x86_64 ;;
  Linux-aarch64|Linux-arm64) platform=linux-aarch64 ;;
  Darwin-x86_64|Darwin-amd64) platform=darwin-x86_64 ;;
  Darwin-arm64|Darwin-aarch64) platform=darwin-aarch64 ;;
  *) echo "unsupported platform $(uname -s)/$(uname -m)" >&2; exit 1 ;;
esac

payload=$(mktemp -d "${TMPDIR:-/tmp}/treer-launcher-payload.XXXXXX")
trap 'rm -rf "$payload"' EXIT HUP INT TERM
commit=$(git rev-parse HEAD)
npm --prefix launchers/agent ci --ignore-scripts
node scripts/prepare-launcher-release.mjs --output "$payload" --source-commit "$commit"
export TREER_LAUNCHER_PAYLOAD_DIR="$payload"
export TREER_REQUIRE_LAUNCHER_BUNDLE=1
cargo build --locked --release -p treer-agent-host -p treer-agent-server -p treer-cli
destination="dist/$platform"
TREER_BUILD_COMMIT=$(git rev-parse HEAD) \
    sh scripts/stage-release-artifacts.sh "$platform" "$destination"
