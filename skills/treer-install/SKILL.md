---
name: treer-install
description: Install a git recipe as a separate Treer command Agent. Use when an installer Agent is given a recipe URL, or when asked to import, apply, or install a community Agent UI or service from git.
---

# Install a Treer recipe

You are the **installer**. The thing you create is a different Agent. Do not
run the recipe's server, `codex app-server`, or UI process in this process.

An operator or another Agent starts you with an explicit command or launch
profile and submits this skill (`treer --skill install`) together with a recipe
URL. Treer does not choose a provider or interpret `--recipe` for you.

## Verify caller context

```bash
test -n "${TREER_AGENT_ID:-}" && test -n "${TREER_AGENT_SERVER_URL:-}"
treer whoami
```

Use `treer --help` for syntax. Control commands print JSON. Keep
`machine.root` from `whoami`: every `--cwd` you pass to Treer must be relative
to it, never absolute.

## Inspect the recipe, then ask which agents to install

The recipe may need `git`, `node`, `npm`, `curl`, `python3` and `treer` on
PATH. You may be a Claude, Cursor, Grok, OpenCode, Pi or Codex agent; do not
assume Codex. If a required tool or provider CLI is missing, tell the human
which one and stop; do not install provider CLIs yourself.

```bash
REPO_URL="<recipe-url>"
DEST="$PWD/$(basename "$REPO_URL" .git)"
if [ ! -f "$DEST/scripts/apply.sh" ] && [ ! -f "$DEST/treer-agent.json" ]; then
  git clone --depth 1 "$REPO_URL" "$DEST"
fi
```

If the current tree already contains the recipe, inspect it instead of cloning.

**Stop once you know the recipe's agent list.** Do not run `apply.sh` yet.
Discover the supported agents from, in order:

1. `"$DEST/scripts/apply.sh" --list` when that flag exists
2. The recipe README's table of harnesses / agents
3. `treer-agent.json` plus `scripts/apply.sh` argument help (`--agent`)

Ask the human which of those to install and wait. Accept a subset, "all
available", or "none". Do not guess.

Then run the checkout's own installer for **that subset** only:

```bash
# Repeat --agent for each chosen harness, e.g. --agent grok --agent cursor
"$DEST/scripts/apply.sh" --dir "$DEST" --agent <chosen> [--agent <chosen> ...]
```

Never run `apply.sh` without `--agent`: that installs every harness the machine
happens to have. If `apply.sh` is missing, create the command Agent from
`treer-agent.json` with `treer agent admin create --kind command --cwd
<root-relative> -- <run.command> <run.args...>`. Do not invent a second path.

## Save a launch profile

Install once. Each created Agent is one thread; extra conversations are another
Agent via Launch, not another install. Unless `apply.sh` already wrote it,
upsert a workspace launch profile from `treer-agent.json`:

```bash
ROOT="$(treer whoami | python3 -c 'import json,sys; print(json.load(sys.stdin)["machine"]["root"])')"
DEST_REL="$(python3 -c 'import os,sys; print(os.path.relpath(sys.argv[1], sys.argv[2]))' "$DEST" "$ROOT")"
read_json() { python3 -c 'import json,sys; d=json.load(open(sys.argv[1])); print(eval(sys.argv[2]))' "$DEST/treer-agent.json" "$1"; }
PROFILE_NAME="$(read_json 'd["name"]')"
PROFILE_DESC="$(read_json 'd.get("description") or ""')"
RUN_CMD="$(read_json '(d.get("run") or {}).get("command") or "./scripts/treer-agent.sh"')"
if treer agent admin profile show "$PROFILE_NAME" >/dev/null 2>&1; then
  treer agent admin profile update "$PROFILE_NAME" \
    --description "$PROFILE_DESC" --cwd "$DEST_REL" --command "$RUN_CMD" --clear-args
else
  treer agent admin profile create "$PROFILE_NAME" \
    --description "$PROFILE_DESC" --cwd "$DEST_REL" "$RUN_CMD"
fi
```

If `run.args` is present, pass each after `--` on create, or as repeated
`--arg` on update instead of `--clear-args`. Launch commands must be an
absolute executable or a bare name on PATH; a relative `./script.sh` is
rejected, so use an explicit interpreter (`sh scripts/treer-agent.sh`) when the
recipe gives a relative path.

## Verify the Agent Interface

Do not use a raw HTTP or service probe as readiness: a response does not prove
the Agent registered its semantic interface. Success is:

1. `treer agent show <path>` exists and is not `failed` or `exited`.
2. Its `interface` uses `treer.agent-interface/v1` and declares the recipe's
   required capabilities, normally `prompt.submit`, `transcript.read` and
   `state.observe` (plus `prompt.queue` for queued prompts).
3. For a browser UI, the Interface descriptor includes a validated `ui_path`.
4. `treer agent admin profile show "$PROFILE_NAME"` returns the profile.

Leave the created Agent running. A recipe may reuse an already healthy
same-type app server and frontend, but every command Agent still runs its own
loopback AIS adapter with a unique `instance_id`, bound to only that Agent's
thread for prompt, transcript, state and abort.

## Boundaries

- Do not put secrets in a launch profile, argv or recipe URL.
- Do not create a service record or publish a port for Treer iframe UIs; the
  per-Agent `treer interface register --ui-path` covers it.
- The adapter registers with `treer interface register`, refreshes after
  Controller restarts, deduplicates prompts by `operation_id`, and clears its
  registration on clean shutdown.
- Do not stop Agents you did not create.
