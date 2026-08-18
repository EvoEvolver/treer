# Architecture

- Status: maintained
- Last source review: 2026-08-18 at `72921f1`

Treer separates the internet-facing control plane from machine-local process
ownership. Shared Rust protocol crates connect the layers; the React application
and CLI remain clients of those contracts.

## System map

```mermaid
flowchart TB
    Browser[Browser user] -->|HTTPS and WSS, session cookie| Proxy
    CLI[treer CLI] -->|loopback HTTP and WS| Controller
    Agent[Managed agent] -->|loopback HTTP and WS| Controller

    subgraph Central[Central control plane]
        Proxy["treer-proxy<br/>auth, metadata, routing"]
        DB[(PostgreSQL)]
        Web[Embedded React application]
        Proxy <--> DB
        Proxy --> Web
    end

    subgraph Machine[Each enrolled machine]
        Controller["treer-agent-server<br/>Controller"]
        Host["treer-agent-host<br/>stable process owner"]
        Runtime["treer-agent-runtime<br/>PTY and replay"]
        Processes[Codex / Claude / shell]
        Controller <-->|Unix socket, bincode| Host
        Host --> Runtime
        Runtime <--> Processes
    end

    Controller <-->|outbound authenticated WebSocket| Proxy
```

## Component ownership

| Component | Owns | Must not own |
| --- | --- | --- |
| [`treer-proxy`](../crates/treer-proxy/src/main.rs) | Public API, user auth, workload token signing, PostgreSQL metadata, workspace projection, command and stream routing | Local process lifetime |
| [`treer-agent-server`](../crates/treer-agent-server/src/main.rs) | Machine Controller, local API, Agent definitions, state detection, Proxy link, network bridge | Durable PTY ownership |
| [`treer-agent-host`](../crates/treer-agent-host/src/main.rs) | Stable child processes, Controller supervision, idempotent mutation cache | Users, workspaces, Agent brands, product policy |
| [`treer-agent-runtime`](../crates/treer-agent-runtime/src/lib.rs) | PTY lifecycle, raw input/output, bounded replay, root-relative working directories | Distributed routing or identity |
| [`treer-cli`](../crates/treer-cli/src/main.rs) | Human and managed-Agent commands, attach, remote shell, file copy | Private wire-model variants |
| [`treer-protocol`](../crates/treer-protocol/src/lib.rs) | Shared public and Controller protocol models and frames | Runtime implementation |
| [`treer-host-protocol`](../crates/treer-host-protocol/src/lib.rs) | Controller-to-Host request, response, and event contract | Proxy or browser concepts |
| [`treer-transfer`](../crates/treer-transfer/src/lib.rs) | Transfer manifests, validation, path containment, atomic upload commit | Session authorization |
| [`web`](../web/src/App.tsx) | Browser control-plane interaction and terminal UI | Backend policy or hidden business state |

## Architectural invariants

- The Host owns local process lifetime so a Controller update does not terminate
  active PTYs.
- The Host remains product-agnostic; Agent-specific interpretation belongs in
  the Controller.
- Shared wire models live in protocol crates. A client and server must not grow
  parallel copies of the same contract.
- Every distributed lookup is scoped by workspace before machine or Agent ID.
- Enrolled machines establish outbound connections to the Proxy.
- Remote working directories and file paths are resolved beneath the machine's
  configured workspace root. This path rule is not filesystem sandboxing.
- Durable identity metadata lives in PostgreSQL; live routing and streams are
  currently single-Proxy in-memory state.
- The web build is embedded into `treer-proxy`; frontend API changes and Proxy
  routes must be changed and verified together.
- `skills/treer/SKILL.md` is embedded into the CLI at build time and is the
  managed-Agent operations contract.

## Protocols and state

| Link | Transport and encoding | Authentication |
| --- | --- | --- |
| Browser to Proxy | HTTP/JSON and WebSocket frames | User or admin session cookie |
| Controller to Proxy | Persistent WebSocket, JSON and binary frames | Workspace-bound machine Bearer credential |
| CLI or managed Agent to Controller | Loopback HTTP/JSON and WebSocket | Local context; workload-token requests require the Agent credential |
| Controller to Host | Length-prefixed bincode on a local Unix socket | Local socket boundary |
| Host to child process | PTY raw bytes | Host process ownership |

PostgreSQL persists users, organizations, memberships, sessions, invitations,
workspaces, enrollment records, machine credentials, the workload signing key,
display names, machine services, and virtual hosts. Connected Controllers, pending commands, workspace
projections, terminal legs, transfers, and network tunnels are held in Proxy
memory and do not yet support horizontal routing across Proxy replicas.

## Primary information flow

```mermaid
sequenceDiagram
    participant B as Browser
    participant P as Proxy
    participant C as Controller
    participant H as Host
    participant A as Agent PTY

    B->>P: command with session cookie
    P->>P: authorize organization and workspace
    P->>C: command envelope over machine WebSocket
    C->>H: Host request with operation ID
    H->>A: spawn, input, resize, or stop
    A-->>H: raw PTY output
    H-->>C: revisioned output event
    C-->>P: state, result, and binary stream frames
    P-->>B: HTTP result, event, and terminal frames
```

Managed Agent coordination starts at the source machine's loopback API, crosses
the Proxy under that machine's credential, and reaches the target Controller
and Host. The source Agent does not need the destination address or a Proxy
credential.

Each managed Agent also receives a random workload credential in its process
environment and Host-owned metadata. For `treer identity token`, the Controller
matches that credential to the Agent, forwards the request under its machine
credential, and the Proxy resolves the requested machine service, evaluates the
`identity.token.issue` policy action, and signs a 60-second Ed25519 JWT bound to
the stable service ID. Services validate through the Proxy JWKS document or the
online verify endpoint. Tokens are requested explicitly and are never injected
into arbitrary virtual-network traffic.

## Network and transfer paths

On Linux, managed Agent TCP and DNS traffic enters a per-Agent network namespace
and TUN interface, then reaches a Controller-owned SOCKS5 boundary. Ordinary
destinations are authorized by the Proxy and then use source-machine egress;
only their route request and direct response cross the Controller WebSocket.
Workspace virtual hosts are relayed through the Proxy to another Controller,
including their TCP payload. Each alias resolves through a durable machine
service record before routing to the target Controller. Services belong to a
machine and outlive the Agent that registers or maintains them. The Controller
can probe the target from the machine host network; Treer does not yet start or
supervise the external service process. This is network containment and routing,
not a VM or private filesystem. A private mount namespace supplies the Agent's
resolver configuration and masks the host `nscd` socket, ensuring DNS lookups
reach the TUN virtual resolver instead of host NSS plugins or caches. These mounts do not
modify the host resolver files or cache service.

`treer scp` creates an authenticated Proxy transfer session between Controllers.
Remote operands are workspace-relative; the transfer engine rejects symlinks
and special files, enforces declared limits, and commits uploads by atomic
rename. Treer does not continuously synchronize project trees or resolve file
conflicts.

For route-level details and additional diagrams, use the dated
[project review](research/2026-08-18-project-review.md). Review this document
whenever a crate boundary, transport, persistence rule, or ownership invariant
changes.
