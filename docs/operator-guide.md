# Treer operator guide

This is the detailed reference for installing, developing, deploying, and
operating Treer. Start with the root [README](../README.md) for the product
overview, then use the section matching the task at hand.

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
and GHCR tag rules are in [deploy/README.md](../deploy/README.md).

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
  python3 -- -m http.server 8080
treer app create --public --machine build-machine --name public-docs --port 8081 \
  python3 -- -m http.server 8081
treer app list
treer app restart docs
```

When wildcard ingress is configured, every Managed App receives a stable
`public_url` on its own origin. It requires Workspace authentication by default;
an Agent can explicitly pass `--public` at creation time to allow anonymous
internet access to that App. Browser assets, root-relative paths, redirects,
and App routes therefore resolve without the control plane's
`/virtual-hosts/.../proxy/` tunnel prefix. Public creation fails when wildcard
ingress is unavailable; private Apps continue to use the authenticated tunnel
as a fallback.

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

Linux nohup startup requires a POSIX shell, coreutils, hostname, and `ps`
from procps (Debian/Ubuntu: `apt-get install coreutils hostname procps`).
`connect` checks startup dependencies before consuming its enrollment key.
It does not require unshare or transparent-network tools.

The CLI discovers local Controller configurations by their saved workspace and
installation hostname. A single local installation needs no connection flags;
use `treer --workspace WORKSPACE_ID …` when several workspaces are installed.
Multiple matches require an explicit `--url` as well. Explicit Agent credentials
never fall back to an installed operator credential.

The separate connection command contains a 10-minute, single-use enrollment key:

```bash
treer-agent-server connect \
  --key 'enr_v1_...' \
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
treer-agent-server connect \
  --key 'enr_v1_...' \
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
Controller. From a managed Agent context it restarts only that Agent's
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
`connect` and `service repair` start the Host directly when its socket is
unavailable; an already-running Host gets a Controller-only restart, preserving
its agents and PTYs. A recoverable restart request failure is not printed as a
standalone `Error`; if recovery fails, the command reports the failure details.
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

An Agent can opt itself into recovery after a full Treer Host restart:

```bash
treer agent startup set --cwd . -- ./scripts/start-agent.sh
treer agent startup show
treer agent startup clear
```

The command is restored once when the Controller observes a new Host epoch. It
is not restarted after an ordinary process exit. `treer agent admin stop`
disables the registration. Agent deletion revokes recovery even if its machine
is offline; the machine may retain an inert local record for rolling-upgrade
compatibility. The Proxy must be reachable to authorize registration and to
validate the Agent before recovery. This does not start Treer after an
operating-system reboot; configure systemd, LaunchAgent, or the container init
system for the Host itself.

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
[the release process](releases.md) and [Canary runbook](canary.md).

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

Optional repository launchers package more involved runtimes behind the same
profile contract. The [ACP launcher](../launchers/acp/README.md) provides headless
Grok, Cursor, Codex, Claude, and OpenCode ACP profiles; its Remote Codex browser
adapter is a separate explicit install and profile. Treer's binary installer,
updater, and release artifacts do not fetch or install repository launchers.

The web Create Agent dialog includes a built-in Terminal option. It starts the
machine user's interactive shell without injecting a command and is not stored
as a launch profile.

Profiles store an executable and ordered arguments. The web editor accepts a
quoted command line, but the Controller launches exact argv without inserting
it into an interactive shell or guessing a provider. To load shell startup files
or perform shell expansion, explicitly use a shell in the profile, for example
`zsh -lc 'exec your-agent'`. Install runtimes and configure network restrictions,
proxy variables, containers, and credentials yourself. Profiles are plaintext
workspace configuration and must not contain secrets.

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
Profiles and command Agents execute exact argv. Terminal starts the user's
interactive shell. The thin `treer` client sends argv and input to this
Controller, which owns parsing, help, skills, and all command business logic.

## Workspace network

Treer forwards explicitly registered TCP/HTTP endpoints. It does not capture
outbound traffic, create network namespaces, or change OS DNS. Newly launched
managed Agents and Apps receive independent explicit HTTP proxy entries and
standard proxy environment variables. Default network mode is `plain`; this
does not enable transparent interception. Old saved `transparent`,
`proxy-env`, and `native-experimental` settings fail with a migration message.
After replacing dependencies on transparent routing, persist the mode with
`TREER_NETWORK_MODE=plain treer-agent-server service --workspace WORKSPACE_ID repair`,
then use the restore command below. Remove an installed native extension
using its normal OS deactivation/uninstall procedure; deleting source does not
deactivate a previously installed extension.

Start a process yourself or use a Managed App. An authenticated Agent registers
its own loopback endpoint; an operator selects `--machine` and may configure
another reachable target address. External containers must expose a reachable
port themselves.

```bash
treer service register --name api --protocol http --port 3000
treer service list
treer service forward SERVICE_ID --listen 127.0.0.1:8080
# In another terminal:
curl http://127.0.0.1:8080/
```

Forwarding binds only a client-side loopback listener and authenticates each
connection. Data and half-close pass through without HTTP interpretation. The
service record is durable, but a record alone does not supervise an externally
started process. Use a Managed App for a supervised HTTP service or a saved
Agent launch for another protocol. Deleting a service removes its publication,
not the external process. Removing an Agent or machine removes its owned records.

For HTTP publication, configure wildcard DNS and TLS once at the Proxy edge:

```bash
export TREER_INGRESS_PUBLIC_URL='https://apps.treer.ai/'
treer service publish SERVICE_ID
# Explicit anonymous access:
treer service publish SERVICE_ID --access public
treer service unpublish SERVICE_ID
```

The Proxy assigns `svc-<service-id>.<base-domain>`; no custom host or slug is
accepted for new publication. The default `workspace` access requires current
membership or a service-audience token. `public` accepts anonymous traffic, so
the application owns any authentication it needs. HTTP bodies, streaming, and
WebSocket upgrades remain supported. Treer consumes its gateway credentials and
strips client-supplied Treer identity headers before forwarding. Legacy alias
records can still be read; they no longer configure system DNS or traffic capture.

Authorization is a separate Proxy subsystem. A workspace owner can select a
private Managed App as its Policy Provider from Workspace settings. Without a
selected Provider, the UI shows the effective stored policy or the explicit
`Treer Default` monitor/allow baseline. Owners can choose an online machine and
install the bundled default Policy App without a source checkout or network
download on that machine. The App provides mode, JSON rule editing, atomic
publication, cache sync status, and a decision simulator; deployment and state
paths are documented in [`apps/policy`](../apps/policy/README.md). The Proxy
validates and caches its versioned bundle, then evaluates
subject/action/resource requests locally. Managed Agent requests carry their
authenticated Agent identity; workload HTTP entries use the same subject and
resolve service-name aliases to stable IDs before authorization.

Managed Apps allocate their service and platform origin automatically:

```bash
treer app create --name dashboard --port 3000 -- /path/to/dashboard
```

Use `--public` only to request anonymous HTTP access. Do not pass `--hostname`.
Apps retain their own configuration, secrets, data, and upgrade procedures.

Raw public TCP is separate from HTTP ingress. Configure a dedicated range on
the Proxy, expose that range through the deployment firewall, and publish
explicitly. There is no workspace-login protocol on an anonymous raw TCP socket:

```bash
# Proxy process configuration:
export TREER_PUBLIC_TCP_PORT_RANGE=20000-20100
export TREER_PUBLIC_TCP_BIND_ADDRESS=0.0.0.0
# Agent/operator command:
treer service publish SERVICE_ID --tcp --access public
treer service unpublish SERVICE_ID --tcp
```

The response returns the allocated endpoint. Allocation is stable while the
mapping exists; exhaustion or unavailable listeners is an explicit error.
Deleting the service or unpublishing its TCP mapping closes active traffic.
For deployments with multiple Proxy replicas, route the configured ports to the
Proxy listener fleet consistently and share the registry database.

`GET /api/workspaces/WORKSPACE_ID/traffic?hours=24` still exposes observed
Treer-relayed logical payload usage. Historical Direct/virtual-network rows
remain readable during retention. Ordinary internet egress is no longer observed
or reported by Treer, and the aggregate ledger is not invoice-grade accounting.

## Restart recovery

After a machine reboot, run the machine-side bootstrap binary, since the thin
CLI cannot contact a Controller that is down:

```bash
treer-agent-server restore --workspace WORKSPACE_ID
# With an already-running Controller:
treer runtime restore --all
```

If a machine restarts immediately after creating an App, recovery waits out the
15-second launch grace period and rechecks persisted intent without requiring
another machine event or a second manual command. An App stopped during that
wait remains stopped.

Recovery reuses the existing installation without re-enrollment. Ordinary
Agents save their actual launch snapshot before spawning. The Controller first
checks Host inventory, so a hot restart or repeated restore attaches surviving
processes. Missing intended-running processes restart under their stable Agent
ID after Proxy credential validation. Explicitly stopped/deleted/revoked items
stay stopped. Normal successful Agent exit disables recovery; failed launches
can be retried explicitly. Managed Apps are restored by their existing Proxy
reconciler, not a duplicate local Agent snapshot. Service registrations and
public mappings persist independently of process availability.

A restore report distinguishes attached, started, skipped stopped, revoked,
failed, and waiting endpoints. An unavailable executable, missing directory,
revoked credential, or offline Proxy is a visible failure. Recovery does not
reconstruct in-memory conversations, PTY history lost with the Host, application
data not saved by the application, or open TCP sessions. Optional systemd or
LaunchAgent startup remains an operator choice. Preserve the machine state
directory, credentials, and required runtime files.

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
treer exec --machine build-machine --cwd repo -- git status --short
treer exec --machine build-machine --cwd . --timeout 10000 -- sh -lc 'make test'
treer agent attach reviewer
treer agent admin delete obsolete-helper
treer agent prompt reviewer "Review the parser changes" --wait --timeout 120000
treer agent read reviewer --lines 80
treer agent transcript reviewer --page 0
treer agent send-keys reviewer ctrl-c
```

For a provider that runs outside the Host, launch a command-owned bridge:

```bash
treer agent admin create --machine build-machine --kind bridge \
  --name provider-bridge --cwd . -- ./bridge --serve
```

The bridge receives other Agents' prompts from its authenticated local
Controller endpoint:

```text
GET /api/agents/{bridge-agent-id}/prompt-queue
```

The endpoint consumes queued prompts and returns JSON. Bridge output is not
available through `treer agent read` or `treer agent transcript`; the provider
owns that transcript. The queue is bounded and in-memory, so a bridge that
needs restart-safe delivery should use Core Messages and explicit
acknowledgements as its durable handoff.

`treer exec` runs one non-interactive argv vector directly on the selected
machine and returns JSON containing stdout, stderr, exit code, duration,
timeout, and truncation state. Its working directory is relative to the Host
root. It defaults to a 30-second timeout, caps each output stream at 64 KiB, and
does not load shell syntax unless the executable is an explicit shell.

The machine details page also uploads one file of up to 16 MiB into an existing
directory beneath the Host root. Proxy sends the content to the Controller in
bounded chunks and the Controller stages it in the destination directory before
renaming it into place. Replacing an existing file requires an explicit UI
choice.

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
operations. The bundled Pi UI and single-Agent [Codex UI](../apps/codex-ui/README.md)
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
    --idempotency-key review-42-ready

printf '%s\n' 'I addressed the comments.' | \
  treer message reply <message-id> --to sender

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

See the [App contract](../apps/README.md), [Mail guide](../apps/mail/README.md), and
[Telegram guide](../apps/telegram/README.md). The App process can use every
credential and operating-system capability available to it. Use a separate
account, container, VM, or stronger sandbox whenever its code is not trusted.

## Workload identity

New managed Agents and Apps automatically inherit a dedicated `http_proxy` /
`https_proxy` entry. Use the service name or stable ID from `treer service list`
as a proxy-only address; ordinary HTTP clients such as curl and Python urllib
need no token code:

```bash
treer service register --name paper --protocol http --port 3000
curl http://paper.treer/path
# Stable-ID addresses also work:
curl http://svc_YOUR_SERVICE_ID.treer/path
python3 -c 'import urllib.request; print(urllib.request.urlopen("http://svc_YOUR_SERVICE_ID.treer/path").read())'
```

Names are unique within the workspace and resolved case-insensitively for each
new request, so renaming immediately changes the name address; ID addresses keep
working. Name addresses accept 1-63 ASCII letters/digits and internal hyphens.
Use ID addresses for display names with spaces, Unicode, dots, or underscores.
Replace the entire `svc_YOUR_SERVICE_ID` with the actual service ID. The proxy
signs each request with that entry's Agent identity and the service audience.
The destination receives `X-Treer-Identity-Token`, plus Bearer Authorization
when the caller has not supplied its own application Authorization. It can
verify the token with the endpoint below. Tokens are not injected into unrelated
internet requests. Ports in `.treer` URLs do not override the registered endpoint.

Loopback is in `no_proxy` by default; inherited bypass entries are retained.
CONNECT also accepts `NAME.treer`, with the same workspace-scoped resolution.
Clients that ignore proxy environment variables must explicitly use
`TREER_HTTP_PROXY` or an explicit `treer service forward`. HTTPS CONNECT and
CONNECT to registered TCP services work without TLS interception; they do not
receive an injected HTTP token. For SSH, an installed HTTP CONNECT client may be
used as `ProxyCommand`, or forward the registered SSH service to a local port
and run ordinary SSH there. SSH keys and host-key checking remain SSH's job.

Hot Controller restart reuses the entry port for surviving processes. Cold
restore recreates entries and injects their addresses into restarted processes.
Stop/exit closes the entry and its connections. Older already-running processes
need relaunching to receive the new environment. These loopback entries are
shared-machine conveniences, not access boundaries against other local code.

A managed Agent can also request a short-lived Proxy-signed Bearer token for a
registered machine service. The audience accepts a service ID or unique service
name and is canonicalized to the stable `service_id`:

```bash
TOKEN="$(treer token create api)"
curl -H "Authorization: Bearer $TOKEN" http://127.0.0.1:8080/
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
wrong-audience token. Manual tokens are useful for clients that do not use the
managed HTTP entry. The proxy handles token acquisition for cooperating HTTP
clients; receiving services still verify identity if their application needs it.

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

Print the agent skill served by the connected Controller:

```bash
treer --skill
treer --skills  # accepted alias for the CLI contract
treer --skill install
treer --skill macos-container
```

Help and skills require a reachable authenticated Controller; the thin client
contains no product command parser or embedded skill copy. The Controller embeds
`skills/treer/SKILL.md`, `skills/treer-install/SKILL.md`, and
`skills/treer-macos-container/SKILL.md`. For a stopped Controller, use
`treer-agent-server --help` and its restore/service commands directly.

Core no longer selects an installer provider or interprets `--recipe`. Install
and configure your runtime separately, save a launch profile, then send installer
instructions through the selected runtime's interface. The install skill remains
available as documentation for user-owned installers.

## Checks

During ordinary development, select the focused check for the changed area;
PostgreSQL-backed checks may be omitted for unrelated changes. For an on-demand
complete gate or release preparation, run:

```bash
just test-db-up
export TREER_TEST_DATABASE_URL=postgres://treer:treer@127.0.0.1:55432/treer_test
just check
```

The complete gate checks documentation links, release tooling, the control-plane
React build, updater tests, and the full Rust workspace. Workspace App and AIS
adapter tests are focused, opt-in checks; while iterating on messaging, use
`just app-test` and `just messaging-e2e`.
