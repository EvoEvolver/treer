---
name: treer
description: Coordinate distributed coding agents and run commands on workspace machines through the Treer CLI. Use when the user explicitly mentions Treer or asks to discover, create, inspect, mail, read an inbox, prompt, wait for, send keys to, read, stop, or remotely access agents and machines registered in a Treer workspace. Requires a Treer-managed agent environment for self-relative operations.
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

`treer whoami` returns the current workspace plus the complete `agent` and
`machine` records. Treat those records as the caller identity; do not infer the
caller from names. `treer discover` also includes the same two records under its
top-level `self` field alongside all workspace machines and agents.

If the environment check fails, do not guess a proxy or local server address.
Explain that the process is not running inside a managed Treer agent. A human
may still use `treer --url <agent-server-url> ...` explicitly outside an agent.

The installed binary is the authority for syntax:

```bash
treer --help
treer agent --help
treer machine --help
treer service --help
treer virtual-host --help
```

Control commands print JSON. Read IDs and state from the response instead of
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
treer whoami
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

## Manage machine services and virtual hosts

Machine services are durable records for long-running processes reachable from
a machine's host network. They outlive the managed Agent that registers or
maintains them. A server started directly inside a Linux managed Agent remains
in that Agent's private network namespace; run long-lived services through a
host facility such as systemd or a Docker published port before registering
them.

Register a service on the current Agent's machine, or select another workspace
machine explicitly:

```bash
treer service register api --port 8080 --protocol http
treer service register git --machine build-machine --port 9418 --protocol tcp
treer service list
treer service probe api
```

Update a destination without changing its virtual hosts. Deleting a service
also deletes aliases that reference it, but does not stop the external process:

```bash
treer service update api --port 8081
treer service delete git
```

Virtual hosts are aliases for registered services. They let every process
inside a managed Linux Agent reach a service by a stable hostname without
publishing the destination machine's port. Records are exact; Treer does not
derive aliases from machine names or reserve a hostname suffix.

Inspect existing records before changing them:

```bash
treer virtual-host list
```

Add a record using a service ID or unique service name:

```bash
treer virtual-host add api.internal api
treer virtual-host add git.internal git
```

Delete only the named alias; this does not delete or stop its service:

```bash
treer virtual-host delete api.internal
```

These commands operate only in `TREER_WORKSPACE_ID`. Service and virtual-host
operations have separate policy actions. Changes take effect immediately for
online Controllers; reconnect and periodic full snapshots provide recovery.

## Authenticate to an identity-aware service

When a registered service explicitly accepts Treer workload identity, request
a short-lived Bearer token using its service ID or unique name:

```bash
TOKEN="$(treer identity token api)"
curl -H "Authorization: Bearer $TOKEN" http://api.internal/
```

The token audience is the stable service ID even when the command uses a name.
Use `treer identity token api --json` only when the service ID or expiry metadata
is needed. Do not print, log, persist, or send the injected
`TREER_WORKLOAD_CREDENTIAL`; only the local Controller consumes it. Tokens
expire after 60 seconds, so request one immediately before use. Treer does not
automatically add the token to virtual-network requests, and services that do
not implement Treer identity continue to work unchanged.

Delete a machine only when it and all of its agents should be removed from the
workspace. This revokes its credential but does not uninstall the service on
that machine:

```bash
treer machine delete <server-id>
```

## Exchange non-interrupting mail

Use durable mail for asynchronous coordination that must not inject terminal
input or start a turn in the recipient. A root message needs one or more
recipients and no context:

```bash
treer mail --to reviewer "Review the parser when you next check your inbox."
```

Use the exact returned `message_id` as context when replying. Repeat `--to` and
`--context` for a group message or a merge in the message graph:

```bash
treer mail --to coordinator --context msg_123 "Review complete; two findings."
treer mail -t coordinator -t tester -c msg_123 -c msg_456 "Both checks agree."
```

Mail does not notify, prompt, wake, or otherwise interrupt recipients. An Agent
sees unread mail only when it explicitly calls:

```bash
treer inbox
treer inbox --limit 100
```

`inbox` returns the oldest unread batch as JSON and marks that returned batch
read. Check `remaining_unread` and call it again when it is nonzero. Preserve
message IDs from the response when a later message should reference them.

Recipient targets use one shared address space for Agents and humans. `--to`
accepts an Agent ID, user ID, unique Agent name, unique preferred name, or
`self`/`.` for the calling Agent. Stable IDs take precedence over display-name
matches. If a name matches more than one Agent or human, Treer returns
`recipient_ambiguous`; use a stable ID. Context messages must belong to the same
workspace and must have been sent or received by the caller. Use `mail` for
deferred collaboration; use `agent prompt` only when intentionally starting
work in another Agent's terminal session.

The human directory for a workspace is its parent organization's member list.
Discover stable human addresses without exposing member email addresses:

```bash
treer human list
```

Use the same repeatable `--to` option for humans and Agents:

```bash
treer mail --to usr_123 "The deployment is ready for review."
treer mail --to reviewer --to Owner "Please coordinate on this result."
```

Preferred names are valid only when unique across the combined Agent and human
directory; IDs remain stable when names change. Humans read their workspace
inbox from the web application; opening it marks only the returned batch read.
Sending still does not notify or interrupt the human or any Agent.

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
- Mail is durable but pull-only. Do not claim that it wakes a recipient, reaches
  a deleted Agent identity, or proves that the message body was acted upon.
- Do not claim strict turn correlation for terminal-oriented `prompt --wait`.
- Use `treer agent stop <target>` only when terminating that process is intended.
- Use `treer agent delete <target>` only when permanent removal is intended.
- Use `treer machine delete <server-id>` only when the user explicitly asks to
  remove that machine; it also removes every agent registered on it.
