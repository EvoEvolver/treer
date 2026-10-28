---
name: treer
description: Coordinate distributed coding agents and run commands on workspace machines through the Treer CLI. Use when the user explicitly mentions Treer or asks to discover, create, inspect, prompt, wait for, send keys to, read, stop, or remotely access agents and machines registered in a Treer workspace. Requires a Treer-managed agent environment for self-relative operations.
---

# Treer

Treer connects agent servers on many machines into one workspace. Use the
`treer` CLI to discover peers and coordinate work without knowing machine
addresses.

## Verify caller context

Before controlling peers, verify that this process is a Treer-managed agent:

```bash
test -n "${TREER_AGENT_ID:-}" && test -n "${TREER_WORKLOAD_CREDENTIAL:-}" && test -n "${TREER_AGENT_SERVER_URL:-}"
treer whoami
```

`treer whoami` returns the workspace plus the caller's complete `agent` and
`machine` records; treat those as the caller identity instead of inferring it
from names. `treer --format json status` returns all machines and agents plus
the same two records under `self`.

If the check fails, do not guess a Proxy or local server address: explain that
the process is not running inside a managed Treer agent. Do not print, log,
persist or send `TREER_WORKLOAD_CREDENTIAL`; only the local Controller consumes
it.

The Controller supplies the command tree, help and skills; the `treer` binary
is a thin client that forwards argv and streams. Business help therefore needs
the Controller online:

```bash
treer --help
treer agent --help
treer agent admin --help
treer agent admin profile --help
treer app --help
treer machine --help
treer exec --help
treer service --help
treer interface --help
treer message --help
treer token --help
treer member --help
```

Control commands print JSON; read IDs and state from the response instead of
predicting them. `status`, `agent ls` and `machine list` render as tables only
when stdout is a terminal. Put `--format json` (or `--format table`) before the
command to choose explicitly; this client-side flag is not forwarded.
Independently configured Agent clients can set `TREER_IDENTITY_MODE=agent` so
missing identity fails instead of falling back to operator credentials.

## Identity, targets and the Tree

Treer injects `TREER_WORKSPACE_ID`, `TREER_SERVER_ID` and `TREER_AGENT_ID` into
every managed agent. All discovery and control stays inside that workspace.
Agent targets may be:

- an opaque agent ID;
- an absolute tree path (`/team/reviewer`) or a relative path (`../team/reviewer`);
- a bare name, only for a sibling in the caller's containing folder;
- `self` or `.` for the calling agent.

Prefer paths. Names are unique within a folder, not the workspace. Relative
paths start at your containing folder; operators start at `/`. A missing bare
sibling returns `agent_not_in_folder` with up to ten same-name absolute paths in
`hint`; it never falls back to a global name match. Agent output includes
`path`, and the Console's **Copy path** value can be pasted as a target.

Discover topology before choosing a peer or machine:

```bash
treer whoami
treer --format json status
treer --format json agent ls -R /
treer agent show self
treer agent show /reviewer
treer member list
```

Organize Agents and Apps in the workspace Tree:

```bash
treer agent mkdir -p /research/experiments
treer agent ls /research
treer agent mv /reviewer /research/experiments/
treer agent mv /research/experiments /research/runs
treer agent rmdir /research/runs
treer agent admin rename self coordinator
treer machine rename self build-machine
```

Paths are case-sensitive. `ls` without a path lists the current folder and
prints `path`, `folders` and `agents`; `-R` includes descendants. `mkdir -p`
creates parents and accepts existing folders. `mv` moves an Agent/App into an
existing folder, or moves/renames a folder without overwriting another. Rename
an Agent/App with `agent admin rename TARGET NAME`; IDs, credentials and server
IDs do not change. `rmdir` rejects nonempty folders and never stops workloads.
Folder IDs from `ls` are accepted where names are ambiguous. These use the
Agent's own credential under the `agent.tree.read` / `agent.tree.manage` Policy
actions.

Creation commands (`agent admin create`, `agent admin profile launch`,
`app create`) place the new node in the caller's own folder unless `--folder
PATH` is given; operators default to root, and `--folder /` selects root
explicitly. `--folder` is independent of `--cwd`. The destination must exist;
the name and placement are reserved atomically, and sibling name conflicts are
rejected. Non-root placement requires `agent.tree.manage`. An unplaced caller
gets `caller_unplaced`: ask the owner to place it or pass an authorized
`--folder`.

Folders and path Deny rules are not a sandbox. Monitor-mode Policy never
enforces Deny. Use reviewed Policy plus external OS isolation when containment
matters.

Managed Apps may expose an App-owned logical subtree. Message targets can
append a relative path such as `/infra/agent_hub/team/tools/agent1`. These
nodes are App addresses, not Host processes; lifecycle and transcript commands
do not apply. See [App-owned trees](../../docs/app-tree.md).

## Create and coordinate a peer

Select an online machine from `treer --format json status`, then create an
explicit command Agent with `--kind command`; provider-specific and auto kinds
are retired (`agent_kind_retired`). Native arguments go after `--`, and `--cwd` is relative to the selected machine's Host root:

```bash
treer agent admin create --machine <server-id> --kind command --name reviewer --cwd . -- codex
treer agent admin create --kind command --name helper --folder /research --cwd repo -- /bin/sh
```

`--machine` accepts a server ID or unique machine name (as for `app create`,
`exec` and `profile launch`).

Use a launch profile when the same executable, directory and arguments are
reused. Profiles are workspace-scoped and addressed by stable ID or unique name:

```bash
treer agent admin profile create reviewer --description "Review current changes" --cwd . \
  codex -- review --base main
treer agent admin profile list
treer agent admin profile show reviewer
treer agent admin profile launch reviewer --machine <server-id> \
  --name review-42 --cwd packages/api --folder ../reviews
treer agent admin profile update reviewer --cwd packages/api
treer agent admin profile update reviewer --arg review --arg=--base --arg main
treer agent admin profile update reviewer --command codex --clear-args
treer agent admin profile delete reviewer
```

Repeating `--arg` replaces the whole argument array; `--clear-args` empties
it. `launch --cwd` overrides the directory for one Agent without changing the
profile. Command and arguments are an argv vector; shell syntax is only
interpreted when the executable is an explicit shell. Profile fields are
plaintext visible to members and authorized Agents: never put secrets in them.
Policy actions are `launch_profile.list|read|create|update|delete|use`, and a
launch also needs `agent.create` on the machine. Inspect a profile before
launching it.

Submit work and wait for that submission to finish:

```bash
treer agent prompt /reviewer \
  "Review the current diff and report only actionable findings." \
  --wait --timeout 120000
```

`prompt` defaults to queue delivery: it runs immediately when idle and queues
behind a busy turn. This needs the target Interface's `prompt.queue`
capability; a terminal-only Agent returns `queue_unsupported` and accepts only
`--steer`, which inserts into the current turn (or the PTY). Interface failures
are never retried through PTY input. `--wait` follows the returned submission
until `done`, `failed` or `interrupted`, even if other turns run first;
`--timeout` (milliseconds, requires `--wait`) leaves the submission in place on
expiry. `--until` is rejected; use `agent wait` for status matching.

`--notify` sends the calling Agent a Core Message when the submission ends,
with submission/turn IDs and a bounded assistant preview. Queue state and
notification retries survive launcher restarts.

```bash
treer agent queue ls /reviewer
treer agent queue cancel /reviewer <submission-id>
treer agent prompt /reviewer "Urgent correction" --steer
treer agent wait /reviewer --until blocked --timeout 120000
treer agent show /reviewer
treer agent read /reviewer --lines 120
treer agent transcript /reviewer --page 0
```

`agent wait` observes lifecycle state; it does not prove that a particular
submission finished. `agent read` returns terminal replay (default 100 lines).
`agent transcript` needs `transcript.read`: each page is one conversation turn
(a user prompt plus following entries), `--cursor` aliases `--page`, `--limit`
returns more turns (default 1), and the JSON includes `page`, `page_count` and
`next_page`.

`treer agent attach <target>` is only for a human at an interactive terminal
(`Ctrl-]` detaches). Automated agents use `prompt`, `read` and `send-keys`.

Delete an Agent only when its process and workspace entry should both go:

```bash
treer agent admin delete /reviewer
```

## Send terminal keys intentionally

Use `prompt` for normal messages. Use `send-keys` only for terminal controls,
approvals, cancellation or applications needing individual keys:

```bash
treer agent send-keys /reviewer esc
treer agent send-keys /reviewer ctrl-c
treer agent send-keys /reviewer y enter
```

Logical keys: `enter`, `tab`, `backspace`, `esc`, `space`, arrow keys, `home`,
`end`, `pageup`, `pagedown`, `delete`, `shift-tab`, `ctrl-a` through `ctrl-z`,
or a single Unicode character. All keys are validated before any bytes are sent.

## Lifecycle and startup commands

```bash
treer agent start /reviewer
treer agent stop /reviewer
treer agent restart /reviewer
```

These accept Agents and managed Apps. Restart keeps the ID and workload
credential; `run_generation` increases for the replacement process. Stop
prevents automatic recovery until an explicit start or restart. Enabled Apps
retry exited processes; ordinary Agents do not restart automatically.

Register this Agent's own command so it returns after a full Host restart:

```bash
treer agent startup set --cwd . -- sh scripts/start-agent.sh
treer agent startup show
treer agent startup clear
```

This is self-only; `treer agent stop` disables it. To repair another Agent
without deleting its identity, use the Policy-controlled admin form (requires
`agent.startup.manage` on the target):

```bash
treer agent admin startup show /reviewer
treer agent admin startup set /reviewer --cwd repo -- sh scripts/start-agent.sh
# Atomically save new argv and restart once:
treer agent restart /reviewer --cwd repo -- codex resume THREAD_ID
```

Omitted `--cwd` keeps the saved directory. Admin `set` alone does not launch.
Keep resume/session arguments when switching launchers to retain context. A
failed launch leaves the new command saved for repair. App runtimes reject
startup edits with `app_startup_managed`; use `treer app update`.

Launch commands need an absolute executable or a bare name found in an absolute
PATH directory. Relative executables (`./start.sh`) are rejected, missing ones
fail with `command_not_found`, and no shell, cwd search or `$HOME` expansion is
implied. Use an explicit interpreter (`sh start.sh`). On Windows bare names
resolve only `.exe`/`.com`. `cwd` must exist beneath the machine root. Never
put credentials in argv.

## Manage long-running Apps

A Managed App is one supervised command with one HTTP UI endpoint. Its service
and ingress address stay stable across runtime exits and Controller reconnects:

```bash
treer app create --machine build-machine --name docs --cwd . --port 8080 \
  python3 -- -m http.server 8080
# Explicitly allow anonymous internet access:
treer app create --public --machine build-machine --name public-docs --cwd . \
  --port 8081 python3 -- -m http.server 8081
treer app list
treer app show /docs
treer app stop /docs
treer app start /docs
treer app restart /docs
treer app update /docs --cwd site --command python3 -- -m http.server 8080
treer app delete /docs
```

Read `public_url` and `access` (`workspace` or `public`) from create/list/show.
`public_url` is the App's external root; the default ingress requires workspace
authentication. Use `--public` only for intended anonymous access; it fails,
and private Apps have no `public_url`, when the Proxy has no wildcard ingress.
`--port` tells Treer where to route; the App must listen there itself.

`app update` keeps the App's ID, credential, service and public origin.
Omitted fields stay unchanged (args too, when only `--command` changes);
arguments after `--` replace the array and `--clear-args` empties it. A running
App restarts once; a stopped App stays stopped. It needs `agent.startup.manage`
plus restart permission. Do not delete/recreate an App to change its command.

Managed Apps do not install dependencies, store secrets, migrate state or
isolate hostile code. Keep secrets in private configuration, never argv.

## Services and forwarding

Register a process on your own loopback, then forward its stable service ID
through an authenticated local listener:

```bash
treer service register --name database --port 5432
treer service list
treer service forward <service-id> --listen 127.0.0.1:15432
treer service publish <service-id>
treer service unpublish <service-id>
treer service delete <service-id>
```

Forwarding is private by default. `service publish` allocates an HTTP address
with workspace authentication; `--access public` removes that authentication.
Raw TCP publication uses `--tcp --access public` and needs a configured
platform port range; `unpublish --tcp` revokes it. Transparent network
proxying is retired (`treer network connect` only reports that); use services.

### Primary HTTP endpoint between Agents

An Agent can expose its own HTTP API. From the serving Agent, after starting
the server:

```bash
treer service register --primary --protocol http --name reviewer-api --port 4180
```

Each Agent has at most one primary HTTP service; repeating the command updates
name and port and keeps the service ID. Put server start and registration in
the saved startup command so recovery re-registers the current port. Callers
discover the address and use an ordinary HTTP client:

```bash
addr=$(treer agent address /reviewer)
curl --fail-with-body "$addr/openapi.json"
treer agent address /reviewer --json
```

`address` defaults to `self` and prints `http://SERVICE_ID.treer/`; `--json`
adds `agent_id` and `service_id`. A missing primary is an error. Treer does not
interpret the schema, queue offline deliveries or retry.

### Identity-aware HTTP

Managed Agents and Apps inherit an HTTP proxy entry (`http_proxy`,
`https_proxy`, and `TREER_HTTP_PROXY` for clients that ignore those). Address a
service by name or stable ID:

```bash
curl http://example-http.treer/path
curl http://svc_YOUR_SERVICE_ID.treer/path
```

Names are workspace-unique, case-insensitive, resolved per request, and limited
to 1-63 ASCII letters/digits with internal hyphens. The proxy authenticates as
this Agent and adds a fresh service-audience token in `X-Treer-Identity-Token`
(and as Bearer `Authorization` if none was set). Localhost bypasses the proxy;
external HTTP/HTTPS passes through without Treer credentials. CONNECT to a
`*.treer` address tunnels to the registered endpoint (SSH, native TLS) but
cannot inject headers inside TLS. Processes started before the proxy entry
existed must be relaunched.

For a client that cannot use the proxy, request a 60-second Bearer token:

```bash
TOKEN="$(treer token create api)"
curl -H "Authorization: Bearer $TOKEN" http://api.internal/
```

The audience is the stable service ID even when a name is used; `--json` also
returns the ID and expiry.

## Agent Interface Server

An Agent-native integration may register a `treer.agent-interface/v1` HTTP
server on its private loopback. Registration is self-only; the Controller
verifies the manifest before publishing capabilities:

```bash
treer interface register --port 4180 --instance-id pi-session-1 \
  --capability prompt.submit --capability transcript.read \
  --capability state.observe --ui-path /
treer interface show
treer interface clear
treer interface contract
```

With `--ui-path`, the Console replaces the terminal view with that page and
carries HTTP and WebSocket traffic to the same private port; the page must use
relative URLs. No service record or published port is needed, and `interface
register` alone creates no service address. The Interface must deduplicate
prompts by `operation_id`, refresh registration after Controller restarts, and
clear it on clean shutdown. `interface contract` reports the running
Controller's launcher compatibility contract.

The single launcher at `launchers/agent` implements this protocol, serves the
shared browser UI and drives Codex app-server or Claude stream-json
(`--harness codex|claude`). Each Agent owns one thread/session. New harnesses
are adapters inside that runtime, not new UIs or AIS Apps. The launcher is
installed with the local Controller binary, not the `treer` CLI:

```bash
treer-agent-server launcher install    # or: update | status | rollback
treer agent admin profile list
```

`launcher install` needs a trusted Node.js 22+ (`--node /abs/path` if not in a
standard location), saves profiles, and never creates or restarts Agents or
the Host. With several local enrollments pass `--workspace WORKSPACE_ID` (and
optionally `--url http://127.0.0.1:PORT`).

## Durable Messages

Core Messages are for asynchronous collaboration that must survive Agent,
Controller or Proxy restarts. A Message is immutable and may reference earlier
visible Messages through ordered `context_ids`.

```bash
treer message receive --wait 30000 --limit 50
treer message ack <delivery-id> --operation-id <stable-operation-id>
printf '%s\n' 'Review is complete.' | \
  treer message send --to /coordinator --idempotency-key task-42-result
printf '%s\n' 'I addressed both findings.' | \
  treer message reply <message-id> --to sender
treer message get <message-id>
treer message list --limit 50
```

`receive` does not change state: the same delivery returns until acknowledged,
so durably record any external effect before `ack`. Send bodies over stdin
rather than `--body` to keep them out of process arguments. Use a
sender-scoped `--idempotency-key` whenever a send may be retried; reusing a key
with different content fails. `reply` uses the parent's sender when `--to` is
omitted or `sender`, and puts the parent first in context.

Bridges forwarding an external channel can pipe one JSON object
`{"body":"…","external_source":{"channel":"…","conversation_id":"…","message_id":"…"}}`
(max 1 MiB) to `treer message send --to TARGET --stdin-envelope
--idempotency-key …`; it conflicts with `--body`. Attribution never replaces
the authenticated sender. Only an authenticated Managed App may send with
`--from-endpoint` for its own logical subtree.

Context edges do not grant access to a parent's body; missing and invisible
Messages return the same error. Policy actions are `message.send`,
`message.read`, `message.receive` and `message.ack`. `core_messages_disabled`
means the deployment has not enabled Messages; report it.

A Message does not wake another Agent. When attention is needed, send the
Message, then `treer agent prompt TARGET "<message-id>"` with only the ID;
`agent.prompt` is a separate, stronger Policy action.

## Machines and bounded commands

```bash
treer machine list
treer machine show <machine>
treer machine put --machine <machine> LOCAL REMOTE
treer machine get --machine <machine> REMOTE LOCAL
treer exec --machine build-machine --cwd packages/api -- git status --short
treer exec --machine build-machine --cwd . --timeout 10000 -- sh -lc 'make test'
```

File transfer: REMOTE is root-relative with `/` separators and an existing
parent; LOCAL is on the CLI machine, `-` meaning stdin/stdout. Existing
destinations are kept unless `--force`. Transfers verify SHA-256, default to a
64 MiB limit and mode 600 (`--mode` excludes group/other write and special
bits). Symlinks, special files, traversal and Core private state are refused.
A lost completion reply is ambiguous: check the destination before retrying,
and never add `--force` automatically. Policy actions are `machine.file.write`
and `machine.file.read`.

`exec` runs a short non-interactive argv (no shell unless you invoke one) with
the machine account's authority under `machine.exec` Policy. Timeout is
milliseconds, default and maximum 30000; each output stream keeps at most
64 KiB. It is not a sandbox and gets no workload credential; do not run
destructive commands unless asked.

If a machine shows Offline while its Agents still exist, do not re-enroll it.
On that host, with the real workspace ID:

```bash
treer-agent-server service --workspace "$TREER_WORKSPACE_ID" status
treer-agent-server service --workspace "$TREER_WORKSPACE_ID" restart-controller
treer-agent-server service --workspace "$TREER_WORKSPACE_ID" start
```

Ready means the Controller reports `proxy_connected`, not just a live loopback
API. `treer machine delete <server-id>` revokes the machine and removes every
Agent on it; it does not uninstall the service.

## Operational boundaries

- Do not stop, restart or delete agents you did not create unless asked.
- Create peers only on machines from the current workspace.
- Use `self` instead of copying the injected ID.
- Read agent output before responding to an unexpected state.
- Never use `agent attach` from an automated workflow.
- Use `agent admin delete` and `machine delete` only for intended permanent removal.
