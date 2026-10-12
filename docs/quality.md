# Quality And Maintenance

For ordinary development, run only the focused checks that cover the changed
boundary. PostgreSQL-backed tests are optional unless the change directly
touches Proxy persistence, authentication, membership, Policy, or Core Message.
Do not start a database solely to satisfy a routine handoff; state which checks
were run and whether database-backed coverage was omitted.

Windows native artifact builds exercise ConPTY without an attached browser.
The Host answers the initial cursor inheritance query required by portable-pty
so child output can begin. The runtime tests cover split query bytes and the
native Windows test covers argv, environment, input, resize, and process stop.

### Service environment and recovery validation

Updater executable staging closes and syncs the writable file before executing
validation. Unix validation retries only transient `ETXTBSY` within the existing
ten-second validation budget; permission and format errors fail immediately.
Linux regression coverage holds a real writable descriptor, confirms `ETXTBSY`,
then closes it and verifies validation succeeds. Isolated subprocess tests cover
both umask 022 and 077 without changing the Rust test runner's process-wide umask;
installation preserves restrictive user permissions rather than forcing 0755.

The shared `treer-platform` environment filter is used by nohup, foreground and
Windows detached launchers, daemon entry points (including launchd/systemd),
Host-spawned Controllers on crash/hot update, and PTY workload inheritance.
Generated launchd/systemd registrations contain configuration paths, not caller
environment snapshots. Explicit workload environment is applied after filtering.
An independent PTY subprocess regression starts from a polluted Host runtime
and verifies that old UI/identity variables are removed while a fresh explicit
workload identity and proxy entry survive.

`real_service_environment_boundary` runs isolated real Host/Controller binaries
with polluted identity/UI variables and a dead loopback proxy. It checks daemon
environment names, restores both Agent and App startup intent through a mock
Proxy, survives transient validation failures without reconnecting, and hot
restarts the Controller without replacing the workloads. It uses private
fixture state and never resolves installed services. Build sibling binaries
first, then run:

```sh
cargo build -p treer-agent-host -p treer-agent-server
cargo test -p treer-agent-server real_service_environment_boundary -- --ignored
```

Run this subprocess fixture on macOS and Linux; Windows cross-target checking
only establishes compilation, not native process-environment behavior.

### Virtual-host routing revisions

Virtual-host database polling preserves the routing revision when projected
content is unchanged. The PostgreSQL regression covers repeated empty and
populated refreshes, a remote service-port change, and deletion leaving an empty
workspace cache entry. Revision changes are routing changes, not polling ticks;
the revision remains local to a Proxy process rather than a durable global clock.

### Output retention and recovery

Controller inventory reconciliation must preserve the cached output and its
revision for the same process generation and stream epoch, including exited
Agent and App instances. An inventory-only update is not an empty transcript.
Previously it rebuilt exited entries with empty text on each recovery scan,
while retaining their nonzero output revision; short-lived Canary probes could
therefore finish successfully but appear to produce no output.

The Controller regression fixture covers output followed by exit, repeated Host
inventory scans, and a replacement Controller replaying exited Agent and App
output from a surviving Host. Same-run revisions cannot move backwards; a new
run must not inherit the previous run's text. Output updates and inventory
replacement share the existing entry lock to avoid losing concurrent output.

Host replay is bounded and in memory, not a durable terminal log archive. A
Controller hot restart can recover retained output from the surviving Host,
subject to replay/history limits. After a Host cold restart, output of exited
processes from the old Host is unavailable; restoring startup intent does not
restore that transcript. This limitation is separate from inventory incorrectly
erasing output that the current Host/Controller still retains.

Run the complete gate on demand or while preparing a release:

```sh
just test-db-up
export TREER_TEST_DATABASE_URL=postgres://treer:treer@127.0.0.1:55432/treer_test
just check
```

The complete gate is not a prerequisite for every local change. When its
database coverage is relevant and Docker is unavailable, record that the
PostgreSQL-backed workspace gate was skipped. Do not point
`TREER_TEST_DATABASE_URL` at a shared or production database just to make the
local gate pass.

Pull requests and pushes to `main` run [CI](../.github/workflows/ci.yml).
The Linux Rust job builds the workspace, checks formatting, runs
`cargo test --workspace --locked` against a healthy PostgreSQL 17 service, and
runs Clippy for all targets with warnings denied. Each database test uses its
own schema; connection failures fail the job rather than skipping coverage.
An independent Node 22 / pnpm job installs the frozen frontend lockfile and runs
`pnpm typecheck` and `pnpm build`. CI requires no deployment credentials and
does not publish artifacts or deploy. Native macOS/Windows PR gates and browser
end-to-end CI remain separate gaps.

Use Slurm for CPU- or memory-heavy builds, Clippy runs, frontend builds, and
non-database workspace tests when the checkout and toolchain are available on
the compute nodes. Slurm does not remove the PostgreSQL requirement: the job
must still receive an isolated `TREER_TEST_DATABASE_URL`, either from a
cluster-provided database or from a supported container runtime. If neither is
available on the allocated node, skip the PostgreSQL-backed tests and report
that limitation instead of treating the partial run as the complete gate.

When explicitly requested, `just check` verifies documentation links, release
tooling, the control-plane frontend, updater tests, the shared Agent launcher,
Rust build/format/tests, and Clippy with warnings denied. Other Workspace Apps
are intentionally outside the release gate; run their focused
checks when changing them. Focused commands are:

```sh
just app-test
just agent-launcher-test
just updater-test
just messaging-e2e
just web-test
just ais-e2e
just service-canary
just machine-artifacts-test
node --test scripts/install-path.test.mjs
cargo test -p treer-proxy message_
cargo test -p treer-proxy -- updater::
cargo test -p treer-proxy -- admin_update_
node scripts/check-docs.mjs
```

## Evidence Matrix

| Area | Present evidence | Remaining gap |
| --- | --- | --- |
| Rust | Linux PR workspace build/tests with PostgreSQL, format, strict Clippy | No native macOS/Windows PR gate; first GitHub run pending |
| Frontend | Control-plane and Mail typecheck/build; mocked desktop/mobile browser workflows | No screenshot visual regression suite |
| Core Message | Store/API/CLI tests for DAG visibility, delivery, idempotency, Policy revision, body-free outbox, and migration | No retention/export/delete, attachments, or load/failure suite |
| App identity | OAuth/PKCE, audience, membership/service invalidation, directory, App Message routes, 60-second browser forwarding and sanitization; Issues iframe/ingress workflows at desktop/390px through a local gateway double | No refresh-token contract or deployed automatic-identity browser acceptance |
| Unified workload lifecycle | Stable identity/generation, single Controller recovery owner, real Host cold/hot recovery and concurrent start, stopped-intent persistence, legacy App migration, and mixed browser tree/lifecycle | PTY-backed runtime; no health check, deployment revision, or durable terminal log archive |
| Mail | Python HTTP contract, frontend build, resumable SQLite/PostgreSQL migration | No unattended real-browser audit |
| Telegram | Fake Bot API, mapping, ack crash ordering, restart, rate-limit and ambiguous-send tests | No live Telegram canary, webhook, or active-active mode |
| Distribution | NATS event/outbox and multi-Proxy routing tests, including terminal epoch/cursor delivery and bidirectional network payload/accounting | No automated partition/failure CI |
| Host supervision | nohup default and PID identity, explicit systemd selection, partial-unit repair, stale-registration cleanup, TUI/Web diagnostics, and cold-Host restoration and explicit service probes | No normal macOS lifecycle CI; no Apple container machine setup CI |
| Machine connection UX | Service workspace listing, `/api/health` `proxy_connected`, duplicate/stale reconnect, real NATS lease-expiry recovery and replacement fencing, Proxy ping idle close, connect reuse, bind conflict, explicit plain-mode migration and service forwarding | NATS tests require isolated `TREER_TEST_NATS_URL`; no physical lid-close in CI; record a manual macOS sleep/wake check after deploy |
| Release | Five-platform metadata/checksums, native Windows build/test job, GHCR publish workflow, signed-manifest Node tests, and Controller signed-channel/manifest plus binary SHA256 verification | Windows runner results pending; no updater downgrade/freshness enforcement; trusted release key must be provisioned locally |
| Self-host update | Updater unit tests; Proxy admin forward tests; `/admin` e2e | No unattended Compose apply against live Docker |
| Security | Explicit trust tier and Policy tests | Allow-by-default Policy; external isolation is operator-owned |

## Review Triggers

Native Windows changes require the Windows MSVC job in
`.github/workflows/build-release-artifacts.yml`: local IPC reconnect and owner
checks, private state replacement, PID identity, ConPTY argv/input/resize/stop,
real Controller/CLI auth, detached Host idempotency and process survival across
Controller replacement, plus native installer checksums/PATH. A cross-target
`cargo check` or Clippy run on macOS only establishes compilation. Real Windows
enrollment, HTTP service routing, App deployment, reboot recovery, and descendant
cleanup under failure still require native acceptance before announcing support.
Windows in-place binary update, automatic login tasks, and system services are
outside the initial implementation.

| Change | Minimum focused evidence |
| --- | --- |
| Shared protocol | Round-trip tests, legacy registration negotiation, unknown-command connection survival, plus affected endpoints |
| Proxy auth or membership | Authorization, revocation, and cross-workspace tests |
| Core Message | DAG/visibility, delivery, idempotency, ack, Policy, outbox body exclusion, and migration tests |
| Mail or Telegram | App unit tests, external API fixture, restart/migration, and frontend build when applicable |
| Host/Controller lifecycle | `just service-canary` plus idempotency and process-survival tests |
| Unified App/Agent lifecycle | Stable service/identity transaction, generation fencing, concurrent start/recovery, explicit stop persistence, legacy adoption, CLI parsing, mixed tree persistence and desktop/mobile workflow |
| Network or ingress | Authentication/header, streaming, WebSocket, endpoint ownership, Agent cross-owner denial, and Canary coverage |
| Workload HTTP entries | Per-entry signed identity, inherited environment, credential/header stripping, denied opens, HTTP/WebSocket/CONNECT, name resolution/rename with stable Policy IDs, stop cleanup, and hot/cold restoration |
| Browser workflow | Typecheck/build, CORS/return-path checks, desktop and mobile validation |
| Agent Interface adapter | Adapter unit tests plus `just ais-e2e` when a live Treer and vendor binary are available |
| Voice ASR or command | Proxy `voice` / `voice_llm` tests plus authenticated `/voice/command` route tests; optional `TREER_VOICE_LLM_LIVE_TEST=1` against a configured upstream |
| Native iOS/Android fleet | `just mobile-ios-ci`, `just mobile-android-ci`, Android `CreateFlowTest`, iOS `CreateFlowTests` / `TreerUITests`; live AOSP + iOS simulator login/create-machine/create-agent/prompt when a Proxy and Host are available |
| Documentation | `node scripts/check-docs.mjs` |
| Release publishing | `just machine-artifacts-test`, `node --test scripts/release-r2.test.mjs`; hosted downloads must match all twelve binaries without redirects; isolated R2 verification when publishing signed manifests |
| Installed Controller updates | Node/Rust signature compatibility, tampered/missing metadata, trusted-key binding, channel/manifest/path binding, duplicate/missing artifacts, size limits, binary SHA256 failures before execution, and existing staging/recovery tests |
| Self-host GHCR or updater | `just updater-test`, Proxy admin update tests, and `/admin` e2e |

Documentation describes the current source, not future intent. Dated research
is historical and does not override implementation, protocol types, or tests.

Retired transparent-network labs under `scripts/network-lab/` and dated research
are historical fixtures, not supported acceptance gates. The native macOS helper
and its build gate have been removed.

Current focused transport and recovery checks:

```sh
cargo test -p treer-agent-server network
cargo test -p treer-agent-server workload_proxy
cargo test -p treer-agent-server --test agent_address
# Optional real HTTP client check; requires curl on PATH:
cargo test -p treer-agent-server curl_uses_inherited_proxy -- --ignored
cargo test -p treer-agent-server recovery
cargo test -p treer-cli --test session
cargo test -p treer-proxy network
```

The subprocess fixtures require freshly built sibling executables:

```sh
cargo build -p treer-agent-host -p treer-agent-server -p treer-cli
cargo test -p treer-agent-server recovery_real_host_cold_restart -- --ignored
cargo test -p treer-cli --test controller -- --ignored
```

The local Linux Docker regression uses real authentication, PostgreSQL, NATS,
two Proxies, and non-root machines. It checks installation dependency preflight,
installed CLI discovery, Agent messages, explicit service forwarding/publication,
App access, per-workload HTTP identities, verified local HTTPS/CONNECT,
WebSocket upgrades, listener cleanup, and hot/cold restoration:

```sh
docker build -t treer-local-e2e:fixed .
node scripts/docker-core-e2e.mjs
```

Requires Node 20+ and Docker; `TREER_E2E_IMAGE` can select another freshly built
image. The runner downloads Debian dependencies, binds host listeners only to
loopback, and stops its own containers in a finalizer. It retains stopped test
containers, the network, and logs/results in the printed temporary directory for
inspection; `resources.json` lists their exact names. It does not deploy Canary
or touch existing containers. Fixtures use Python services, without vendor Agent
CLIs. Browser, public DNS/TLS, and OS boot-manager checks remain separate.

Proxy persistence tests require an isolated `TREER_TEST_DATABASE_URL`. Set
`TREER_TEST_NATS_URL` to an isolated broker for cross-Proxy tests; without it,
a test that returns early is not distributed-path evidence. Configured raw-TCP
allocation tests additionally require an isolated port range and are ignored by
default. Local cold-Host tests do not prove physical reboot, OS boot-manager,
public DNS/TLS, or production firewall behavior; record those separately.
