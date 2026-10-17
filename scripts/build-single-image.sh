#!/bin/sh
# Build only. Never push an image or start/replace a running Treer server.
set -eu

fail() { printf 'treer-single: %s\n' "$*" >&2; exit 1; }
[ "$#" -eq 0 ] || fail 'usage: scripts/build-single-image.sh'
command -v docker >/dev/null 2>&1 || fail 'Docker is missing; install Docker with Buildx, then retry.'
command -v git >/dev/null 2>&1 || fail 'Git is missing; run this script from a Treer checkout.'
# Resolve only the local Docker transport from the user's context. Build calls
# below use a private HOME and a whitelist, not Agent/cloud/Git credentials.
endpoint=${DOCKER_HOST:-}
if [ -z "$endpoint" ]; then
  endpoint=$(env -i PATH="$PATH" HOME="$HOME" docker context inspect --format '{{.Endpoints.docker.Host}}') \
    || fail 'Cannot resolve Docker context; start Docker and select a local context.'
fi
case "$endpoint" in unix://*) ;; *) fail 'Select a local Unix-socket Docker context.';; esac

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
repo=$(git -C "$script_dir/.." rev-parse --show-toplevel) || fail 'Not a Treer Git checkout.'
git -C "$repo" diff --quiet HEAD -- || fail 'Tracked changes are uncommitted; commit or stash them before building.'
commit=$(git -C "$repo" rev-parse HEAD)
case "$commit" in *[!0-9a-f]*|'') fail 'Cannot determine the exact source commit.';; esac
[ "${#commit}" -eq 40 ] || fail 'Expected a full 40-character source commit.'
short=$(git -C "$repo" rev-parse --short=12 HEAD)
tag="treer-single:$short"

umask 077
work=$(mktemp -d "${TMPDIR:-/tmp}/treer-single-build.XXXXXXXX")
trap 'rm -rf -- "$work"' EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
mkdir -p "$work/home/.docker/cli-plugins"
# Docker Desktop installs Buildx per user. Reuse only its executable, never
# copy config.json, registry credentials or credential-helper configuration.
if [ -x "$HOME/.docker/cli-plugins/docker-buildx" ]; then
  ln -s "$HOME/.docker/cli-plugins/docker-buildx" "$work/home/.docker/cli-plugins/docker-buildx"
fi
docker() { env -i PATH="$PATH" HOME="$work/home" DOCKER_HOST="$endpoint" docker "$@"; }
mkdir "$work/source"
docker info >/dev/null 2>&1 || fail 'Docker daemon is unavailable; start Docker, then retry.'
docker buildx version >/dev/null 2>&1 || fail 'Docker Buildx is missing; install/enable Buildx, then retry.'
case "$(docker info --format '{{.Architecture}}')" in
  aarch64|arm64) platform=linux/arm64 ;;
  x86_64|amd64) platform=linux/amd64 ;;
  *) fail 'Only Linux amd64/arm64 Docker engines are supported.' ;;
esac

# A committed archive excludes ignored/untracked files, local credentials,
# build outputs and .git. No host directory or Docker socket enters a container.
git -C "$repo" archive --format=tar "$commit" > "$work/source.tar"
tar -xf "$work/source.tar" -C "$work/source"
version=$(awk '/^\[workspace\.package\]/{inside=1;next} /^\[/{inside=0} inside && /^version *=/{gsub(/[" ]/,"",$0); sub(/^version=/,"",$0); print; exit}' "$work/source/Cargo.toml")
[ -n "$version" ] || fail 'Cannot read the workspace version.'

printf 'Building %s from %s for %s.\n' "$tag" "$commit" "$platform" >&2
# One native multi-stage build; CI's prebuilt-input path is independent.
docker buildx build --platform "$platform" --target local --load \
  -f "$work/source/Dockerfile.single" \
  --build-arg "TREER_BUILD_COMMIT=$commit" --build-arg "TREER_VERSION=$version" \
  --tag "$tag" "$work/source"
actual=$(docker image inspect --format '{{index .Config.Labels "org.opencontainers.image.revision"}}' "$tag")
[ "$actual" = "$commit" ] || fail 'Built image revision does not match the source checkout.'
printf '\nBuilt local image: %s\n' "$tag"
printf 'docker run -d --name treer --restart unless-stopped --stop-timeout 65 --read-only --tmpfs /tmp:rw,nosuid,nodev --cap-drop ALL --cap-add CHOWN --cap-add DAC_OVERRIDE --cap-add SETUID --cap-add SETGID --cap-add KILL --security-opt no-new-privileges:true -p 127.0.0.1:8080:8080 -v treer-data:/data %s\n' "$tag"
printf 'Console: http://console.treer.localhost:8080/\nProxy: http://proxy.treer.localhost:8080/\nData volume: treer-data (keep it when rebuilding/upgrading).\n'
printf 'Admin password: docker exec -u 0 treer cat /data/private/admin-password (long-lived credential; keep private).\n'
