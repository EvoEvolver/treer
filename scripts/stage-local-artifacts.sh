#!/bin/sh
set -eu

case "$(uname -s)-$(uname -m)" in
  Linux-x86_64|Linux-amd64) platform=linux-x86_64 ;;
  Linux-aarch64|Linux-arm64) platform=linux-aarch64 ;;
  Darwin-x86_64|Darwin-amd64) platform=darwin-x86_64 ;;
  Darwin-arm64|Darwin-aarch64) platform=darwin-aarch64 ;;
  *) echo "unsupported platform $(uname -s)/$(uname -m)" >&2; exit 1 ;;
esac

cargo build --release -p treer-agent-host -p treer-agent-server -p treer-cli
destination="dist/$platform"
mkdir -p "$destination"
cp target/release/treer "$destination/treer"
cp target/release/treer-agent-server "$destination/treer-agent-server"
cp target/release/treer-agent-host "$destination/treer-agent-host"
echo "staged Treer artifacts in $destination"
