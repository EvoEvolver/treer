#!/bin/sh
treer_agent_bootstrap() (
  set -eu
  bootstrap_ref=
  bootstrap_repository=https://github.com/EvoEvolver/treer.git
  bootstrap_option=
  for bootstrap_argument do
    if [ -n "$bootstrap_option" ]; then
      case "$bootstrap_option" in
        --ref) bootstrap_ref=$bootstrap_argument ;;
        --repository) bootstrap_repository=$bootstrap_argument ;;
      esac
      bootstrap_option=
    else
      case "$bootstrap_argument" in
        --ref|--repository) bootstrap_option=$bootstrap_argument ;;
        --source) printf '%s\n' 'bootstrap: --source is not allowed; the verified commit supplies the installer.' >&2; exit 1 ;;
      esac
    fi
  done
  if [ -n "$bootstrap_option" ]; then
    printf '%s\n' "bootstrap: $bootstrap_option requires a value." >&2; exit 1
  fi
  case "$bootstrap_ref" in
    *[!0123456789abcdef]*|'') printf '%s\n' 'bootstrap: --ref requires a full 40-character lowercase commit SHA, never a branch/tag.' >&2; exit 1 ;;
  esac
  if [ "${#bootstrap_ref}" -ne 40 ]; then
    printf '%s\n' 'bootstrap: --ref requires a full 40-character lowercase commit SHA, never a branch/tag.' >&2; exit 1
  fi
  for bootstrap_tool in treer node npm git tar; do
    if ! command -v "$bootstrap_tool" >/dev/null 2>&1; then
      case "$bootstrap_tool" in
        treer) bootstrap_hint='Install and connect the Treer CLI first; see docs/operator-guide.md in the Treer repository.' ;;
        node|npm) bootstrap_hint='Install Node.js 22+ (including npm) from https://nodejs.org/ or your package manager.' ;;
        git) bootstrap_hint='Install Git using your OS package manager or https://git-scm.com/downloads.' ;;
        tar) bootstrap_hint='Install tar using your OS package manager (for example apt install tar or brew install gnu-tar).' ;;
      esac
      printf '%s\n' "bootstrap: missing $bootstrap_tool. $bootstrap_hint" >&2; exit 1
    fi
  done
  if ! node -e 'if (Number(process.versions.node.split(".")[0]) < 22) process.exit(1)' </dev/null; then
    printf '%s\n' 'bootstrap: Node.js 22+ is required; upgrade from https://nodejs.org/ or your package manager.' >&2; exit 1
  fi
  bootstrap_tmp=$(mktemp -d "${TMPDIR:-/tmp}/treer-agent-bootstrap.XXXXXX")
  trap 'rm -rf -- "$bootstrap_tmp"' 0
  trap 'exit 1' HUP INT TERM
  bootstrap_checkout=$bootstrap_tmp/checkout
  git clone --depth 1 --filter=blob:none --no-checkout -- "$bootstrap_repository" "$bootstrap_checkout" </dev/null
  git -C "$bootstrap_checkout" sparse-checkout init --cone </dev/null
  git -C "$bootstrap_checkout" sparse-checkout set launchers/agent </dev/null
  git -C "$bootstrap_checkout" fetch --depth 1 origin "$bootstrap_ref" </dev/null
  git -C "$bootstrap_checkout" checkout --detach FETCH_HEAD </dev/null
  bootstrap_head=$(git -C "$bootstrap_checkout" rev-parse HEAD </dev/null)
  if [ "$bootstrap_head" != "$bootstrap_ref" ]; then
    printf '%s\n' 'bootstrap: downloaded HEAD does not match the requested SHA; refusing to run installer.' >&2; exit 1
  fi
  sh "$bootstrap_checkout/launchers/agent/install.sh" --source "$bootstrap_checkout" "$@" </dev/null
)
treer_agent_bootstrap "$@" </dev/null
