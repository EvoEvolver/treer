#!/bin/sh
# Stage the native Proxy binary of one Linux platform in the same layout as the machine artifacts
# (binary + SHA256SUMS + build-metadata.json) so a single image job can consume it by exact SHA.
set -eu

platform=${1:-}
destination=${2:-}
[ -n "$platform" ] && [ -n "$destination" ] || {
    echo "usage: scripts/stage-proxy-artifact.sh <platform> <destination>" >&2
    exit 2
}

case "$platform" in
  linux-x86_64) expected_architecture=x86_64; file_pattern='ELF 64-bit.*x86-64' ;;
  linux-aarch64) expected_architecture=aarch64; file_pattern='ELF 64-bit.*(ARM aarch64|ARM64)' ;;
  *) echo "the Proxy image supports linux-x86_64 and linux-aarch64, not $platform" >&2; exit 2 ;;
esac
[ "$(uname -s)" = Linux ] && [ "$(uname -m)" = "$expected_architecture" ] || {
    echo "$platform requires native Linux/$expected_architecture, got $(uname -s)/$(uname -m)" >&2
    exit 1
}

commit=${TREER_BUILD_COMMIT:-$(git rev-parse HEAD)}
case "$commit" in
  *[!0-9a-f]*|'') echo "TREER_BUILD_COMMIT must be 40 lowercase hex characters" >&2; exit 1 ;;
esac
[ "${#commit}" -eq 40 ] || { echo "TREER_BUILD_COMMIT must be 40 lowercase hex characters" >&2; exit 1; }
[ "$(git rev-parse HEAD)" = "$commit" ] || { echo "checkout HEAD differs from TREER_BUILD_COMMIT" >&2; exit 1; }

source=${TREER_RELEASE_SOURCE_DIR:-target/release}/treer-proxy
[ -x "$source" ] || { echo "missing release binary $source" >&2; exit 1; }
mkdir -p "$destination"
cp "$source" "$destination/treer-proxy"
chmod 755 "$destination/treer-proxy"
file "$destination/treer-proxy" | grep -Eq "$file_pattern" || {
    echo "$destination/treer-proxy does not match $platform" >&2
    exit 1
}

version=$(awk '/^\[workspace\.package\]/{f=1;next} /^\[/{f=0} f && /^version *=/{gsub(/[" ]/,"",$0); sub(/^version=/,"",$0); print; exit}' Cargo.toml)
case "$version" in
  [0-9]*.[0-9]*.[0-9]*) ;;
  *) echo "cannot read workspace version" >&2; exit 1 ;;
esac

(
    cd "$destination"
    printf '%s  %s\n' "$(sha256sum treer-proxy | awk '{print $1}')" treer-proxy > SHA256SUMS
)
cat > "$destination/build-metadata.json" <<EOF
{
  "schema_version": 1,
  "git_commit": "$commit",
  "version": "$version",
  "platform": "$platform",
  "rustc": "$(rustc --version)"
}
EOF
echo "staged Proxy $platform for $commit in $destination"
