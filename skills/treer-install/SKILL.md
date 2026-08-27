---
name: treer-install
description: Install a git recipe as a separate Treer command Agent. Use when Treer starts an installer with a recipe URL, or when asked to import, apply, or install a community Agent UI or service from git.
---

# Install a Treer recipe

You are the **installer**. The thing you create is a different Agent.
Do not run the recipe's server, `codex app-server`, or UI process in this
process.

Treer starts this turn with this skill as the base prompt and a recipe URL.

## Verify caller context

This process must be a Treer-managed Agent:

```bash
test -n "${TREER_AGENT_ID:-}" && test -n "${TREER_AGENT_SERVER_URL:-}"
treer whoami
```

Use `treer --help` for syntax. Control commands print JSON.

## Inspect the recipe, then ask which agents to install

Read the **Recipe URL** from the `This install` section of this prompt.
Need `git`, `node`, `npm`, `curl`, and `treer` on PATH when the recipe requires
them. You may already be a running Claude, Cursor, Grok, OpenCode, Pi, or Codex
agent; do not assume you are Codex. If a required command is missing, install
it before continuing. If the recipe needs `codex` and it is missing:

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

**Stop after you know the recipe's agent list.** Do not run `apply.sh` yet.
Discover the supported agents from, in order:

1. `"$DEST/scripts/apply.sh" --list` when that flag exists
2. The recipe README table of harnesses / agents
3. `treer-agent.json` plus `scripts/apply.sh` argument help (`--agent`)

Then ask the human which of those agents to install. Wait for their answer.
Accept a subset, "all available", or "none". Do not guess.

Only after they answer, run the checkout's installer for **that subset**:

```bash
if [ -f "$DEST/scripts/apply.sh" ]; then
  # Repeat --agent for each chosen harness, for example:
  # "$DEST/scripts/apply.sh" --dir "$DEST" --agent grok --agent cursor
  "$DEST/scripts/apply.sh" --dir "$DEST" --agent <chosen> [--agent <chosen> ...]
elif [ -f "$DEST/treer-agent.json" ]; then
  echo "apply.sh missing; follow treer-agent.json with treer agent admin create"
  exit 1
fi
```

Prefer the checkout's own installer. Do not invent a second path.
If this checkout already contains `scripts/apply.sh`, skip clone and inspect
that tree. Never run `apply.sh` with no `--agent` from this installer flow:
that installs every harness the machine happens to have.

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

## Verify the Agent Interface

Do not use `treer network service probe` as the readiness signal. A successful
HTTP response does not prove that the Agent registered its semantic interface
or that the required capabilities are present.

Success is workspace discovery plus a reusable Launch option:

1. `treer agent show <name>` exists and is not `failed` or `exited`.
2. That Agent's `interface` uses `treer.agent-interface/v1` and declares the
   recipe's required capabilities, normally `prompt.submit`, `transcript.read`,
   and `state.observe`.
3. For a browser UI, that Interface descriptor includes a validated `ui_path`.
4. `treer agent admin profile show` returns the recipe's launch profile.

Leave the created Agent running. Extra conversations use Launch to create
another Agent. A recipe may reuse an already healthy same-type app server and
frontend, but every command Agent still represents one thread and must run its
own loopback AIS adapter with a unique `instance_id`. That adapter must bind all
prompt, transcript, state, and abort operations to only that Agent's thread.

## Boundaries

- Do not put secrets in a launch profile or recipe URL.
- Do not use `--publish` or create a service record for Treer iframe UIs. The
  per-Agent Interface registration includes `--ui-path` and its private port.
- Register the per-Agent adapter with `treer interface register`, refresh the
  registration after Controller restarts, deduplicate prompts by
  `operation_id`, and clear it on clean shutdown.
- Do not stop Agents you did not create.
