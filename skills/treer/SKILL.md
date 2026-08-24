---
name: treer
description: Coordinate distributed coding agents and run commands on workspace machines through the Treer CLI. Use when the user explicitly mentions Treer or asks to discover, create, inspect, prompt, wait for, send keys to, read, stop, or remotely access agents and machines registered in a Treer workspace. Requires a Treer-managed agent environment for self-relative operations.
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
caller from names. `treer status` also includes the same two records under its
top-level `self` field alongside all workspace machines and agents.

If the environment check fails, do not guess a proxy or local server address.
Explain that the process is not running inside a managed Treer agent. A human
may still use `treer --url <agent-server-url> ...` explicitly outside an agent.

The installed binary is the authority for syntax:

```bash
treer --help
treer agent --help
treer agent admin --help
treer agent admin profile --help
treer machine --help
treer message --help
treer network --help
treer ui --help
treer member --help
treer token --help
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
treer status
treer agent list
treer agent show self
treer agent show reviewer
```

Use stable IDs when renaming an object. `self` and `.` are accepted for both
the current agent and its machine:

```bash
treer agent admin rename self coordinator
treer machine rename self build-machine
```

Names are workspace-visible labels; agent IDs and server IDs do not change.

List organization members addressable from the workspace without exposing
their email addresses:

```bash
treer member list
```

## Manage machine services and virtual hosts

Machine services are durable records for long-running processes. Host-network
services outlive the managed Agent that registers them. Agent-scoped services
target a managed Agent's private loopback and are deleted with that Agent.

## Register machine services

Register a service on the current Agent's machine, or select another workspace
machine explicitly:

```bash
treer network service create api --port 8080 --protocol http
treer network service create git --machine build-machine --port 9418 --protocol tcp
treer network service list
treer network service probe api
```

#### Agent-scoped: HTTP server inside an Agent sandbox

If your service runs inside an Agent sandbox (its private network namespace),
register it with `--agent`. Use `--agent self` when creating it from that
Agent, no port forwarding or host-side helper needed:

```bash
treer network service create my-dashboard --agent self --port 8766 --protocol http
treer network service probe my-dashboard
```

The Controller dials the Agent's private service socket directly. From the
outside, an agent-scoped service behaves like a regular machine service:
vhosts, `--service` references, probe, and publish all work the same. Agents in
other namespaces reach it through the Controller instead of raw TCP.

To present an HTTP Agent UI from inside the namespace to a host-loopback
client, create the Agent with `--publish <port>` (`publish_ports` on the API).
Treer maps `127.0.0.1:<port>` on the machine into the namespace. Register that
host loopback port as a machine service (not `--agent`) and run `treer ui set`.

Update a destination without changing its virtual hosts. Deleting a service
also deletes aliases that reference it, but does not stop the external process:

```bash
treer network service update api --port 8081
treer network service delete git
```

Virtual hosts are aliases for registered services. They let every process
inside a managed Linux Agent reach a service by a stable hostname without
publishing the destination machine's port. Records are exact; Treer does not
derive aliases from machine names or reserve a hostname suffix.

Inspect existing records before changing them:

```bash
treer network host list
```

Add a record using a service ID or unique service name:

```bash
treer network host create api.internal api
treer network host create git.internal git
```

Delete only the named alias; this does not delete or stop its service:

```bash
treer network host delete api.internal
```

These commands operate only in `TREER_WORKSPACE_ID`. Service and virtual-host
operations have separate policy actions. Changes take effect immediately for
online Controllers; reconnect and periodic full snapshots provide recovery.

## Publish a custom Agent interface

A managed Agent can replace its terminal in the Treer web application with an
HTTP service registered on its own machine:

```bash
treer network service create agent-dashboard --port 4173 --protocol http
treer ui set agent-dashboard
treer ui show
```

Use `--path` when the application is mounted below its service root:

```bash
treer ui set agent-dashboard --path /treer/
```

The page must use relative asset, fetch, and WebSocket URLs. Treer embeds the
page through the Proxy and carries both ordinary HTTP and WebSocket Upgrade
traffic over the existing Controller WebSocket; do not publish or connect to a
machine port directly. The selected service must use HTTP and belong to the
current Agent's machine. Return to the normal terminal view with:

```bash
treer ui clear
```

Deleting the service, changing it to TCP, or moving it to another machine also
clears the custom interface. The declaration changes only the web presentation;
it does not start, stop, or supervise the service process.

## Publish an HTTP service

Publish a registered HTTP service through the Proxy's wildcard HTTPS domain.
Any HTTP-registered service is publishable, including agent-scoped services
backed by a managed Agent's private loopback:

```bash
treer network publish create my-dashboard --slug dashboard --access public
treer network publish create api --slug issue-tracker --access public
treer network publish list
```

`public` means Treer does not require an identity at the edge; the application
can still use its own cookies, API keys, or `Authorization` header. Use
`workspace` to admit organization members and managed Agents only:

```bash
treer network publish access issue-tracker-a81f.apps.example workspace
TOKEN=$(treer token create api)
curl -H "Treer-Authorization: Bearer $TOKEN" \
  https://issue-tracker-a81f.apps.example/
```

Pause or remove an endpoint without stopping its machine service:

```bash
treer network publish disable issue-tracker-a81f.apps.example
treer network publish enable issue-tracker-a81f.apps.example
treer network publish delete issue-tracker-a81f.apps.example
```

An Agent may publish only services on its own machine. Publishing supports HTTP
and WebSocket traffic; arbitrary TCP remains available only through workspace
virtual hosts. Treer reserves `/.treer/` on every published hostname.

## Authenticate to an identity-aware service

When a registered service explicitly accepts Treer workload identity, request
a short-lived Bearer token using its service ID or unique name:

```bash
TOKEN="$(treer token create api)"
curl -H "Authorization: Bearer $TOKEN" http://api.internal/
```

The token audience is the stable service ID even when the command uses a name.
Use `treer token create api --json` only when the service ID or expiry metadata
is needed. Do not print, log, persist, or send the injected
`TREER_WORKLOAD_CREDENTIAL`; only the local Controller consumes it. Tokens
expire after 60 seconds, so request one immediately before use. Treer does not
automatically add the token to virtual-network requests, and services that do
not implement Treer identity continue to work unchanged.

## Exchange durable Messages

Use Core Messages for asynchronous collaboration that must survive Agent,
Controller, or Proxy restarts. A Message is immutable and may reference one or
more earlier visible Messages through ordered `context_ids`; those edges form a
workspace-scoped DAG.

Inspect unacknowledged deliveries without changing their state:

```bash
treer message receive --wait 30000 --limit 50
```

Record the returned `delivery_id` and `message.message_id`. The same delivery
will be returned again until it is explicitly acknowledged. Process and durably
record any external effect before acknowledging:

```bash
treer message ack <delivery-id> --operation-id <stable-operation-id>
```

Send a new Message using a stable recipient ID or unique name. Prefer stdin for
multiline or generated content so the body is not exposed in process arguments:

```bash
printf '%s\n' 'Review is complete.' | \
  treer message send --to coordinator --idempotency-key task-42-result --body-file -
```

Use a sender-scoped idempotency key whenever a send may be retried. Repeating an
identical request returns the original Message; reusing the key with different
content fails. Reply by stable Message ID to preserve conversational context:

```bash
printf '%s\n' 'I addressed both findings.' | \
  treer message reply <message-id> --to sender --body-file -
```

`reply` reads the parent first, uses its sender when `--to` is omitted or is
`sender`, and creates an ordinary `message.send` operation with the parent as
the first context. Read history without acknowledging deliveries:

```bash
treer message get <message-id>
treer message list --limit 50
```

Context edges do not grant access to a parent's body. A missing and an invisible
Message produce the same external error. Message policy actions are separate:
`message.send`, `message.read`, `message.receive`, and `message.ack`. Import is
reserved for a local operator performing an explicit migration.

A Message does not wake or write to another Agent's terminal. When immediate
attention is required, send the durable Message first and then use
`treer agent prompt` with only the Message ID. `agent.prompt` is a separate,
stronger policy action; do not copy the Message body into the prompt.

An operator may run a channel App inside a dedicated managed Agent. Such a
process uses the same `treer` CLI and identity as the Agent; there is no App
installer, broker, or sandbox exposed through this skill. Core still evaluates
the Agent's workspace Policy for each command. App configuration, secrets,
state, supervision, and isolation are operator workflows documented under
`apps/`.

Core Message routes have an operator-controlled rollout gate and default off. A
`core_messages_disabled` result means the deployment is not enabled for the
workflow; report it instead of inventing another integration path.

Delete a machine only when it and all of its agents should be removed from the
workspace. This revokes its credential but does not uninstall the service on
that machine:

```bash
treer machine delete <server-id>
```


## Create and coordinate a peer

Use a launch profile when the same executable, working directory, and arguments
will be reused. Profiles are workspace-scoped and are addressable by stable ID
or unique name:

```bash
treer agent admin profile create reviewer --description "Review current changes" --cwd . \
  codex -- review --base main
treer agent admin profile list
treer agent admin profile show reviewer
treer agent admin profile launch reviewer --machine <server-id> --name review-42
```

Edit individual fields without replacing the profile. Repeat `--arg` to replace
the complete argument array, or use `--clear-args` to empty it:

```bash
treer agent admin profile update reviewer --cwd packages/api
treer agent admin profile update reviewer --arg review --arg=--base --arg main
treer agent admin profile delete reviewer
```

The command and arguments are passed directly as an argv vector. Shell syntax
is not interpreted unless the executable is an explicit shell such as `sh` and
its arguments request evaluation. Profile fields are stored as plaintext and
may be read by workspace members and policy-authorized Agents; never put API
keys, tokens, passwords, or other secrets in a profile.

Profile operations have separate `launch_profile.list`,
`launch_profile.read`, `launch_profile.create`, `launch_profile.update`,
`launch_profile.delete`, and `launch_profile.use` policy actions. A launch must
also pass `agent.create` for its selected machine. Inspect a profile before
launching it, especially when another principal last updated it.

Select an online machine from `treer status`, then create the requested
agent kind. Preserve the current working directory unless the task requires a
different relative directory.

```bash
treer agent admin create --machine <server-id> --kind codex --name reviewer --cwd .
```

```bash
treer agent admin create --machine <server-id> --kind command --name codex-ui --cwd . --publish 4173 -- /path/to/codex-agent-ui/scripts/treer-agent.sh
```

Native agent arguments go after `--`:

```bash
treer agent admin create --machine <server-id> --kind codex --name reviewer --cwd . -- <agent-args...>
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
treer agent show reviewer
treer agent read reviewer --lines 120
```

`treer agent attach <target>` is reserved for a human using an interactive
terminal on an Agent Server machine. Agents must use `prompt`, `read`, and
`send-keys` instead of opening an attached terminal session. A human can press
`Ctrl-]` to detach without stopping the target.

Delete an agent only when its process and workspace entry should both be
removed. Deletion is persistent and is different from merely stopping it:

```bash
treer agent admin delete reviewer
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
- Do not claim strict turn correlation for terminal-oriented `prompt --wait`.
- Use `treer agent admin stop <target>` only when terminating that process is intended.
- Use `treer agent admin delete <target>` only when permanent removal is intended.
- Use `treer machine delete <server-id>` only when the user explicitly asks to
  remove that machine; it also removes every agent registered on it.
