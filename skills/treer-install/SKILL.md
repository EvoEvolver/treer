---
name: treer-install
description: Install a git recipe as a separate Treer command Agent. Use when Treer starts an installer with a recipe URL, or when asked to import, apply, or install a community Agent UI or service from git.
---

# Install a Treer recipe

You are the **installer**. The thing you create is a different Agent.
Do not run the recipe's server, `codex app-server`, or UI process in this
process.

Treer starts this turn with this skill as the base prompt and a recipe URL.
Do not wait for another human prompt.

## Verify caller context

This process must be a Treer-managed Agent:

```bash
test -n "${TREER_AGENT_ID:-}" && test -n "${TREER_AGENT_SERVER_URL:-}"
treer whoami
```

Use `treer --help` for syntax. Control commands print JSON.

## Install from the recipe URL

Read the **Recipe URL** from the `This install` section of this prompt.
Need `git`, `node`, `npm`, `curl`, and `treer` on PATH when the recipe requires
them. If the recipe needs `codex` and it is missing:

```bash
npm install -g @openai/codex
```

```bash
REPO_URL="<recipe-url>"
DEST="${TREER_RECIPE_DIR:-$PWD/$(basename "$REPO_URL" .git)}"
if [ ! -f "$DEST/scripts/apply.sh" ] && [ ! -f "$DEST/treer-agent.json" ]; then
  git clone --depth 1 "$REPO_URL" "$DEST"
fi
```

Prefer the checkout's own installer. Do not invent a second path.

```bash
if [ -f "$DEST/scripts/apply.sh" ]; then
  "$DEST/scripts/apply.sh" --dir "$DEST"
elif [ -f "$DEST/treer-agent.json" ]; then
  # Follow install/run from treer-agent.json using Host-relative --cwd.
  # Create a command Agent; do not launch the server here.
  echo "apply.sh missing; follow treer-agent.json with treer agent admin create"
  exit 1
fi
```

If this checkout already contains `scripts/apply.sh`, skip clone and run that
script.

`--cwd` for `treer agent admin create` must be relative to the Host root from
`treer whoami` (`machine.root`). Do not pass an absolute working directory.

## Save a launch profile

Install once. Each created Agent is one thread. Extra conversations are
another Agent via Launch, not another install. After `apply.sh` (or after you
create the first command Agent), upsert a workspace launch profile from
`treer-agent.json` so Launch can create another Agent of this recipe:

```bash
# $DEST is the recipe checkout. --cwd must be Host-relative, same as the Agent.
PROFILE_NAME="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["name"])' "$DEST/treer-agent.json")"
PROFILE_DESC="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1])).get("description") or "")' "$DEST/treer-agent.json")"
RUN_CMD="$(python3 -c 'import json,sys; print((json.load(open(sys.argv[1])).get("run") or {}).get("command") or "./scripts/treer-agent.sh")' "$DEST/treer-agent.json")"
if treer agent admin profile show "$PROFILE_NAME" >/dev/null 2>&1; then
  treer agent admin profile update "$PROFILE_NAME" \
    --description "$PROFILE_DESC" --cwd "$DEST_REL" --command "$RUN_CMD" --clear-args
else
  treer agent admin profile create "$PROFILE_NAME" \
    --description "$PROFILE_DESC" --cwd "$DEST_REL" "$RUN_CMD"
fi
```

Prefer the checkout's `apply.sh` if it already writes this profile. Do not
invent a second command line. Prefer `run.command` / `run.args` from
`treer-agent.json`.

## Do not probe another Agent's service

`treer network service probe` is rejected with `service_not_owned` when the
service belongs to the Agent you created. That is expected. Do not retry probe
in a loop.

Success is workspace discovery plus a reusable Launch option:

1. `treer agent show <name>` exists and is not `failed` or `exited`.
2. `treer network service list` shows an Agent-scoped HTTP service for that Agent.
3. `treer status` includes an `agent_uis` entry for that Agent.
4. `treer agent admin profile show` returns the recipe's launch profile.

If `apply.sh` is still waiting on probe after the UI is registered, treat the
install as done and stop. Leave the created Agent running. Extra conversations
use Launch to create another Agent. A recipe start script may attach to an
already healthy same-type listener instead of starting another process.

## Boundaries

- Do not put secrets in a launch profile or recipe URL.
- Do not use `--publish` for Treer iframe UIs. The start script registers
  `--agent self` and `treer ui set`.
- Do not stop Agents you did not create.
