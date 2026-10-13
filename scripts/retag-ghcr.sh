#!/usr/bin/env bash
# Workflow inputs are data in the environment, never interpolated shell source.
set -euo pipefail
version=${TREER_RELEASE_VERSION:-}
channel=${TREER_RELEASE_CHANNEL:-}
if [[ ! "$version" =~ ^v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(-[0-9A-Za-z][0-9A-Za-z.-]*)?$ ]]; then
    echo 'release version must be a vMAJOR.MINOR.PATCH[-PRERELEASE] tag' >&2
    exit 1
fi
case "$channel" in
    canary|stable) ;;
    *) echo 'release channel must be canary or stable' >&2; exit 1 ;;
esac
case "${1:-}" in
    validate) exit 0 ;;
    apply) ;;
    *) echo 'usage: retag-ghcr.sh validate|apply' >&2; exit 1 ;;
esac
image=${IMAGE:?IMAGE is required}
if [[ ! "$image" =~ ^ghcr\.io/[a-z0-9][a-z0-9._-]*/[a-z0-9][a-z0-9._-]*$ ]]; then
    echo 'image must be a GHCR owner/repository' >&2
    exit 1
fi
docker buildx imagetools create -t "$image:$channel" "$image:$version"
