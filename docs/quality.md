# Quality And Maintenance

Start the isolated PostgreSQL test service, then run the complete gate:

```sh
just test-db-up
export TREER_TEST_DATABASE_URL=postgres://treer:treer@127.0.0.1:55432/treer_test
just check
```

The complete gate is a release and CI requirement, not a prerequisite for every
local change. When Docker is unavailable, run the focused checks that cover the
change and record that the PostgreSQL-backed workspace gate was skipped. Do not
point `TREER_TEST_DATABASE_URL` at a shared or production database just to make
the local gate pass.

Use Slurm for CPU- or memory-heavy builds, Clippy runs, frontend builds, and
non-database workspace tests when the checkout and toolchain are available on
the compute nodes. Slurm does not remove the PostgreSQL requirement: the job
must still receive an isolated `TREER_TEST_DATABASE_URL`, either from a
cluster-provided database or from a supported container runtime. If neither is
available on the allocated node, skip the PostgreSQL-backed tests and report
that limitation instead of treating the partial run as the complete gate.

`just check` verifies documentation links, release tooling, the control-plane
and Mail frontends, Mail/Telegram tests, Rust build/format/tests, and Clippy with
warnings denied. Focused commands are:

```sh
just app-test
just messaging-e2e
just web-test
cargo test -p treer-proxy message_
node scripts/check-docs.mjs
```

## Evidence Matrix

| Area | Present evidence | Remaining gap |
| --- | --- | --- |
| Rust | Workspace tests, format, strict Clippy | No normal cross-platform PR CI |
| Frontend | Control-plane and Mail typecheck/build; mocked desktop/mobile browser workflows | No screenshot visual regression suite |
| Core Message | Store/API/CLI tests for DAG visibility, delivery, idempotency, Policy revision, body-free outbox, and migration | No retention/export/delete, attachments, or load/failure suite |
| App identity | OAuth/PKCE, audience, membership/service invalidation, directory, and App Message routes | No refresh-token contract or unattended browser test |
| Mail | Python HTTP contract, frontend build, resumable SQLite/PostgreSQL migration | No unattended real-browser audit |
| Telegram | Fake Bot API, mapping, ack crash ordering, restart, rate-limit and ambiguous-send tests | No live Telegram canary, webhook, or active-active mode |
| Distribution | NATS event/outbox and multi-Proxy routing tests | No automated partition/failure CI |
| Release | Four-platform metadata/checksums and signed-manifest Node tests | Installed updater does not enforce signatures |
| Security | Explicit trust tier and Policy tests | Missing allow-by-default hardening and production isolation backend |

## Review Triggers

| Change | Minimum focused evidence |
| --- | --- |
| Shared protocol | Round-trip tests plus affected endpoints |
| Proxy auth or membership | Authorization, revocation, and cross-workspace tests |
| Core Message | DAG/visibility, delivery, idempotency, ack, Policy, outbox body exclusion, and migration tests |
| Mail or Telegram | App unit tests, external API fixture, restart/migration, and frontend build when applicable |
| Host/Controller lifecycle | Idempotency and process-survival tests |
| Network or ingress | Authentication/header, streaming, WebSocket, containment, and Canary coverage |
| Browser workflow | Typecheck/build, CORS/return-path checks, desktop and mobile validation |
| Documentation | `node scripts/check-docs.mjs` |
| Release publishing | `node --test scripts/release-r2.test.mjs` plus isolated R2 verification |

Documentation describes the current source, not future intent. Dated research
is historical and does not override implementation, protocol types, or tests.
