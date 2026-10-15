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
test -n "${TREER_AGENT_ID:-}" && test -n "${TREER_WORKLOAD_CREDENTIAL:-}" && test -n "${TREER_AGENT_SERVER_URL:-}"
treer whoami
```

`treer whoami` returns the current workspace plus the complete `agent` and
`machine` records. Treat those records as the caller identity; do not infer the
caller from names. `treer --format json status` also includes the same two records under its
top-level `self` field alongside all workspace machines and agents.

If the environment check fails, do not guess a proxy or local server address.
Explain that the process is not running inside a managed Treer agent. A human
may still use `treer --url <agent-server-url> ...` explicitly outside an agent.

The Controller supplies the command tree, help, and skills. The thin client
preserves argv and streams stdin and output. Set `TREER_IDENTITY_MODE=agent`
for independently configured Agent clients: missing identity or credentials
fail without reading operator credentials. Connection options belong before
the business command. With neither `TREER_WORKSPACE_ID` nor
`TREER_AGENT_SERVER_URL` set, independent clients may explicitly select their
connection: credentials are sent to the explicit URL, which the caller must
trust. If either binding exists, explicit connection flags must match their
corresponding binding; overriding a missing counterpart also fails closed.
The client displays `status`, `agent ls`, and `machine list` as tables only when
stdout is a terminal; pipes retain original JSON. Use `--format json` before
these commands when parsing their full response in an interactive terminal,
or `--format table` for a compact display. This display flag is not forwarded.

The Controller must be online for business help:

```bash
treer --help
treer agent --help
treer agent admin --help
treer agent admin profile --help
treer machine --help
treer exec --help
treer message --help
treer service --help
treer runtime --help
treer interface --help
treer member --help
treer token --help
```

Control commands print JSON. Read IDs and state from the response instead of
predicting them.

Default launcher installation is a separate local Controller-binary operation:
`treer-agent-server launcher install|update|status|rollback`. It needs trusted
Node.js 22+, installs the binary's verified matching payload and saves profiles;
it never creates/restarts Agents or restarts the Host/Controller. Run
`treer interface contract` for the authenticated enrolled machine/root and actual
Controller compatibility. Missing support means update is needed, not permission
to fall back silently. See the operator guide before legacy/source installation.

## Understand identity and scope

Treer injects these values into every managed agent:

```bash
printf '%s\n' "$TREER_WORKSPACE_ID" "$TREER_SERVER_ID" "$TREER_AGENT_ID"
```

All discovery and control stays inside `TREER_WORKSPACE_ID`. Agent targets may
be:

- an opaque agent ID;
- an absolute tree path (`/team/reviewer`) or a relative path (`../team/reviewer`);
- a bare name only for a sibling in the caller's containing folder;
- `self` or `.` for the calling agent.

Prefer paths for collaboration. Agent names are unique within a folder, not the
whole workspace. Relative paths start at your containing Tree folder; operators
start at `/`. A missing bare sibling returns `agent_not_in_folder` with up to ten
same-name absolute paths in `hint`; it never falls back to a global name match.
Agent output includes `path`, and Tree **Copy path** can be pasted as a CLI target.
New CLI clients require Controller capability `agent.path-targets.v1`; an older
Controller returns an upgrade error before dispatch, without disconnecting.

Agent creation, profile launch and `treer app create` omit `--folder` to inherit
your authenticated identity's direct Tree folder. Human/operator callers start
at root. Use `--folder /` for explicit root or `--folder PATH` for another
authorized destination; `cwd` is unrelated. Proxy reserves identity/name/placement
atomically before startup. An unplaced caller receives `caller_unplaced`; ask
the owner to place it or request an authorized explicit destination. Existing
nodes are not moved. Creation requires `workload.create-placement.v1`; older
Controller/Proxy versions fail instead of silently placing at root.

Folders and path Deny are not a sandbox or containment boundary. Be cautious
granting `agent.create` and `agent.tree.manage`: explicit creation elsewhere or
moving to an Allow path remains possible. Monitor mode never enforces Deny;
unplaced legacy identities do not match path rules. Use reviewed Policy and
external OS isolation when needed.

Discover current topology before choosing a peer or server:

```bash
treer whoami
treer --format json status
treer --format json agent ls -R /
treer agent show self
treer agent show /reviewer
```

Use stable IDs when renaming an object. `self` and `.` are accepted for both
the current agent and its machine:

```bash
treer agent admin rename self coordinator
treer machine rename self build-machine
```

Register an explicit command when this Agent should return after a full Treer
Host restart:

```bash
treer agent startup set --cwd . -- ./scripts/start-agent.sh
treer agent startup show
treer agent startup clear
```

This is self-only persistence. It does not automatically restart an ordinary
Agent after a successful exit, and `treer agent stop` disables it. Do not put
secrets in argv. Explicit start can launch a stopped or completed workload again:

```bash
treer agent start /reviewer
treer agent stop /reviewer
treer agent restart /reviewer
treer agent read /reviewer --lines 100
```

These lifecycle commands also accept managed App runtime IDs, paths, and sibling names.
To repair a stuck peer without deleting its identity, use the separate
Policy-controlled startup management commands:

```bash
treer agent admin startup show /reviewer
treer agent admin startup set /reviewer --cwd repo -- ./scripts/start-agent.sh
treer agent restart /reviewer
# Atomically save new argv and restart once:
treer agent restart /reviewer --cwd repo -- codex --no-daemon resume THREAD_ID
```

Targets accept ID, tree path, sibling name, or `self`. Omitted `--cwd` keeps the target's
saved directory. Admin set alone preserves stopped intent and does not launch.
Admin show/set require `agent.startup.manage` on the target; atomic restart also
requires ordinary lifecycle permissions. This does not change the self-only
startup commands above. App runtimes reject startup edits with
`app_startup_managed`; their command belongs to the App deployment. Controllers
without `agent.startup-admin.v1` return `unsupported_command` without disconnecting.
Keep resume/session arguments when switching launchers to retain context. Never
embed credentials in argv. A failed launch leaves the new command saved for
repair/retry. The browser Agent menu also offers **Edit startup command**.

`treer app start/stop/restart` and `treer agent admin start/stop/restart` remain
compatibility commands. Restart keeps the Agent ID and workload credential;
`run_generation` in Agent output increases for a replacement process. The
Controller owns startup and recovery for both kinds, adopts surviving Host
processes after a hot replacement, and validates identities before cold
recovery. Enabled Apps retry exited processes; ordinary Agents do not restart
automatically on the same Host. Stop prevents automatic recovery until an
explicit start or restart.

Edit managed App launch fields through the deployment, preserving its stable
ID, credential, service and public origin:

```bash
treer app update /infra/issues --cwd apps-deployed/issues --command ./start.sh --clear-args
treer app update /infra/issues --command python3 -- issues.py
```

Omitted fields remain unchanged (even args when only command changes).
Arguments after `--` replace the full array; `--clear-args` empties it.
`cwd` must exist beneath the machine root. A running App restarts once; a stopped
App remains stopped. This requires `agent.startup.manage` plus ordinary restart
permissions. `app_update_unsupported`, `unsupported_command` / `app.update.v1`, or a missing Proxy route
means upgrade; do not delete/recreate as a workaround. Failed launch leaves the
new specification saved for repair/start/restart or explicit rollback. Keep
secrets/environment in private entry configuration, never argv.

Names are workspace-visible labels; agent IDs and server IDs do not change.

Organize Agents and Apps in the shared workspace tree:

```bash
treer agent mkdir -p /research/experiments
treer --format json agent ls /research
treer --format json agent ls -R /
treer agent mv /reviewer /research/experiments/
treer agent mv /research/experiments /research/runs
treer agent mv /research/experiments/reviewer /
treer agent rmdir /research/runs
```

Paths are case-sensitive. For an Agent, relative paths start in the folder
containing that Agent; for an operator they start at workspace root. `.` and
`..` traverse from there, while `/` starts at workspace root. `ls` without a
path lists the current folder and includes a `..` entry outside root. `ls`
prints JSON `path`, `folders`, and `agents` fields; `ls -R /` lists the entire
tree. `mkdir -p` creates missing parents and accepts existing folders. `-R`
includes descendants. `mv` moves an Agent/App by ID, name in the current
folder, `self`, or tree path into an existing folder. `.` means the current
folder. It also moves or renames a folder without overwriting another folder.
To rename an Agent/App, use `treer agent admin rename TARGET NAME`. `rmdir` rejects nonempty folders and
never stops or deletes workloads. Folder IDs from `ls` are also accepted by
`ls`, `mv`, and `rmdir` when names are ambiguous. These operations use the
Agent's existing credential and the `agent.tree.read` / `agent.tree.manage`
Policy actions; no user session or extra token is needed.

List organization members addressable from the workspace without exposing
their email addresses:

```bash
treer member list
```

## Manage long-running Apps

A Managed App is one supervised command with one HTTP UI endpoint. Its service
and platform-assigned ingress address remain stable when the runtime exits or the Controller
reconnects:

```bash
treer app create --machine build-machine --name docs --cwd . --port 8080 \
  python3 -- -m http.server 8080
# Explicitly allow anonymous internet access to this Managed App:
treer app create --public --machine build-machine --name public-docs --cwd . \
  --port 8081 python3 -- -m http.server 8081
treer app list
treer app show /docs
treer app stop /docs
treer app start /docs
treer app restart /docs
treer app delete /docs
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
store secrets, migrate state, or isolate hostile code. Service and ingress
records are owned by the Managed App. `--public` selects access for that owned
ingress. Agents can register and publish their own services under Policy; they
cannot register unrelated workloads. Use the browser control plane for
externally supervised or non-HTTP processes.

## Connect to an existing service

Register a running process on your own loopback, then forward its stable service
ID through an authenticated local listener:

```bash
treer service register --name database --port 5432
treer service forward <service-id> --listen 127.0.0.1:15432
```

Private forwarding is the default. `treer service publish <service-id>`
allocates a platform-managed HTTP address with workspace authentication;
`--access public` explicitly removes that ingress authentication. Raw TCP
publication uses `--tcp --access public` and requires a configured platform
port range. Treer registers and routes processes; the user owns their runtime
environment and its isolation. Transparent network proxying is retired.
Use `treer service unpublish <service-id>` to revoke HTTP ingress while retaining
the running service, or add `--tcp` to revoke its public TCP listener.

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

`treer agent prompt` defaults to `delivery:queue`: it sends immediately when
idle and queues behind a busy turn. It requires `prompt.queue`; a terminal-only
or older Interface returns `queue_unsupported` rather than inserting text.
Use `--steer` explicitly for insertion (including legacy PTY input). Interface
failures after dispatch are never retried through PTY input. Read a structured interface transcript
with:

```bash
treer agent transcript /reviewer --page 0
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

The single `launchers/agent` launcher registers this protocol and serves the
shared browser UI. Its `src/harness/` adapters speak native Codex app-server,
native Claude stream-json, or generic ACP (Gemini, Grok, Cursor, OpenCode).
Install/update explicitly with `launchers/agent/install.sh --agent PROVIDER`,
then inspect the saved profile with `treer agent admin profile show NAME`.
Installation does not create or restart an Agent. Launch Cursor with
`cursor-agent`, not `agent`. Each Agent owns one thread/session. New integrations
implement `HarnessRuntime` and reuse the server/UI; do not add another AIS App.
Provider runtimes belong in explicit commands/profiles, not built-in Agent kinds.

## Communicate with another Agent over HTTP

An Agent can expose its own HTTP API with any schema. From the serving Agent's
runtime, register its primary communication endpoint after starting the server:

```bash
treer service register --primary --protocol http --name reviewer-api --port 4180
```

Each Agent has at most one primary HTTP service. Repeating this command updates
its name and port through the service update API while preserving its service ID.
If no primary exists, a same-name service owned by this Agent is promoted in
place; another Agent's service is never adopted.
Put server startup and registration in the saved launch command so cold recovery
re-registers the current port. The service record itself does not start a server.
Primary registration requires a managed Agent identity; it cannot bind another
Agent's endpoint. Ordinary services remain available alongside the primary.

The calling Agent discovers the address, then uses an ordinary HTTP client:

```bash
addr=$(treer agent address /reviewer)
curl --fail-with-body "$addr/openapi.json"
curl --fail-with-body -X POST "$addr/review" \
  -H 'Content-Type: application/json' -d @request.json
treer agent address /reviewer --json
```

The schema path and request routes above are examples owned by the serving Agent.
Treer does not interpret its schema. `address` accepts a tree path, sibling name, or ID,
defaults to `self`, and prints only `http://SERVICE_ID.treer/` with a newline.
`--json` returns `agent_id`, `service_id`, and `address`. A missing primary is an
error; other owned services are never selected implicitly. Addresses remain
stable across Agent restarts and service renames, but deletion/recreation of the
service assigns a new ID. Calls use the inherited HTTP proxy and source identity
described below. HTTP delivery requires a running destination; it adds no offline
queue or automatic retries. An AIS listener may also be registered as the primary
service, but `treer interface register` alone does not create a service address.

## Authenticate to an identity-aware service

New managed Agents and Apps inherit a dedicated HTTP proxy entry. For ordinary
HTTP code or curl, use a service name or stable ID from `treer service list`:

```bash
curl http://example-http.treer/path
# Or use the stable ID:
curl http://svc_YOUR_SERVICE_ID.treer/path
```

Names are unique within the workspace, matched case-insensitively, and resolved
for each new request. Renaming changes the name address while ID addresses keep
working. Name addresses accept 1-63 ASCII letters/digits with internal hyphens;
use the service ID for other display names. CONNECT accepts either form too.
Replace the entire `svc_YOUR_SERVICE_ID` with the real ID. The proxy-only name
needs no DNS setup. The proxy authenticates as this Agent and supplies a fresh
service-audience token through `X-Treer-Identity-Token`, also setting Bearer
Authorization if no application Authorization was supplied. Subprocesses inherit
the same `http_proxy`/`https_proxy` environment; clients ignoring these variables
must explicitly configure `TREER_HTTP_PROXY`. Localhost bypasses the proxy.
External HTTP/HTTPS pass through without Treer credentials. CONNECT to a
`SERVICE_ID.treer` or `NAME.treer` address tunnels its registered endpoint
(including SSH/native TLS), but cannot inject headers inside TLS or replace
native authentication. Use service forwarding when a client cannot use HTTP
CONNECT. Entry ports are
rebound on hot restart and recreated on cold restore; older running processes
must be relaunched to receive the proxy environment.

For a client that does not use this entry, request a short-lived Bearer token
using the service ID or unique name:

```bash
TOKEN="$(treer token create api)"
curl -H "Authorization: Bearer $TOKEN" http://api.internal/
```

The token audience is the stable service ID even when the command uses a name.
Use `treer token create api --json` only when the service ID or expiry metadata
is needed. Do not print, log, persist, or send the injected
`TREER_WORKLOAD_CREDENTIAL`; only the local Controller consumes it. Tokens
expire after 60 seconds, so request one immediately before manual use. Services
that do not implement Treer identity continue to work through the proxy.

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

Send a new Message using a stable recipient ID, tree path, or sibling name. Prefer stdin for
multiline or generated content so the body is not exposed in process arguments:

```bash
printf '%s\n' 'Review is complete.' | \
  treer message send --to /coordinator --idempotency-key task-42-result
```

Bridges sending an external-channel Message can keep text and attribution out of argv:
pipe a single JSON object `{"body":"text","external_source":{"channel":"telegram","conversation_id":"chat-id","message_id":"external-id","metadata":{"user_id":"user-id"}}}`
to `treer message send --to /coordinator --stdin-envelope --idempotency-key telegram-update-42`.
Generate and pipe this JSON in the bridge process; do not put private text in a
shell command or an argument to `printf`. The envelope is limited to 1 MiB;
`external_source` is optional (or null), and ordinary Message limits still apply.
`--stdin-envelope` conflicts with `--body`, `--body-file` and
`--external-source-file`. Older Controllers reject this unknown flag; upgrade
instead of falling back to putting text in argv. Attribution does not replace
the authenticated sender or grant permissions.

Use a sender-scoped idempotency key whenever a send may be retried. Repeating an
identical request returns the original Message; reusing the key with different
content fails. Reply by stable Message ID to preserve conversational context:

```bash
printf '%s\n' 'I addressed both findings.' | \
  treer message reply <message-id> --to sender
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

Use `treer machine show <machine>` to inspect its enrolled root before file
transfer. `treer machine put --machine <machine> LOCAL REMOTE` uploads one file;
`treer machine get --machine <machine> REMOTE LOCAL` downloads one file.
REMOTE is root-relative, `/` separated, and its parent must already exist.
LOCAL refers to the CLI machine; `-` reads stdin for put or writes verified
stdout for get. `--force` permits replacement, otherwise existing destinations
are preserved. Both directions verify SHA-256 and default to a 64 MiB limit and
private permissions; optional Unix `--mode` excludes group/other write and
special bits (Windows only supports 600). Symlinks/reparse points, special
files, traversal, Core private state and reserved temporary names are refused.
Use physical local paths rather than macOS `/tmp` or junction aliases.
`file_transfer_unsupported` means upgrade the relevant CLI/Proxy/Controller;
never substitute an unchecked upload. A lost completion reply is ambiguous:
check the destination before retrying, and never automatically add `--force`.
Policy actions are `machine.file.write` and `machine.file.read`; filesystem
paths are not workspace Tree path selectors. These operations are not a sandbox.

If a machine shows Offline while Agents still exist, do not re-enroll it.
Recover on that host with the real workspace ID (`ws_…` from `treer --format json status` or
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

## Run a bounded machine command

Use `treer exec` for a short, non-interactive command when creating a persistent
Agent would add no value. Select the machine explicitly and keep the working
directory relative to that machine's Host root:

```bash
treer exec --machine build-machine --cwd packages/api -- git status --short
treer exec --machine build-machine --cwd . --timeout 10000 -- sh -lc 'make test'
```

The command is an argv vector; Treer does not interpret shell syntax. Use an
explicit shell as in the second example when pipes, redirects, or expansion are
required. The timeout is milliseconds, defaults to 30000, and cannot exceed
30000. The JSON response contains stdout, stderr, exit code, duration, timeout,
and truncation state. Each output stream retains at most 64 KiB.

This executes with the selected machine account's authority. It is subject to
the `machine.exec` Policy action, but it is not an Agent sandbox and receives no
Agent workload credential. Inspect the machine first and do not run destructive
commands unless the user explicitly requested them.


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
`--machine` accepts a server ID or unique machine name, as with `agent admin
create`, `app create`, and `exec`.

`agent admin create` and `agent admin profile launch` accept `--folder PATH`
to choose the new Agent's workspace Tree folder. Without it, a managed Agent
creates peers in its own Tree folder; an operator defaults to workspace root.
Relative Tree paths (including `.` and `..`) start at the caller's folder;
absolute paths start at `/`, and folder IDs are also accepted. Use `--folder /`
to create at root explicitly. This is independent of the process directory
selected by `--cwd` or the profile's saved `cwd`.

```bash
treer agent admin profile launch reviewer --name review-42 --folder ../reviews
treer agent admin create --kind command --name helper --folder /research --cwd repo -- /bin/sh
```

The destination must exist and is checked before creating the Agent. Proxy
reserves the name and placement atomically before starting the process;
non-root placement requires `agent.tree.manage`. Sibling name conflicts are
rejected before dispatch, including when two callers create concurrently.

Select an online machine from `treer --format json status`, then create an explicit command
Agent. Preserve the current working directory unless the task requires a
different relative directory.

```bash
treer agent admin create --machine <server-id> --kind command --name reviewer --cwd . -- codex
```

```bash
treer agent admin create --machine <server-id> --kind command --name codex-ui --cwd . -- /path/to/codex-agent-ui/scripts/treer-agent.sh
```

To install a public git recipe, select and launch an explicit installer command
or saved launch profile. Read the bundled install skill and submit it with the
recipe URL to that Agent. Automatic provider selection and `--recipe` creation
are retired.

```bash
treer --skill install
treer agent admin create --machine <server-id> --kind command --name installer -- codex
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
treer agent admin create --machine <server-id> --kind command --name reviewer --cwd . -- codex <agent-args...>
```

Submit work by path and wait for that submission to finish:

```bash
treer agent prompt /reviewer \
  "Review the current diff and report only actionable findings." \
  --wait --timeout 120000
```

With explicit `--notify`, Agent senders receive a Core Message on `done`,
`failed`, or `interrupted`, including submission/turn IDs and a bounded assistant
preview. Without it, queued and steering prompts do not request completion
messages; `--wait` alone polls its receipt without enabling messages.
Notifications use the target launcher's identity and normal Core Message
Policy. Queue state and notification retries
survive launcher restarts. Inspect or cancel pending work with:

```bash
treer agent queue ls /reviewer
treer agent queue cancel /reviewer <submission-id>
treer agent prompt /reviewer "Urgent correction" --steer
```

`prompt --wait` follows the returned submission ID until `done`, `failed`, or
`interrupted`, even when other turns run first. `--timeout` is milliseconds;
timing out leaves the submission queued/running. It requires receipt support
and rejects `--until`. Use the separate lifecycle wait for status matching:

```bash
treer agent wait /reviewer --until blocked --timeout 120000
```

`agent wait` observes lifecycle state/output revisions. It does not prove that
a particular submission finished; `prompt --wait` does provide that correlation.

Inspect the result after a wait, especially after `blocked` or `failed`:

```bash
treer agent show /reviewer
treer agent read /reviewer --lines 120
```

`treer agent attach <target>` is reserved for a human using an interactive
terminal on an Agent Server machine. Agents must use `prompt`, `read`, and
`send-keys` instead of opening an attached terminal session. A human can press
`Ctrl-]` to detach without stopping the target.

Delete an agent only when its process and workspace entry should both be
removed. Deletion is persistent and is different from merely stopping it:

```bash
treer agent admin delete /reviewer
```

## Send terminal keys intentionally

Use `prompt` for normal messages. Use `send-keys` only for interactive terminal
controls, approvals, cancellation, or applications that need individual keys:

```bash
treer agent send-keys /reviewer esc
treer agent send-keys /reviewer ctrl-c
treer agent send-keys /reviewer y enter
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
- Terminal-only Agents support explicit `prompt --steer`, not queued receipts or `prompt --wait`.
- Use `treer agent admin stop <target>` only when terminating that process is intended.
- Use `treer agent admin delete <target>` only when permanent removal is intended.
- Use `treer machine delete <server-id>` only when the user explicitly asks to
  remove that machine; it also removes every agent registered on it.
