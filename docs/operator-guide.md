# Treer operator guide

The web console's **Add machine** Windows connection command creates
`%LOCALAPPDATA%\Treer\workspace` and explicitly passes it as `connect --root`.
Run it for a new enrollment only. It does not migrate an existing enrollment;
an existing root mismatch is rejected and requires the explicit, offline root
migration procedure below. Remote cleartext HTTP installer commands remain
withheld.

This is the detailed reference for installing, developing, deploying, and
operating Treer. Start with the root [README](../README.md) for the product
overview, then use the section matching the task at hand.

To try Treer without hosting it, use the canary: console
`https://app.canary.treer.ai/`, installer `https://proxy.canary.treer.ai/install.sh`.
Everything below is for running your own control plane.

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

Browser control-plane writes require an exact `Origin` matching
`TREER_APP_PUBLIC_URL`; absent/null/foreign origins fail with `csrf_denied`.
Use a trusted Console build with the Proxy update: OAuth login starts through
JSON POST with `X-Treer-Console: 1`, and admin update checks use POST. Legacy
OAuth start GET returns 405 without creating login state. Native password
clients use `X-Treer-Client` only on pre-authentication requests without browser
cookies or Fetch Metadata. Authenticated native/operator clients use Bearer
credentials; adding Authorization never exempts cookie-only admin routes.
See [control-plane CSRF boundaries](security.md#control-plane-browser-csrf),
including the distinct App OAuth and ingress navigation exceptions.

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
  --allow-unauthenticated-listen \
  --listen 0.0.0.0:8787 \
  --public-url http://PROXY_HOST:8787 \
  --app-public-url http://127.0.0.1:5173

# In another terminal:
cd web
pnpm dev
```

`--disable-auth` is intended for local testing. It skips the login screen and
uses a synthetic local user, and every caller (including machine and
administrator routes) is trusted. The Proxy refuses it on a non-loopback
address unless `--allow-unauthenticated-listen` (`TREER_ALLOW_UNAUTHENTICATED_LISTEN`)
is also given; use that only on an isolated network. A loopback listener can still be
exposed by a reverse proxy or port forward, so this guard is not complete remote isolation.
Omit it and set `ADMIN_PASSWORD` for shared or
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
treer agent restart /docs
treer agent read /docs --lines 80
```

### Deploy new Apps from version directories (POSIX)

`scripts/app-deploy` is an explicit, local App-layer tool for macOS/Linux with
Python 3.11+ and Git. It does not update Core, enroll machines, create Apps, or
control remote files. Run it on the App's machine using an authorized Treer CLI
identity. Its absolute physical deployment root must be inside the enrolled
machine root. Windows is explicitly unsupported by this symlink-based tool.
Existing Apps need the Core App-update capability before migrating their saved
command to this entry point; do not install a shim in a live source checkout.

For a **new** App, initialize and stage before creating the Managed App:

```bash
# Use physical absolute paths and an independently reviewed full commit SHA.
python3 scripts/app-deploy init --root /machine/apps-deployed/example \
  --machine-root /machine --command /usr/bin/python3 --entry server.py --db-env EXAMPLE_DB
python3 scripts/app-deploy stage --root /machine/apps-deployed/example \
  --repo /source/treer --commit FULL_40_CHARACTER_SHA --component apps/example \
  --check-json '["/usr/bin/python3","-m","unittest","discover"]'
python3 scripts/app-deploy bootstrap --root /machine/apps-deployed/example \
  --release FULL_40_CHARACTER_SHA
# init prints the exact command/args/cwd for treer app create --machine ... .
# Use its Python executable and entry.py, not a mutable checkout or current path.
```

The stable, private `entry.py run --root ROOT` loads `config.json`, resolves
`current` once to `releases/<SHA>`, verifies its file hashes and executes the
configured interpreter with an absolute release script and physical cwd. Put all
runtime writes outside the release. The selected database environment variable
always points to `ROOT/state/app.sqlite3`; Python bytecode writing is disabled,
and the child starts with umask 077. Normal workload identity remains intact.
Initialization validates executable targets but preserves the absolute interpreter
and launcher paths supplied by the caller, including symlink/shim paths. A shim
can therefore follow interpreter updates without pinning an obsolete patch-version
directory; maintaining that executable path remains the operator's responsibility.
The interpreter is outside the release manifest's hash protection: changing a
shim's target changes the interpreter that runs with the App's credentials. Keep
the shim and its targets in directories writable only by trusted operators, and
keep the initialization interpreter (including any virtual environment) available
for future App starts. File permissions do not isolate Agents sharing an OS user.
The entry is a frozen copy of the deployment tool at initialization. Updating
`scripts/app-deploy` does not update installed entries; reinitializing with a
different entry digest fails closed. To upgrade the entry, stage a separately
reviewed deployment root and explicitly migrate the stopped App's saved command
with Core App-update, preserving its stable identity. Do not overwrite a live
entry in place.
Apps must expose a deployment-health JSON response on loopback **and** through
their real Treer service route, containing `version` (from `APP_DEPLOY_VERSION`),
`app_id` (the stable App ID), `ready: true`, and `database_ready: true`. Return
readiness only after initialization/migrations complete and the database works.
Serve deployment health without caches (`Cache-Control: no-store`).
No HTTP redirects, URL credentials, query secrets, or oversized health bodies
are accepted. An HTTP 200 alone is insufficient. The service-route probe uses
the deployer's ordinary HTTP proxy identity; it does not mint an owner token.

Stage the next full commit with the component's complete tests/build as
`--check-json` (an absolute executable and argv; no shell), then deploy explicitly:

```bash
python3 scripts/app-deploy deploy --root /machine/apps-deployed/example \
  --treer /absolute/path/to/treer --app STABLE_APP_ID \
  --release NEXT_FULL_SHA --operation-id deploy-unique-id \
  --health-url http://127.0.0.1:8080/deployment-health \
  --route-url http://example.treer/deployment-health --timeout 20
# After a crash, inspect the private receipt, then explicitly recover:
python3 scripts/app-deploy recover --root /machine/apps-deployed/example \
  --treer /absolute/path/to/treer --app STABLE_APP_ID --operation-id deploy-unique-id
```

The machine/App-specific private root has a nonblocking `flock`. Deployment
checks the local machine and exact saved entry command, confirms old-version
health, binds the private root to that App's stable identity, persists a receipt,
stops that App, makes an SQLite backup with the
SQLite backup API (including WAL, with a 120-second backup budget) and
`integrity_check`, switches `previous` and
`current` atomically, starts the same App, and verifies both health surfaces.
Other Agents, Host and Controller are never restarted. A stopped App is rejected
before deployment; the tool does not change its stopped intent. Replaying a
completed operation verifies health without restarting again.

Failure stops the candidate before restoring the old SQLite snapshot and
version, then restarts and verifies the old App. Restoration removes WAL/SHM
sidecars and syncs their directory before publishing the old main database, so
an interruption cannot replay the candidate's WAL against the restored database.
This deliberately discards all
candidate writes since the backup: use only for Apps whose single SQLite
database is the complete rollback state, with no other writers. External side
effects, other files/databases, queues and remote migrations are not rolled back.
Any external writer must be quiesced separately; stopping this App is not a
distributed write lock. This is maintenance downtime, not zero-downtime delivery.
`rollback_failed` and incomplete receipts block new deployments. A crash requires
explicit `recover`; it never guesses success or silently reruns a migration.

The exported git component ignores dirty checkout changes and rejects archive
links/special files; staged checks run without inherited Treer/UI credentials.
Hashes detect accidental asset corruption, not a malicious same-account writer
or an unauthenticated release origin. Review the source and check command before
staging: they execute with the local OS account's authority. Versions are retained
for rollback; do not edit/prune any directory still used by a process. Backup,
config and receipt directories are 0700, files 0600 (executable assets 0700).
Backups contain complete business data and have no automatic retention or
garbage collection. Monitor disk space and set an operator-owned retention
policy; never delete a backup referenced by an incomplete recovery receipt.
Receipts omit argv, tokens and application content; they contain paths, App
identity, versions, health URLs and backup hashes. Same-account processes can
read/change these files; use separate OS accounts/containers for isolation.

Change an existing App's launch fields without recreating its identity or routes:

```bash
treer app update /docs --cwd apps-deployed/docs/releases/COMMIT --command python3 -- server.py
treer app update /docs --cwd apps-deployed/docs/current
treer app update /docs --clear-args
```

Omitted fields are preserved, including arguments when only `--command` changes.
Arguments after `--` replace the whole argument array; `--clear-args` empties it.
The directory is relative to the enrolled machine root and must exist inside it.
Running Apps restart once; stopped Apps keep their stopped intent. App/runtime
IDs, credentials, service/virtual host and public origin stay fixed. Startup
management and ordinary restart Policy permissions are required. Older
Controllers/Proxies return `app_update_unsupported` during CLI/local preflight
(or `unsupported_command` naming `app.update.v1` for a remote target Controller).
Direct API calls to older Proxies return a missing-route error. Upgrade rather
than falling back to delete/create.
If launch fails after saving, the new fields remain saved: repair and start or
restart, or explicitly update back to the old specification. Treer does not
roll back application data. Put environment/secrets in private entry configuration,
never command arguments; this update API does not accept environment fields.

Before deploying, save the previous launch fields in private storage so the
deployment script can explicitly restore them if health checks fail. For example,
`(umask 077; treer app show /docs > previous-app.json)` saves a local snapshot;
keep it private, since existing arguments may contain sensitive information.
Restoring launch fields does not restore application data or its schema.

Coordinate Proxy database backups with Controller startup state. If an older
database backup has a lower startup revision than the Controller, reconciliation
fails closed with `stale_app_startup`; do not delete Controller state or recreate
the App to bypass it. Restore matching state, or explicitly publish the desired
specification until its Proxy revision exceeds the retained Controller revision.
A single update is not sufficient when the revision gap is larger than one.
Update preflight and application each have a 35-second command-response timeout;
the App lifecycle lock remains held during those requests. A timeout after saving
does not prove that the Controller did not apply the new specification.

Workspace ingress browser login requires cookies on the App hostname. On
HTTPS, the gateway uses short-lived host-only handshake cookies and clears them
after callback; it retains up to eight concurrent flows per hostname. If a
callback is expired, evicted, replayed, or arrives without its browser cookie,
start login again from the App URL. During mixed old/new Proxy rollout, new
logins fail closed until the pool supports browser-bound codes; existing ingress
sessions are unchanged. HTTP development does not provide the HTTPS cookie
planting protection described in the security model.

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

With wildcard ingress, every Agent that registers an interface UI is also
reachable standalone at `https://ag-<id>.<apps domain>/` (`ag-` followed by the
32 hex digits of its Agent ID). Opening it sends the browser through the Proxy
authorize step: a Console login, current workspace membership and Policy
`agent.output.read` plus `agent.prompt` on that Agent are required, after which
the origin receives its own 12-hour ingress session. The URL appears as
`interface.origin_url` on `treer agent show` and in the Console for members who
pass that gate; Bearer tokens are refused on these origins. Put the apps
domain under a registrable domain separate from the Console and Proxy hosts
(for example `treer-apps.example` next to `console.example.com`); when the
Proxy sees them sharing one it logs `agent_origins_share_site` at startup. That
check compares the last two labels only, so a public-suffix host such as
`example.co.uk` may be reported or missed either way; the recommendation stands
regardless. Deleting the Agent deletes its origin and sessions; Console logout
ends the user's ingress sessions. With authentication disabled (development
only) the Agent origin path skips the interface gate like every other route, so
never combine that mode with an apps domain that others can reach.

Apps receive `TREER_PROXY_URL` from the Controller's configured Proxy base URL
(HTTP(S) origin with URL credentials, query and fragment removed), overriding
an inherited value. This is public routing configuration, not an identity token.

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

The installed Controller updater consumes these signed channels and manifests.
Before its first update, provision the publisher's SPKI Ed25519 public key from
a trusted source at `~/.config/treer/release-signing-key.pub.pem` (under
`XDG_CONFIG_HOME` when set), or pass `--public-key` / `TREER_RELEASE_PUBLIC_KEY`.
It never downloads its trust key from the distributor. No release key is bundled
in the repository, and missing keys or unsigned releases fail closed.
Publishing an R2 release does not automatically update running machines;
operators invoke `treer-agent-server update` separately.

Open the web UI, select a workspace, and choose **Add machine**. The dialog
separates installation from workspace enrollment. The installation command is
public and reusable:

```bash
curl -fsSL 'https://PROXY_HOST/install.sh' | sh
```

The script detects the target platform, installs `treer` to `~/.local/bin`, and
puts the Host and Controller binaries in `~/.local/libexec/treer`. It also exposes
`treer-agent-server` in `~/.local/bin` as a symlink to the Controller, so both
commands are available from the same PATH entry. The installer automatically adds
the installation directory to the selected shell's startup files: `.zprofile`
and `.zshrc` for zsh (respecting `ZDOTDIR`), `.bashrc` and the existing login file
for bash, or `.profile` for POSIX shells. It preserves existing content and does
not duplicate its PATH entry on reinstall. Open a new terminal afterward, or run
the printed `export PATH=...` command in the current shell. Set
`TREER_INSTALL_MODIFY_PATH=0` to leave shell startup files untouched; other shells
receive manual PATH instructions. The script does not contain a
credential, create workspace configuration, register a service, or start a
process. Override `TREER_INSTALL_DIR` or `TREER_AGENT_SERVER_INSTALL_DIR` when
needed.

Linux nohup startup requires a POSIX shell, coreutils, hostname, and `ps`
from procps (Debian/Ubuntu: `apt-get install coreutils hostname procps`).
`connect` checks startup dependencies before consuming its enrollment key.
It does not require unshare or transparent-network tools.

### Native Windows

The initial native target is Windows 11 x64 with PowerShell and ConPTY. Download
the PowerShell installer from the Proxy and run it in the current user session.
The web Add machine dialog also offers **Windows · PowerShell** commands:

```powershell
Invoke-WebRequest https://PROXY_HOST/install.ps1 -OutFile "$env:TEMP\treer-install.ps1"
& "$env:TEMP\treer-install.ps1"
New-Item -ItemType Directory -Force -Path "$env:LOCALAPPDATA\Treer\workspace" | Out-Null
treer-agent-server connect --root "$env:LOCALAPPDATA\Treer\workspace" --key 'enr_v1_...' --proxy 'https://PROXY_HOST/'
```

The installer verifies SHA-256 and matching build identities before installing
`treer.exe`, `treer-agent-host.exe`, and `treer-agent-server.exe` together under
`%LOCALAPPDATA%\Treer\bin`. It updates the current session and user PATH. Other
already-open shells must be reopened. Treer on Windows assumes an account in
the Administrators group: the installer, `connect`, `restore`, service
start/repair and the Controller process itself check the account first and
refuse other accounts with the fixed code `windows_requires_administrator`
before anything is written or started (UAC elevation is not required; both the
elevated and the UAC-limited administrator token pass). `service status`
prints the same code under a non-administrator account so the refusal is
visible locally; an installation that an earlier release accepted under a
standard account is refused at Controller startup after upgrading. The hosted
Windows release job proves the refusal natively with
`scripts/windows-standard-user-check.ps1`, which creates a temporary standard
user on a disposable GitHub-hosted runner only (it refuses to run unless
`TREER_DISPOSABLE_RUNNER=1`, `GITHUB_ACTIONS=true` and
`RUNNER_ENVIRONMENT=github-hosted`), stages the installer and binary in a
directory readable by Users, runs the installer, `service status` and
`connect` as that user with its profile loaded, and asserts the fixed code
for all three, a non-zero exit for the installer and `connect`, and no
`%LOCALAPPDATA%\Treer` in that user's profile. Installation
does not enroll the machine. For a new enrollment, create the user-owned
workspace directory and pass `--root` explicitly as shown above; this avoids
accidentally using a shell's current directory such as `C:\Windows\System32`.
Choose another user-owned directory with `--root` if desired. Existing enrollments
retain their configured root, including legacy System32 roots; reinstalling the
binaries does not migrate, reconnect, or restart them. A session that blocks
unsigned scripts can run the saved installer
with `powershell.exe -NoProfile -ExecutionPolicy Bypass -File "$env:TEMP\treer-install.ps1"`;
then open a fresh terminal to pick up PATH. No global execution-policy change is needed.

Windows `auto` selects `detached`: a background user Host independent of the
installing terminal. Configuration lives in `%LOCALAPPDATA%\Treer\agent-servers`,
state and logs in `%LOCALAPPDATA%\Treer\state`; `TREER_STATE_DIR` still overrides
state. Local IPC uses a current-user Named Pipe, not a filesystem socket.
`--service-mode foreground` remains available for diagnosis. No login task or
system service is registered, and the Host does not automatically restart after
a crash or reboot. Restore saved workload intent with:

```powershell
treer-agent-server restore --workspace WORKSPACE_ID
```

Controller replacement preserves live Agent processes; stopping the Host cleans
up its process tree. Recovery restarts saved commands, preserving workload
identity and explicit stop intent, rather than restoring process memory.
Install Agent/App dependencies yourself and use Windows executable/argv launch
profiles. The interactive fallback is `%ComSpec%` (`cmd.exe`). Unix-specific
recipes are not translated automatically. Use `curl.exe` for HTTP examples to
avoid Windows PowerShell's `curl` alias; injected proxy identity works as on Unix.

In-place `update` is deliberately unavailable on Windows. Stop each installed
workspace with `treer-agent-server stop --workspace WORKSPACE_ID`, reinstall,
then run `restore`. The installer refuses to replace live binaries in its target
directory. The implementation has cross-compilation coverage; native Windows
runtime acceptance is a separate release gate (see [quality](quality.md)).

The CLI discovers local Controller configurations by their saved workspace and
installation hostname. A single local installation needs no connection flags;
use `treer --workspace WORKSPACE_ID …` when several workspaces are installed.
Multiple matches require an explicit `--url` as well. Explicit Agent credentials
never fall back to an installed operator credential.

An environment-bound Agent/operator identity cannot be switched by flags:
explicit `--workspace` must match `TREER_WORKSPACE_ID`, and explicit `--url`
must match its bound `TREER_AGENT_SERVER_URL`. A conflict fails before sending
credentials. Use an operator terminal configured with the destination
workspace's own credentials instead; flag-free managed CLI behavior is unchanged.
The guard applies only to managed identities or bound operators; a pure operator
with no workspace/URL binding can use explicit `--workspace`/`--url` normally.
An independent Agent client with neither binding may also explicitly select its
connection; its credentials are sent to the explicit URL, which it must trust.
If either binding exists, explicitly overriding a missing counterpart is rejected.

**One OS account = one trust domain.** Same-account workspaces/Controllers are
mutually trusted: A's exec/Agent can read and impersonate B's local identities.
0700/0600 permissions (or current-user Windows ACLs) do not isolate the same
user. Use independent OS users or containers when isolation is required. For
trusted multi-workspace use, pass different `connect --root` directories to
avoid shared launcher assets. Connect emits a non-blocking shared-account
warning based only on other enrollment filenames, without reading their contents.
New enrollment checks an explicit `--listen` for an occupied port before
exchanging the key or saving enrollment identity/configuration; release the port
or choose another and retry the same command/key. This is a best-effort preflight:
it does not reserve the port, so a concurrent bind after the check can still
make activation fail closed (the check/bind TOCTOU is not eliminated).

The separate connection command contains a 10-minute, single-use enrollment key:

```bash
treer-agent-server connect \
  --key 'enr_v1_...' \
  --proxy 'https://PROXY_HOST/'
```

`connect` decodes the workspace ID from the key, uses the current directory as
the workspace root, registers the Host service, and starts it. Before exchanging
the single-use key, it validates local paths and binaries. On Linux and macOS,
`auto` starts the Host detached with `nohup` in its own process group, records
its PID and process start identity, and writes output under the Treer state
directory. The separate process group keeps the Host and its workloads alive
when the command that started it is reaped, for example when a launchd job or
CI step that ran `service restart` exits. This mode
deliberately provides no start-at-boot or Host crash restart. Select a native
user service explicitly with `--service-mode systemd` on Linux or
`--service-mode launchd` on macOS; attached diagnostics remain available with
`--service-mode foreground`.

Host and Controller launch with a cleaned environment, including launches from
managed Agents, service managers, CI, foreground mode and updater hot restarts.
Treer workload identity variables, `TREER_BIN`, `AGENT_UI_*`, `CODEX_UI_*` and
loopback HTTP/HTTPS/ALL proxy variables (both cases) are removed. Ordinary
account, PATH, locale, temporary-directory and install/state configuration
variables remain; external user proxies remain available to ordinary tools.
Controller control-plane HTTP requests connect directly to the configured
Proxy, as its WebSocket already does. They do not use environment proxies.
Startup validation has a request timeout; transient failures are logged and
retried on the connected heartbeat until recovery succeeds.

`service stop` and `service restart` called inside a managed Agent warn that
stopping the Host terminates that Agent too. Use a host terminal or a detached
process session when the command must finish after its caller exits. The
warning does not prevent execution. Direct diagnostic Controller invocations
must pass identity/configuration with CLI flags or use `run --config`; daemon
startup strips inherited identity credentials before parsing its configuration.

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

### Controller hot updates

Pull and hot-activate the latest Controller and agent-facing CLI with one command:

```bash
treer-agent-server update
treer-agent-server update --channel canary --public-key /trusted/release-key.pub.pem
# Self-hosted signed distributor:
treer-agent-server update --release-base-url https://releases.example/ --public-key /trusted/release-key.pub.pem
```

`update` defaults to the `stable` channel at `https://releases.treer.ai/`.
`--channel` / `TREER_RELEASE_CHANNEL` and `--release-base-url` /
`TREER_RELEASE_BASE_URL` select another signed release channel and distributor.
`--proxy` remains an alias for `--release-base-url`; the old unsigned Proxy
`/artifacts/` endpoints are no longer accepted as an update source. HTTPS is
required except for loopback test distributors, and redirects are rejected.
The updater verifies the channel and manifest Ed25519 signatures using the
local trust key, checks the manifest digest bound by the channel, and verifies
both platform binaries' byte lengths and SHA256 against the signed manifest.
Only after both pass does it execute their `--help` checks and atomically replace
the installed files. Verification failures leave installed binaries and running
services untouched. A valid signature authenticates a release, but does not yet
prevent replay of an older signed channel or enforce a minimum version.
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
treer agent startup set --cwd . -- sh scripts/start-agent.sh
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

### Repair another Agent's launch

A workspace human/operator or Policy-authorized Agent can inspect and replace
another Agent's saved command without changing its ID or workload credential:

```bash
treer agent admin startup show /reviewer
treer agent admin startup set /reviewer --cwd repo -- codex --no-daemon resume THREAD_ID
treer agent restart /reviewer
# Or save the command and restart atomically in one Controller operation:
treer agent restart /reviewer --cwd repo -- codex --no-daemon resume THREAD_ID
```

Targets accept ID, tree path, sibling name, or `self` for a managed caller. Omitting `--cwd`
preserves the saved directory. Admin set alone does not start a stopped Agent or
change its recovery intent; restart increments `run_generation`. The webpage's
Agent menu offers **Edit startup command**, displaying saved argv and cwd and
submitting one save-and-restart request on desktop and mobile. Commands are an
executable plus literal arguments: use `sh -lc '...'` explicitly for shell syntax.
Do not embed credentials or other secrets in argv. To retain a coding agent's
context, preserve its resume/thread arguments when repairing its launcher.

Admin read/set require `agent.startup.manage` on the target; replace-and-restart
also requires the usual stop/create lifecycle permissions. Existing self-only
`treer agent startup set/show/clear` behavior is unchanged. App runtimes reject
startup management with `app_startup_managed`; update their deployment instead.
The target Controller must advertise `agent.startup-admin.v1`; otherwise the
request returns `unsupported_command` and leaves the connection usable. Saving
occurs before stopping the old process. If launching fails, the new command
remains saved for repair/retry; there is no implicit rollback to the old command.

### Create an Agent with the launcher

After enrolling a machine, install the single Agent launcher on it with
`treer-agent-server launcher --node /absolute/node install` (Node.js 22+). It
installs the per-user `treer-agent` command, so one saved launch profile works on
every machine that has the Treer bin directory on `PATH`. In the console choose
**+ → Add agent**, pick the machine and a profile; from the CLI use
`treer agent admin create --machine <m> --kind command --name <n> -- treer-agent --harness claude`.
Installation, `PATH`, profile migration and bundle delivery are in the
[launcher guide](../launchers/agent/README.md#create-an-agent). The runtime and its
provider login remain the user's responsibility.

New workspaces start with six portable `treer-agent` profiles: **Claude + UI**,
**Codex + UI**, **Cursor + UI**, **Gemini + UI**, **Grok + UI**, and
**OpenCode + UI**. Their descriptions and arguments come from the launcher's
`profiles.json`; all use `cwd=.`. **Claude (terminal)** and **Codex (terminal)**
are additional options without the launcher. Core does not migrate existing
workspace profiles; the launcher installer manages its own UI profiles.
The UI profiles default to unattended full authority: Codex disables approvals,
Claude skips permissions, and ACP adapters approve permission requests (Gemini
also uses `--yolo`). Members with `agent.create` authorization can launch them;
neither a profile nor folder placement provides a sandbox. Use separate OS
accounts, containers or VMs when isolation is needed. Terminal profiles resolve
bare `claude`/`codex` only through absolute directories in the Agent's final
`PATH`. Do not launch either kind in an untrusted repository: providers may load
project hooks, MCP servers and settings with the machine account's authority.
If a UI profile reports `command_not_found`, install the launcher on the selected
machine with `treer-agent-server launcher install` and ensure its bin directory
is on that machine's `PATH`.

## Users, administrators, and invitations

The platform administrator is not a Treer user and does not belong to an
organization. Open `/admin` and use the password supplied in `ADMIN_PASSWORD`
to access the separate admin dashboard. It reports platform-wide user,
organization, machine and Agent counts plus aggregate traffic. Inventories,
user password resets/session revocation, and platform activity details are not
available through the admin API; old routes return `410 admin_details_removed`.
The page still creates single-use user invitations and lists their redacted state.
On a self-hosted Compose stack it also checks and applies
GHCR image updates for Proxy, App, and the updater sidecar. Workspace Settings
does not include that control. A user who
registers from an administrator invitation receives an organization named
`<preferred name> Personal` and owns it. Treer does not seed an initial
organization or workspace.

`GET /api/admin/traffic?hours=6|24|168|720` (default 24) accepts only the platform
admin cookie. It returns `{hours, traffic:[{window_start, traffic_class,
payload_bytes, payload_frames, billable_bytes}]}` for all workspaces combined,
without identity or route details. Time windows include the current hourly bucket;
absent buckets mean zero recorded traffic. The response uses persisted shared
PostgreSQL totals, including legacy machine metering, and does not add the
mirrored Agent counters a second time. Recent local counters normally appear
after the 10-second flush; failures/backlogs can extend that delay. All Proxies
query the same database view rather than adding instance-local pending usage.
Normal workspace-authorized audit and traffic APIs are unchanged.

### Workspace audit events

On Proxies advertising `workspace.audit.v1` in `/api/auth/config`, read
`GET /api/workspaces/WORKSPACE_ID/audit-events` with your user session. The
response is `{events, next_cursor}` in descending sequence order. Use
`?limit=50&before=SEQUENCE` for the next page; stop when `next_cursor` is null.
Optional exact-match filters are `actor_id`, `action` and `resource_kind`.
`since` includes the RFC3339 instant and `until` excludes it; paired bounds may
span at most 366 days. Limits clamp to 1..100. Unknown parameters are errors.

Current workspace owners, including organization owners/admins, can read by
default. An enforced deny can restrict them. To grant another workspace member
or Agent access, use an explicit allow rule for `workspace.audit.read` on the
workspace resource ID in **enforce** mode; monitor and default allow do not grant
audit access. Agent/machine clients use the authenticated `/agent/workspaces/ID/`
variant. Apps use `/api/apps/SERVICE_ID/audit-events` with a service-audience
human/Agent token, evaluated as that caller; an App's own runtime credential is
not sufficient. A 403 means denied; old Proxy 404/405 means unsupported. Do not
silently fall back to an organization feed and filter it in the client.

The workspace feed excludes organization-only events and sensitive payloads,
including file paths/hashes, argv, environment and configuration. Actor IDs are
stable references; stored display names are omitted. Coverage is partial and
best effort for several runtime mutations. Existing attributed history is shown,
but missing history is not backfilled. Newly written events preserve immutable
workspace attribution even after deletion. Audit reads themselves are not recorded. The organization view
retains its original permissions and output.

On `audit.maintenance.v1` Proxies, both audit views add `maintenance` with
`retention_days` and a scoped `deleted_through` watermark (null before cleanup
has deleted anything in that scope). This is not a completeness guarantee or the
time of the oldest event. Add `include_statistics=true` for a fixed recent 30-day
UTC `rows_per_day` list (`day`, `action`, `rows`), `statistics_since` and
`statistics_truncated` (at 4096 groups), independent of feed filters. Scoped
`oldest_expired_at` is null when no expired rows remain; otherwise it indicates
cleanup backlog. Check the
capability before using the new query against old Proxies. Statistics require
the same authorization as audit reads and can fail rather than run longer than
their two-second database statement budget. An opted-in statistics failure fails
that request; retry without `include_statistics` to read the ordinary feed.

Retention starts at 180 days for every organization audit kind, including
security-sensitive events. Set Proxy `--audit-retention-days DAYS` or
`TREER_AUDIT_RETENTION_DAYS` (30..3650) to explicitly update the shared database
setting. Omission preserves the existing setting; keep explicit settings equal
across replicas: the last explicit configuration wins. A configuration change
records old/new days in platform audit; startup logs effective days and source.
Idle checks run each minute; cleanup commits 256-row batches for up to one second
or 64 batches per cycle, then resumes next second while backlogged. The budget
limits starting the next batch; a started batch finishes under a five-second
statement timeout rather than being repeatedly rolled back at one second. Backlogs can
still exceed the configured age. Deletion begins at the first worker tick after
startup: set a longer lifetime before deploying if needed. There is no forever
setting or built-in export. Deletion is irreversible without an
independent backup; increasing retention does not move deletion watermarks back.
This setting does not clean backups, platform audit or Policy decision logs.

Accepted prompt auditing uses a shared 4096-entry PostgreSQL outbox and a fixed
batch worker. Admission still waits for durable storage (up to one second), and
failed audit admission never turns an accepted prompt into a retryable failure.
The global settings lock serializes admission and drain; no tenant fairness
quota prevents one workspace from filling this audit queue for others.
Monitor body-free `audit_queue_overflow`, `audit_enqueue_unconfirmed`,
`audit_drain_failed`, `audit_cleanup_failed` and shutdown-flush warnings. Database
operators can inspect `pending`, `queue_capacity` and `overflow_total` in
`audit_maintenance_settings`; the global counter is deliberately absent from
workspace responses. Shutdown flush is bounded to ten seconds, with committed
entries replayed after restart. Old-only Proxy rollback pauses drain/cleanup,
retains pending entries, and resumes direct writes. Budget audit rows/day along
with index/WAL/outbox/backup overhead; no universal bytes-per-row estimate applies.

`GET /api/admin/invitations` returns the latest 100 personal registration
invitations as `{invitations:[{invitation_id,token_prefix,created_at,used_at}]}`.
`used_at: null` means pending. `invitation_id` is a non-bearer stable handle;
use `DELETE /api/admin/invitations/INVITATION_ID` to revoke a pending invitation.
Used, missing and old full-token DELETE URLs return the same 404. The list never
returns full tokens, links, recipient emails or names. Save the link returned
by POST creation when it is displayed; it cannot be read back from the list.
Admin statistics and invitation responses use `Cache-Control: no-store`.

Organization owners and administrators can create member invitations from
**Members**. Preview the named organization and explicitly choose **Join**.
An existing user signs in, then accepts; signing in alone does not join.
Registration with explicit `join_organization:true` consumes the invite and joins
as member without creating another personal organization. Without that flag,
invitation-only registration rejects the org invite without creating an account;
open-registration deployments may create a personal organization and leave it unused.
OAuth carries consent in a POST body and retains an unaccepted invitation for
ten minutes in the human cookie session; returning users must choose Join.
Declining clears that pending entry.
If pending storage fails after OAuth login, login still succeeds; reopen the
original invitation link to confirm joining. Preview/accept currently share a
60-per-minute budget by TCP peer IP: reverse-proxy deployments may put all users
in the same bucket because forwarded IP headers are not trusted.
Invitations themselves currently have no expiry field; the pending handoff TTL
is not an invitation expiry. Upgrade all
Proxy replicas before deploying the new Console invitation flow, which requires
`organization.invitation-accept.v1`; old endpoints must show unsupported rather
than imply success. Users sign in
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

### Recovering from sign-in lockout

Password sign-in is limited per account and for the administrator (see
[security](security.md)). If a sustained failed-login flood keeps an account or
the administrator refused, a single reset only restores the budget for an instant:

1. Limit or isolate the failing traffic at a trusted ingress (firewall, load
   balancer rule or allowlist for `/api/auth/login` and `/api/admin/login`).
2. On a machine with database access run
   `DATABASE_URL=... treer-proxy --reset-login-attempts`. It clears every counter,
   prints how many it removed, and exits without starting a listener.
3. Sign in, then lift the temporary restriction. Existing sessions were never
   affected. Do not use `--disable-auth` as a recovery method.

## Embedded Agent interfaces on their own origin

With a wildcard ingress domain (`TREER_INGRESS_PUBLIC_URL`, HTTPS) the Proxy
advertises `agent.interface-embed.v1` and the Console embeds a registered
Agent interface on `https://ag-<id>.<apps domain>/` instead of the Proxy-origin
tunnel: the Console mints a 30-second one-time code for its iframe, a
Proxy-owned bootstrap page on the Agent origin redeems it for a partitioned
(`Partitioned; SameSite=None; Secure`) session cookie and verifies the browser
sends it back before loading the Agent UI. Browsers without partitioned-cookie
support (older Safari, strict third-party-cookie settings) fail that check and
the Console falls back to the Proxy-origin tunnel, which the user sees.
`TREER_AGENT_EMBED_FALLBACK=refuse` (`--agent-embed-fallback refuse`) closes
that fallback: the tunnel answers `503 embedded_ui_requires_isolated_origin`
and the Console offers "open in new tab" only. Every Console host (Proxy `app_dist`, Worker, nginx) must send the same
`frame-src 'self' <Proxy origin> https://*.<apps domain>` so the browser lets
the Console frame the Agent origin; without it the frame is blocked silently
and the Console times out (incident on 0c5c40ae). Give the Console and the apps
domain separate registrable domains where possible; see
[the security model](security.md#embed-sessions) for why. Supported browsers
for the embedded path: Chrome and Edge 114+, Firefox 141+, Safari 26.2.

## Serving the browser App from the Proxy

`TREER_APP_DIST_DIR` (or `--app-dist-dir`) makes the Proxy serve the web App
build itself, for the single-container deployment. Requests whose Host is the
App public URL receive files from that directory with `index.html` as the SPA
fallback, `GET /config.json` with the Proxy URL and `GET /health`, all with the
same headers as the standalone nginx image (`X-Frame-Options: DENY`,
`Content-Security-Policy: frame-src 'self' <Proxy origin> [https://*.<apps
domain>]; frame-ancestors 'none'; object-src 'none'; base-uri 'none'`, where the
wildcard source is present only when `TREER_INGRESS_PUBLIC_URL` configures an
apps domain and lets the Console frame the isolated Agent origins there, the
same `frame-src` the Worker and nginx hosts generate from the same inputs;
`X-Content-Type-Options: nosniff`, `Referrer-Policy: no-referrer`,
`Cache-Control: no-cache`, `no-store` for `/config.json` and `/health`), and
HTML documents (the page and every SPA fallback) additionally carry
`Cross-Origin-Opener-Policy: same-origin`; JSON, assets, Core APIs, tunnels
and OAuth callbacks never do (#182). Files
are resolved only inside the canonical directory (no traversal, no symlink
escape); `/api/`, `/agent/`, `/artifacts/`, `/.treer/`, `/.well-known/` and
`/install.*` are never served as App content. In this mode the Proxy applies a strict Host allowlist: exactly one Host header,
compared as host:port (default port from each URL's scheme) against the App
URL, the Proxy URL and the configured ingress suffix; any other host receives a
fixed 421 `host_not_configured`, which also defends `docker run -p` setups
against DNS rebinding. Core routes never execute on the App host (JSON 404),
and the Proxy host never returns the Console. Forwarded headers are not
consulted: a TLS terminator must pass the original Host. The local zero-DNS
default is Console `http://console.treer.localhost:8080` and Proxy
`http://proxy.treer.localhost:8080`: two origins on one port that share a
site, so the Strict session cookie keeps working while the Console stays a
separate origin (`*.localhost` resolves to loopback in Chromium; add one
`/etc/hosts` line for browsers that do not). Public deployments use two HTTPS
names under one registrable domain (for example `console.example.com` and
`api.example.com`) on the same port with a certificate covering both; the two
names must share a site for the Strict cookie. Same site is not same origin:
isolation between Console, API and App content relies on the Host routing and
on the Origin/CSRF checks (writes are accepted only from the Console origin and
CORS grants credentials to no other origin), not on SameSite. Put App-UI and
service-ingress names under a different registrable domain where you can;
other local services on the same hostname and another port can read host-only
cookies, so do not run untrusted services on `*.treer.localhost`. When the App public
URL equals the Proxy public URL, embedded App interfaces and virtual-host
tunnels answer `embedded_ui_requires_separate_app_origin` (503): App content
must never share the Console origin, so configure a separate App hostname to
use them. Cookie, Origin and CSRF rules are unchanged.

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

Open the App URL to create Agents and manage Agents and Apps together in the
workspace's Tree sidebar. Each row offers Start, Stop, Restart, and Logs;
App rows also offer App settings for access controls and the service URL.
Choose New Agent from a folder's menu to use the same terminal, custom command,
or launch profile dialog as the Tree's New button. Proxy reserves the Agent's
name and final folder atomically before starting it. A name conflict leaves
the creation form open for correction and does not start an extra process.
Folder and Agent menus offer Copy path for CLI-style absolute tree paths, such
as `/research/experiments/reviewer` or `/reviewer` at the root. A status message
confirms copying; when clipboard access is unavailable or denied, a dialog
shows the selected path for manual copying on desktop or mobile.
Use New folder to organize workloads into nested folders. Drag a workload or
folder onto another folder, or use its Move to folder action to select a
destination with a keyboard or touch screen. Drop onto Workspace root to move
an item out of its folder. Folder names and placements persist for that
workspace across reloads and devices. A folder cannot be moved into itself or
one of its descendants; deleting a folder moves its contents to its parent
(the workspace root for a top-level folder). These actions organize the control-plane view without moving processes,
working directories, or files. Machines remain available in Workspace settings.

The browser terminal supports ANSI colors, alternate
screens, cursor movement, per-keystroke input, paste, and dynamic resize. PTY
input, replay, and live output remain raw bytes from the Host through the
Controller and Proxy to the browser. The Host socket uses length-prefixed binary
frames, and both WebSocket hops use binary frames instead of Base64 JSON payloads.
On mobile, the workspace opens on the Tree sidebar without mounting a
terminal. Selecting a workload opens its terminal or Agent Interface directly
in full-screen mode; closing it returns to the tree.
Agents inherit `TREER_WORKSPACE_ID`,
`TREER_SERVER_ID`, `TREER_AGENT_ID`, `TREER_AGENT_SERVER_URL`, and a private
workload credential; they can use the local agent server API to discover or
control other agents in the same workspace. The credential is consumed by the
CLI and should not be printed or forwarded directly.

Each Agent also receives `TREER_AGENT_PRIVATE_STATE_DIR`, a Controller-owned
directory for private state such as launcher attachments. It lives under the
Treer state directory (`agent-private/<server_id>/<agent_id>` below
`~/.local/state/treer`, `$XDG_STATE_HOME/treer`, `TREER_STATE_DIR`, or
`%LOCALAPPDATA%\Treer\state`), never in the checkout, and survives stop,
restart and Controller replacement. It is removed, together with everything in
it, only after the Proxy confirms the Agent's permanent deletion; machines whose
Proxy predates this confirmation keep the directory until an operator removes
it. The variable is omitted when the directory would be inside the Agent's
working directory (for example a machine root at the home directory) or fails
its safety check; the Agent still starts and the launcher reports attachments
as unavailable. Usage is capped at 256 MiB per Agent and 4 GiB per machine,
and the Controller refuses new content when the volume would drop below 1 GiB
free. Directories of previous server enrollments are not touched.

Windows requires an account in the Administrators group; under a standard
account private state is not served at all and the status reads
`windows_requires_administrator`. The Controller only trusts an
`agent-private` level whose owner is its own process token (user or token
owner) and whose protected DACL names only that user and LocalSystem. If the Controller later runs under a different
account or elevation than the one that created the directories, the
Controller log reports `insecure_dacl` with a categorical owner/ACE summary,
the variable is omitted and attachments stay unavailable; nothing is deleted
or repaired automatically, because the directories hold the Agents' sent
attachments. To recover, stop the Controller, back up or move the affected
`agent-private/<server_id>` tree, then either recreate it under the account
and elevation the Controller runs with, or repair the existing tree in two
separate steps: set the owner to the Controller's own token user (or its token
owner), and reapply a protected DACL whose allow entries name only the
Controller's token user and `SYSTEM` (never the token-owner group), then start
the Controller again.

Workspace members and managed Agents can also save reusable launch profiles:

```bash
treer agent admin profile create reviewer --cwd . codex -- review --base main
treer agent admin profile list
treer agent admin profile launch reviewer --machine build-machine \
  --name review-42 --cwd packages/api
```

Optional repository launchers package more involved runtimes behind the same
profile contract. The [Agent launcher](../launchers/agent/README.md) provides one
shared browser UI/AIS server with native Codex/Claude and generic ACP adapters
for Gemini, Grok, Cursor and OpenCode. Matching release Controllers embed its
verified portable runtime. Install it without cloning a repository or running
npm on the target:

```bash
treer-agent-server launcher install
treer-agent-server launcher status
treer-agent-server launcher update
treer-agent-server launcher rollback
```

On a machine with several local Controller enrollments, launcher commands fail
with `launcher_controller_ambiguous` rather than selecting one. Pass
`treer-agent-server launcher --workspace WORKSPACE_ID --url http://127.0.0.1:PORT status`
(the URL is needed only if the workspace still matches multiple enrollments).
Candidates list at most eight local server IDs, workspace IDs and listen addresses;
credentials and machine roots are omitted. The selected Controller is bound to
the compatibility probe, installer profile writes/readbacks and status queries.
Managed or bound operator identities cannot override their workspace/URL bindings.
Discovery failures, query/authentication failures (`launcher_controller_probe_failed`)
and a missing/incompatible contract (`launcher_controller_unsupported`) are distinct;
none authorizes installation against another Controller. Even read-only launcher
status requires a successful query of the selected Controller. The standalone
`treer` CLI already rejects ambiguous local discovery; its connection flags go
before the business command (`treer --workspace WORKSPACE_ID status`).

These local commands save/read back launch profiles but never create or restart
Agents, the Host or Controller. Node.js 22+ and the selected harness executable
must already be installed. Select a trusted absolute executable with `--node`
and, if needed, `--treer`; automatic Node discovery uses known installation paths,
not arbitrary PATH entries. On Windows, the operator must ensure these paths and
their ancestors have trusted ownership/ACLs; the command does not authenticate
the Node publisher or audit every Windows executable ACL. Installation uses the
authenticated enrolled root reported by `treer interface contract`, with a
relative `--cwd` (default `.`). Update the running Controller first if it reports
`launcher_controller_unsupported`. Status against older Controllers reports
unknown rather than certifying compatibility.

Launcher status is best-effort: startup reads have a one-second per-Agent limit
and a five-second shared budget (the Agent list has a separate two-second limit).
`agent_startups.partial` and each unavailable entry's `reason` distinguish missing
metadata or a timed-out nested query from a confirmed launcher version. A failed
installation includes a bounded, single-line diagnostic with paths and known
credential values removed; inspect private state locally if more detail is needed.

Versions remain in the enrolled root's private `.treer/launchers/agent` directory;
running launchers keep their resolved physical assets. Rollback verifies the
retained bundle and the running Controller's compatibility/security floor.
Keep versions used by running Agents. Interrupted installation can leave a
private `.install-lock`: inspect state and ensure no installer is running before
manually removing that exact lock; do not delete the whole launcher directory.
Source-only debug builds require explicit `--allow-network --npm-cli` with a
trusted absolute npm CLI JavaScript path. Repository `install.sh`/`bootstrap.sh`
remain legacy/development entry points; they are separate from the signed
embedded release path. Binary updates do not automatically activate a launcher.

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

Agent/App executables may be absolute paths or bare names found in absolute
directories of the final workload PATH. Empty/relative PATH entries are ignored;
lookup failure returns `command_not_found` without falling back to the working
directory. Relative commands such as `./start.sh` are rejected: use an absolute
script path or an explicit interpreter such as `sh start.sh` instead. Windows
bare-name lookup accepts only `.exe`/`.com`, ignores PATHEXT overrides and skips
UNC/device search directories; explicit absolute commands are unchanged. No
`$HOME` or `%LOCALAPPDATA%` expansion is performed. Startup specifications retain
the original command; each start/restart/restore resolves it again. Upgrade all
target Controllers before installing shared profiles relying on this behavior;
older Controllers retain their previous PATH/cwd search behavior. Install the
launcher shim on every target and check its bin directory is on the Controller's
PATH before migrating shared launcher profiles.
Before upgrading, replace any existing relative executable in saved startup
specifications with an absolute path or an explicit interpreter. Running
processes are unaffected until their next start/restart/recovery.

`profile launch --cwd` overrides the saved working directory for that launch
without changing the profile. The directory is relative to the selected
machine's Host root and must resolve inside that root. Omitting it uses the
profile's saved `cwd`.

For CLI creation, `treer agent admin create`, `treer agent admin profile launch`
and `treer app create` accept `--folder PATH` for the workspace Tree destination.
Omit it to inherit the authenticated Agent/App caller's direct Tree folder at
the Proxy transaction boundary; human/operator callers default to `/`.
Relative paths, `.` and `..` resolve from that folder. Absolute paths and folder
IDs work as with `agent ls`; `--folder /` explicitly selects root. This option
does not change `--cwd` or the profile's process working directory.

An unplaced workload caller gets `caller_unplaced` rather than accidental root
creation; ask the owner to place it or use an explicitly authorized destination.
New Agent/App identities have explicit placement before startup. Existing
workspace nodes stay where they are, including legacy unplaced nodes. Creation
requires `workload.create-placement.v1` on the CLI interpreter, Proxy and target
Controller; unsupported versions fail clearly without silent root fallback.

**Safety checklist:** Folder membership and path Deny are not a sandbox or a
containment boundary. Review who has `agent.create` and `agent.tree.manage`:
those callers can create elsewhere or move into paths with more permissions.
Review monitor versus enforce and unmatched Allow defaults; inventory unplaced
nodes (missing placement is not root for Policy); restrict high-risk machine
exec/file actions and use external account/container isolation when required.
Moving old unplaced nodes changes which path rules match, so preview first.

```bash
treer agent admin profile launch reviewer --name review-42 --folder ../reviews
treer agent admin create --machine build-machine --kind command --name helper \
  --folder /research --cwd repo -- /bin/sh
```

CLI resolves the folder before creation. Proxy reserves the name and final
placement atomically before dispatching process startup, appending after existing
siblings. Tree reads require `agent.tree.read`; non-root creation placement
requires `agent.tree.manage`. Conflicts fail before any process starts.

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

For local CPU/RAM/NVIDIA collection, retention, query limits and owner controls,
see [Machine hardware metrics](machine-metrics.md). History stays on the source
machine; an offline machine cannot be queried from a central history copy.

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

### Retired macOS network extension: existing installations

Removing source or upgrading the Controller does not uninstall an already active
extension. Before switching an existing machine to plain networking:

1. Inventory Agent endpoints that relied on private namespaces or virtual names.
   Make them reachable from the Controller and register explicit service ports.
2. If the old signed TreerNetwork application is installed, use that installed
   application's `deactivate` / `uninstall` commands to remove its own provider
   configuration, then verify that the extension is inactive. Consult the old
   build's documentation if macOS requires an approval or a reboot.
3. Check for resolver configuration left by that installation. Remove only files
   positively identified as owned by Treer, after verifying they are no longer
   used; do not alter VPN, Tailscale, or system-wide DNS settings.
4. Start or repair the Controller with `TREER_NETWORK_MODE=plain`. Retired saved
   modes are rejected with migration guidance rather than silently activated.

Treer does not execute these machine-level cleanup steps during an ordinary
source update. Existing installed binaries must remain available until cleanup
is verified. This document does not claim that an installed extension has been
removed or that its prior experimental gates passed.

### Register and forward services

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

An Agent can designate one of its HTTP services as its primary communication
endpoint. Run registration from that Agent's managed runtime:

```bash
treer service register --primary --protocol http --name reviewer-api --port 4180
# From another managed Agent:
addr=$(treer agent address /reviewer)
curl --fail-with-body "$addr/openapi.json"
curl --fail-with-body -X POST "$addr/review" \
  -H 'Content-Type: application/json' -d @request.json
```

The serving Agent owns its API/schema; these paths are examples. `address`
prints only the stable service-ID URL; `--json` includes the Agent and service
IDs. It accepts a tree path, sibling name, or Agent ID and defaults to `self`. It fails when
no primary service is registered. The URL requires a Treer HTTP proxy, which
managed processes inherit; it is not a public DNS address. Repeating primary
registration updates the existing service through ownership and update Policy
checks, preserving its ID even when its name or port changes.
If no primary exists, an existing same-name service owned by this Agent is
promoted in place. Include server
startup and registration in the Agent's saved launch command for cold recovery.
No process supervision is added by registering the service. AIS registration
alone does not expose a `.treer` address; register its listener as a service too
when other Agents need direct HTTP access. Offline delivery/retries belong to
the calling/serving application.

For HTTP publication, configure wildcard DNS and TLS once at the Proxy edge:

```bash
export TREER_INGRESS_PUBLIC_URL='https://apps.treer.ai/'
# Behind Cloudflare or a load balancer: name the front so public ingress
# rate-limits real clients instead of the front's address (never trust raw
# X-Forwarded-For). Without these the TCP peer is the client. Peers on
# private, loopback or link-local addresses are always treated as fronts or
# internal callers: they are exempt from the per-client request limit and the
# per-peer connection cap and bounded only by the per-service limits and the
# global connection budget, so do not expose a Proxy directly to untrusted
# clients on a private network without a front in between.
export TREER_INGRESS_TRUSTED_PROXIES='173.245.48.0/20,103.21.244.0/22'
export TREER_INGRESS_CLIENT_IP_HEADER='CF-Connecting-IP'
treer service publish SERVICE_ID
# Explicit anonymous access:
treer service publish SERVICE_ID --access public
treer service unpublish SERVICE_ID
```

**Hosting behind Railway.** Railway documents `X-Real-IP` as its client-address
header but publishes no proxy address ranges and no overwrite or append rule for
client-supplied copies, and its support answers disagree; a range observed today
(for example `100.64.0.0/10`, which is shared address space) is not a contract
and may change. Treer therefore does not trust Railway's edge: on Railway leave
`TREER_INGRESS_TRUSTED_PROXIES` and `TREER_INGRESS_CLIENT_IP_HEADER` unset and do
not use `--ingress-allow-wide-trust`. The consequences are by design: the
per-client request limit and the per-peer connection cap are off (the Proxy
logs the fixed warning once); the per-service limits and the global connection
budget are the protection; and `X-Treer-Client-IP` carries the TCP peer, which
is Railway's private edge address for every client, so it is not a client
address there. An App must not use it for per-client accounting on such a
deployment (treat a private, loopback or link-local value as "no client
address"); lockouts and budgets keyed by handle or global counters still work.
A deployment that wants per-client limits needs a front with a published
address range and a documented overwrite rule; then set the two variables to
exactly that range and header.

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
download on that machine. New installations pass `--initialize-admin-token-file`:
the App generates a private administration token on the target machine, never in
the Proxy. Retrieve it only through an authorized private machine terminal, not
Treer exec/file APIs or Messages; see the App's token-file instructions. Existing
installations retain their startup command and credentials; if they lack a token,
use the App's explicit local token-file recovery command. Upgrading assets does
not automatically initialize or rotate a missing token.
The App provides mode, JSON rule editing, atomic
publication, cache sync status, and a decision simulator; deployment and state
paths are documented in [`apps/policy`](../apps/policy/README.md). The Proxy
validates and caches its versioned bundle, then evaluates
subject/action/resource requests locally. Managed Agent requests carry their
authenticated Agent identity; workload HTTP entries use the same subject and
resolve service-name aliases to stable IDs before authorization.

Policy publication defaults to `owner_session`. An owner may explicitly choose
`provider_operator` in the Provider binding to let holders of the Policy App's
private administration token publish without per-request human approval. This
is full administration authority; default monitor mode does not isolate anyone.
Use a dedicated folder for the administration Agent, allow its path to use the
Policy App and deny other Agents, then verify in monitor before enforcing.
Also restrict tree moves, control of the administration Agent, machine exec and
file access. Run the App/administrator in a separate unprivileged OS account or
container with a private port and secret mounts: **0600 files cannot isolate
Agents sharing an OS user**, and loopback access bypasses Treer ingress policy.
See the [Policy App isolation example and private JSON client](../apps/policy/README.md#isolating-the-policy-administration-agent).

Path rules use bundle schema 2; consult the
[fixed contract](research/2026-10-13-policy-schema-v2-contract.md) for complete
JSON examples. `/team/*` selects direct children, `/infra/**` includes the
folder and all descendants, and typed folder relations compare real Tree
placement. Renaming/moving changes which rules match immediately; review rules
before organizing a protected subtree. No Tree placement means no path match,
not implicit root membership. New CLI/Controller/Proxy capability checks reject
unsupported v2 Providers explicitly; update the Proxy and Provider's Controller
before selecting a v2-only Provider. Existing v1 Providers remain compatible.

Path rules authorize by a node's current Tree position; they are not containment.
An unplaced node matches no path selector, a new Agent created without a folder
is placed at the root rather than inheriting its creator's path, and a subject
allowed `agent.tree.manage` can move or rename nodes into an Allow-covered
subtree. To make a path Deny meaningful, use enforce mode with explicit default
Deny and restrict `agent.create`, App creation and `agent.tree.manage` for the
same subjects; verify with `/policy/explain` after each Tree change.

Owners can call `/api/workspaces/{id}/policy/explain`, `/policy/decisions` and
`/policy/validate` using their authenticated user/operator session. Explain can
resolve actor/target canonical paths and simulate a candidate document without
installing it. Only the configured Provider can access the App diagnostic bridge
with both its runtime credential and the current Owner's service-audience token.
The decision feed is the latest 100 monitor would-deny records on that Proxy,
not durable or complete cluster history. Provider selection/removal/reinstallation
and Owner Tree management remain available even if delegated rules deny all or
the Provider is down. Core does not override denials on ordinary Agent commands.

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

Recovery reuses the existing installation without re-enrollment. Agents and
Apps save their actual launch snapshot before spawning.

Re-adding a machine after deleting it in the Console works from the same
installation (its row is reactivated under the same machine ID) and, since
#126, from a new installation identity as well: the Proxy then mints a NEW
machine ID and leaves the deleted machine's row revoked, so nothing (Policy
rules, history, Agents) is inherited by a different physical installation.
Two local files take part: the installation identity in
`<state dir>/machines/<hostname-key>/machine-identity.json` and the registered
service in `<config dir>/<key>-controller.json` plus `<key>-host.json` (the
config directory is separate from the state directory, so deleting
`~/.local/state/treer` alone regenerates the identity but keeps the service).
When a persistent service configuration still names the deleted machine, the
Controller adopts the new machine ID itself: it journals the replacement,
keeps `.bak-<timestamp>` copies of the previous files, writes the new
configuration, stops and removes the previous registration (this ends the
previous Host and any Agents still running on it, as `service uninstall`
would; the Controller prints the machine ID it is stopping first), then
registers and starts the new machine. The backups
`<config dir>/<key>-controller.json.bak-<timestamp>` and
`<key>-host.json.bak-<timestamp>` are private (0600) and still contain the
previous, already revoked machine token; delete them once the new machine is
confirmed working. An interrupted replacement is finished by the
next `connect` or `service repair`; the previous machine's startup store and
`agent-private/<old machine>` directory are never read or migrated and remain
on disk for manual cleanup. The audit event `machine.enroll` carries
`replaced_revoked_machine: true` and `previous_server_id` for such a
replacement. A live machine keeps its binding; a conflict while reusing an
installed service names the configuration directory and the reset:
`treer-agent-server service --workspace WORKSPACE_ID uninstall`, then connect
again. The Controller first
checks Host inventory, so a hot restart or repeated restore attaches surviving
processes. Missing intended-running processes restart under their stable Agent
ID after Proxy credential validation. Explicitly stopped/deleted/revoked items
stay stopped. Normal successful Agent exit disables recovery; failed launches
can be retried explicitly. One Controller restore path owns process recovery
for both Agents and Apps. On reconnect the Proxy can declare a legacy App's
launch specification, but it does not run a competing process-restarter.
Restart and recovery preserve the Agent ID; a new run generation identifies
each replacement process. Service registrations and public mappings persist
independently of process availability.

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

For `treer status`, `treer agent ls` and `treer machine list`, a stdout terminal
defaults to a compact table. Pipes and redirected output retain the original
JSON bytes. Use `treer --format json status` for full machine-readable details
in a terminal, or `treer machine list --format table` to request a table in a
pipe. The client consumes this display option; Controller command semantics
are unchanged. Table cells are single-line and truncated to 64 characters;
use JSON for complete paths and fields. Help, failed commands, unknown response
schemas and replies exceeding the 8 MiB presentation buffer retain raw output.

```bash
treer whoami
treer status
treer agent ls -R /
treer agent show /reviewer
treer agent start /reviewer
treer agent stop /reviewer
treer agent restart /reviewer
treer agent admin rename /reviewer code-reviewer
treer machine rename self build-machine
treer machine delete srv_obsolete
treer exec --machine build-machine --cwd repo -- git status --short
treer exec --machine build-machine --cwd . --timeout 10000 -- sh -lc 'make test'
treer agent attach /reviewer
treer agent admin delete /obsolete-helper
treer agent prompt /reviewer "Review the parser changes" --wait --timeout 120000
treer agent read /reviewer --lines 80
treer agent transcript /reviewer --page 0
treer agent send-keys /reviewer ctrl-c
```

The same `treer agent start`, `stop`, `restart`, and `read` commands accept App
names or their stable Agent IDs. Restart retains the Agent ID and increments
its run generation to identify the new process. App-specific deployment,
service, access, and configuration commands remain under `treer app`.

Organize both kinds using commands under the same prefix:

```bash
treer agent mkdir -p /research/experiments
treer agent ls /research
treer agent ls -R /
treer agent mv /reviewer /research/experiments/
treer agent mv /research/experiments /research/runs
treer agent mv /research/experiments/reviewer /
treer agent rmdir /research/runs
```

These are case-sensitive workspace tree paths. An Agent's current folder is its
placement in that tree; operators start at `/`. Relative paths, `.` and `..`
resolve from there, while absolute paths start at `/`. `ls` without a path lists
the current folder and includes a `..` entry outside root. `ls` returns JSON
`path`, `folders`, and `agents` fields, including dormant Apps; `-R` includes
descendants. Folder IDs are accepted alongside paths for `ls`, `mv`, and
`rmdir`. `mkdir -p` creates missing parents and accepts existing folders. `mv`
moves an Agent/App by ID, name in the current folder, `self`, or tree path into
an existing directory; `.` is the current folder. It can also move or rename
folders. Workload renaming remains `treer agent admin rename`.
No destination folder is overwritten. `rmdir` removes only empty folders,
checking emptiness under the same database lock as placement changes. It never
deletes or stops a process. Browser folder deletion keeps its existing behavior
of promoting children to the parent. Commands use workload or local operator
authentication; Proxy Policy actions `agent.tree.read` and `agent.tree.manage`
govern access to the workspace's `agent.tree` resource.

For a provider that runs outside the Host, launch a command-owned bridge:

```bash
treer agent admin create --machine build-machine --kind bridge \
  --name provider-bridge --cwd . -- /absolute/path/to/bridge --serve
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
Exec receives only an OS/session baseline, including Windows system and user
profile variables. It receives no `TREER_*` identity, proxy variables, provider
tokens or Node/loader hooks; it is not an implicit way to act as the calling
Agent. PATH is the Controller's OS baseline without Treer's previous CLI-directory
prepend; use an absolute path for tools absent from it. Configure additional
tools explicitly in the command or a trusted wrapper.
The command still has the machine OS user's filesystem authority, so this is
environment hygiene rather than credential or filesystem isolation.

The machine details page also uploads one file of up to 128 MiB into an
existing directory beneath the Host root (16 MiB for Controllers that predate
`machine.upload.v3` and, for now, for Windows Controllers, which do not
advertise the chunked contract and refuse its commands until the validation
tracked in #171; the Console then falls back to the legacy route for small
files and reports `controller_update_required` for larger ones). The browser
sends the file in parts of at most 8 MiB, each a short request that survives a
slow link and the five-minute request limit of hosted edges; a dropped
connection resumes from the committed offset, and the Controller stages the
bytes in the destination directory before renaming them into place once
exactly the declared size has arrived. Replacing an existing file requires an
explicit UI choice. Each machine stages at most eight uploads at a time, so
plan for up to 1 GiB of staging space in the destination directories; the
destination volume must keep the file size plus the larger of 5 percent and
128 MiB free, and staging files (`.treer-transfer-upl_...`, owner-only) are
visible in the destination directory while an upload is active. A restarted
Controller sweeps the staging names its predecessor recorded; a name it cannot
remove (for example one still held open by the predecessor on Windows) is kept
for the next start and logged as a warning naming the path, so a partial file
of user content may stay on disk until a later start succeeds. Once no
Controller holds it, an operator may delete such a `.treer-transfer-upl_...`
file by hand. Two uploads
per user and eight per workspace may be active at once; an upload with no part
for an hour, or older than six hours, is cancelled and its staging file
removed. The Controller remembers committed receipts for 24 hours (at most
256 receipts; failed receipts are evicted first, then the oldest committed
ones, so with the two-per-user and eight-per-machine quotas a committed
receipt can only be lost before the Proxy reconciled it if one Controller
publishes more than 256 uploads in a day; in that same case a new failed
receipt is the one evicted at once, and the Console then learns of the
failure only when the upload's lifetime ends instead of immediately); if a
Controller is unreachable for longer
than that right after publishing a file whose commit response the Proxy lost,
the Console reports the outcome as unconfirmed (`outcome_unknown`) and the
operator should check the destination directory. A commit whose publish
provably did not happen (the target was refused, or the atomic rename or link
itself failed) leaves a definitive failure receipt next to the success
receipts, so the Console reports the fixed error at once instead of
`outcome_unknown` after six hours. A restarted Controller sweeps the staging
names its predecessor recorded; a name it cannot remove (for example one still
held open by the predecessor on Windows) is kept for the next start and logged
as a warning naming the path, so a partial file of user content may stay on
disk until a later start succeeds. After five consecutive starts that failed
to remove the same name the Controller drops the record with a final warning
and leaves the file alone; a directory that no longer exists counts as swept.
Once no Controller holds it, an operator may delete such a
`.treer-transfer-upl_...` file by hand. The Proxy's per-upload part and status
rate limits (160 parts and 60 status reads per minute, 32 part bodies in
flight) are counted per Proxy instance; a multi-instance deployment multiplies
them by the instance count, while the active-upload quotas (two per user,
eight per workspace and per machine, 1 GiB declared per machine) are held in
the database and are not. With authentication disabled (development only) the
upload routes skip the Policy gate like every other machine route.

Transfer single files from the CLI (directories and implicit parent creation are
not supported):

```bash
treer machine show build-machine
treer machine put --machine build-machine ./report.pdf research/report.pdf
treer machine get --machine build-machine research/report.pdf ./download.pdf
tar -cf - research | treer machine put --machine build-machine - research.tar
treer machine get --machine build-machine research.tar - > download.tar
```

REMOTE paths use `/` and are relative to the `root` shown by `machine show`, not
the Agent's cwd. LOCAL paths belong to the machine running the CLI. Both sides
reject symlinks/reparse points and special files; use a physical path, for
example `/private/tmp` rather than the macOS `/tmp` symlink. `--force` explicitly
permits destination replacement for either direction. Files default to 0600;
`--mode 640` selects ordinary Unix bits, excluding group/other writes and special
bits. Windows supports private current-user/SYSTEM ACLs and only mode 600.
The default per-file limit is 128 MiB; set `TREER_MACHINE_FILE_MAX_BYTES` in the
Controller launch environment to change it (maximum 1 GiB). Transfers verify
SHA-256 before publishing downloads or uploads; `get ... -` buffers on private
local disk before writing stdout. The limit also bounds that spool.

The Proxy, local CLI/Controller and target Controller must support
`machine.file-transfer.v1`; an unsupported peer fails explicitly, without falling
back to legacy upload. `machine.file.write` and `machine.file.read` are separate
Policy actions. Existing machine identity targeting restrictions still apply;
an Agent identity can address another workspace machine according to Policy,
while a bare machine/operator route retains its own-machine restriction.
An interruption discards staging on disconnect/expiry; after a crash, recorded
staging is cleaned on the next transfer. If publication succeeded but its reply
was lost, inspect the destination before retrying; retries never auto-force.
`machine_file_commit_uncertain` likewise means publication happened but cleanup
or directory durability could not be confirmed; inspect rather than retrying.
Have the host operator verify the destination's size/hash and inspect any
same-directory `.treer-transfer-*` residue. Do not blindly remove a live
transfer's file or add `--force`; a no-force retry may correctly return
`machine_file_exists` for the already-published destination. Crash recovery
removes recorded temporary hard links without deleting the published file.

New Windows enrollments default to `%LOCALAPPDATA%\Treer\root` (USERPROFILE
fallback), independently of the install shell's cwd. An explicit `--root` must
already exist. Windows roots previously enrolled from `C:\Windows\System32`
remain unchanged. `connect` will not move an existing enrollment; an explicit
different root is rejected. From a **host terminal**, schedule downtime, back up
private configuration without exporting credentials, and move only your own
project files into a new existing directory. Never copy all of System32:

```powershell
treer-agent-server.exe service --workspace WORKSPACE_ID stop
treer-agent-server.exe service --workspace WORKSPACE_ID relocate-root --root "C:\Users\YOU\TreerRoot" --accept-workload-migration
treer-agent-server.exe restore --workspace WORKSPACE_ID
treer.exe machine show MACHINE_ID
```

Stopping the Host terminates its Agents/Apps. Relocation preserves machine ID,
credentials and saved startup state, but does not move files, rewrite cwd/absolute
executable paths, or start anything. Check those paths and launcher installations
before restore. Reinstall the launcher in the new root and update saved profiles
and startup paths explicitly; the old `.treer` directory is not moved or cleaned.
`relocate-root` currently supports Windows only, using the same lock as Host
startup. Individual configuration writes are atomic; startup rejects a
Host/Controller root mismatch after an interrupted migration. Rerun relocation
with the intended root while stopped to reconcile it. Changing root is not a
side effect of transfer or binary installation. Treer root constraints do not make machine file access
a sandbox: monitor/allow Policy can still permit ordinary sensitive files inside
that root, and direct same-account OS access remains unrestricted.

Provider CLI installation from the Console (`provider-clis` routes) needs Node
22 with `npm` on the Controller account's PATH; the Controller shows the exact
npm commands before anything runs, installs into npm's default global prefix
for that Node (a non-writable prefix fails visibly, and Treer never elevates or
edits PATH), keeps results under `<state dir>/provider-cli/` for 24 hours, and
leaves provider login to the user. The confirmation also shows what the install
would replace: the version the installed binary reports (or that something
unreadable is there) next to the pinned target, with the action `install`,
`replace` or `noop` (already at the pinned version, nothing to confirm). A
downgrade or unknown existing version needs an extra explicit acknowledgement,
and an installation that changes between confirmation and the install step is
refused as `provider_cli_state_changed` without touching the prefix. Machines
whose Controller predates this observation answer `provider_cli_controller_outdated`
until updated. Windows machines list every recipe as unsupported in this
release. See [security](security.md#provider-cli-installation).

Windows machine exec has a 30-second ceiling and is owned by the Controller.
Windows PowerShell 5.1 receives a fixed system module search directory from
Win32 rather than an inherited PSModulePath. The path supplied by Treer excludes
`ProgramFiles/WindowsPowerShell/Modules` and user Documents modules;
third-party `Install-Module` auto-loading is not part of this baseline. Extra
module configuration requires a separate explicit design. PowerShell can
reconstruct its environment during startup, so the environment boundary is
not a module-execution sandbox. `-NoProfile` skips profile scripts; it does not
remove the native runtime's OS/module initialization requirements.
Without a usable pre-generated module cache, PowerShell 5.1 auto-loading can
be slow: hosted Windows 2022/2025 probes took 12–28s, while tested Windows 11
calls took less than one second. The 30s exec ceiling remains unchanged;
neither observation guarantees latency on another machine. Treer does not
inherit an analysis-cache path or manage that cache in production. Hosted-image
cache compensation is confined to a CI-guarded unit-test fixture; see the
quality guide for its limits and separate native Windows 11 evidence.
Running `service restart-controller` inside that exec can lose its reply when
the Controller is replaced, even if restart succeeded. Run activation from a
host terminal or a Host-owned managed command instead; inspect its output after
reconnecting. `Start-Process` alone is not a Windows Job-detachment guarantee.
Host updates are cold replacements and terminate all workloads; the Rust
in-place updater remains unsupported on Windows. See the
[Windows update and acceptance plan](research/2026-10-13-windows-runtime-readiness.md)
for staging, whole-set rollback and native test requirements.

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
operations. The single [Agent launcher](../launchers/agent/README.md) performs
registration automatically and serves the same embedded UI for all harnesses.
Provider adapters live under `launchers/agent/src/harness/`; `apps/` contains
workspace services. Terminal-only Agents use ordinary command/shell profiles.

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
    --to /coordinator \
    --idempotency-key review-42-ready

printf '%s\n' 'I addressed the comments.' | \
  treer message reply <message-id> --to sender

treer message ack <delivery-id> --operation-id ack-review-42
```

Bridges can pipe one JSON object with `body` and optional
`external_source` to `treer message send --to TARGET --stdin-envelope` so neither
message text nor external metadata enters process arguments. Generate this JSON
in the bridge itself; interpolating private text into shell commands defeats the
protection. The envelope accepts at most 1 MiB and retains the ordinary Message
validation and authenticated sender. It cannot be combined with `--body`,
`--body-file`, or `--external-source-file`; existing plain-stdin sends are unchanged.
An older Controller rejects the new flag without sending; upgrade rather than
retrying with a private `--body` argument.

Use stable recipient IDs, paths, or sibling names and a sender-scoped idempotency key for
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

Telegram's point-of-contact (POC) routing is changed at runtime in the chat (`/poc`)
or by Core control Messages to the bridge Agent, with no confirmation gate; the
bridge Agent's Message Policy is therefore the authorization boundary for IM
conversations, so allow only a minimal set of senders (see "Limits, audit and
revoking access" in the Telegram guide).

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
treer service register --name example-http --protocol http --port 3000
curl http://example-http.treer/path
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

Targets accept a stable Agent/App ID, tree path, sibling name, or `self`/`.` from inside a
managed agent. `agent prompt TARGET TEXT` now defaults to queue delivery: idle
targets start immediately; busy targets finish their current turn first.
`--steer` explicitly inserts into the current turn. Missing `prompt.queue`
capability (including PTY-only Agents) returns `queue_unsupported`; it never
silently falls back to insertion. Older Controllers reject the negotiated
`agent.prompt-delivery.v1` command without dropping their connection.

`prompt --wait --timeout 120000` follows this submission's receipt until
`done`, `failed`, or `interrupted`, rather than watching unrelated idle events.
Timeout is in milliseconds and does not cancel the work. `--until` belongs to
the separate `agent wait` command. `treer agent queue ls TARGET` lists pending
items; `treer agent queue cancel TARGET ID` cancels a pending submission.
`agent show` includes live `busy` and `queue_length` when the Interface supports
status observation (or `activity_error` if status is unavailable).

Agent-to-Agent submissions request a completion Core Message only with
`--notify`, for both queued and steering delivery. `--wait` alone polls the
submission receipt without requesting a message. Omitted/null API `notify`
remains disabled, including interrupted results. Human/operator API callers
must supply `notify.to` when requesting notification. Completion delivery uses the target
launcher's identity and normal message authorization, with a durable outbox
and idempotency key. Notification failure does not rerun the task. API clients
may also explicitly select no outcomes with `notify:{on:[]}`.

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

### Managed App email sending

`POST /api/apps/{service_id}/email` reuses the Cloudflare sender configured for
registration/recovery. Configure `CLOUDFLARE_API_TOKEN`, the existing Cloudflare
account configuration, and `TREER_EMAIL_FROM` on the Proxy only. Do not give an
App these credentials. `TREER_EMAIL_ALLOWLIST` is a comma-separated list of
exact addresses and `@domain.example` entries (exact domains, no subdomain
matching). Without an allowlist, only verified organization-member addresses
are admitted. `email.send` is evaluated for the Managed App's runtime Agent
against its owned `machine.service`; ordinary Agents and human tokens cannot
use this route. No SMTP configuration is required.

Request JSON contains only `to`, `subject` (1–200 bytes, no controls), `text`
(1–4096 bytes), and `idempotency_key` (1–128 bytes). No caller HTML, From or
Reply-To is accepted. Success returns `email_id` and `status=sent`. Errors include
`email_not_configured` (503), `email_recipient_denied` (403), `email_rate_limited`
(429), `email_delivery_failed` (502), and `email_in_progress` (503). Each workspace
limits App attempts to 20/hour and recipient attempts to 5/hour. Failed attempts
consume limits; identical completed sends return the original result after
current admission and Policy checks.

Inspect body-free delivery history in PostgreSQL `app_email_attempts`: `app_id`,
`recipient_hash`, `created_at`, `status`. Configure retention/backup according to
local audit requirements; history is not automatically deleted. Provider errors
are sanitized. The reservation and remote provider dispatch are not a single
transaction: a crash or ambiguous provider response can cause a duplicate on a
later retry. In-flight reservations can be retried after 60 seconds.

Deploy [email-endpoint](../apps/email-endpoint/README.md) as a workspace Managed
App, initially in `outbox` mode. `proxy` mode falls back to local outbox only on
`email_not_configured`; Policy/recipient failures remain visible retry failures.
Older Proxies without this route return a visible failure; select outbox
explicitly until the Proxy upgrade. Real email acceptance requires operator
coordination after the route is deployed; automated tests use local fakes only.

### Agent path targets

Agent/App names are case-sensitive and unique within their Tree folder. Use
`/team/reviewer`, `./reviewer`, `../team/reviewer`, or `sub/reviewer`; relative
paths start at the caller Agent's containing folder, or `/` for an operator.
Bare names match siblings only. A missing sibling returns `agent_not_in_folder`
with at most ten same-name paths in `hint`, even if there is only one global
match. Stable `ag_…` / `app_…` IDs always work; `self` and `.` are the calling
Agent when an Agent target is required. Folder commands keep `.` as the folder.

Agent discovery, show, whoami, status, and Message recipient snapshots include
`path`. Tree **Copy path** values can be used directly as `treer` targets. Moves
and renames preserve IDs and Policy rules. Creation, rename, and moves reject
sibling collisions; folder deletion also rejects promotion into conflicting
siblings. Services and `NAME.treer` retain separate workspace-wide naming.
Launch profile names and machine names keep their existing separate namespaces.

A new CLI checks `agent.path-targets.v1` on the Controller before dispatch. An
older Controller yields a clear upgrade error; the client does not upgrade or
restart it automatically. See the [design notes](research/2026-10-12-agent-path-targets.md)
for compatibility and atomic creation decisions.

### Machine updates with a reviewed Canary artifact set

For a full-SHA, `canary_passed` machine train including launcher installation and
an isolated smoke Agent, use the [manual rollout coordinator](../scripts/machine-rollout.md).

`scripts/update-machine.py` automates the manual machine update workflow on Unix
machines with Python 3.11+, `nohup`, and an installed Controller. It is separate
from the signed release-channel updater above: the manual workflow pins binary
SHA256 values to the current Canary Proxy's HTTPS `SHA256SUMS`. This checksum
comparison does not replace signed release metadata or a locally trusted key.
Use the signed updater for normal distributed releases. Do not pass credentials
in arguments, artifact URLs, or job configuration.

Plan without changing or contacting a machine:

```sh
python3 scripts/update-machine.py --machine local
python3 scripts/update-machine.py --machine mariana --workspace ws_…
```

After the coordinator has approved the Controller reconnect, apply the plan:

```sh
python3 scripts/update-machine.py --machine local --workspace ws_… --apply
python3 scripts/update-machine.py --machine mariana --workspace ws_… --apply
python3 scripts/update-machine.py --machine mariana --artifacts dist --apply
```

`本机` is an alias for `local`. Remote machines use `treer exec` only for short
commands with no stdin, a 25-second timeout and bounded output. Long work runs
under `nohup env -i` in its own Unix session (`setsid` via `start_new_session`),
then the caller polls a small result document. No launchd or cron restart task
is created. Local artifact directories may contain platform subdirectories;
remote copies use bounded chunks and must still match the online Canary
checksum manifest. Redirects and non-HTTPS artifact authorities are rejected
except loopback test servers. Inherited workload credentials, UI settings and
proxy variables are absent from the detached worker and service commands.
For Linux user-service control, the worker derives the target user's private
`/run/user/UID` and bus socket locally; it does not inherit a caller's DBus
address. An explicit configuration directory is mapped to `XDG_CONFIG_HOME`.

The worker checks and stages binaries, executes `--version`, backs up installed
files under `~/.local/share/treer-backups/<old-commit>-<timestamp>-<job>/`, then
atomically replaces **Controller first** and runs
`service --workspace WS restart-controller`. It requires a fresh Controller
epoch, matching build/workspace/server identity, `proxy_connected`, path-target
capability, and unchanged Agent IDs/PIDs/generations before replacing CLI.
It verifies installed hashes and versions afterward. The Host is untouched;
different Controller/Host versions, including an older locally built Host, are
normal while the protocol remains compatible. With multiple local installs,
`--workspace` is required and ambiguous selection fails with candidate commands.
Candidate commands contain only workspace/machine/config-directory selection,
never `--apply` or `--accept-shared-binaries`, and are never executed automatically.
After selecting a workspace, applying still requires the operator to add those
flags explicitly; printing a command or supplying `--apply` is not confirmation
of shared-binary impact.
Replacing the shared binaries affects other workspaces on their next start;
the tool warns and requires `--accept-shared-binaries` before staging or
activation. Only the selected Controller is activated. Coordinate compatibility
and any later activation of the other Controllers; this flag does not update
them as a group or promise cross-version CLI compatibility. The worker repeats
selection/confirmation under its binary lock to detect newly added installs.
The guard is conservative: multiple local enrollments require confirmation even
when an operator has arranged separate binary directories.

```sh
python3 scripts/update-machine.py --machine local --workspace ws_… \
  --accept-shared-binaries --apply
```

This Python workflow remains Unix-only. Native Windows configurations use
per-server filenames but often share `%LOCALAPPDATA%\Treer\bin` and a default
root. An occupied shared `.exe` cannot be treated like a replaceable Unix inode;
the Windows installer refuses replacement of running binaries. Use an explicitly
coordinated Windows installation procedure; the Python tool rejects Windows
before creating an update job or replacing files.

Failure after activation restores the saved binaries and reactivates the old
Controller. `result.json` distinguishes `rolled_back` from `rollback_failed`;
`rollback_failed` carries `rollback_error.code`/`message` for the restore,
reactivation or live-Agent verification step that failed, and a verified rollback
includes `verification` (live Agents preserved, exited records not relisted,
Agents that appeared). Retain the backup and investigate any rollback failure.
The Agent check covers Agents that had a process before the update, plus stopped
Agents persisted in the selected startup store (they must still be listed): a
Controller restart, new or old build, does not relist exited Agents it only held
in memory, so those are reported in `inventory`/`verification` with a top-level
`warnings` entry (`exited_records_not_relisted`), never a failure. Receipts
carry fixed codes, hashes, counts and at most 32 Agent IDs per list: only the
updater's own fixed messages reach a receipt, every other exception is reduced
to its class name and a fixed placeholder, and the full text stays in the
private job directory's `error.log`. An unreadable startup store never disables
the persisted-Agent check: every stopped Agent is then protected and the receipt
warns `startup_store_unreadable`. On Windows an inventory read that fails after
the rollback restart is `rollback_failed` (`rollback_verification_unconfirmed`),
never a silent success. The Windows
updater applies the same live-row rule and warnings but has no startup-store
persistence check. An interrupted caller
can resume polling with the printed job path:

```sh
python3 scripts/update-machine.py --machine mariana --resume /home/USER/.local/share/treer-updates/job-ID
```

Job and backup receipts contain only versions, paths, workspace/server
identifiers and process metadata; existing operator credentials are read only
in memory for the loopback inventory request. A leftover
`.treer-machine-update.lock` after a power failure is not removed automatically:
first verify that no updater is running, then resolve it under operator control.

**Host update terminates every Agent and PTY owned by that Host**, including a
managed caller on that machine. Obtain explicit approval for that impact first:

```sh
python3 scripts/update-machine.py --machine mariana --workspace ws_… \
  --host --acknowledge-agent-interruption --apply
```

The Host path remains detached and uses the same clean environment. Cold
recovery checks stable Agent IDs and new PIDs/generations for previously live
Agents. Binary rollback can restore the prior software, but cannot undo the
interruption or restore original process PIDs. No machine update is performed
by repository tests; run `python3 -m unittest discover -s scripts/tests -p
 'test_update_machine.py'` for fake Controller/CLI, checksum, transport,
activation-order, detachment and rollback checks.

### Windows split-step manual machine update

For a reviewed Canary set, use `scripts/update-machine.ps1` from a trusted local
copy. This is separate from signed release-channel updates: same-origin
`SHA256SUMS` is pinned to the file's SHA-256 copied from the independently
delivered, trusted release receipt (`-SumsSha256`), before parsing or executing
any downloaded binary. It still does not provide a publisher signature. It uses
PowerShell 5.1/7 and .NET; no git, Python or shell launcher is required. It never
updates or restarts the Host, changes the enrolled root, or restarts Agents.
Only one local detached workspace install may share the binary directory.

Use the machine account's real `LOCALAPPDATA`, `SystemRoot`, `windir`,
`USERPROFILE`, `USERNAME`, `APPDATA`, `ProgramData`, `HOMEDRIVE`/`HOMEPATH`,
`ComSpec`, `PATHEXT`, `PATH`, `TEMP` and `TMP`. The fixed exec environment
preserves this Windows baseline while excluding workload credentials, provider
settings and loader hooks. Updating an old Controller does not itself repair a
polluted invocation environment: stage from an ordinary trusted user PowerShell
terminal if the old exec path lacks these values. Do not forward Agent secrets
or invent user directories. Node, PowerShell crypto and tasklist failures can
indicate a missing baseline rather than a broken application. Node's trusted
absolute executable path may be needed when it is absent from PATH.

```powershell
# Read-only plan; no network or process changes.
.\update-machine.ps1
# Fill these from the reviewed release and this machine's installed service.
$commit = 'FULL_40_CHARACTER_SOURCE_SHA'
$sums = 'WINDOWS_SHA256SUMS_FILE_SHA256_FROM_TRUSTED_RELEASE_RECEIPT'
$config = "$env:LOCALAPPDATA\Treer\agent-servers\SERVER-controller.json"
$bin = "$env:LOCALAPPDATA\Treer\bin"
$stage = "$env:LOCALAPPDATA\Treer\stage-$commit"
New-Item -ItemType Directory -Force "$env:LOCALAPPDATA\Treer" | Out-Null
.\update-machine.ps1 -Action Prepare -Commit $commit -SumsSha256 $sums -ConfigPath $config -BinDirectory $bin
.\update-machine.ps1 -Action Download -StageDirectory $stage -Binary treer-agent-server
.\update-machine.ps1 -Action Download -StageDirectory $stage -Binary treer
# Run Apply in an independent local user terminal, NEVER through treer exec.
.\update-machine.ps1 -Action Apply -StageDirectory $stage -LocalTerminal
# Query in a new invocation after the Controller reconnects.
.\update-machine.ps1 -Action Status -StageDirectory $stage
```

Each download is separate and deadline-bounded for the 30-second exec limit.
Prepare, Download and Status may run through exec. Apply must run from an
independent local user terminal with `-LocalTerminal`; it verifies that the
process is outside every Windows Job and otherwise refuses activation. Neither
Start-Process nor DETACHED_PROCESS proves escape from a Controller/Host Job.
Apply submits a clean-environment PowerShell worker with a process-scoped
`-ExecutionPolicy Bypass` for its private script copy (no machine policy change);
do not synchronously wait
on `restart-controller` through the Controller being replaced. `submitted` is not success. The private stage holds progress
and an atomic `result.json`; poll until `complete`, `rolled_back`,
`rollback_failed` or `failed`. Never retry activation blindly after a lost reply.
If a worker disappears, inspect the receipt, running Controller build and backup
locally before taking further action. A machine sleep, power loss or worker
termination is not an automatically recoverable transaction.

The worker checks staged hashes and source commits before mutation, saves both
old executables in a private `backup-<oldsha>-<unique-id>` directory, renames
running exe files to unique `.old` names and installs the replacement. It replaces
Controller first, requests only `service --workspace ... restart-controller`,
then requires a fresh Controller epoch, matching machine/workspace/build,
Proxy connection, path-aware capability and preserved live Agents (identical
PID/generation for every Agent that had a process; exited in-memory records are
not relisted and are reported in the receipt). Only then does it replace CLI. Failure restores saved binaries and verifies the
old Controller's readiness and Agent inventory; `rollback_failed` requires local
operator repair. Preserve backups and `.old` images until verified and no
process uses them. No automatic cleanup or root migration occurs. In particular,
existing `C:\Windows\System32` roots remain unchanged; new enrollment must use
an explicit user-owned `--root` as described above.

Receipts contain only status, commit, preserved Host commit, backup path and
preservation flags. Record the old Host/new Controller compatibility result in
the release receipt; leaving the Host unchanged depends on that protocol
compatibility. After the rollback window, an operator may remove only inspected
`.old` files whose processes have exited and backups no longer needed; these
images can contain old vulnerabilities. Never remove a running image or the only
verified recovery copy. Local
operator credentials are read only in memory for loopback inventory and are
never passed in argv or written to progress/error logs. The worker strips
inherited Treer, HTTP proxy and Node/loader variables from child processes.
This protects accidental disclosure; the same Windows user can still alter
scripts, files and processes and is not an isolation boundary.

## Voice APIs (opt-in)

The native iOS/Android clients were retired on 2026-10-16. Core Voice ASR and
command routes remain available to authenticated clients; this configuration
does not install a Voice UI. Native-client Bearer/session compatibility also
remains; browser sessions and CSRF checks are unchanged.

### Voice ASR

ASR is enabled when the **Proxy process** has
`TREER_VOICE_ASR_PROVIDER=qwen` and `TREER_VOICE_ASR_API_KEY` or
`DASHSCOPE_API_KEY` (shell env, Compose `.env`, or the process supervisor). Do
not put these in client code or browser storage.
`GET /api/workspaces/{workspace_id}/voice/asr` reports configuration/status;
stream 16 kHz PCM16 audio over
`GET WS /api/workspaces/{workspace_id}/voice/asr/stream`

(`Authorization: Bearer`). Audio is forwarded to Qwen
`qwen3-asr-flash-realtime` and is not stored in Core. Optional
`TREER_VOICE_ASR_URL` (default Singapore
`wss://dashscope-intl.aliyuncs.com/api-ws/v1/realtime`) and
`TREER_VOICE_ASR_MODEL`. Unconfigured deployments report that ASR is unavailable.
Use a Beijing key only with `wss://dashscope.aliyuncs.com/api-ws/v1/realtime`;
keys are region-specific.

### Voice command

ASR text is turned into Treer actions on the **Proxy**, not the client. Enable
with `TREER_VOICE_LLM_API_KEY`. Optional:

- `TREER_VOICE_LLM_BASE_URL` (default `https://sub.lnz-study.com`)
- `TREER_VOICE_LLM_WIRE_API` (`responses` or `completions`, default `responses`)
- `TREER_VOICE_LLM_MODEL` (default `gpt-5.6-luna`)

`GET /api/workspaces/{workspace_id}/voice/command` reports availability.
An authenticated client submits the utterance with
`POST /api/workspaces/{workspace_id}/voice/command`

`{"text":"...","history":[{"role":"user","text":"..."},{"role":"assistant","text":"..."}]}`
→ `{"reply":"...","utterance":"...","tools":[...]}`. `history` is optional and
is the prior spoken turns in the client, not including the current
`text`. The Proxy keeps at most the last 12 user/assistant turns.

The LLM receives the bundled voice skill (`skills/treer-voice/SKILL.md`) plus a
live workspace roster and that spoken history. It may call a `treer` tool whose argv matches
`status`, `whoami`, `machine list`, and `agent ls|show|prompt|read`. Those
commands run in-process under the signed-in user. Clients choose how to present
the returned `reply`; no native TTS client is shipped in this repository.
Vendor LLM keys stay in the Proxy process. This is not the later Voice Live
confirm-card protocol.
