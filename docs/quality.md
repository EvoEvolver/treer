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
| Host supervision | nohup default and PID identity, explicit systemd selection, partial-unit repair, stale-registration cleanup, TUI/Web diagnostics, and manual Apple machine capture/service probes | No normal macOS lifecycle CI; no Apple container machine setup CI |
| Machine connection UX | Service workspace listing, `/api/health` `proxy_connected`, duplicate/stale reconnect, Proxy ping idle close, connect reuse, bind conflict, Linux namespace probing with `proxy-env` fallback, `proxy-env` local Direct classification | No physical lid-close in CI; record a manual macOS sleep/wake check after deploy |
| Release | Four-platform metadata/checksums, GHCR publish workflow, and signed-manifest Node tests | Installed machine updater does not enforce signatures |
| Self-host update | Updater unit tests; Proxy admin forward tests; `/admin` e2e | No unattended Compose apply against live Docker |
| Security | Explicit trust tier and Policy tests | Missing allow-by-default hardening and production isolation backend |

## Review Triggers

| Change | Minimum focused evidence |
| --- | --- |
| Shared protocol | Round-trip tests, legacy registration negotiation, unknown-command connection survival, plus affected endpoints |
| Proxy auth or membership | Authorization, revocation, and cross-workspace tests |
| Core Message | DAG/visibility, delivery, idempotency, ack, Policy, outbox body exclusion, and migration tests |
| Mail or Telegram | App unit tests, external API fixture, restart/migration, and frontend build when applicable |
| Host/Controller lifecycle | `just service-canary` plus idempotency and process-survival tests |
| Managed App lifecycle | Stable service/vhost transaction, direct runtime launch, reconnect/restart behavior, CLI parse, and browser workflow |
| Network or ingress | Authentication/header, streaming, WebSocket, containment, Agent mutation denial, and Canary coverage |
| Browser workflow | Typecheck/build, CORS/return-path checks, desktop and mobile validation |
| Agent Interface adapter | Adapter unit tests plus `just ais-e2e` when a live Treer and vendor binary are available |
| Voice ASR or command | Proxy `voice` / `voice_llm` tests plus authenticated `/voice/command` route tests; optional `TREER_VOICE_LLM_LIVE_TEST=1` against a configured upstream |
| Native iOS/Android fleet | `just mobile-ios-ci`, `just mobile-android-ci`, Android `CreateFlowTest`, iOS `CreateFlowTests` / `TreerUITests`; live AOSP + iOS simulator login/create-machine/create-agent/prompt when a Proxy and Host are available |
| Documentation | `node scripts/check-docs.mjs` |
| Release publishing | `node --test scripts/release-r2.test.mjs` plus isolated R2 verification |
| Self-host GHCR or updater | `just updater-test`, Proxy admin update tests, and `/admin` e2e |

Documentation describes the current source, not future intent. Dated research
is historical and does not override implementation, protocol types, or tests.

The opt-in [network lab](../scripts/network-lab/README.md) distinguishes actual
Linux sandbox capture from its SOCKS fixture's Policy/counters. Its native
macOS candidate requires system-extension approval and is not a supported
transparent Controller backend. A separate live probe uses real Mac Hosts,
two Proxies, an isolated NATS broker, and a disposable PostgreSQL database to
check virtual-host identity, Policy, and persisted relay usage. Its optional
half-close case records a known upstream failure. The [2026-09-05 report](research/2026-09-05-macos-transparent-network.md)
records the versions and limits. Run the NATS test with `TREER_TEST_NATS_URL`
set to a development broker to exercise the distributed path; without it the
test returns early and its `ok` result is not distributed evidence.

Run `python3 scripts/check-macos-network.py --developer-dir FULL_XCODE_DEVELOPER_DIR`
for the owned native backend's account-free gate: real TCP and copier tests,
kernel audit-token/registration tests, unsigned app build and bundle checks.
The script does not install or activate an extension. Run
`cargo test -p treer-agent-server network` for offline rejection, stale Open
suppression on reconnect, Direct reset, half-close and shared-port ingress.
`cargo test -p treer-proxy agent_socket` covers reauthorization and legacy Open
compatibility. The live lab's `--revoke-active --local-target` variant verifies
Policy revocation over two real Proxies/NATS using a loopback target; it does
not verify Apple guest reachability or the owned signed extension.

The Controller network tests also cover UDP boundaries (including empty packets),
real UDP loopback, Direct byte/datagram counts, denial and reset. The native check
covers private checkpoint permissions, same-boot restore, stale identity rejection
and TCP-carried datagram framing. These are not installed-extension recovery proofs.
Add `--datagram` to the live lab to exercise the owned UDP Controller contract
through two Proxies/NATS and a UDP echo service, including Policy revocation and
machine/Agent ledger rows. This variant does not use OS capture. Run
`cargo test -p treer-proxy traffic::tests` with `TREER_TEST_DATABASE_URL` for
Direct deduplication, failed-flush retention and separate Agent detail storage.

The current native gate passes eight Swift core tests and seven platform tests,
including UDP/TCP DNS queries on real loopback sockets and resolver-file recovery
in a temporary directory. No system resolver files are changed by that gate.
`cargo test -p treer-proxy network_schema_upgrades` with the test database verifies
upgrades from the old TCP/HTTP and relay-only ledger constraints. Controller network
coverage also includes persistent-report restart, stale acknowledgements and
concurrent clients with a stalled handshake. The Direct lab variant additionally
checks committed final receipts and an empty outbox after acknowledgement.
