# Treer Prototype Plan

## 1. Goal

Build an independent distributed coding-agent runtime with these properties:

1. A central proxy server maintains logical workspaces and routes commands.
2. Every participating machine runs a stable Host that owns local processes,
   PTYs, output buffers, and lifecycle state, plus a replaceable Controller that
   speaks the Proxy protocol.
3. Agent servers and agents can discover the other servers and agents in the
   same Treer workspace, but cannot see resources in another workspace.
4. A user or agent can create, prompt, inspect, wait for, and stop an agent on
   another machine through the proxy.
5. A simple web UI can list workspaces, machines, and agents and invoke the same
   operations.

Treer does not depend on another terminal manager or agent runtime. The first
prototype should prove routing, discovery, local process ownership, lifecycle
synchronization, and remote agent creation.

## 2. Non-goals for the Prototype

- Authentication, authorization, TLS, certificates, or untrusted multi-tenancy.
- Direct peer-to-peer connections between machines.
- Migrating a running PTY or agent process between machines.
- A shared cross-machine filesystem.
- Durable exactly-once delivery.
- Surviving a Host process or machine restart without restarting child agents.
- Automatic load balancing beyond explicit or first-online server selection.
- Container, VM, or filesystem sandboxing.
- Migrating an attached terminal session between proxy instances.

## 3. Architecture

```text
Browser / treer CLI / coding agent
                |
         HTTP + WebSocket
                |
        +------------------+
        |   treer-proxy    |
        | workspace index  |
        | command router   |
        | state projection |
        +------------------+
           /       |       \
          WS       WS       WS       Controllers initiate connections
         /         |         \
 +------------+ +------------+ +------------+
 | Controller | | Controller | | Controller |
 +------------+ +------------+ +------------+
       | Unix socket   |              |
 +------------+ +------------+ +------------+
 |    Host    | |    Host    | |    Host    |
 +------------+ +------------+ +------------+
       |             |              |
  PTY/processes  PTY/processes  PTY/processes
```

Runtime ownership stays on the machine that launches the agent. The proxy stores
workspace membership and a projection of server/agent state; it does not own
PTYs, parse terminal output, or directly manage child processes.

## 4. Components

### 4.1 `treer-proxy`

Responsibilities:

- Create and list logical Treer workspaces.
- Accept persistent outbound WebSocket connections from agent servers.
- Track online agent servers and their workspace memberships.
- Maintain the latest server and agent projection for each workspace.
- Route commands to a selected agent server and correlate responses.
- Broadcast workspace-scoped changes to browser and CLI clients.
- Serve the prototype REST API, WebSocket event stream, and static web UI.

Prototype storage is in memory. Agent servers send a full snapshot after every
connect, so restarting the proxy reconstructs live state as machines reconnect.

### 4.2 `treer-agent-host`

The Host is the long-lived process boundary on each machine. It should:

- Own PTY masters, child handles, input queues, terminal geometry, and bounded
  revisioned output buffers.
- Expose only `sync`, `spawn`, `read`, `write`, `resize`, and `stop` over a
  versioned Unix-socket protocol.
- Treat command, arguments, environment, cwd, and metadata as opaque process
  data; it does not understand Codex, Claude, prompts, workspaces, or Proxy
  messages.
- Cache mutation results by `operation_id`, including failures, so retries do
  not execute twice.
- Supervise the Controller and replace only that child on a hot restart.

The Host is authoritative for local process and terminal state. If the Host
exits, its child agents are terminated.

### 4.3 `treer-agent-server`

`treer-agent-server` is the hot-updatable Controller. It should:

- Load or generate a stable `server_id`.
- Connect outbound to the proxy and reconnect with bounded backoff.
- Join one configured Treer workspace in v0.
- Translate agent kinds, prompts, and Proxy commands into Host operations.
- Detect agent state from replayed and live terminal output.
- Rebuild its complete state from Host `sync` after every restart.
- Publish full snapshots and incremental lifecycle events to the proxy.
- Expose a localhost API used by agents and the local `treer` CLI.

Controller replacement may briefly disconnect Proxy and browser terminal
streams, but it does not interrupt the underlying agents or PTYs.

### 4.4 `treer-agent-runtime`

This library provides the low-level runtime used only by the Host:

- PTY creation, process spawning, input writing, resize, and termination.
- Bounded raw terminal chunks with stream epochs and monotonic revisions.
- Output-activity and process-exit observation.
- Stable local handles independent from OS process IDs.

Agent definitions and idle/working/blocked detection live in the Controller so
they can change without updating the Host.

### 4.5 `treer-cli`

The CLI is both a human tool and the first agent-facing interface:

```text
treer workspace list
treer server list
treer agent list
treer agent create --server <server-id> --kind codex --name reviewer
treer agent prompt <agent-id> "Review the current change"
treer agent read <agent-id> --lines 100
treer agent wait <agent-id> --until idle,done,blocked
treer agent stop <agent-id>
```

When invoked inside a Treer-created agent, the CLI automatically uses the local
agent server through these injected values:

```text
TREER_WORKSPACE_ID
TREER_SERVER_ID
TREER_AGENT_ID
TREER_AGENT_SERVER_URL
```

The local server forwards remote operations over its existing proxy connection.
An agent therefore does not need the proxy URL or another machine's address.

### 4.6 `treer-web`

The web UI combines the operational dashboard with a streamed browser terminal:

- Workspace selector.
- Online/offline server list.
- Agent table with server, kind, name, status, and last update.
- Create-agent form with target server, kind, name, cwd, and arguments.
- A full-width xterm.js terminal with raw PTY replay and live output.
- Per-keystroke input, paste, terminal resize, reconnect, and stop controls.

Terminal sessions are multiplexed through the existing agent-server WebSocket.
The proxy routes opaque PTY bytes and does not parse terminal escape sequences.

## 5. Workspace Model and Isolation

A Treer workspace is a distributed collaboration namespace. It contains:

- Zero or more registered agent servers.
- A separate local root directory on each registered server.
- All agents launched by those servers for that workspace.
- A workspace-scoped event revision and discovery snapshot.

Resources use globally unambiguous IDs:

```text
workspace_id: ws_<uuid>
server_id:    srv_<uuid>
agent_id:     ag_<uuid>
command_id:   cmd_<uuid>
```

Every proxy query and command is resolved inside one workspace:

```text
(workspace_id, server_id, agent_id)
```

The proxy rejects a command when the target server or agent is not a member of
that workspace. This is logical namespace isolation only. Agent processes are
not sandboxed from the rest of their host machine in the prototype.

Each server registration includes its local root for the workspace. A remote
create request uses a path relative to that root. The agent server canonicalizes
the path locally and rejects paths outside the root. Absolute paths are never
treated as portable between machines.

## 6. Local Agent Runtime

### 6.1 Agent record

The authoritative runtime record contains:

```text
agent_id
workspace_id
server_id
kind
name
relative_cwd
command + args
status
pid
started_at / updated_at / exited_at
exit_code
output_revision
```

OS PIDs are diagnostic fields, never distributed identities.

### 6.2 Lifecycle states

Use these internal states:

```text
starting -> working <-> idle
                    <-> blocked
         -> exited
         -> failed
         -> unknown
```

For v0:

- `starting`: process spawned but readiness has not been detected.
- `working`: a prompt was submitted or an agent-specific working indicator is
  visible.
- `idle`: a recognized live input prompt is visible.
- `blocked`: a recognized confirmation, permission, or question UI is visible.
- `exited`: process ended after successful launch.
- `failed`: spawn or startup failed.
- `unknown`: process is alive but no rule can classify the current screen.

`done` is a client presentation derived from an idle transition after work; it
is not a separate process state.

### 6.3 PTY and output

The Host starts every interactive agent in a PTY so it behaves like it does in
a terminal. Host and Controller together maintain:

- A bounded raw ANSI chunk ring with a stream epoch and monotonic revision.
- Replay from a supplied cursor, including explicit gap detection.
- Controller-owned recent plain text for status detection and API reads.
- One serialized input queue per agent.
- Initial terminal geometry followed by live browser-driven resize updates.

Use a proven PTY library and terminal parser. The prototype should not implement
terminal escape parsing from scratch.

### 6.4 Agent definitions

Agent launch behavior is configured independently from proxy routing:

```toml
[agents.codex]
command = "codex"

[agents.claude]
command = "claude"

[agents.command]
command = "sh"
```

Definitions may provide argument construction and detector manifests. The
Controller resolves them to opaque Host spawn requests.

## 7. Proxy-Agent Server Protocol

Use JSON messages over one persistent WebSocket per agent server. Every message
has a common envelope:

```json
{
  "type": "command",
  "protocol": 1,
  "workspace_id": "ws_example",
  "server_id": "srv_example",
  "request_id": "cmd_example",
  "payload": {}
}
```

Initial message types:

| Direction | Type | Purpose |
| --- | --- | --- |
| agent server -> proxy | `server.register` | Identity, workspace, root label, capabilities |
| agent server -> proxy | `server.snapshot` | Full local agent projection |
| agent server -> proxy | `server.heartbeat` | Liveness and snapshot revision |
| agent server -> proxy | `agent.event` | Created, status, output revision, or exit change |
| proxy -> agent server | `command` | Create, prompt, read, wait, resize, or stop |
| agent server -> proxy | `command.result` | Correlated success or structured error |

The proxy assigns one monotonically increasing projection revision per
workspace. Browser clients receive an initial workspace snapshot followed by
revisioned events. On a gap or reconnect, the browser requests a new snapshot.

The Proxy retains in-flight envelopes and resends them with the same
`command_id` after a Controller reconnect. The Host keeps a bounded cache of
recent mutation results keyed by that ID. Receiving the same command again
returns the cached success or failure instead of repeating the operation.

## 8. Treer API

Minimum proxy API:

```text
POST /api/workspaces
GET  /api/workspaces
GET  /api/workspaces/:workspaceId/snapshot
GET  /api/workspaces/:workspaceId/servers
GET  /api/workspaces/:workspaceId/agents
POST /api/workspaces/:workspaceId/agents
POST /api/workspaces/:workspaceId/agents/:agentId/prompt
GET  /api/workspaces/:workspaceId/agents/:agentId/output
POST /api/workspaces/:workspaceId/agents/:agentId/wait
POST /api/workspaces/:workspaceId/agents/:agentId/stop
GET  /api/workspaces/:workspaceId/events        WebSocket upgrade
```

Agent creation request:

```json
{
  "server_id": "srv_machine_a",
  "kind": "codex",
  "name": "reviewer",
  "cwd": "project-a",
  "args": []
}
```

If `server_id` is omitted, the prototype chooses the first online server in a
stable sorted order and returns the selected server in the response.

## 9. Remote Agent Creation

The proxy and target agent server execute `agent.create` as follows:

1. The proxy validates workspace membership and allocates `agent_id` and
   `command_id`.
2. The proxy routes the request to the selected online server.
3. The Controller resolves the agent definition and builds an opaque Host spawn
   request with the Treer context variables.
4. The Host resolves the relative cwd under its configured root.
5. The Host creates a PTY, spawns the process, and records the initial revision.
6. The Controller projects the Host record and publishes `agent.created`.
7. Startup detection moves the agent to `idle`, `blocked`, `unknown`, or
   `failed` and returns the current record.
8. Later terminal and process changes publish revisioned events.

If spawning fails, the allocated `agent_id` remains in a terminal `failed`
record so all clients observe the same outcome.

## 10. Agent Discovery and Interaction

Discovery returns only the current Treer workspace projection:

```json
{
  "workspace_id": "ws_example",
  "servers": [
    {"server_id": "srv_a", "status": "online", "labels": {"os": "macos"}}
  ],
  "agents": [
    {
      "agent_id": "ag_reviewer",
      "server_id": "srv_a",
      "kind": "codex",
      "name": "reviewer",
      "status": "idle"
    }
  ]
}
```

For v0, interaction is terminal-oriented control: create an agent, send a
prompt, read its output, wait for lifecycle state, and stop it. Typed inter-agent
tasks, mailboxes, artifact transfer, and turn correlation are a later protocol.
The prototype must not claim that an `idle` transition identifies completion of
one specific prompt.

## 11. Repository Layout

Use one Rust workspace so the proxy and machine daemon share protocol and model
types and can ship as standalone binaries:

```text
treer/
  Cargo.toml
  crates/
    treer-protocol/       shared wire/API models
    treer-host-protocol/  stable Controller-to-Host socket contract
    treer-proxy/          central HTTP/WebSocket server
    treer-agent-runtime/  low-level PTY, process, and output ownership
    treer-agent-host/     stable daemon and Controller supervisor
    treer-agent-server/   replaceable Controller and proxy connection
    treer-cli/            human and agent-facing CLI
  skills/treer/           bundled agent collaboration skill
  web/                    small browser dashboard
  tests/
    e2e/                  proxy + multiple agent servers
  README.md
  PLAN.md
```

Recommended initial libraries:

- Tokio for async runtime.
- Axum for proxy and local-agent-server HTTP/WebSocket endpoints.
- Serde/serde_json for protocol types.
- Reqwest for CLI HTTP calls.
- UUID for global resource IDs.
- `portable-pty` or an equivalent established PTY library.
- An established terminal parser for screen snapshots and ANSI handling.

Keep PTY and process implementation inside `treer-agent-runtime`. Keep agent
definitions and detection inside the Controller. The proxy and web UI should
depend only on Treer resource models.

## 12. Delivery Milestones

### Milestone 0: Contract and distributed test harness

- Create the Rust workspace and shared protocol types.
- Build fake agent-server connections with deterministic snapshots and events.
- Implement an in-memory proxy workspace projection.
- Freeze protocol version 1 fixtures.

Acceptance: one integration test starts a proxy and two fake servers in one
workspace and observes both in the workspace snapshot.

### Milestone 1: Registration and discovery

- Implement proxy workspace creation.
- Implement agent-server registration, heartbeat, reconnect, and snapshot.
- Implement workspace-scoped server and agent listing.
- Implement CLI discovery commands.

Acceptance: stopping and restarting the proxy reconstructs the workspace view
after both agent servers reconnect; another workspace sees none of those agents.

### Milestone 2: Independent local agent runtime

- Implement PTY ownership, spawn, input, resize, output ring buffers, and stop.
- Implement `command` agent kind for deterministic shell fixtures.
- Add data-driven definitions for Codex and Claude.
- Implement initial screen/status detection and lifecycle events.

Acceptance: the local agent-server API can create a fixture agent, submit input,
read incremental output, observe idle/working/exit transitions, and stop it.

### Milestone 3: Remote agent control

- Route create, prompt, read, wait, and stop through the proxy.
- Add explicit target-server and first-online selection.
- Add command result caching for retry idempotency.
- Propagate runtime events to the workspace projection.

Acceptance: a client connected only to the proxy creates one agent on machine B,
then prompts, waits for, reads, and stops it without knowing machine B's address.

### Milestone 4: Web dashboard

- Serve the workspace/server/agent dashboard.
- Subscribe to workspace events over WebSocket.
- Add create, prompt, read, wait, and stop controls.
- Display reconnecting/offline/error states without clearing the last snapshot.

Acceptance: two browser windows remain synchronized while agents on two machines
change state and while one agent server disconnects and reconnects.

### Milestone 5: Agent-to-agent usability

- Make `treer` CLI available inside Treer-created agent processes.
- Inject workspace/server/agent context variables.
- Add a short agent skill describing discovery and remote helper creation.
- Add an end-to-end scenario where an agent on A creates and prompts an agent on
  B, waits for it, and reads its response.

Acceptance: the initiating agent completes the workflow using only Treer commands
and never receives the proxy address or remote machine address.

### Milestone 6: Hot-updatable machine Controller

- Split stable PTY/process ownership into `treer-agent-host`.
- Add the versioned Unix-socket Host protocol and revisioned replay.
- Make `treer-agent-server` rebuild state from Host snapshots.
- Resend in-flight Proxy commands with stable IDs after Controller reconnect.
- Make the installer update binaries and restart only the Controller when the
  Host is already online.
- Reattach browser terminals and replace their screen from Host replay.

Acceptance: restart the Controller while a command and continuous terminal
output are in flight; the agent PID is unchanged, the command executes once,
and terminal output resumes from the Host buffer.

## 13. Prototype Test Matrix

Required automated scenarios:

- Two servers in one workspace discover each other.
- Servers in separate workspaces do not appear in each other's snapshots.
- Duplicate server registration replaces the stale connection deterministically.
- Proxy restart followed by server reconnect reconstructs current state.
- Agent-server disconnect marks its projection offline without deleting it.
- A routed command returns success, timeout, target-offline, and spawn error.
- Repeated `command_id` returns the cached result instead of creating two agents.
- A pending Proxy command is resent with the same ID after Controller reconnect.
- Controller restart preserves the agent PID and replays output without a gap.
- Runtime output buffers remain bounded under continuous output.
- Concurrent input to one agent is serialized in arrival order.
- Process exit produces one terminal lifecycle event with the exit code.
- Remote create selects the requested server and preserves global agent identity.
- Agent status events reach proxy subscribers in revision order.
- An agent on server A creates, prompts, waits for, and reads an agent on B.

## 14. First Implementation Slice

The first code change after this plan should implement only this vertical slice:

1. Rust workspace with `treer-protocol`, `treer-proxy`, and
   `treer-agent-server`.
2. In-memory workspace `default`.
3. WebSocket registration and heartbeat.
4. Fake agent snapshots from two agent-server processes.
5. `GET /api/workspaces/default/snapshot`.
6. One end-to-end test proving workspace isolation and discovery.

The first slice deliberately uses fake agent records. The second slice adds the
independent PTY/process runtime behind the already-tested distributed contract.
