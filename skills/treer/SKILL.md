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
treer interface --help
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

## Manage long-running Apps

A Managed App is one supervised command with one HTTP UI endpoint. Its service
and virtual hostname remain stable when the runtime exits or the Controller
reconnects:

```bash
treer app create --machine build-machine --name docs --cwd . --port 8080 \
  --hostname docs.internal python3 -- -m http.server 8080
# Explicitly allow anonymous internet access to this Managed App:
treer app create --public --machine build-machine --name public-docs --cwd . \
  --port 8081 --hostname public-docs.internal python3 -- -m http.server 8081
treer app list
treer app show docs
treer app stop docs
treer app start docs
treer app restart docs
treer app delete docs
```

Read `public_url` and `access` from `treer app create`, `list`, or `show`. Use
`public_url` as the App's external root when present; `access` is `workspace` or
`public`. The URL has no control-plane `/proxy/` suffix and preserves
root-relative assets and redirects. The default managed ingress
requires Workspace authentication. Pass `--public` only when anonymous internet
access is intended; the App must provide its own authentication if it needs one.
Public creation fails, and `public_url` is absent for private Apps, when the
Proxy has no wildcard ingress configured.

Use this only for a single-process HTTP App. It does not install dependencies,
store secrets, migrate state, or isolate hostile code. Service, virtual-host,
and ingress records are owned by the Managed App. `--public` selects access for
that owned ingress, but an Agent cannot create or mutate arbitrary network
records, including through an older CLI. Use the browser control plane for
externally supervised or non-HTTP processes.

## Connect to an existing service

Use an existing virtual hostname normally from a managed Agent. Use the stdio
bridge when a TCP client accepts a proxy command:

```bash
treer network connect database.internal 5432
```

`treer network` intentionally exposes no service, host, or publish mutation
commands. To expose an HTTP process, deploy it with `treer app create`. To show
an Agent-native UI, register an Agent Interface Server.

## Use an Agent Interface Server

An Agent-native integration may register a `treer.agent-interface/v1` HTTP
server on its own private loopback. Registration is self-only and the Controller
verifies the server manifest before publishing its capabilities:

```bash
treer interface register --port 4180 --instance-id pi-session-1 \
  --capability prompt.submit --capability transcript.read \
  --capability state.observe --ui-path /
treer interface show
```

`--ui-path` is optional. When present, Treer replaces the Agent terminal view
with that page and transparently carries HTTP and WebSocket traffic to the same
private Interface port. The page must use relative asset, fetch, and WebSocket
URLs. No machine service, virtual host, or published port is required.

When `prompt.submit` is present, `treer agent prompt` uses the interface instead
of writing to the terminal. Interface failures after dispatch are returned and
are never retried through PTY input. Read a structured interface transcript
with:

```bash
treer agent transcript reviewer --page 0
```

Each page is one conversation turn: a user prompt plus the following entries
until the next user prompt. `--cursor` is an alias for `--page`. `--limit`
selects how many turns to return and defaults to 1. The JSON includes
`page`, `page_count`, and `next_page`.

Transcript requires `transcript.read`; ordinary `treer agent read` continues to
read terminal replay. Attach, send-keys, resize, stop, and delete remain terminal
or Host operations. The interface must deduplicate prompts by `operation_id` and
clear registration during a clean shutdown. The Controller caches a
process-bound descriptor and revalidates it after a hot restart:

```bash
treer interface clear
```

In-tree adapters register the same protocol from launch profiles. Use
`apps/pi-ui` and `apps/codex-ui` for bundled browser UIs, `apps/codex-ais` for a
Codex app-server sidecar without HTML, `apps/opencode-ais` for OpenCode HTTP,
`apps/dsh-ais` for DeepSeek Harness, `apps/claude-ais` for Claude Code
stream-json, `apps/grok-ais` for Grok Build ACP, and `apps/cursor-ais` for
Cursor ACP. Launch Cursor with `cursor-agent`, not `agent`. Each Agent is one
thread/session. Built-in `--kind codex` and `--kind claude` stay on the terminal
path and are not Interfaces.

## Install the Host thread UI

The generic ACP thread UI is installed once per Host. It is not per Agent and
there is no `--ui` on `agent create`. `treer-acp` looks up that Host checkout
and serves it at `/` when `--ui-dist` is unset.

```bash
treer ui install
treer ui install https://github.com/dufangshi/remote-codex-thread-ui-rust.git --ref main
treer ui install --dir /path/to/local-checkout
treer ui show
```

Default git is `https://github.com/dufangshi/remote-codex-thread-ui-rust.git`.
`--dir` uses a local checkout (tests and operators with an existing tree).
Install home, first match:

1. `$TREER_UI_HOME`
2. `$TREER_HOST_ROOT/.treer/ui`
3. the enrolled Host root's `.treer/ui`, found by walking from the current
   directory until `.treer/server-id` exists
4. `~/.treer/ui`

The git checkout is `remote-codex-thread-ui-rust` under that home. `treer ui
show` prints JSON with `git`, `ref`, `path`, `dist_path`, and `installed`.
`treer-acp` registers AIS `ui_path=/`. The Treer iframe should append
`?presentation=embedded-single-thread&explorer=1&shell=0&permissions=0&nav=0`.
ACP permission prompts are auto-allowed on trusted machines; there is no
permission card.

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

If a machine shows Offline while Agents still exist, do not re-enroll it.
Recover on that host with the real workspace ID (`ws_…` from `treer status` or
the copied web command). `service status` with no `--workspace` lists local
installs. `service start` is ready only when the Controller reports
`proxy_connected`; a live loopback API is not a Proxy lease. Duplicate fencing
and lid-close sleep reconnect automatically; if they do not, copy:

```bash
treer-agent-server service --workspace "$TREER_WORKSPACE_ID" restart-controller
treer-agent-server service --workspace "$TREER_WORKSPACE_ID" start
```

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
treer agent admin profile launch reviewer --machine <server-id> \
  --name review-42 --cwd packages/api
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

Use `profile launch --cwd <relative-path>` to override the saved working
directory for one Agent without changing the profile. The path is relative to
the selected machine's Host root and must stay inside that root.

Select an online machine from `treer status`, then create the requested
agent kind. Preserve the current working directory unless the task requires a
different relative directory.

```bash
treer agent admin create --machine <server-id> --kind codex --name reviewer --cwd .
```

```bash
treer agent admin create --machine <server-id> --kind command --name codex-ui --cwd . -- /path/to/codex-agent-ui/scripts/treer-agent.sh
```

To install a public git recipe, pass `--recipe`. `--kind auto` reuses an idle
interactive Agent on that machine when one exists; otherwise Treer starts an
available CLI (Claude, Cursor, Grok, OpenCode, Pi, or Codex) and installs a
missing default if needed. Treer then prompts that Agent with the bundled
install skill (`treer --skill install`). Do not write a second prompt.

```bash
treer --skill install
treer agent admin create --machine <server-id> --kind auto --name installer \
  --recipe https://github.com/example/recipe.git
```

The installer creates a different command Agent and must save a launch
profile from `treer-agent.json` so Launch can create another Agent of that
recipe. Each Agent is one thread. A recipe start script may attach to an
already healthy same-type app server instead of starting another shared
backend. It must still run and register a per-Agent AIS adapter with a unique
`instance_id` that binds semantic operations to that Agent's thread. Do not use
a raw service probe as Interface readiness. Wait until `treer agent show`
reports the required capabilities; for browser recipes also confirm the
Interface descriptor's `ui_path`, then confirm
`treer agent admin profile show`.

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
