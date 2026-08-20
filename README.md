# treer

Treer is a distributed runtime and control plane for coding agents.

Each machine runs a stable Treer Host that owns local agent processes, PTYs, and
terminal history. A hot-updatable Controller connects that Host to the central
Proxy, which groups machines into logical workspaces and routes discovery and
control commands between them.

The Proxy is designed to be internet-facing. Browser users authenticate with
sessions, while machines enroll with short-lived one-time links and then use a
workspace-bound machine credential. See [PLAN.md](PLAN.md) for architecture,
protocol, and delivery milestones.

## Documentation

Start with [docs/README.md](docs/README.md) for the maintained product,
architecture, security, and quality map. Repository-development agents should
use [AGENTS.md](AGENTS.md) as the short navigation layer. Managed agents use the
separate [embedded Treer skill](skills/treer/SKILL.md) for runtime CLI
operations.

The dated [source-level project review](docs/research/2026-08-18-project-review.md)
contains the original technology survey, detailed information-flow diagrams,
and comparisons with Herdr and AgentENV.

## Run the prototype

Start the proxy and web control plane:

```bash
just test-db-up
export DATABASE_URL=postgres://treer:treer@127.0.0.1:55432/treer_test
just stage-artifacts
cargo run -p treer-proxy -- \
  --disable-auth \
  --listen 0.0.0.0:8787 \
  --public-url http://PROXY_HOST:8787 \
  --app-public-url http://127.0.0.1:5173

# In another terminal:
cd web
pnpm dev
```

`--disable-auth` is intended for local testing. It skips the login screen and
uses a synthetic local user. Omit it and set `ADMIN_PASSWORD` for shared or
deployed servers.

`--public-url` is the URL that other machines can reach. `stage-artifacts`
places the current platform's `treer-agent-host`, `treer-agent-server`, and
`treer` binaries under `dist/<platform>` for the proxy to serve. Release
deployments should stage all required Linux and macOS platform directories or
set `--artifacts-dir` to an equivalent artifact tree. When a platform artifact
is not present locally, the Proxy redirects to the latest Treer GitHub Release;
override its base with `TREER_RELEASE_ARTIFACT_BASE_URL` when mirroring release
assets elsewhere.

The `Build macOS ARM64 artifacts` GitHub workflow builds `darwin-aarch64`
binaries on a native Apple Silicon runner. Manual runs retain the files as a
workflow artifact for 14 days. Pushing a `v*` tag creates or updates that
GitHub Release with the three binaries and a `SHA256SUMS-darwin-aarch64.txt`
file. Ordinary branch pushes do not run the artifact build.

### Publish signed R2 releases

Production release artifacts live in the `treer-releases` Cloudflare R2 bucket
at `https://releases.treer.ai/`. The publisher keeps versioned objects immutable
under `releases/<version>/` and updates the separately signed `canary` and
`stable` pointers under `channels/`. R2 is only the distributor: every manifest
and channel pointer has a detached Ed25519 signature, and every artifact is
identified by its byte length and SHA-256 digest.

Generate the release key once on the trusted publishing machine:

```bash
just artifacts-keygen
```

The private key defaults to
`~/.config/treer/release-signing-key.pem` with owner-only permissions; its public
key is written next to it. Back up the private key outside the repository. Set
`TREER_RELEASE_SIGNING_KEY` and `TREER_RELEASE_PUBLIC_KEY` to use different
paths. Neither key path nor Cloudflare credentials is written into the release
manifest.

Before publishing, collect all three binaries for every supported platform in
the existing artifact tree:

```text
dist/<platform>/treer
dist/<platform>/treer-agent-server
dist/<platform>/treer-agent-host
```

The default platform set is `linux-x86_64`, `linux-aarch64`, `darwin-x86_64`,
and `darwin-aarch64`. A deliberately partial release can override it with a
comma-separated `TREER_RELEASE_PLATFORMS`. The publisher requires a clean
worktree and requires the release version to match `[workspace.package]` in
`Cargo.toml`.

Prepare locally, publish to canary, test it, and then promote the exact signed
release to stable:

```bash
just artifacts-prepare v0.2.0
just artifacts-canary v0.2.0
just artifacts-verify v0.2.0
git tag v0.2.0 # if the canary commit is not already tagged
just artifacts-stable v0.2.0
```

Stable promotion requires the local version tag to point to the commit recorded
in the canary manifest. It never uploads binaries again. Re-running an identical
publish is idempotent; trying to reuse a version with different manifest bytes
is rejected. Uploads use Wrangler v4 and its authenticated account. Override
`TREER_RELEASE_BUCKET`, `TREER_RELEASE_BASE_URL`, `TREER_CLOUDFLARE_PROFILE`, or
`TREER_WRANGLER` when publishing to another R2 environment.

The current installed Controller updater still downloads the Proxy's flat
`/artifacts/<platform>/...` endpoints. Signed channel consumption and remote
rollout orchestration are separate follow-up work; publishing an R2 release does
not yet change running machines.

Open the web UI, select a workspace, and choose **Add machine**. The dialog
separates installation from workspace enrollment. The installation command is
public and reusable:

```bash
curl -fsSL 'https://PROXY_HOST/install.sh' | sh
```

The script detects the target platform, installs `treer` to `~/.local/bin`, and
puts the Host and Controller binaries in `~/.local/libexec/treer`. It also exposes
`treer-agent-server` in `~/.local/bin` as a symlink to the Controller, so both
commands are available from the same PATH entry. It does not contain a
credential, create workspace configuration, register a service, or start a
process. Override `TREER_INSTALL_DIR` or `TREER_AGENT_SERVER_INSTALL_DIR` when
needed.

The separate connection command contains a 10-minute, single-use enrollment key:

```bash
TREER_ENROLLMENT_KEY='enr_v1_...' \
  treer-agent-server connect \
  --proxy 'https://PROXY_HOST/'
```

`connect` decodes the workspace ID from the key, exchanges it for a long-lived
machine credential, uses the current directory as the workspace root, registers
the Host service, and starts it. Linux uses a systemd user service with restart
and linger enabled; macOS uses a per-user LaunchAgent with `KeepAlive`. Override
`TREER_WORKSPACE_ROOT`, `TREER_STATE_DIR`, or `TREER_AGENT_SERVER_LISTEN` when
needed. The first available loopback port starting at `8790` is saved per
workspace.

Setup is interactive by default. Before enrollment it explains that the Agent
Server is a persistent proxy and agent host running with the current user's
system permissions, and recommends a dedicated account, VM, container, or
other sandbox. On the first setup it asks for a machine name. Treer stores a
random installation identity and that name in the machine-level state directory;
later setup runs reuse both, so claiming another enrollment link does not create
a duplicate machine. The identity is random and does not contain or derive from
a MAC address.

Automation must opt in explicitly and provide a name on first setup:

```bash
TREER_ENROLLMENT_KEY='enr_v1_...' \
  treer-agent-server connect \
  --proxy 'https://PROXY_HOST/' \
  --non-interactive --accept-risk --name 'build-machine'
```

Pull and hot-activate the latest Controller and agent-facing CLI with one command:

```bash
treer-agent-server update --workspace default
```

`update` downloads the current platform's `treer-agent-server` and `treer`
artifacts from the configured Proxy, validates both executables, and replaces
them atomically. It asks the stable Host to restart only the Controller and waits
for a new Controller epoch to become healthy. If activation fails, both binaries
are restored and the old Controller is restarted. The Host, existing agents,
PTYs, and buffered terminal output remain alive while the browser reconnects.

The long-lived `treer-agent-host` is deliberately not part of this hot update.
Installing a newer Host still requires a full service restart, which terminates
the agents and PTYs owned by that Host.

The enrollment key can be used once and contains a versioned encoding of its
workspace ID. The Proxy verifies that embedded workspace against its enrollment
record, then creates a stable server ID and a long-lived credential bound to that
server and workspace.
The credential is stored in the Controller configuration with owner-only file
permissions and is required for both the Controller WebSocket and agent-facing
Proxy API. Production mode requires an HTTPS public URL.

The host administrator manages the service through the agent-server binary, not
the agent-facing `treer` command:

```bash
treer-agent-server update --workspace default
treer-agent-server --tui --workspace default
treer-agent-server service status
treer-agent-server service logs --follow
treer-agent-server service restart-controller
treer-agent-server service stop
treer-agent-server service start
treer-agent-server service restart
treer-agent-server service uninstall
```

`restart-controller` activates a Controller binary installed manually and
preserves running agents. `restart` restarts the long-lived Host itself and
therefore terminates the agents and PTYs owned by that Host.

`--tui` opens an interactive dashboard for the installed workspace. It shows
the local Controller health, Proxy reachability, and Agents on this machine,
and provides start, stop, full restart, and Controller-only restart actions.
Stop and full restart require confirmation because they terminate Host-owned
Agents and PTYs. Press `?` in the dashboard to show all key bindings.

Add `--workspace WORKSPACE_ID` after `service` when managing a workspace other
than `default`. On Linux, installation prints an actionable warning if systemd
linger cannot be enabled automatically. On macOS, a LaunchAgent starts at user
login; an always-on pre-login LaunchDaemon would require a separate privileged
installation flow.

## Users, administrators, and invitations

The platform administrator is not a Treer user and does not belong to an
organization. Open `/admin` and use the password supplied in `ADMIN_PASSWORD`
to access the separate admin dashboard. It reports the current platform-wide
machine and Agent totals and creates single-use user invitations. A user who
registers from an administrator invitation receives an organization named
`<preferred name> Personal` and owns it. Treer does not seed an initial
organization or workspace.

Organization owners and administrators can create member invitations from
**Members**. Registering from an organization invitation only joins that
organization; it does not create another personal organization. Users sign in
with email/password or a configured GitHub or Google account, and can update
their email or preferred name without changing their stable identity or
organization access. Organization owners and administrators can also rename
their organization.

Users, invitations, sessions, organizations, machine credentials, Agent mail,
services, and workload signing keys are stored in PostgreSQL. The Proxy requires
`DATABASE_URL`; `just test-db-up` starts the local Docker database used by the
test suite. Changing `ADMIN_PASSWORD` changes the administrator's next login
password without rewriting user accounts.

Registration welcome messages and password recovery use Cloudflare Email Sending.
Set `CLOUDFLARE_API_TOKEN` on
the Proxy service; the token must be allowed to send email for the configured
account. The default account ID is `84188a5eaca91f5c9914fa67494c84c1` and the
default sender is `service@treer.ai`; override them with
`CLOUDFLARE_ACCOUNT_ID` and `TREER_EMAIL_FROM`. Reset links expire
after 30 minutes, can be used once, and revoke every existing user session when
the password changes. Requests return the same response for known and unknown
email addresses.

GitHub and Google OAuth are optional. Configure either provider with both its
client ID and client secret:

```text
GITHUB_OAUTH_CLIENT_ID=...
GITHUB_OAUTH_CLIENT_SECRET=...
GOOGLE_OAUTH_CLIENT_ID=...
GOOGLE_OAUTH_CLIENT_SECRET=...
```

Register these exact server-side callback URLs, using the public Proxy origin:

```text
https://<proxy-origin>/api/auth/oauth/github/callback
https://<proxy-origin>/api/auth/oauth/google/callback
```

GitHub requests `user:email`; Google requests `openid email profile`. Treer only
links accounts using provider-verified email addresses, then persists the
provider's stable subject ID for later logins. Provider access tokens are not
stored. When a verified OAuth email first claims an existing account whose
email was not previously verified, Treer revokes that account's old password
and sessions to prevent email pre-registration attacks. New accounts still
require an invitation by default. Set
`TREER_INVITATION_REQUIRED=false` to allow open password and OAuth registration;
every open registration creates a user-owned personal organization.

## NATS event bus and multi-Proxy routing

The Proxy can publish revisioned workspace changes as versioned domain events
to NATS JetStream. NATS is optional: without `TREER_NATS_URL`, the same event
contract runs in process and the browser event stream continues to work, but
that Proxy is a standalone routing instance. Configure NATS before running more
than one Proxy replica.

With NATS configured, Treer uses four broker paths:

- short-lived JetStream KV leases track the Proxy that owns each Controller;
  heartbeats renew only this small ownership record, while a separate KV bucket
  updates the full machine snapshot only when machine or Agent state changes;
- file-backed JetStream KV retains the latest workspace, rename, and deletion
  projections so replicas that reconnect do not miss control-plane changes;
- Core NATS request/reply routes commands, terminal sessions, and
  virtual-network frames to the owning or initiating Proxy;
- JetStream stores versioned domain events. PTY and TCP bytes are
  never retained in JetStream.

Replica IDs come from `TREER_PROXY_INSTANCE_ID`, then
`RAILWAY_REPLICA_ID`, or a generated process ID. Each live replica must have a
unique ID. Load balancers do not need sticky sessions: browser sessions and
Controller connections may land on different replicas.

Each Controller heartbeat also rechecks its machine against PostgreSQL, so a
revoked machine is disconnected even if NATS delivery is interrupted.

For a single-host deployment with separate PostgreSQL, NATS, Proxy, and App
processes, use the checked-in Compose stack:

```bash
ADMIN_PASSWORD='replace-this' \
POSTGRES_PASSWORD='replace-this-too' \
DATABASE_URL='postgres://treer:replace-this-too@postgres:5432/treer' \
docker compose up --build -d
curl -fsS http://127.0.0.1:8222/jsz
```

This persists PostgreSQL and JetStream data in separate volumes. NATS client
and monitoring ports bind only to host loopback; the Proxy is available on port
8787 and the App on port 3000. Set `TREER_PROXY_PUBLIC_URL` when other machines
must reach the Proxy and `TREER_APP_PUBLIC_URL` to the exact browser origin. If
credentials contain URL-reserved characters, percent-encode them in
`DATABASE_URL`.

For a Proxy started outside Compose, configure:

```bash
export TREER_NATS_URL='nats://127.0.0.1:4222'
export TREER_NATS_STREAM='TREER_EVENTS'
export TREER_NATS_SUBJECT_PREFIX='treer.v1.events'
export TREER_NATS_CLUSTER_SUBJECT_PREFIX='treer.v1.cluster'
export TREER_PROXY_INSTANCE_ID='proxy-a' # optional outside an orchestrator
```

Subjects use
`treer.v1.events.workspace_<encoded-workspace-id>.<action>`. Payloads use the
shared `DomainEventEnvelope` with a stable event ID, schema version, actor,
action, resource, timestamp, revision, correlation fields, and JSON payload.
JetStream retains up to 1 GiB or 30 days and deduplicates retried publishes by
event ID within a 10-minute window. A configured Proxy fails startup when it
cannot initialize the expected stream or when an existing stream does not
capture its configured subject prefix.

Runtime disconnects are retried from a bounded in-memory queue and do not fail
the originating workspace mutation. This is not yet a transactional outbox: a
Proxy crash or a full queue can lose unpublished events. Durable database state
therefore remains authoritative until the outbox is implemented.

## Managed deployment

The root `Dockerfile` and `railway.json` deploy `treer-proxy` to Railway.
`web/wrangler.jsonc` deploys the independent React App to Cloudflare Workers
Static Assets. Railway's injected `PORT` and `RAILWAY_PUBLIC_DOMAIN` are
detected automatically by the Proxy.

1. Create a Proxy Railway service from the repository root.
2. Add a Railway PostgreSQL service and expose its `DATABASE_URL` to Treer.
3. For more than one Treer replica, add a NATS service with JetStream enabled
   and expose its private URL as `TREER_NATS_URL`.
4. Set `ADMIN_PASSWORD`, `CLOUDFLARE_API_TOKEN`, `TREER_PROXY_PUBLIC_URL`, and
   `TREER_APP_PUBLIC_URL` on the Proxy service. Add the GitHub and/or Google
   OAuth client variables above when those login methods are enabled. To publish
   Agent-maintained HTTP services, also set
   `TREER_INGRESS_PUBLIC_URL=https://apps.example.com/` and attach
   `*.apps.example.com` to the same Proxy service.
5. Authenticate Wrangler and deploy the `canary` or `production` environment
   defined in `web/wrangler.jsonc`.
6. Add the Proxy public domain, then increase the Proxy replica count. Railway
   supplies a distinct `RAILWAY_REPLICA_ID` to each Proxy replica. Wrangler
   owns the App custom domains.

The Proxy image builds and serves Linux agent binaries for its own CPU
architecture. The App Worker contains only the frontend and runtime config.

Changes intended for the managed Railway deployment go to Canary before
Production. From an authenticated operator checkout, run:

```bash
just release-canary HEAD
```

The command verifies a clean commit, runs the complete local gate, deploys the
Canary Proxy and Cloudflare App, then verifies cross-machine virtual networking,
public wildcard ingress, and traffic accounting. It writes a release manifest;
Production accepts only that manifest through `just promote-production`. See
[the release process](docs/releases.md) and [Canary runbook](docs/canary.md).

Open the App URL to discover servers, create agents, and attach to
their live terminals. The browser terminal supports ANSI colors, alternate
screens, cursor movement, per-keystroke input, paste, and dynamic resize. PTY
input, replay, and live output remain raw bytes from the Host through the
Controller and Proxy to the browser. The Host socket uses length-prefixed binary
frames, and both WebSocket hops use binary frames instead of Base64 JSON payloads.
Agents inherit `TREER_WORKSPACE_ID`,
`TREER_SERVER_ID`, `TREER_AGENT_ID`, `TREER_AGENT_SERVER_URL`, and a private
workload credential; they can use the local agent server API to discover or
control other agents in the same workspace. The credential is consumed by the
CLI and should not be printed or forwarded directly.

Workspace members and managed Agents can also save reusable launch profiles:

```bash
treer profile create reviewer --cwd . codex -- review --base main
treer profile list
treer profile launch reviewer --machine build-machine --name review-42
```

Profiles store an executable and ordered arguments rather than a shell command
string. Use an explicit shell executable such as `sh -lc` only when shell
expansion is required. Profiles are plaintext workspace configuration and must
not contain secrets.

## Host and Controller

`treer-agent-host` is the stable process boundary. It understands only raw
process commands, PTY input/output, resize, stop, and revisioned output replay.
It does not understand agent kinds, prompts, workspaces, or the Proxy protocol.

`treer-agent-server` is the replaceable Controller. It translates Proxy
commands, detects agent status, exposes the local API, and rebuilds its full
snapshot from the Host after every restart. Mutating Host commands carry stable
operation IDs so a reconnect or retry cannot spawn or stop an agent twice.
Codex and Claude agents start inside the user's interactive shell before their
commands are entered through the PTY. This loads shell configuration such as
`.bashrc` or `.zshrc` before command lookup.

## Workspace network

On Linux, each managed agent runs in a rootless user and network namespace. A
TUN adapter captures its TCP traffic and sends it to the Controller's loopback
SOCKS5 endpoint, so applications do not need to support proxy environment
variables. Namespace sockets are created by the parent Controller process and
passed into the namespace; this keeps the Proxy WebSocket and machine egress
outside the sandbox. Linux requires `unshare(1)` from `util-linux` and a kernel
that permits unprivileged user namespaces.

The namespace bind-mounts private resolver configuration with `hosts: files
dns` and a non-loopback nameserver. This bypasses host NSS plugins such as mDNS
that may reject workspace virtual-host suffixes before a DNS packet reaches the
TUN adapter. It also masks the host's `nscd` socket so cached host lookups cannot
bypass the private resolver. `tun2proxy` answers those DNS requests from its
virtual pool and restores the original hostname for Treer routing. The host's
resolver files and cache service are not modified, and virtual-host changes
remain dynamic.

In transparent mode the Controller clears the standard HTTP(S) and all-protocol
proxy environment variables, because its loopback SOCKS listener is outside the
agent network namespace and normal application traffic must enter the TUN
adapter. `TREER_NETWORK_PROXY` remains available for diagnostics. Set
`TREER_NETWORK_MODE=proxy-env` before starting the Controller to disable the
transparent namespace wrapper and inject the SOCKS URL through `ALL_PROXY` and
`all_proxy` instead. Native macOS currently uses this compatibility mode; use a
Linux container when transparent capture is required.

Managed agents reach the Controller's local API through the reserved TEST-NET-1
address `192.0.2.1`. Using an IP bypasses libc NSS and mDNS entirely. The local
SOCKS endpoint recognizes this address and bridges HTTP and WebSocket traffic
directly to the Controller's loopback listener; it is not a workspace virtual
host and never traverses the Proxy. This keeps `treer` usable inside transparent
network namespaces.

Every TCP connection asks the Proxy to resolve the destination and apply network
policy. For an ordinary hostname or IP address, the Proxy returns a direct route
and the source Controller opens the outbound socket locally; application payload
bytes do not traverse the Proxy. Workspace virtual-host streams are multiplexed
as binary frames over the Controllers' existing `/agent/connect` WebSockets, so
target machines need no inbound port. Each relayed stream has an independent
flow-control window, and terminal and relayed network frames share the same
authenticated connection.

Network access is allowed by default. A machine service is a durable record for
a long-running host-network process: machine, target host, target port, and TCP
or HTTP protocol. Virtual hosts are independent aliases that map any valid
hostname to a service. Both are managed from the Network view. For example,
registering `build-machine` port `8080` as `api`, then mapping `api.internal` to
that service, makes this work without exposing port 8080:

```bash
curl http://api.internal/
```

Deleting a machine also deletes its services and their virtual hosts. Deleting
a service removes its aliases but does not stop the external process. Virtual
host names are exact and case-insensitive; no suffix or naming convention is
reserved. Only explicitly configured records are treated as workspace virtual
hosts. Other hostnames and IP addresses use ordinary outbound access through
the source machine, subject to the same network policy boundary.

Virtual-host changes are active without a Proxy or Agent Server restart. The
Proxy updates its in-memory routing table and immediately broadcasts a full,
revisioned snapshot to every online Controller in that workspace. A Controller
receives a fresh snapshot after every WebSocket registration and ignores stale
revisions on the same connection. The Proxy also reloads PostgreSQL and broadcasts
snapshots every five seconds, which repairs missed or out-of-band changes.

HTTP services can also be published through wildcard HTTPS ingress. Configure
one wildcard domain on the Proxy; creating an endpoint changes only PostgreSQL
routing metadata and does not create another DNS record or TLS certificate:

```bash
export TREER_INGRESS_PUBLIC_URL='https://apps.treer.ai/'
treer publish create api --slug issue-tracker --access public
treer publish list
```

`public` leaves authentication to the application. `workspace` redirects human
visitors through their Treer login and accepts a managed Agent's audience-bound
token in `Treer-Authorization`. Application `Authorization`, cookies,
`Set-Cookie`, streaming responses, and WebSocket upgrades pass through unchanged.
Treer consumes its own ingress cookie/header and strips client-supplied
`X-Treer-*` identity headers before forwarding. Only HTTP services can be
published. Disabling or deleting an ingress does not stop the machine service.

The Proxy also records hourly payload totals for each relayed machine direction.
Workspace members can query the last 1 to 720 hours without scanning individual
connections:

```text
GET /api/workspaces/<workspace_id>/traffic?hours=24
```

The response reports `source_server_id`, `destination_server_id`,
`payload_bytes`, and `payload_frames`. Counters flush to PostgreSQL every ten
seconds; protocol overhead, PTY bytes, public ingress, and direct internet
egress are intentionally excluded. Hourly rows are retained for 90 days.

Deleting an online machine sends a confirmed shutdown command over its existing
Controller WebSocket before revoking the machine credential. A capable
Controller then stops the local systemd user service or macOS LaunchAgent, which
also terminates its Host and managed agents. The service remains installed and
can still be started manually. Offline machines and older Controllers are
deleted without waiting; their revoked credential prevents a later reconnect.

Authorization is a separate Proxy subsystem. The current policy engine defaults
to allow and evaluates ordered asynchronous policy evaluators using
subject/action/resource context. Future agent, terminal, file, shell, and
network rules can share that boundary without changing virtual-host resolution.
Agent proxy URLs carry the agent ID through SOCKS5 authentication, so network
policy requests already identify their originating agent; local machine shells
fall back to a machine-level subject.

Managed agents can control workspace discovery records through the local Agent
Server without receiving Proxy credentials:

```bash
treer service register api --port 8080 --protocol http
treer service probe api
treer virtual-host list
treer virtual-host add api.internal api
treer virtual-host delete api.internal
```

The Agent Server forwards the caller identity under its machine credential, and
the Proxy evaluates service and virtual-host actions independently. They
currently inherit the allow-all default. On Linux, a process started directly
inside a managed Agent remains in its private network namespace. Register
services started on the machine host network, such as systemd services or
Docker containers with published ports.

## Agent collaboration

The `treer` binary talks to the local agent server by default. Managed agents
receive its location in `PATH` and `TREER_BIN`, so they can discover and contact
peers without knowing the proxy address. `treer whoami` returns the current
workspace, agent, and machine records. `treer discover` includes those current
agent and machine records under `self` so callers can distinguish themselves
from peers without matching names.

```bash
treer whoami
treer agent list
treer agent get reviewer
treer agent rename reviewer code-reviewer
treer machine rename self build-machine
treer machine delete srv_obsolete
treer agent attach reviewer
treer agent delete obsolete-helper
treer agent prompt reviewer "Review the parser changes" --wait --timeout 120000
treer agent read reviewer --lines 80
treer agent send-keys reviewer ctrl-c
```

For asynchronous coordination, `mail` persists a message without writing to a
recipient's PTY or changing its runtime state. Recipients see mail only when
they explicitly call `inbox`; the returned batch is then marked read:

```bash
treer mail --to reviewer "Review the parser when you next check your inbox."
treer inbox
treer mail --to coordinator --context msg_RETURNED_BY_INBOX "Review complete."
treer mail -t coordinator -t tester -c msg_first -c msg_second "Checks agree."
treer inbox --limit 100
```

Both `--to` and `--context` may be repeated. A first message omits context;
replies use one or more exact prior message IDs to form a message graph. Mail
is workspace-scoped and requires a managed Agent workload credential. Messages,
recipient-specific unread state, and context edges live in PostgreSQL, so any
Proxy replica can serve the next inbox call without NATS or sticky routing.

Agents can also discover and address the humans who belong to the workspace's
organization. The directory deliberately returns stable user IDs, preferred
names, and roles without exposing email addresses:

```bash
treer human list
treer mail --to usr_123 "The deployment is ready."
treer mail --to reviewer --to Owner "Please coordinate on this result."
```

`--to` is the only recipient option. It accepts Agent IDs, user IDs, unique
Agent names, and unique preferred names in one shared address space. Stable IDs
take precedence; an ambiguous display name is rejected with a request to use an
ID. Human recipients open **Inbox** in the web sidebar for the selected
workspace. The central mailbox keeps recent received messages available after
they are read and reconstructs visible context chains for tracing. Opening it
marks unread deliveries in the returned batch read and does not receive a push
notification.

## Workload identity

A managed Agent can request a short-lived Proxy-signed Bearer token for a
registered machine service. The audience accepts a service ID or unique service
name and is canonicalized to the stable `service_id`:

```bash
TOKEN="$(treer identity token api)"
curl -H "Authorization: Bearer $TOKEN" http://api.internal/
treer identity token api --json
```

Tokens use Ed25519, expire after 60 seconds, and contain the Agent, machine,
workspace, and service IDs. The signing key remains in Proxy PostgreSQL storage.
Services can cache the public key set from `/.well-known/jwks.json` and verify
tokens locally, or submit a token and expected service ID to
`POST /.treer/identity/verify`:

```json
{"token":"eyJ...","audience":"svc_..."}
```

The verify endpoint returns `{"active":false}` for an invalid, expired, or
wrong-audience token. Treer does not inject these credentials into network
traffic; the Agent and target application explicitly participate in the
authentication exchange.

On the machine running an Agent Server, `treer agent attach <target>` opens the
agent's live PTY in the current native terminal. Input, colors, cursor control,
and terminal resize are passed through directly. Press `Ctrl-]` to detach
without stopping the agent. The shorter `treer attach <target>` alias is also
available.

Targets accept an agent id, a unique agent name, or `self`/`.` from inside a
managed agent. `prompt --wait` waits for observed activity followed by `idle`,
`blocked`, `exited`, or `failed` by default; repeat `--until STATUS` to select
other states. It is state-based coordination, not strict per-prompt turn
correlation.

The original top-level `list`, `prompt`, `read`, and `stop` commands remain as
compatibility aliases. To create a peer:

```bash
treer create --server SERVER_ID --kind command --name shell -- /bin/sh
```

Print the agent skill bundled with the installed binary:

```bash
treer --skill
treer --skills  # accepted alias
```

This does not require a running proxy. The source is
`skills/treer/SKILL.md`, embedded at build time so its instructions match the
CLI version that prints them.

## Checks

```bash
cargo fmt --all -- --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

The same sequence is available as `just check` when `just` is installed.

## License

Copyright 2026 Zijian Zhang.

Treer is licensed under the [Apache License 2.0](LICENSE). Vendored third-party
assets retain their own license terms; see the license files under `web/vendor`.
