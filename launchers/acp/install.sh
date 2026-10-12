#!/bin/sh
set -eu

AGENTS=""
REF=main
REPOSITORY=https://github.com/EvoEvolver/treer.git
SOURCE=""
UI=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    --agent) AGENTS="$AGENTS $2"; shift 2 ;;
    --ref) REF="$2"; shift 2 ;;
    --repository) REPOSITORY="$2"; shift 2 ;;
    --source) SOURCE="$2"; shift 2 ;;
    --ui) UI="$2"; shift 2 ;;
    --help)
      echo 'Usage: install.sh --agent PROVIDER [--agent PROVIDER] [--ref BRANCH_OR_TAG] [--source CHECKOUT] [--ui builtin|remote-codex]'
      exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done
[ -n "$AGENTS" ] || { echo '--agent is required' >&2; exit 2; }
for tool in treer python3 git; do
  command -v "$tool" >/dev/null 2>&1 || { echo "$tool is not on PATH" >&2; exit 1; }
done
WHOAMI="$(treer whoami)"
MACHINE_ROOT="$(printf '%s' "$WHOAMI" | python3 -c 'import json,os,sys
root=json.load(sys.stdin)["machine"]["root"]
if not root or not os.path.isabs(root) or not os.path.isdir(root):
    raise SystemExit("treer whoami did not return an existing absolute machine.root")
print(os.path.realpath(root))')"
INSTALL_ROOT="$MACHINE_ROOT/.treer/launchers/acp"
mkdir -p "$INSTALL_ROOT"
LOCK="$INSTALL_ROOT/.install-lock"
mkdir "$LOCK" 2>/dev/null || { echo "another installation owns $LOCK" >&2; exit 1; }
STAGE=""
cleanup() {
  if [ -n "$STAGE" ]; then rm -rf -- "$STAGE"; fi
  rmdir "$LOCK"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
STAGE="$(mktemp -d "$INSTALL_ROOT/.install.XXXXXXXX")"
if [ -n "$SOURCE" ]; then
  # Test/local installs use a committed revision, never a dirty source snapshot.
  git -C "$SOURCE" archive HEAD | tar -xf - -C "$STAGE"
  COMMIT="$(git -C "$SOURCE" rev-parse HEAD)"
else
  git clone --depth 1 --filter=blob:none --sparse --branch "$REF" "$REPOSITORY" "$STAGE/source"
  git -C "$STAGE/source" sparse-checkout set launchers/acp crates/treer-protocol
  COMMIT="$(git -C "$STAGE/source" rev-parse HEAD)"
  # The published version is a normal source tree, without a mutable Git checkout.
  cp -R "$STAGE/source/." "$STAGE/tree"
  rm -rf -- "$STAGE/tree/.git"
  mv "$STAGE/tree" "$STAGE/package"
fi
case "$COMMIT" in ''|*[!0-9a-f]*) echo 'invalid source commit' >&2; exit 1 ;; esac
[ "${#COMMIT}" -eq 40 ] || { echo 'source must resolve to a full commit' >&2; exit 1; }
if [ -n "$SOURCE" ]; then
  PACKAGE="$STAGE"
else
  PACKAGE="$STAGE/package"
fi
VERSION="$INSTALL_ROOT/$COMMIT"

download_and_verify_precompiled() {
  # Future releases: download a commit/platform archive into PACKAGE, then its
  # SHA256SUMS; verify every runtime/UI byte before returning success. Never
  # activate an unverified download. There is no artifact publication yet.
  echo 'ACP precompiled artifacts are not published; using local cargo build' >&2
  return 1
}
case "$UI" in
  ""|builtin) VARIANT=runtime-headless ;;
  remote-codex) VARIANT=runtime-remote-codex ;;
  *) echo "unsupported --ui value: $UI" >&2; exit 2 ;;
esac
NEEDS_BUILD=0
[ -x "$VERSION/launchers/acp/.build/$VARIANT/release/treer-acp" ] || NEEDS_BUILD=1
if [ "$UI" = remote-codex ] && [ ! -f "$VERSION/launchers/acp/.build/remote-codex-ui/source/apps/agent-ui-web/dist/index.html" ]; then
  NEEDS_BUILD=1
fi
if [ "$NEEDS_BUILD" -eq 1 ]; then
  if ! download_and_verify_precompiled; then
    command -v cargo >/dev/null 2>&1 || { echo 'cargo is required for the source build' >&2; exit 1; }
    set -- "$PACKAGE/launchers/acp/scripts/apply.sh" --dir "$PACKAGE" --build-only
    for agent in $AGENTS; do set -- "$@" --agent "$agent"; done
    if [ -n "$UI" ]; then set -- "$@" --ui "$UI"; fi
    sh "$@"
  fi
  if [ -d "$VERSION" ]; then
    # A second presentation can add its missing build directories for the same
    # source commit. Never replace any previously installed binary or assets.
    python3 - "$PACKAGE/launchers/acp/.build" "$VERSION/launchers/acp/.build" <<'PY'
import os,sys
from pathlib import Path
source,destination=map(Path,sys.argv[1:])
destination.mkdir(parents=True,exist_ok=True)
for entry in source.iterdir():
    target=destination/entry.name
    if not target.exists(): os.rename(entry,target)
PY
  elif [ "$PACKAGE" = "$STAGE" ]; then
    mv "$STAGE" "$VERSION"
    STAGE=""
  else
    mv "$PACKAGE" "$VERSION"
  fi
fi
# Publish an atomic symlink. Existing launchers resolve their physical directory
# once, so replacement never changes a running Agent's executable or assets.
PREVIOUS="$(python3 -c 'import os,sys; p=sys.argv[1]; print(os.readlink(p) if os.path.islink(p) else "")' "$INSTALL_ROOT/current")"
python3 - "$INSTALL_ROOT" "$COMMIT" <<'PY'
import os,sys,uuid
root,commit=sys.argv[1:]
temporary=os.path.join(root,".current-"+uuid.uuid4().hex)
try:
    os.symlink(commit,temporary)
    os.replace(temporary,os.path.join(root,"current"))
finally:
    if os.path.lexists(temporary): os.unlink(temporary)
PY
set -- "$INSTALL_ROOT/current/launchers/acp/scripts/apply.sh" \
  --dir "$INSTALL_ROOT/current" --profile-root "$INSTALL_ROOT/current" --skip-build --no-launch
for agent in $AGENTS; do set -- "$@" --agent "$agent"; done
if [ -n "$UI" ]; then set -- "$@" --ui "$UI"; fi
if ! sh "$@"; then
  # Profile/verification failures must not move future launches to a broken
  # version. Keep the candidate directory for inspection/retry.
  python3 - "$INSTALL_ROOT" "$PREVIOUS" <<'PY'
import os,sys,uuid
root,previous=sys.argv[1:]
current=os.path.join(root,"current")
if previous:
    temporary=os.path.join(root,".rollback-"+uuid.uuid4().hex)
    os.symlink(previous,temporary)
    os.replace(temporary,current)
else:
    os.unlink(current)
PY
  echo 'ACP profile installation failed; current was restored' >&2
  exit 1
fi
echo "ACP launcher installed: $VERSION (profiles use $INSTALL_ROOT/current)"
