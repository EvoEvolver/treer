# Quality And Maintenance

For ordinary development, run only the focused checks that cover the changed
boundary. PostgreSQL-backed tests are optional unless the change directly
touches Proxy persistence, authentication, membership, Policy, or Core Message.
Do not start a database solely to satisfy a routine handoff; state which checks
were run and whether database-backed coverage was omitted.

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

Use Slurm for CPU- or memory-heavy builds, Clippy runs, frontend builds, and
non-database workspace tests when the checkout and toolchain are available on
the compute nodes. Slurm does not remove the PostgreSQL requirement: the job
must still receive an isolated `TREER_TEST_DATABASE_URL`, either from a
cluster-provided database or from a supported container runtime. If neither is
available on the allocated node, skip the PostgreSQL-backed tests and report
that limitation instead of treating the partial run as the complete gate.

When explicitly requested, `just check` verifies documentation links, release
tooling, the control-plane frontend, updater tests, the isolated ACP launcher,
Rust build/format/tests, and Clippy with warnings denied. Other Workspace Apps
and AIS adapters are intentionally outside the release gate; run their focused
checks when changing them. Focused commands are:

```sh
just app-test
just acp-launcher-test
just updater-test
just messaging-e2e
just web-test
just ais-e2e
just service-canary
cargo test -p treer-proxy message_
cargo test -p treer-proxy -- updater::
cargo test -p treer-proxy -- admin_update_
node scripts/check-docs.mjs
```

## Evidence Matrix

| Area | Present evidence | Remaining gap |
| --- | --- | --- |
| Rust | Workspace tests, format, strict Clippy | No normal cross-platform PR CI |
| Frontend | Control-plane and Mail typecheck/build; mocked desktop/mobile browser workflows | No screenshot visual regression suite |
| Core Message | Store/API/CLI tests for DAG visibility, delivery, idempotency, Policy revision, body-free outbox, and migration | No retention/export/delete, attachments, or load/failure suite |
| App identity | OAuth/PKCE, audience, membership/service invalidation, directory, and App Message routes | No refresh-token contract or unattended browser test |
| Managed App lifecycle | Transactional service/vhost ownership, runtime replacement, direct launch, CLI parsing, frontend build, and mocked browser lifecycle | PTY-backed first adapter; no health check, deployment revision, or Host pipes supervisor |
| Mail | Python HTTP contract, frontend build, resumable SQLite/PostgreSQL migration | No unattended real-browser audit |
| Telegram | Fake Bot API, mapping, ack crash ordering, restart, rate-limit and ambiguous-send tests | No live Telegram canary, webhook, or active-active mode |
| Distribution | NATS event/outbox and multi-Proxy routing tests, including terminal epoch/cursor delivery and bidirectional network payload/accounting | No automated partition/failure CI |
| Host supervision | nohup default and PID identity, explicit systemd selection, partial-unit repair, stale-registration cleanup, TUI/Web diagnostics, and cold-Host restoration and explicit service probes | No normal macOS lifecycle CI; no Apple container machine setup CI |
| Machine connection UX | Service workspace listing, `/api/health` `proxy_connected`, duplicate/stale reconnect, Proxy ping idle close, connect reuse, bind conflict, explicit plain-mode migration and service forwarding | No physical lid-close in CI; record a manual macOS sleep/wake check after deploy |
| Release | Four-platform metadata/checksums, GHCR publish workflow, and signed-manifest Node tests | Installed machine updater does not enforce signatures |
| Self-host update | Updater unit tests; Proxy admin forward tests; `/admin` e2e | No unattended Compose apply against live Docker |
| Security | Explicit trust tier and Policy tests | Allow-by-default Policy; external isolation is operator-owned |

## Review Triggers

| Change | Minimum focused evidence |
| --- | --- |
| Shared protocol | Round-trip tests, legacy registration negotiation, unknown-command connection survival, plus affected endpoints |
| Proxy auth or membership | Authorization, revocation, and cross-workspace tests |
| Core Message | DAG/visibility, delivery, idempotency, ack, Policy, outbox body exclusion, and migration tests |
| Mail or Telegram | App unit tests, external API fixture, restart/migration, and frontend build when applicable |
| Host/Controller lifecycle | `just service-canary` plus idempotency and process-survival tests |
| Managed App lifecycle | Stable service/vhost transaction, direct runtime launch, reconnect/restart behavior, CLI parse, and browser workflow |
| Network or ingress | Authentication/header, streaming, WebSocket, endpoint ownership, Agent cross-owner denial, and Canary coverage |
| Workload HTTP entries | Per-entry signed identity, inherited environment, credential/header stripping, denied opens, HTTP/WebSocket/CONNECT, name resolution/rename with stable Policy IDs, stop cleanup, and hot/cold restoration |
| Browser workflow | Typecheck/build, CORS/return-path checks, desktop and mobile validation |
| Agent Interface adapter | Adapter unit tests plus `just ais-e2e` when a live Treer and vendor binary are available |
| Voice ASR or command | Proxy `voice` / `voice_llm` tests plus authenticated `/voice/command` route tests; optional `TREER_VOICE_LLM_LIVE_TEST=1` against a configured upstream |
| Native iOS/Android fleet | `just mobile-ios-ci`, `just mobile-android-ci`, Android `CreateFlowTest`, iOS `CreateFlowTests` / `TreerUITests`; live AOSP + iOS simulator login/create-machine/create-agent/prompt when a Proxy and Host are available |
| Documentation | `node scripts/check-docs.mjs` |
| Release publishing | `node --test scripts/release-r2.test.mjs` plus isolated R2 verification |
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
