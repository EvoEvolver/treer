---
name: treer
description: Coordinate distributed coding agents through the Treer CLI. Use when the user explicitly mentions Treer or asks to discover, create, inspect, prompt, wait for, send keys to, read, or stop agents registered in a Treer workspace. Requires a Treer-managed agent environment for self-relative operations.
---

# Treer

Treer connects agent servers from multiple machines into one workspace. Use the
`treer` CLI to discover peers and coordinate work without knowing their machine
addresses.

## Verify caller context

Before controlling peers, verify that this process is a Treer-managed agent:

```bash
test -n "${TREER_AGENT_ID:-}" && test -n "${TREER_AGENT_SERVER_URL:-}"
treer whoami
```

If the environment check fails, do not guess a proxy or local server address.
Explain that the process is not running inside a managed Treer agent. A human
may still use `treer --url <agent-server-url> ...` explicitly outside an agent.

The installed binary is the authority for syntax:

```bash
treer --help
treer agent --help
treer machine --help
```

Successful commands print JSON. Read IDs and state from the response instead of
predicting them.

## Understand identity and scope

Treer injects these values into every managed agent:

```bash
printf '%s\n' "$TREER_WORKSPACE_ID" "$TREER_SERVER_ID" "$TREER_AGENT_ID"
```

All discovery and control stays inside `TREER_WORKSPACE_ID`. Agent targets may
be:

- an opaque agent ID;
- a unique agent name;
- `self` or `.` for the calling agent.

Prefer a short unique name for collaboration. If names are duplicated, Treer
returns `agent_ambiguous`; use the exact agent ID.

Discover current topology before choosing a peer or server:

```bash
treer discover
treer agent list
treer agent get self
treer agent get reviewer
```

Use stable IDs when renaming an object. `self` and `.` are accepted for both
the current agent and its machine:

```bash
treer agent rename self coordinator
treer machine rename self build-machine
```

Names are workspace-visible labels; agent IDs and server IDs do not change.

## Create and coordinate a peer

Select an online `server_id` from `treer discover`, then create the requested
agent kind. Preserve the current working directory unless the task requires a
different relative directory.

```bash
treer create --server <server-id> --kind codex --name reviewer --cwd .
```

Native agent arguments go after `--`:

```bash
treer create --server <server-id> --kind codex --name reviewer --cwd . -- <agent-args...>
```

Submit work by unique name and wait for a settled state:

```bash
treer agent prompt reviewer \
  "Review the current diff and report only actionable findings." \
  --wait --timeout 120000
```

Without `--until`, `prompt --wait` settles on `idle`, `blocked`, `exited`, or
`failed` after observed activity. Repeat `--until` when a workflow needs an
exact state:

```bash
treer agent wait reviewer --until blocked --timeout 120000
```

The wait observes lifecycle state and output revisions, not a correlated task
or turn ID. If the target was already working, another active turn may satisfy
the wait. Treat `idle` as terminal readiness, not proof that a specific task is
correct or complete.

Inspect the result after a wait, especially after `blocked` or `failed`:

```bash
treer agent get reviewer
treer agent read reviewer --lines 120
```

`treer agent attach <target>` is reserved for a human using an interactive
terminal on an Agent Server machine. Agents must use `prompt`, `read`, and
`send-keys` instead of opening an attached terminal session. A human can press
`Ctrl-]` to detach without stopping the target.

Delete an agent only when its process and workspace entry should both be
removed. Deletion is persistent and is different from merely stopping it:

```bash
treer agent delete reviewer
```

## Send terminal keys intentionally

Use `prompt` for normal messages. Use `send-keys` only for interactive terminal
controls, approvals, cancellation, or applications that need individual keys:

```bash
treer agent send-keys reviewer esc
treer agent send-keys reviewer ctrl-c
treer agent send-keys reviewer y enter
```

Supported logical keys include `enter`, `tab`, `backspace`, `esc`, `space`,
arrow keys, `home`, `end`, `pageup`, `pagedown`, `delete`, `shift-tab`, and
`ctrl-a` through `ctrl-z`. A single Unicode character is also accepted. Treer
validates every key before sending any bytes.

## Operational boundaries

- Do not stop agents you did not create unless the user asked for it.
- Do not create peers on arbitrary machines; select from the current workspace.
- Use `self` instead of copying the caller's injected ID.
- Read agent output before responding to an unexpected state.
- Do not use `agent attach` from an automated agent workflow; it requires a
  human-operated TTY.
- Do not claim reliable task delivery, durable mailboxes, or strict turn
  correlation; the current collaboration surface is terminal-oriented.
- Use `treer agent stop <target>` only when terminating that process is intended.
- Use `treer agent delete <target>` only when permanent removal is intended.
