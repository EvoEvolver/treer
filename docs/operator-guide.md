# Treer operator guide

Task-oriented instructions for running Treer: the control plane (Proxy,
Console, PostgreSQL, optional NATS) and the machines that enroll into it. For
how the pieces fit together read [Architecture](architecture.md); for trust
boundaries read the [security model](security.md).

To try Treer without hosting it, use the hosted Canary: Console
`https://app.canary.treer.ai/`, installer
`https://proxy.canary.treer.ai/install.sh`.

## Choose a deployment

| Deployment | Use it for | Details |
| --- | --- | --- |
| Docker Compose (GHCR images) | A self-hosted control plane with NATS and `/admin` image updates | [Self-host with Compose](#self-host-with-compose), [deploy/README.md](../deploy/README.md) |
| Single container | One self-built image with PostgreSQL, Proxy and Console | [deploy/README.md](../deploy/README.md#single-container) |
| Hosted SaaS (Railway + Cloudflare) | The official Canary/Production service | [Hosted deployment](#hosted-deployment), [Release process](releases.md) |
| Local development | Working on Treer itself | [Local development](#local-development) |

Machines, Controllers, Hosts, Agents and Apps always run separately on the
machines you enroll; none of these deployments runs them.

## Self-host with Compose

`compose.yaml` runs PostgreSQL 17, NATS (JetStream), the Proxy (`:8787`), the
Console (`:3000`) and the updater sidecar from
`ghcr.io/evoevolver/treer-{proxy,app,updater}`.

```bash
cp .env.example .env    # set ADMIN_PASSWORD, POSTGRES_PASSWORD, DATABASE_URL, TREER_UPDATER_TOKEN
docker compose pull
docker compose up -d
```

Set `TREER_PROXY_PUBLIC_URL` to the URL machines can reach and
`TREER_APP_PUBLIC_URL` to the exact Console origin; put TLS in front of both for
anything beyond loopback. `TREER_IMAGE_TAG` selects `stable` (default),
`canary`, or an immutable `vX.Y.Z` / `sha-<commit>`. Build from source instead
with `docker compose -f compose.yaml -f compose.dev.yaml up --build`.

Open `/admin` with `ADMIN_PASSWORD` to create the first user invitation, then
register an ordinary account and create a workspace. Image updates, tags and
rollback are in [Updates](#updates) and [deploy/README.md](../deploy/README.md).

### Proxy configuration

Every flag has an environment variable; `treer-proxy --help` lists them all.

| Variable | Purpose |
| --- | --- |
| `DATABASE_URL` | PostgreSQL (required; schema migrates at startup) |
| `PORT` / `TREER_PROXY_LISTEN` | Listen on `0.0.0.0:PORT` or an explicit address |
| `TREER_PROXY_PUBLIC_URL` | URL machines and browsers use for the API; embedded in install commands |
| `TREER_APP_PUBLIC_URL` | Exact Console origin; cookie writes must carry this `Origin` |
| `ADMIN_PASSWORD` | Platform administrator password for `/admin` |
| `TREER_INVITATION_REQUIRED` | Default `true`; `false` allows open registration |
| `GITHUB_OAUTH_CLIENT_ID`/`_SECRET`, `GOOGLE_OAUTH_CLIENT_ID`/`_SECRET` | Optional OAuth login (set both of a pair) |
| `CLOUDFLARE_API_TOKEN`, `CLOUDFLARE_ACCOUNT_ID`, `TREER_EMAIL_FROM` | Account email (welcome, password reset) and App email via Cloudflare Email Sending |
| `TREER_ENABLE_CORE_MESSAGES` | Enable Core Message routes (default `false`; Compose and single-container set `true`) |
| `TREER_NATS_URL` (+ `TREER_NATS_STREAM`, `..._SUBJECT_PREFIX`, `..._CLUSTER_SUBJECT_PREFIX`) | Required for more than one Proxy replica |
| `TREER_PROXY_INSTANCE_ID` | Unique replica ID (falls back to `RAILWAY_REPLICA_ID`, then generated) |
| `TREER_INGRESS_PUBLIC_URL` | Wildcard base for App, service and Agent origins, e.g. `https://apps.example.net/` |
| `TREER_INGRESS_TRUSTED_PROXIES`, `TREER_INGRESS_CLIENT_IP_HEADER` | Trusted front for client addresses on public ingress |
| `TREER_AGENT_EMBED_FALLBACK` | `tunnel` (default) or `refuse` when a browser blocks embed cookies |
| `TREER_PUBLIC_TCP_PORT_RANGE`, `TREER_PUBLIC_TCP_BIND_ADDRESS` | Optional anonymous raw TCP publication (max 1,024 ports) |
| `TREER_ARTIFACTS_DIR`, `TREER_RELEASE_ARTIFACT_BASE_URL` | Machine binaries served at `/artifacts`; missing platforms redirect to the release base |
| `TREER_APP_DIST_DIR` | Serve the Console build from the Proxy (single-container mode) |
| `TREER_AUDIT_RETENTION_DAYS` | Explicitly set audit retention (30..3650; stored in the database, initially 180) |
| `TREER_UPDATER_URL`, `TREER_UPDATER_TOKEN` | Compose updater sidecar for `/admin` image updates |
| `TREER_DISABLE_AUTH`, `TREER_ALLOW_UNAUTHENTICATED_LISTEN` | Development only; see below |

OAuth callbacks to register with the provider are
`https://<proxy-origin>/api/auth/oauth/github/callback` and
`.../google/callback`. Treer links only provider-verified emails.

### Multiple Proxy replicas

Configure NATS with JetStream, give each replica a unique instance ID, and
point all replicas at the same PostgreSQL. Load balancers need no sticky
sessions. Raw TCP ports, if used, must be routed consistently to the Proxy
fleet. Upgrade every replica before turning on a feature that a new Console or
Provider depends on; one replica's capability response is not a fleet-wide
guarantee.

### Origins and the browser

Keep the Console, the Proxy and the apps domain on distinct origins. The
Console and Proxy must share a registrable domain (session cookies are
`SameSite=Strict`), for example `console.example.com` and `api.example.com`.
Put `TREER_INGRESS_PUBLIC_URL` under a different registrable domain
(`apps.example.net`) so App and Agent JavaScript cannot touch Console cookies;
the Proxy logs `agent_origins_share_site` when it detects otherwise. A TLS
terminator in front of the Proxy must preserve `Host` and support WebSockets.

## Hosted deployment

The official service runs the Proxy on Railway (`Dockerfile.hosted`, with
PostgreSQL and NATS services) and the Console on Cloudflare Workers
(`web/wrangler.jsonc`). Railway's `PORT`, `RAILWAY_PUBLIC_DOMAIN` and
`RAILWAY_REPLICA_ID` are picked up automatically. Deploy with
`just deploy canary`, verify with `just test-canary`, then
`just deploy production`; see [Release process](releases.md).

On Railway leave `TREER_INGRESS_TRUSTED_PROXIES` and
`TREER_INGRESS_CLIENT_IP_HEADER` unset: Railway publishes no edge address
ranges, so Treer does not trust its client-address header. Per-client ingress
limits are then off (logged once at startup), per-service limits and the global
connection budget still apply, and `X-Treer-Client-IP` carries Railway's edge
address. Apps must not use it for per-client accounting there.

## Local development

```bash
just test-db-up                         # PostgreSQL 17 on 127.0.0.1:55432
export DATABASE_URL=postgres://treer:treer@127.0.0.1:55432/treer_test
export TREER_ENABLE_CORE_MESSAGES=true
just stage-artifacts                    # this platform's binaries into dist/<platform>
cargo run -p treer-proxy -- --disable-auth \
  --listen 127.0.0.1:8787 \
  --public-url http://127.0.0.1:8787 \
  --app-public-url http://127.0.0.1:5173
# second terminal
cd web && pnpm install && pnpm dev
```

`--disable-auth` skips login and trusts every caller, including machine and
admin routes. The Proxy refuses it on a non-loopback address unless
`--allow-unauthenticated-listen` is also set; use that only on an isolated
network. Use `--listen 0.0.0.0:8787 --public-url http://<lan-host>:8787` to
enroll other machines on a trusted LAN. Verification commands are in
[Quality](quality.md).

## Users, organizations and administration

- The **platform administrator** is not a user. `/admin` (password from
  `ADMIN_PASSWORD`) shows platform counts and aggregate traffic, creates and
  revokes single-use user invitations, and on Compose checks and applies image
  updates. It cannot see tenant inventories.
- A user registering from an admin invitation gets a personal organization
  (`<name> Personal`). With `TREER_INVITATION_REQUIRED=false` anyone can
  register and receives one.
- Organization owners/admins invite members from **Members**; the invitee
  previews the organization and explicitly chooses **Join**. An invitation has
  a role (only owners invite owners), an expiry (1–720 h, default 168), and
  optional groups and workspace grants applied on acceptance; it can be listed
  and revoked (`GET|POST /api/organizations/{org}/invitations`,
  `DELETE .../invitations/{id}`). With the Cloudflare email sender configured
  and an `email` given, Treer also emails the link.
- Members leave with `POST /api/organizations/{org}/leave`; an owner hands over
  with `POST .../transfer {"user_id"}` (caller becomes admin) and deletes an
  organization without workspaces with `DELETE /api/organizations/{org}
  {"confirm_name"}`. Every organization keeps at least one owner.
- A workspace with machines is deleted in one step with
  `DELETE /api/workspaces/{id} {"confirm_name","revoke_machines":true}`, which
  revokes every machine credential in the same transaction.
- Users list and end their sessions (`GET|DELETE /api/auth/sessions[/{id}]`)
  and change their password (`POST /api/auth/password`; other sessions end).
- Workspaces are visible to the whole organization by default; a workspace
  owner can restrict one to selected members and groups. Organization
  owners/admins are implicit owners of every workspace.
- Password reset links (when email is configured) expire after 30 minutes and
  revoke existing sessions.

**Sign-in lockout.** Password sign-in allows 10 attempts per account per 15
minutes across all replicas. If a sustained flood keeps an account locked,
block the traffic at your ingress, then run
`DATABASE_URL=... treer-proxy --reset-login-attempts` (clears all counters and
exits). Never use `--disable-auth` as recovery.

**Audit.** Workspace owners read `GET /api/workspaces/{ws}/audit-events`
(`limit`, `before`, `since`, `until`, `actor_id`, `action`, `resource_kind`, ...)
and the chart data at `.../audit-summary`; the Console shows both. Others need
an explicit enforce-mode allow for `workspace.audit.read`. Retention is shared
across replicas and changed only by an explicit `--audit-retention-days`;
deletion is irreversible without your own backup.

## Enroll machines

In the Console choose **Add machine**. It shows a reusable install command and
a separate single-use enrollment command (key valid ten minutes).

### Linux and macOS

```bash
curl -fsSL 'https://PROXY_HOST/install.sh' | sh
treer-agent-server connect --key 'enr_v1_...' --proxy 'https://PROXY_HOST/'
```

The installer puts `treer`, the `treer-launch` Agent launcher (plus its
deprecated `treer-agent` alias) and a `treer-agent-server` symlink in
`~/.local/bin` and the Host/Controller in `~/.local/libexec/treer`, and adds the bin directory
to your shell startup file (`TREER_INSTALL_MODIFY_PATH=0` to skip;
`TREER_INSTALL_DIR`, `TREER_AGENT_SERVER_INSTALL_DIR` to relocate). On the
official origins it requires Node.js 22+ and verifies the signed release
channel before replacing files. Self-hosted Proxies serve their own
`/artifacts` with SHA-256 checks, and withhold copyable commands on cleartext
non-loopback URLs. Linux needs a POSIX shell, coreutils, `hostname` and `ps`
(procps).

`connect` uses the current directory as the enrolled **root** (`--root` to
choose another). The root bounds machine exec, file transfer and relative
working directories. It asks for a machine name on first setup and explains
that the Host runs with your account's permissions. For automation:

```bash
treer-agent-server connect --key 'enr_v1_...' --proxy 'https://PROXY_HOST/' \
  --non-interactive --accept-risk --name build-machine
```

`connect` succeeds only when the Controller reports `proxy_connected`.
Reconnecting the same hostname into the same workspace with the same root and
Proxy reuses its machine ID and rotates the credential; running workloads are
untouched. Running the Console's connect command again also replaces a stale
local registration, so you never need `service uninstall` first: if the root
(`--root`) or Proxy differs, or the Proxy no longer accepts the old machine
identity (for example, the machine was deleted in the Console), the old Host
and Controller for that workspace stop and the new configuration is registered
and started, like a reinstall. Files under the old root are not moved or
deleted. When the old Host still runs workloads, `connect` says how many will
stop and asks once (`y/N`); non-interactive runs need `--replace`.

**Service modes** (`--service-mode`): `auto`/`nohup` (default; detached, survives
logout, not reboot), `systemd` (Linux user unit; enable linger for it to
survive logout), `launchd` (macOS LaunchAgent), `foreground` (diagnosis).
An Apple container machine is a Linux guest: follow `treer --skill
macos-container` and never reuse a Mac's `server_id` inside the guest.

### Windows

Windows 11 x64, run as your normal (standard or administrator) user account;
everything is per user, as on Unix. The Console offers the PowerShell form:

```powershell
Invoke-WebRequest https://PROXY_HOST/install.ps1 -OutFile "$env:TEMP\treer-install.ps1"
& "$env:TEMP\treer-install.ps1"
New-Item -ItemType Directory -Force -Path "$env:LOCALAPPDATA\Treer\workspace" | Out-Null
treer-agent-server connect --root "$env:LOCALAPPDATA\Treer\workspace" --key 'enr_v1_...' --proxy 'https://PROXY_HOST/'
```

If scripts are blocked, run the saved installer with
`powershell.exe -NoProfile -ExecutionPolicy Bypass -File "$env:TEMP\treer-install.ps1"`.
Binaries go to `%LOCALAPPDATA%\Treer\bin`; configuration to
`%LOCALAPPDATA%\Treer\agent-servers`, state to `%LOCALAPPDATA%\Treer\state`.
The Host runs detached as your user; nothing is registered to start at login,
so run `treer-agent-server restore --workspace WS` after a reboot. Use Windows
executables in launch profiles (the interactive fallback is `cmd.exe`) and
`curl.exe` rather than PowerShell's `curl` alias. To move an existing root,
rerun the connect command with the new `--root` and a fresh enrollment key; it
does not move files or rewrite saved paths.

### Managing a machine

```bash
treer-agent-server service status            # lists every local install
treer-agent-server service logs --follow
treer-agent-server service restart-controller # replace the Controller; workloads keep running
treer-agent-server service restart           # restart the Host; stops all workloads
treer-agent-server service stop | start | repair | uninstall
treer-agent-server --tui --workspace WS      # local dashboard
treer --version; treer-agent-server --version; treer-agent-host --version
```

With several installs on one host, add `--workspace WS` after `service`
(otherwise the command prints the table and exits 2). The `treer` CLI also
needs `treer --workspace WS ...` in that case. `service repair` rebuilds a
partial installation or switches service mode without a new key. Stopping or
restarting the Host from inside an Agent kills that Agent too; run it from a
host terminal.

One OS account is one trust domain: two workspaces enrolled under the same
user can read each other's credentials. Use separate OS users, containers or
VMs when that matters, and at least separate `--root` directories.

## Launch profiles and the launcher

A launch profile is a saved executable, argv and cwd. Treer runs exactly that
argv; it never wraps it in a shell or installs the program. Use
`sh -lc '...'` explicitly when you need shell syntax, and keep secrets out of
profiles (they are plaintext workspace configuration).

```bash
treer agent admin profile create reviewer --cwd . codex -- review --base main
treer agent admin profile list
treer agent admin profile launch reviewer --machine build-machine --name review-42 --cwd packages/api
treer agent admin create --machine build-machine --kind command --name helper --folder /research -- /bin/sh
```

New workspaces start with **Codex + UI** and **Claude + UI** (run
`treer-launch --harness codex|claude`) and **Codex (terminal)** /
**Claude (terminal)** (plain `codex`/`claude` in a PTY). The UI profiles default
to unattended full authority. Agent kinds are `shell` (the Console's built-in
Terminal), `command`, `bridge` and `app`.

### The Agent launcher

The UI profiles run `treer-launch`, which every Treer install puts next to
`treer` and every Controller update replaces. It needs no Node.js. Profiles
saved before the rename call `treer-agent`, a deprecated alias the install
also provides.

```bash
treer-launch --version
treer-launch update                          # match the installed Controller
treer-agent-server launcher install          # repair: reinstall from the signed release
treer-agent-server launcher status
```

If a UI profile reports `command_not_found`, run `treer-agent-server launcher
install` on that machine. Provider CLIs (`codex`, `claude`) and their logins
are yours to install; `CODEX_BIN` / `CLAUDE_BIN` select a provider outside
PATH. Details: [launcher README](../launchers/agent/README.md).

### Startup commands and repairs

```bash
treer agent startup set --cwd . -- sh scripts/start-agent.sh   # self: relaunch after a Host restart
treer agent startup show | clear
treer agent admin startup show /reviewer                       # another Agent (agent.startup.manage)
treer agent restart /reviewer --cwd repo -- codex resume THREAD_ID   # save and restart atomically
```

The Console's Agent menu has **Edit startup command** for the same operation.
Saving happens before the old process stops; a failed launch keeps the new
command for repair. App runtimes are changed with `treer app update` instead.

## Working with Agents

`treer` talks to the local Controller; inside a managed Agent it needs no
flags. Output is a table on a terminal and JSON when piped
(`treer --format json status` forces JSON).

```bash
treer whoami
treer status
treer agent ls -R /
treer agent mkdir -p /research/experiments
treer agent mv /reviewer /research/experiments/
treer agent show /research/experiments/reviewer
treer agent prompt /reviewer "Review the parser changes" --wait --timeout 120000
treer agent queue ls /reviewer
treer agent read /reviewer --lines 80
treer agent transcript /reviewer --page 0
treer agent attach /reviewer          # Ctrl-] detaches
treer agent send-keys /reviewer ctrl-c
treer agent start|stop|restart /reviewer
treer agent admin rename /reviewer code-reviewer
treer agent admin delete /obsolete-helper
treer exec --machine build-machine --cwd repo -- git status --short
treer machine put --machine build-machine ./report.pdf research/report.pdf
treer machine get --machine build-machine research/report.pdf -
treer member list
treer --skill            # the Controller's CLI contract for Agents
```

Targets are an ID, a Tree path, a sibling name, or `self`. Prompts queue by
default; `--steer` inserts into the running turn, `--notify` requests a
completion Message, and Agents without a queue-capable interface return
`queue_unsupported`. `exec` runs one argv (30-second default timeout, 64 KiB
per stream) with a minimal environment that carries no Treer identity. File
transfer paths are relative to the machine root shown by `treer machine show`;
symlinks and special files are refused, `--force` allows replacement, and the
default limit is 128 MiB (`TREER_MACHINE_FILE_MAX_BYTES` on the Controller, up
to 1 GiB).

Durable Messages:

```bash
printf '%s\n' 'Review is ready.' | treer message send --to /coordinator --idempotency-key review-42-ready
treer message receive --wait 30000 --limit 50
treer message reply MESSAGE_ID --to sender < reply.txt
treer message ack DELIVERY_ID --operation-id ack-review-42
```

A Message does not wake an Agent; send it, then prompt with the Message ID if
attention is needed. Bridges can pipe `{"body": ..., "external_source": ...}`
to `--stdin-envelope` to keep content out of argv.

## Apps

A Managed App is one supervised command with one HTTP UI port; Treer keeps its
identity, service and URL stable across restarts.

```bash
treer app create --machine build-machine --name docs --port 8080 python3 -- -m http.server 8080
treer app create --public --machine build-machine --name public-docs --port 8081 python3 -- -m http.server 8081
treer app list
treer app update /docs --cwd apps/docs/current --command python3 -- server.py
treer agent restart /docs
treer agent read /docs --lines 80
treer app delete /docs
```

With wildcard ingress each App gets `https://svc-<service id>.<apps domain>/`,
requiring workspace login unless created with `--public`; the App settings
view switches access later. Without wildcard ingress Apps are reachable through
the Console's authenticated tunnel. Apps receive `TREER_PROXY_URL` and a
60-second `X-Treer-Identity-Token` for the signed-in human on each request.
Treer does not install dependencies, store App secrets or migrate App data:
keep configuration in the App's private files.

`treer app update` keeps a stopped App stopped and restarts a running one once.
A failed start leaves the new fields saved; repair and restart, or update back.
Save `treer app show /docs` privately first if you may need to restore.

`scripts/app-deploy` (POSIX, Python 3.11+, Git) deploys an App from immutable,
hash-checked release directories with an SQLite backup and automatic rollback
when health checks fail (`init`, `stage`, `bootstrap`, `deploy`, `recover`;
see `python3 scripts/app-deploy --help`). It suits Apps whose single SQLite
database is their whole state.

### App email

Apps send email through the Proxy's Cloudflare sender (the variables above),
so the token never reaches an App. A Managed App runtime calls
`treer email send --to ADDRESS --subject TEXT --idempotency-key KEY` (body on
stdin or `--body`) or `POST /api/apps/{service_id}/email` with its service token
and `{to, subject, text, idempotency_key}`; ordinary Agents and humans get
`email_app_required`. Policy action `email.send` is evaluated for the App
runtime on its `machine.service`. `to` accepts any valid address. Instead of
`to`, `to_member` (`--to-member USER_ID`) names a workspace member and Core
sends to that member's verified account email; exactly one of the two is
required (`email_recipient_invalid`, 400). A non-member is `email_member_not_found`
(404), a member without a verified email `email_member_unverified` (409). The
sender, Reply-To and HTML are fixed by the Proxy. Each
workspace allows 20 attempts per App and 5 per recipient per hour, failures
included. Errors: `email_invalid` (400), `email_idempotency_conflict` (400),
`email_rate_limited` (429), `email_delivery_failed` (502),
`email_not_configured` (503), `email_in_progress` (503, retry after 60 s).
To resolve an uncertain send without resending, use `treer email status KEY`
or `GET /api/apps/{service_id}/email/{idempotency_key}`: `sent`, `failed` or
`pending` (reserved, outcome not recorded), or `email_not_found` (404) when this
App never used the key.
The token needs `Email Sending: Edit` and the sender domain must be onboarded
in that account.

Body-free history is in PostgreSQL `app_email_attempts` (`app_id`,
`recipient_hash`, `status`, `created_at`); it is not pruned automatically. Sender
failures log only a fixed `category` (`transport`, `timeout`, `http_status`,
`response_read`, `response_too_large`, `response_invalid`,
`provider_rejected`) and the upstream HTTP status, never addresses, bodies or
credentials. Reservation and provider dispatch are not one transaction: a
crash or ambiguous provider response can duplicate a later retry.

The in-repo Apps (Mail, Telegram, Issues, Gits, Policy and others) and the App
contract are documented in [apps/README.md](../apps/README.md).

## Networking

Treer forwards only explicitly registered endpoints. It does not capture
traffic, create network namespaces or change DNS.

```bash
treer service register --name api --protocol http --port 3000
treer service list
treer service forward SERVICE_ID --listen 127.0.0.1:8080   # from any machine in the workspace
curl http://api.treer/health                               # from a managed workload, via its HTTP proxy entry
treer service register --primary --protocol http --name reviewer-api --port 4180
curl "$(treer agent address /reviewer)/openapi.json"
TOKEN="$(treer token create api)"                          # 60-second Bearer for clients without the proxy entry
```

Managed workloads inherit `http_proxy`/`https_proxy`/`TREER_HTTP_PROXY` for
their own loopback entry. `NAME.treer` and `SERVICE_ID.treer` addresses get an
identity token injected; HTTPS and SSH work through CONNECT without
injection; other hosts are fetched directly. Programs that ignore proxy
variables need `service forward`. Registering a service does not supervise
the process; use a Managed App or a startup command for that. Services verify
tokens with `/.well-known/jwks.json` or `POST /.treer/identity/verify`.

### Public ingress

Point a wildcard DNS name and certificate at the Proxy and set
`TREER_INGRESS_PUBLIC_URL=https://apps.example.net/`. Then:

```bash
treer service publish SERVICE_ID                  # workspace login required
treer service publish SERVICE_ID --access public  # anonymous
treer service unpublish SERVICE_ID
```

Behind Cloudflare or a load balancer with published address ranges, name the
front so public ingress limits real clients:

```bash
TREER_INGRESS_TRUSTED_PROXIES='173.245.48.0/20,103.21.244.0/22'
TREER_INGRESS_CLIENT_IP_HEADER='CF-Connecting-IP'
```

Without them the TCP peer is the client. Peers on private, loopback or
link-local addresses are treated as fronts and bypass the per-client limits,
so do not expose a Proxy directly to untrusted clients on a private network.

Every Agent with an interface UI is also served at
`https://ag-<agent id hex>.<apps domain>/` (shown as `interface.origin_url` in
`treer agent show`). Opening it requires a Console login, membership and
`agent.output.read` plus `agent.prompt`. The Console embeds the same origin
with a partitioned-cookie handshake (Chrome/Edge 114+, Firefox 141+, Safari
26.2); other browsers fall back to the Proxy-origin tunnel unless
`TREER_AGENT_EMBED_FALLBACK=refuse`.

### Raw TCP

Set `TREER_PUBLIC_TCP_PORT_RANGE=20000-20100` (and optionally
`TREER_PUBLIC_TCP_BIND_ADDRESS`) on the Proxy, open the range in your firewall,
then `treer service publish SERVICE_ID --tcp --access public`. The service must
authenticate its own clients; Treer injects nothing.

## Policy

Without configuration a workspace runs **Treer Default**: monitor mode, allow
everything. To govern Agents:

1. In Workspace settings, install the bundled Policy App on an online machine
   (or select another private Managed App implementing the
   [Policy Provider protocol](policy-provider.md)).
2. Edit rules in the App, watch would-deny decisions in monitor mode, and use
   **Explain** (`/api/workspaces/{ws}/policy/explain`) to test them.
3. Switch to enforce once the rules are right.

Path rules (`/team/*`, `/infra/**`) match current Tree placement and are not
containment: restrict `agent.create`, App creation and `agent.tree.manage` for
the same subjects, and restrict machine exec/file access. Owners can always
change or remove the Provider, even when rules deny everything. Setup of the
Policy App and its administration token is in
[apps/policy/README.md](../apps/policy/README.md).

**Delegating administration to an Agent.** Agents never inherit org or
workspace administration from monitor mode, action defaults, Tree placement or
a fail-open Provider. Grant it with an explicit allow rule that names the Agent
(`id`, or a `group` whose principals list its ID) and lists the actions, e.g.:

```json
{"id": "ops-bot-admin", "priority": 100, "effect": "allow",
 "subjects": [{"kind": "agent", "id": "AGENT_ID"}],
 "actions": ["org.invite.manage", "org.member.manage", "workspace.access.manage"],
 "resources": [{}]}
```

The Agent then calls `$TREER_AGENT_SERVER_URL/api/org-admin/...` with its
workload credential ([Treer skill](../skills/treer/SKILL.md#administration-over-http)).
Workspace actions (`workspace.access.manage`, `machine.enroll`, renaming its
workspace, its own workspace's audit) need only that rule. Organization actions
(`org.*`, organization-wide `audit.read`, listing/creating/deleting workspaces)
also need an organization grant from an owner or admin: Console Organization
settings → Agent permissions, or
`PUT /api/organizations/{org}/agent-grants/{agent_id}` with
`{"workspace_id": ..., "actions": [...]}`. A grant stops working when its
creator is no longer an owner/admin. Agents never invite or assign
`owner`/`admin`, change roles, remove owners, delete or transfer the
organization, and are never granted `policy.provider.manage`. Rules with
administration actions must be published with a verified owner, even under
`provider_operator`. Every change is audited with the Agent as actor.

## Updates

### Control plane

- **Compose**: `/admin` → Check → Apply pulls the configured channel and
  recreates Proxy, App and updater. Roll back by setting `TREER_IMAGE_TAG` to a
  previous `vX.Y.Z` or `sha-<commit>` and running `docker compose up -d`.
- **Single container**: rebuild from a new commit and recreate the container on
  the same volume after a backup; see
  [deploy/README.md](../deploy/README.md#single-container-updates-and-backups).
- **Hosted**: `just deploy canary|production` ([Release process](releases.md)).

PostgreSQL schema changes are expand-only, so old and new Proxy replicas can
overlap during a rolling update. Never run an older Proxy against a database
migrated by a newer one without its matching backup.

Control-plane updates do not touch machines.

### Machine hardware metrics

The Console machine page shows current CPU, memory and NVIDIA GPU load with
15m/1h/6h/24h history. From a shell or an Agent:

```bash
treer machine metrics <machine> --window 15m
curl "$PROXY/api/workspaces/$WS/machines/$SERVER/metrics?window_seconds=900"
```

The Controller keeps this history in memory only (one hour of 5-second samples,
24 hours of minute means), so it restarts empty after a Controller restart.
GPU readings need `nvidia-smi` on the Controller's `PATH` (Linux, Windows);
other GPUs and macOS are not reported. Controllers built before this feature do not
advertise `machine.metrics.v1`; update them to see the panel.

### Machines

Update a machine from its Console page (enter a version such as `v0.2.1`), or
via the API with any principal allowed `machine.exec` on it:

```bash
curl -X POST "$PROXY/api/workspaces/$WS/machines/$SERVER/controller-update" \
  -H 'content-type: application/json' -d '{"version":"v0.2.1"}'
curl "$PROXY/api/workspaces/$WS/machines/$SERVER/controller-update"   # idle|downloading|installing|restarting|failed|done
```

or on the machine itself:

```bash
treer-agent-server update                       # stable channel
treer-agent-server update --version v0.2.1
treer-agent-server update --release-base-url https://releases.example/ --public-key /trusted/release.pub.pem
```

The Controller verifies the signed manifest and every binary, replaces
`treer-agent-server`, `treer`, `treer-agent-host` and `treer-launch` in place
(keeping one `<name>.old`), and restarts only the Controller; workloads keep running. The
new Host takes effect at the next `service restart`, which stops workloads. To
roll back, rename each `<name>.old` over `<name>` and run
`treer-agent-server service restart-controller`. Controllers that predate the
Console update (no `machine.controller-update.v2`) must be updated once
locally. Custom distributors need their own trusted key (`--public-key`); the
bundled key is used only for `https://releases.treer.ai/`. On machines without
the key, `node scripts/install-release-trust.mjs` installs it.

Running Agents keep the launcher image they started with; new and restarted
Agents use the updated `treer-launch`.

## Backup and restore

| What | Where | How |
| --- | --- | --- |
| Control plane | PostgreSQL | `docker compose exec postgres pg_dump -U treer -Fc treer > treer.dump` (keep it private: it holds credentials, signing keys, sessions, Messages and audit) |
| Single container | `/data` volume | Cold tar backup and restore into a new volume; see [deploy/README.md](../deploy/README.md#single-container-updates-and-backups) |
| NATS | JetStream volume | Not authoritative; rebuilt from PostgreSQL and live connections |
| Machine | Controller config and state directories (see [Architecture](architecture.md#data-stores)) | Back up with the service stopped; they contain the machine credential |
| Apps | Each App's state directory | Per App README; stop the App first |

Restore PostgreSQL and Controller state from the same point in time. If the
database is older than a Controller's saved App launch, reconciliation fails
with `stale_app_startup`; restore matching state or republish the App
specification (`treer app update`) until the Proxy revision is ahead. Do not
delete Controller state or recreate the App to bypass it.

## Troubleshooting

**Machine shows Offline.** Run `treer-agent-server service status` on it. If
the Host is running, `treer-agent-server service --workspace WS restart-controller`;
otherwise `service start` or `treer-agent-server restore --workspace WS`. After
sleep, a Proxy restart or a duplicate-connection fence the Controller
reconnects by itself.

**After a reboot.** `treer-agent-server restore --workspace WS` starts the Host
and relaunches intended-running Agents and Apps. With the Controller already
running use `treer runtime restore --all`. The report distinguishes started,
attached, stopped, revoked, failed and waiting workloads; a missing executable
or directory is reported, not retried forever.

**Partial installation.** `treer-agent-server service --workspace WS repair`
(optionally `--service-mode systemd|launchd|nohup`). A running Host is never
orphaned; if its current manager cannot stop it, repair tells you what to stop.

**Re-adding a deleted machine.** Run the new `connect` command, with a new
`--root` if you want one. The same installation is reactivated under its machine
ID; when the Proxy no longer accepts the old identity it enrolls a new machine,
and `connect` replaces the stale local service registration (keeping
`.bak-<timestamp>` copies of its config files when the machine ID changes; they
still hold the revoked token, delete them once the new machine works).

**`command_not_found` on launch.** Bare commands resolve only through absolute
`PATH` directories of the workload environment; relative commands such as
`./start.sh` are rejected. Use an absolute path or `sh start.sh`.

**Agent missing from the Console but maybe running.**
`treer --workspace WS agent admin local-inventory AG_ID` (operator terminal on
that machine) shows whether the local Controller knows the ID.

**Retired network mode.** A Controller saved with `transparent`, `proxy-env`
or `native-experimental` refuses to start. Register explicit services for what
relied on it, deactivate any installed network extension with its own
uninstaller, then
`TREER_NETWORK_MODE=plain treer-agent-server service --workspace WS repair`.

**Private state unavailable.** `TREER_AGENT_PRIVATE_STATE_DIR` is withheld when
the Controller cannot create or open `agent-private/<server_id>/<agent_id>` in
its state directory; the Controller log names the reason
(`agent_private_state_*`). Fix the directory (it must be writable by the
machine account) while the Controller is stopped.

**File transfer uncertain.** `machine_file_commit_uncertain` or a lost reply
means the file may be published. Inspect the destination before retrying;
never add `--force` blindly. Leftover `.treer-transfer-*` files can be removed
once no Controller holds them.

