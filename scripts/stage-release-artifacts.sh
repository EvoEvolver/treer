#!/bin/sh
set -eu

platform=${1:-}
destination=${2:-}
[ -n "$platform" ] && [ -n "$destination" ] || {
    echo "usage: scripts/stage-release-artifacts.sh <platform> <destination>" >&2
    exit 2
}

case "$platform" in
  linux-x86_64) expected_system=Linux; expected_architecture=x86_64; file_pattern='ELF 64-bit.*x86-64' ;;
  linux-aarch64) expected_system=Linux; expected_architecture=aarch64; file_pattern='ELF 64-bit.*(ARM aarch64|ARM64)' ;;
  darwin-x86_64) expected_system=Darwin; expected_architecture=x86_64; file_pattern='Mach-O 64-bit executable x86_64' ;;
  darwin-aarch64) expected_system=Darwin; expected_architecture=arm64; file_pattern='Mach-O 64-bit executable arm64' ;;
  *) echo "unsupported release platform $platform" >&2; exit 2 ;;
esac

actual_system=$(uname -s)
actual_architecture=$(uname -m)
[ "$actual_system" = "$expected_system" ] && [ "$actual_architecture" = "$expected_architecture" ] || {
    echo "$platform requires $expected_system/$expected_architecture, got $actual_system/$actual_architecture" >&2
    exit 1
}

commit=${TREER_BUILD_COMMIT:-$(git rev-parse HEAD)}
case "$commit" in
  *[!0-9a-fA-F]*|'') echo "invalid TREER_BUILD_COMMIT $commit" >&2; exit 1 ;;
esac
[ "${#commit}" -ge 7 ] && [ "${#commit}" -le 64 ] || {
    echo "invalid TREER_BUILD_COMMIT length" >&2
    exit 1
}

source_dir=${TREER_RELEASE_SOURCE_DIR:-target/release}
mkdir -p "$destination"

hash_file() {
    if command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$1" | awk '{print $1}'
    else
        sha256sum "$1" | awk '{print $1}'
    fi
}

version=
rustc_identity=$(rustc --version)
for binary in treer treer-agent-host treer-agent-server; do
    source="$source_dir/$binary"
    artifact="$destination/$binary"
    [ -x "$source" ] || { echo "missing release binary $source" >&2; exit 1; }
    cp "$source" "$artifact"
    chmod 755 "$artifact"

    if [ "$expected_system" = Darwin ]; then
        codesign --force --sign - "$artifact"
        codesign --verify --strict "$artifact"
    fi
    file "$artifact" | grep -Eq "$file_pattern" || {
        echo "$artifact does not match $platform" >&2
        file "$artifact" >&2
        exit 1
    }

    identity=$("$artifact" --version)
    printf '%s\n' "$identity" | grep -F "($commit)" >/dev/null || {
        echo "$artifact does not report build commit $commit: $identity" >&2
        exit 1
    }
    binary_version=$(printf '%s\n' "$identity" | awk '{print $2}')
    if [ -z "$version" ]; then
        version=$binary_version
    elif [ "$version" != "$binary_version" ]; then
        echo "release binaries do not share one version" >&2
        exit 1
    fi
done

(
    cd "$destination"
    for binary in treer treer-agent-host treer-agent-server; do
        printf '%s  %s\n' "$(hash_file "$binary")" "$binary"
    done > SHA256SUMS
)

cat > "$destination/build-metadata.json" <<EOF
{
  "schema_version": 1,
  "git_commit": "$commit",
  "version": "$version",
  "platform": "$platform",
  "rustc": "$rustc_identity"
}
EOF

echo "staged $platform artifacts for $commit in $destination"
