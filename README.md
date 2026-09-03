# treer

Treer is a distributed runtime and control plane for coding agents.

Each machine runs a stable Treer Host that owns local agent processes, PTYs, and
terminal history. A hot-updatable Controller connects that Host to the central
Proxy, which groups machines into logical workspaces and routes discovery and
control commands between them. Core also stores durable workspace Messages,
per-recipient delivery state, and an ordered context DAG. External channels such
as Mail and Telegram are ordinary workspace Apps over those public contracts.

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
and comparisons with Herdr and AgentENV. Self-hosted Compose, GHCR images, and
`/admin` control-plane updates are in [deploy/README.md](deploy/README.md).

## Self-hosted deployment

A single-host install runs PostgreSQL, NATS, Proxy, the browser App, and an
updater sidecar from published GHCR images. Copy `.env.example`, set
`ADMIN_PASSWORD`, `POSTGRES_PASSWORD`, `DATABASE_URL`, and a long random
`TREER_UPDATER_TOKEN`, then pull and start:

```bash
cp .env.example .env
docker compose pull
docker compose up -d
```

The App is on port 3000, the Proxy on 8787. Open `/admin` with `ADMIN_PASSWORD`
to invite users and to update the control-plane images. Apply talks to the
updater sidecar; Proxy never mounts the Docker socket. After the control plane
moves, each enrolled machine still runs `treer-agent-server update`. Default
`TREER_IMAGE_TAG=stable`. Pin `canary` or a version tag such as `v0.1.3` when
you want that channel instead.

Set `TREER_PROXY_PUBLIC_URL` to the URL other machines can reach and
`TREER_APP_PUBLIC_URL` to the exact browser origin. Operator detail, rollback,
and GHCR tag rules are in [deploy/README.md](deploy/README.md).

Local source builds overlay `compose.dev.yaml` and pass `--build`. The hosted
Railway plus Cloudflare path is a separate source rebuild of Proxy plus a
Worker App; see [Managed deployment](#managed-deployment).

## Run the prototype

Start the proxy and web control plane:

```bash
just test-db-up
export DATABASE_URL=postgres://treer:treer@127.0.0.1:55432/treer_test
export TREER_ENABLE_CORE_MESSAGES=true
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

Core Message routes are rollout-gated and default off. Enable the Proxy gate
above only after the database migration and policy defaults are ready. Treer
can supervise a single-process HTTP App while preserving its service and
virtual host across runtime replacement and Controller reconnects:

```bash
treer app create --machine build-machine --name docs --port 8080 \
  --hostname docs.internal python3 -- -m http.server 8080
treer app list
treer app restart docs
```

When wildcard ingress is configured, every Managed App receives a stable,
workspace-authenticated `public_url` on its own origin. Browser assets,
root-relative paths, redirects, and App routes therefore resolve without the
control plane's `/virtual-hosts/.../proxy/` tunnel prefix. Installations without
wildcard ingress continue to use that authenticated tunnel as a fallback.

Managed Apps do not add a new security boundary or secret store. More complex
Apps may still use an external supervisor and register ordinary Treer services.

`--public-url` is the URL that other machines can reach. `stage-artifacts`
places the current platform's `treer-agent-host`, `treer-agent-server`, and
`treer` binaries under `dist/<platform>` for the proxy to serve. Release
deployments should stage all required Linux and macOS platform directories or
set `--artifacts-dir` to an equivalent artifact tree. When a platform artifact
is not present locally, the Proxy redirects to the latest Treer GitHub Release;
override its base with `TREER_RELEASE_ARTIFACT_BASE_URL` when mirroring release
assets elsewhere.

The `Build release artifacts` GitHub workflow builds `linux-x86_64`,
`linux-aarch64`, `darwin-x86_64`, and `darwin-aarch64` on native runners. It
runs manually or for a pushed `v*` tag and retains commit-attributed artifacts
for 14 days. It never creates a GitHub Release or publishes to R2. After a
successful run, collect the exact commit locally with the authenticated GitHub
CLI:

```bash
just collect-artifacts HEAD
```

The collector rejects missing platforms, mismatched commit metadata, and
invalid checksums before placing the files under `dist/<platform>`. Set
`TREER_ARTIFACT_RUN_ID` to select a particular successful workflow run.

### Publish signed R2 releases

Production release artifacts live in the `treer-releases` Cloudflare R2 bucket
at `https://releases.treer.ai/`. The publisher keeps versioned objects immutable
under `releases/<version>/` and updates the separately signed `canary` and
`stable` pointers under `channels/`. R2 is only the distributor: every manifest
and channel pointer has a detached Ed25519 signature, and every artifact is
identified by its byte length and SHA-256 digest. The signed version manifest
also records each platform's source commit, package version, and Rust compiler.

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
the artifact tree:

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

`connect` decodes the workspace ID from the key, uses the current directory as
the workspace root, registers the Host service, and starts it. Before exchanging
the single-use key, it validates local paths and binaries. On Linux and macOS,
`auto` starts the Host detached with `nohup`, records its PID and process start
identity, and writes output under the Treer state directory. This mode
deliberately provides no start-at-boot or Host crash restart. Select a native
user service explicitly with `--service-mode systemd` on Linux or
`--service-mode launchd` on macOS; attached diagnostics remain available with
`--service-mode foreground`.

Explicit Linux systemd setup checks linger and prints an actionable warning
when the service will stop after the last login session exits. It does not
attempt a privileged linger change. `connect` reports success only after the
local Controller health endpoint reports `proxy_connected`. Override
`TREER_WORKSPACE_ROOT`, `TREER_STATE_DIR`, `TREER_RUNTIME_DIR`, or
`TREER_AGENT_SERVER_LISTEN` when needed. The first available loopback port
starting at `8790` is saved per installed machine.

Setup is interactive by default. Before enrollment it explains that the Agent
Server is a persistent proxy and agent host running with the current user's
system permissions, and recommends a dedicated account, VM, container, or
other sandbox. On the first setup it asks for a machine name. Treer stores a
random installation identity and that name under a hostname-scoped machine
state directory; later setup runs on that host reuse both, so claiming another
enrollment link does not create a duplicate machine. Shared home directories
therefore give each host a separate installation identity. The identity is
random and does not contain or derive from a MAC address.

Reconnecting an installed workspace uses the new enrollment key to atomically
bind that installation identity and rotate the machine credential. It does not
require the locally saved machine credential to remain valid, so a machine that
was removed or whose credential was revoked can be enrolled again without
deleting its local service configuration first.

Controller and Host configuration and service-manager entries are named by the
Proxy-issued server ID, not by workspace. The Linux unit is pinned to the
installation hostname with `ConditionHost`, and the Host Unix socket lives in
the node-local runtime directory (`$XDG_RUNTIME_DIR/treer`, normally
`/run/user/$UID/treer`) rather than persistent or network-mounted state.

Automation must opt in explicitly and provide a name on first setup:

```bash
TREER_ENROLLMENT_KEY='enr_v1_...' \
  treer-agent-server connect \
  --proxy 'https://PROXY_HOST/' \
  --non-interactive --accept-risk --name 'build-machine'
```

Pull and hot-activate the latest Controller and agent-facing CLI with one command:

```bash
treer-agent-server update
treer-agent-server update --proxy https://proxy.canary.treer.ai/
```

`update` downloads the current platform's `treer-agent-server` and `treer`
artifacts, validates both executables, and replaces them atomically. `--proxy`
selects an explicit download source; without it, the updater orders the
machine's installed services by server ID and uses the first service's Proxy.
From a normal host shell it asks every installed stable Host to restart its
Controller. From a managed Agent sandbox it restarts only that Agent's
Controller and checks the new epoch through `TREER_AGENT_SERVER_URL`; the shared
binary is still available to other Controllers on their next restart. If
activation fails, both binaries are restored and the affected Controller set is
restarted. Hosts, existing agents, PTYs, and buffered terminal output remain
alive while browsers reconnect.

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
treer-agent-server update
treer-agent-server --tui --workspace WORKSPACE_ID
treer-agent-server service status
treer-agent-server service logs --follow
treer-agent-server service restart-controller
treer-agent-server service repair
treer-agent-server service stop
treer-agent-server service start
treer-agent-server service restart
treer-agent-server service uninstall
```

`service status` with no `--workspace` lists every local install (`workspace`,
`server_id`, listen address, manager, and Proxy lease). Other service commands
use the unique local install when there is exactly one; if several exist they
print that table and exit 2. Pass the real workspace ID (`ws_…`) from Add
machine or from that table. `service start` is ready only when `/api/health`
reports `proxy_connected`; a live local API is not enough. After lid-close
sleep, Proxy bounce, or a duplicate fence, the Controller reconnects by itself.
If the webpage still shows Offline, copy:

```bash
treer-agent-server service --workspace ws_… restart-controller
```

Each installed binary reports the package version and exact source commit:

```bash
treer --version
treer-agent-server --version
treer-agent-host --version
```

The Controller's loopback `/api/health` response and each machine in the web
application show separate Controller and Host build identities. They can differ
after a hot Controller update because `update` deliberately leaves the stable
Host running. A full reinstall and service restart is required to activate a
new Host binary.

`restart-controller` activates a Controller binary installed manually and
preserves running agents. `restart` restarts the long-lived Host itself and
therefore terminates the agents and PTYs owned by that Host.

If enrollment saved the machine configuration but service registration or
startup failed, repair it without another enrollment key:

```bash
treer-agent-server service --workspace WORKSPACE_ID repair
treer-agent-server service --workspace WORKSPACE_ID repair --service-mode systemd
```

The default `auto` repair switches the Host to detached `nohup` mode under the
same rules as `connect`. When switching supervision modes, Treer removes the old
systemd unit or LaunchAgent, or stops the old `nohup` process, before activating
the replacement. An inactive partial systemd registration can be cleaned even
while the user bus is unavailable. A running Host is never orphaned: if its
current manager cannot stop it, repair exits and asks the operator to stop the
existing foreground command or restore the user manager before retrying.

`--tui` opens an interactive dashboard for the installed workspace. It shows
the local Controller health, Proxy reachability, current supervision mode, and
Host-owned Agents on this machine. The same mode and any persisted fallback
reason appear in the web machine overview after the Controller connects. The
Agent list remains available from local state while the Proxy is unreachable,
and the dashboard provides start, stop, full restart, and Controller-only
restart actions.
Stop and full restart require confirmation because they terminate Host-owned
Agents and PTYs. Press `?` in the dashboard to show all key bindings.

Add `--workspace WORKSPACE_ID` after `service` when this host has more than one
install. The default `nohup` Host survives terminal logout but not reboot or
process failure; run `service start` after either event. Connecting the same
hostname into the same workspace reuses the existing machine identity instead
of creating a second Host registration.

## Users, administrators, and invitations

The platform administrator is not a Treer user and does not belong to an
organization. Open `/admin` and use the password supplied in `ADMIN_PASSWORD`
to access the separate admin dashboard. It reports platform-wide user, machine,
and Agent totals. Click a count to expand that inventory: users can be searched,
issued a one-time password-reset link, and signed out of every session; machines
list enrolled hosts and expand to their live Agents. The same page still creates
single-use user invitations and can list pending invites, organizations, and
recent admin activity. On a self-hosted Compose stack it also checks and applies
GHCR image updates for Proxy, App, and the updater sidecar. Workspace Settings
does not include that control. A user who
registers from an administrator invitation receives an organization named
`<preferred name> Personal` and owns it. Treer does not seed an initial
organization or workspace.

Organization owners and administrators can create member invitations from
**Members**. Registering from an organization invitation only joins that
organization; it does not create another personal organization. Users sign in
with email/password or a configured GitHub or Google account, and can update
their email or preferred name without changing their stable identity or
organization access. Organization owners and administrators can also rename
their organization. Organization members can create and rename workspaces;
renaming keeps the workspace ID, enrolled machines, Agents, and services intact.
Workspaces are organization-visible by default. A workspace owner can restrict
one to explicitly added organization members and organization groups. Workspace
grants use `owner` and `member`; only an owner can change access or delete the
workspace. Organization owners and administrators are implicit owners of every
workspace, and the workspace creator starts as an explicit owner.

Users, invitations, sessions, organizations, machine credentials, services,
App OAuth codes, and workload signing keys are stored in PostgreSQL. The Proxy requires
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

The checked-in Compose stack starts NATS next to PostgreSQL, Proxy, App, and
the updater. See [Self-hosted deployment](#self-hosted-deployment). NATS client
and monitoring ports bind only to host loopback. If credentials contain
URL-reserved characters, percent-encode them in `DATABASE_URL`.

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
On mobile, the workspace opens on the machine and Agent lists without mounting
a terminal. Selecting an Agent opens its terminal directly in full-screen mode;
closing it returns to those lists.
Agents inherit `TREER_WORKSPACE_ID`,
`TREER_SERVER_ID`, `TREER_AGENT_ID`, `TREER_AGENT_SERVER_URL`, and a private
workload credential; they can use the local agent server API to discover or
control other agents in the same workspace. The credential is consumed by the
CLI and should not be printed or forwarded directly.

Workspace members and managed Agents can also save reusable launch profiles:

```bash
treer agent admin profile create reviewer --cwd . codex -- review --base main
treer agent admin profile list
treer agent admin profile launch reviewer --machine build-machine \
  --name review-42 --cwd packages/api
```

The web Create Agent dialog includes a built-in Terminal option. It starts the
machine user's interactive shell without injecting a command and is not stored
as a launch profile.

The dialog compares recognized launch-profile commands with the CLIs reported
by the selected machine. A missing built-in CLI is shown as `Install <name>` in
the Launch list. Selecting it creates an ordinary terminal Agent on that
machine, runs the cataloged upstream installer, and then starts the CLI so the
operator can finish login. The built-in catalog covers Codex, Claude, OpenCode,
and Pi; a custom Cursor profile uses the same flow. The Controller rechecks its
PATH on every heartbeat, so a successful install appears as available without a
Controller restart. Installation does not copy provider credentials from
another machine.

Profiles store an executable and ordered arguments rather than a shell command
string. The web editor presents them as one quoted command line and the Create
Agent dialog can launch any saved workspace profile. The Controller starts the
machine user's interactive shell, waits for its startup files such as `.bashrc`
or `.zshrc` to load, and then enters the safely quoted profile command through
the PTY. Shell operators stored as arguments are not interpreted; use an
explicit shell executable such as `sh` with `-lc` when shell expansion is
required. Profiles are plaintext workspace configuration and must not contain
secrets. New workspaces include editable and deletable Codex, Claude, Pi, and
OpenCode profiles; existing workspaces are left unchanged.

`profile launch --cwd` overrides the saved working directory for that launch
without changing the profile. The directory is relative to the selected
machine's Host root and must resolve inside that root. Omitting it uses the
profile's saved `cwd`.

## Host and Controller

`treer-agent-host` is the stable process boundary. It understands only raw
process commands, PTY input/output, resize, stop, and revisioned output replay.
It does not understand agent kinds, prompts, workspaces, or the Proxy protocol.

`treer-agent-server` is the replaceable Controller. It translates Proxy
commands, detects agent status, exposes the local API, and rebuilds its full
snapshot from the Host after every restart. Mutating Host commands carry stable
operation IDs so a reconnect or retry cannot spawn or stop an agent twice.
Codex, Claude, and launch-profile agents start inside the user's interactive
shell before their commands are entered through the PTY. This loads shell
configuration such as `.bashrc` or `.zshrc` before command lookup. Explicit
`command`-kind API and CLI launches remain direct argv execution.

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
transparent namespace wrapper and inject the same loopback listener through
`ALL_PROXY`/`all_proxy` as SOCKS5h and through `HTTPS_PROXY`/`https_proxy` as
HTTP CONNECT. Plain HTTP continues through SOCKS5h because the HTTP listener
implements CONNECT tunnels rather than forward-proxy requests. HTTPS-only
clients such as Codex's reqwest stack use the HTTPS proxy variable instead. The
listener classifies locally: workspace virtual-host names from the Controller
snapshot still take the Treer path; every other destination, including
`api.github.com`, is dialed on this machine and does not wait on the Proxy
WebSocket. Disconnect tears down only relayed streams, so `gh`, npm, and curl
to the public internet keep working while the machine is Offline. In
this mode, `NO_PROXY` and `no_proxy` contain `127.0.0.1,localhost,::1`, so
Controller and App loopback calls do not enter the proxy path. Treer also
configures Git to invoke its
network bridge, so native `git://` remotes retain workspace virtual-host routing
even though Git does not honor `ALL_PROXY`. Other TCP clients can use the same
stdio bridge as their proxy command: `treer network connect HOST PORT`. Native
macOS currently uses this compatibility mode; use a Linux container when
transparent capture is required.
A transparent Agent cannot publish an arbitrary namespace-local listener. Use
`treer app create` for a supervised HTTP App, or register an Agent Interface
Server for a semantic Agent UI. AIS uses the sandbox's Unix bridge directly;
when its descriptor includes `ui_path`, the browser iframe reaches that path on
the Interface port without a service record. Operator-created Agent-scoped
services use the same bridge. `publish_ports` remains an internal Managed App
and runtime mechanism, not an Agent CLI capability.

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

Network access is allowed by default. A service is a durable record for either a
long-running host-network process or a managed runtime's private loopback:
machine, optional runtime, target host, target port, and TCP or HTTP protocol.
Virtual hosts are independent aliases that map any valid hostname to a service.
Logged-in workspace users manage both from the Network view or authenticated
public API. Managed Agents cannot mutate service, virtual-host, or ingress
records. An existing alias such as `api.internal` remains directly usable:

```bash
curl http://api.internal/
```

Deleting a machine also deletes its services and their virtual hosts. Deleting
an Agent deletes services scoped to that Agent. Deleting a service removes its
aliases but does not stop the external process. Virtual host names are exact and
case-insensitive; no suffix or naming convention is reserved. Only explicitly
configured records are treated as workspace virtual hosts. Other hostnames and
IP addresses use ordinary outbound access through the source machine, subject
to the same network policy boundary.

Virtual-host changes are active without a Proxy or Agent Server restart. The
Proxy updates its in-memory routing table and immediately broadcasts a full,
revisioned snapshot to every online Controller in that workspace. A Controller
receives a fresh snapshot after every WebSocket registration and ignores stale
revisions on the same connection. The Proxy also reloads PostgreSQL and broadcasts
snapshots every five seconds, which repairs missed or out-of-band changes.

HTTP services can also be published through wildcard HTTPS ingress. Configure
one wildcard domain on the Proxy; a logged-in workspace user creates the
endpoint from the Network view or authenticated public API. This changes only
PostgreSQL routing metadata and does not create another DNS record or TLS
certificate:

```bash
export TREER_INGRESS_PUBLIC_URL='https://apps.treer.ai/'
```

`public` leaves authentication to the application. `workspace` redirects human
visitors through their Treer login and accepts a managed Agent's audience-bound
token in `Treer-Authorization`. Application `Authorization`, cookies,
`Set-Cookie`, streaming responses, and WebSocket upgrades pass through unchanged.
Treer consumes its own ingress cookie/header and strips client-supplied
`X-Treer-*` identity headers before forwarding. Only HTTP services can be
published. Disabling or deleting an ingress does not stop the service.

The Proxy also records an hourly logical-usage ledger for payload successfully
delivered through Treer. Meter version 1 treats payload bytes in both directions
as billable and classifies them as `virtual_network`, `service_ingress`,
`virtual_host`, or `agent_interface`. Browser-side traffic uses the stable
`browser` client endpoint, so requests appear as `browser` to the destination
machine and responses use the reverse direction. Workspace members can query
the last 1 to 720 hourly buckets without scanning individual connections:

```text
GET /api/workspaces/<workspace_id>/traffic?hours=24
```

The response reports the traffic class, endpoint types and IDs, raw
`payload_bytes`, `payload_frames`, `billable_bytes`, and `meter_version`.
Queries include the serving Proxy's not-yet-flushed counters; counters flush to
PostgreSQL every ten seconds and on graceful shutdown. Another Proxy replica's
newest counters may remain invisible until its next flush. Legacy machine
traffic rows remain readable during the 90-day retention window. NATS protocol
overhead, control-plane messages, retries, PTY bytes, and direct internet egress
are intentionally excluded: broker topology must not change customer usage.
This aggregate ledger is a billing foundation, not yet an invoice-grade source;
hard process failure can lose the pending interval until a durable usage journal
is added.

Deleting an online machine sends a confirmed shutdown command over its existing
Controller WebSocket before revoking the machine credential. A capable
Controller then stops the local `nohup` process, systemd user service, or macOS
LaunchAgent, which also terminates its Host and managed agents. The service
remains installed and can still be started manually. Offline machines and older
Controllers are deleted without waiting; their revoked credential prevents a
later reconnect.

Authorization is a separate Proxy subsystem. The current policy engine defaults
to allow and evaluates ordered asynchronous policy evaluators using
subject/action/resource context. Future agent, terminal, file, shell, and
network rules can share that boundary without changing virtual-host resolution.
Agent proxy URLs carry the agent ID through SOCKS5 authentication, so network
policy requests already identify their originating agent; local machine shells
fall back to a machine-level subject.

Managed Agents cannot publish their sandbox through discovery records. The
`treer network` command exposes only the stdio connection bridge:

```bash
treer network connect database.internal 5432
```

Use `treer app create` for a supervised HTTP process with a stable internal
hostname. Core allocates its service and virtual host atomically and owns their
lifecycle. Use `treer interface register` for an Agent UI. Proxy rejects direct
Agent service, virtual-host, and ingress mutations with `managed_app_required`,
including requests from an older CLI. Logged-in users retain direct network
record management for operator-supervised machine services.

## Agent collaboration

The `treer` binary talks to the local agent server by default. Managed agents
receive its location in `PATH` and `TREER_BIN`, so they can discover and contact
peers without knowing the proxy address. `treer whoami` returns the current
workspace, agent, and machine records. `treer status` includes those current
agent and machine records under `self` so callers can distinguish themselves
from peers without matching names.

```bash
treer whoami
treer status
treer agent list
treer agent show reviewer
treer agent admin rename reviewer code-reviewer
treer machine rename self build-machine
treer machine delete srv_obsolete
treer agent attach reviewer
treer agent admin delete obsolete-helper
treer agent prompt reviewer "Review the parser changes" --wait --timeout 120000
treer agent read reviewer --lines 80
treer agent transcript reviewer --page 0
treer agent send-keys reviewer ctrl-c
```

An Agent may register a `treer.agent-interface/v1` server on its private
loopback. The Controller verifies its manifest and automatically sends semantic
prompts to it when `prompt.submit` is declared; otherwise prompt continues to
use terminal input. Structured transcript reads require `transcript.read` and return one
conversation turn per page.
When `--ui-path` is present, Treer embeds that path and transparently tunnels
its relative HTTP and WebSocket traffic to the same Interface port:

```bash
treer interface register --port 4180 --instance-id pi-1 \
  --capability prompt.submit --capability transcript.read \
  --capability state.observe --ui-path /
treer interface show
treer agent transcript self --page 0
treer interface clear
```

The Controller keeps a local, process-bound registration cache and revalidates
the Interface manifest before restoring it after a hot restart. Interface
processes register once at startup and should deduplicate prompts by
`operation_id`. Raw keys, terminal attach, stop, and delete remain PTY/Host
operations. The bundled Pi UI and single-Agent [Codex UI](apps/codex-ui/README.md)
perform this registration automatically. Launch-profile sidecars without a
bundled page live under `apps/codex-ais`, `apps/opencode-ais`, `apps/dsh-ais`,
`apps/claude-ais`, `apps/grok-ais`, and `apps/cursor-ais`. Built-in `--kind
codex` and `--kind claude` stay on the terminal path.

Agents can discover humans who belong to the workspace's organization. The
directory deliberately returns stable user IDs, preferred names, and roles
without exposing email addresses:

```bash
treer member list
```

### Durable Core Messages

Message data, recipient deliveries, policy, idempotency, acknowledgement, and
ordered context edges are Core capabilities. Managed Agents use the local CLI;
history reads do not acknowledge inbox delivery, and `receive` repeats a stable
delivery until `ack` succeeds:

```bash
treer message receive --wait 30000 --limit 50
treer message get <message-id>
treer message list --limit 50

printf '%s\n' 'Review is ready.' | \
  treer message send \
    --to coordinator \
    --idempotency-key review-42-ready \
    --body-file -

printf '%s\n' 'I addressed the comments.' | \
  treer message reply <message-id> --to sender --body-file -

treer message ack <delivery-id> --operation-id ack-review-42
```

Use stable recipient IDs or unique names and a sender-scoped idempotency key for
retryable sends. A reply creates an ordinary immutable Message whose first
`context_id` is the parent; contexts can have several ordered parents and form a
workspace-scoped DAG. An edge never grants access to an otherwise invisible
parent. Message policy actions are `message.send`, `message.read`,
`message.receive`, and `message.ack`; `message.import` is reserved for a local
operator running an explicit migration.

A Message does not wake an Agent. Send the durable Message first, then use
`treer agent prompt <agent> <message-id>` only when immediate attention is
needed. Prompting has its own stronger policy action, and the body should not be
copied into the terminal prompt.

### Workspace Apps

Apps are regular services with no special installer, manifest, broker, or
sandbox claim. Browser Apps authenticate with standard App OAuth and call
service-scoped public APIs. Apps running inside a managed Agent may use the
normal `treer` CLI and that Agent's Policy subject.

Mail uses App OAuth plus the App directory and Message APIs. Telegram uses the
official Bot API and runs as a dedicated managed Agent so its `treer message`
commands carry the bridge Agent identity. Start them with ordinary process
supervision:

```bash
TREER_APP_CONFIG=/etc/treer/mail.json \
TREER_APP_STATE_DIR=/var/lib/treer/apps/mail \
python3 apps/mail/mail.py

TREER_APP_CONFIG=/etc/treer/telegram.json \
TREER_APP_STATE_DIR=/var/lib/treer/apps/telegram \
TELEGRAM_BOT_TOKEN='<BotFather token>' \
python3 apps/telegram/telegram.py
```

See the [App contract](apps/README.md), [Mail guide](apps/mail/README.md), and
[Telegram guide](apps/telegram/README.md). The App process can use every
credential and operating-system capability available to it. Use a separate
account, container, VM, or stronger sandbox whenever its code is not trusted.

## Workload identity

A managed Agent can request a short-lived Proxy-signed Bearer token for a
registered machine service. The audience accepts a service ID or unique service
name and is canonicalized to the stable `service_id`:

```bash
TOKEN="$(treer token create api)"
curl -H "Authorization: Bearer $TOKEN" http://api.internal/
treer token create api --json
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
without stopping the agent.

Targets accept an agent id, a unique agent name, or `self`/`.` from inside a
managed agent. `prompt --wait` waits for observed activity followed by `idle`,
`blocked`, `exited`, or `failed` by default; repeat `--until STATUS` to select
other states. It is state-based coordination, not strict per-prompt turn
correlation.

To create a peer:

```bash
treer agent admin create --machine SERVER_ID --kind command --name shell -- /bin/sh
```

Print the agent skill bundled with the installed binary:

```bash
treer --skill
treer --skills  # accepted alias for the CLI contract
treer --skill install
treer --skill macos-container
```

This does not require a running proxy. The CLI contract is
`skills/treer/SKILL.md`. Recipe installs use `skills/treer-install/SKILL.md`.
Apple container Host setup on a Mac uses
`skills/treer-macos-container/SKILL.md`. They are embedded at build time so
the printed instructions match the binary.

Create an installer that receives the install skill as its first prompt:

```bash
treer agent admin create --machine SERVER_ID --kind auto --name installer \
  --recipe https://github.com/example/recipe.git
```

A successful install saves a launch profile (name and `run` from
`treer-agent.json`). Each created Agent is one thread. Extra conversations
use Launch to create another Agent. A recipe may reuse an already healthy
same-type app server, but each Agent must register its own AIS adapter with a
unique instance ID and thread binding. Do not run Install recipe again.

## Checks

```bash
just test-db-up
just check
```

The complete gate checks documentation links, release tooling, the control-plane
React build, updater tests, and the full Rust workspace. Workspace App and AIS
adapter tests are focused, opt-in checks; while iterating on messaging, use
`just app-test` and `just messaging-e2e`.

## License

Copyright 2026 Zijian Zhang.

Treer is licensed under the [Apache License 2.0](LICENSE). Vendored third-party
assets retain their own license terms; see the license files under `web/vendor`.
