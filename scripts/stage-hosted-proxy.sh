#!/bin/sh
set -eu
root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$root"
commit=$(git rev-parse "${1:?source commit is required}^{commit}")
destination=${2:?empty upload directory is required}
[ "$commit" = "$(git rev-parse HEAD)" ] || { echo "requested commit must be HEAD" >&2; exit 1; }
git diff --quiet && git diff --cached --quiet && \
    [ -z "$(git ls-files --others --exclude-standard)" ] || {
    echo "hosted deployment requires a clean worktree" >&2; exit 1;
}
sh scripts/ensure-machine-artifacts.sh "$commit"
git archive "$commit" | tar -x -C "$destination"
cp -R dist "$destination/dist"
# Config-as-code takes priority over Railway variables. Use the hosted image only here.
jq '.build.dockerfilePath = "Dockerfile.hosted"' railway.json > "$destination/railway.json"
sh scripts/verify-machine-artifacts.sh "$commit" "$destination/dist"
