# Quality and maintenance

- Status: maintained
- Last reviewed: 2026-08-21 at `07e02cd`

Treer keeps the local feedback loop as the canonical project gate. Add process
only when it removes repeated rediscovery or catches a demonstrated class of
regression.

## Canonical verification

Run the complete local gate from the repository root:

```bash
just test-db-up
just check
```

Proxy tests use isolated schemas in the Docker PostgreSQL instance. Override
`TREER_TEST_DATABASE_URL` when using a different local test database.

It executes:

```text
node scripts/check-docs.mjs
node --test scripts/check-plugins.test.mjs
node scripts/check-plugins.mjs
node --test scripts/release-r2.test.mjs
pnpm --dir web typecheck
pnpm --dir web build
pnpm --dir plugins/mail/web typecheck
pnpm --dir plugins/mail/web build
python3 -m unittest discover -s plugins/mail/tests -p 'test_*.py' -v
python3 -m unittest discover -s plugins/telegram/tests -p 'test_*.py' -v
cargo run -p treer-cli -- plugin validate plugins/mail
cargo run -p treer-cli -- plugin validate plugins/telegram
cargo build --workspace
python3 scripts/test-messaging-plugins-e2e.py
cargo fmt --all -- --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

The plugin boundary and real-process messaging checks also have focused recipes:

```bash
just plugin-boundary-test
just messaging-e2e
```

Run the closest focused check while iterating, then run the full gate before
handoff. The docs-only GitHub Actions workflow checks documentation structure
and links on relevant pull requests and pushes to `main`; it does not replace
the full local gate.

Before Production promotion, deploy the exact candidate revision and run the
black-box Canary gate:

```bash
just release-canary HEAD
```

The Canary workflow restarts two persistent Railway development-machine
deployments without replacing their image and
verifies the Cloudflare App, cross-machine virtual networking, wildcard public
ingress, and directional traffic accounting. Machine-image replacement is an
explicit `just test-canary-provision` operation, not part of a routine release.
The workflow produces the only manifest accepted by `just promote-production`.
See [Release process](releases.md).

## Documentation contract

- Root [AGENTS.md](../AGENTS.md) is a concise development map, not a manual.
- [docs/README.md](README.md) indexes maintained and historical knowledge.
- Stable product, roadmap, architecture, security, and quality facts live in
  focused maintained documents.
- [PLAN.md](../PLAN.md) and dated research are historical; label snapshots with
  their revision or review date.
- [`skills/treer/SKILL.md`](../skills/treer/SKILL.md) remains the embedded runtime
  contract for managed agents. Do not move it or duplicate it into AGENTS.md.
- Behavior, trust assumptions, commands, and ownership changes update the
  closest maintained document in the same pull request.
- Prefer source links and executable checks over prose that cannot be verified.
- Create a dated execution plan only for work that benefits from a durable
  decision and progress log. Archive its outcome when complete.

`node scripts/check-docs.mjs` verifies required entry points, index links,
relative Markdown targets, and the CLI's embedded-skill path. It intentionally
does not judge prose correctness; reviewers must compare claims with code and
tests.

## Current engineering evidence

At completion-gate revision `07e02cd`, the plugin boundary, Mail, Telegram,
formatting, strict Clippy, clean Docker build, and real-process messaging checks
pass. The E2E
harness starts an authenticated Proxy, isolated PostgreSQL database, two real
Host/Controller pairs, two workspaces, five command Agents, both plugins through
`treer plugin run`, and a fake Telegram Bot API. It verifies Core Message DAGs,
repeatable receive and ack, send idempotency across Proxy restart, Controller
restart identity, workspace isolation, SQLite and real-`psql` Mail migration,
Mail browser OAuth/API compatibility, Telegram policy denial/native replies,
plugin restart, and absence of Message bodies from outbox events and Proxy logs.
The source uses shared protocol crates, forbids unsafe Rust workspace-wide, and
treats Clippy warnings as errors.

| Area | Present evidence | Remaining gap |
| --- | --- | --- |
| Rust behavior | Workspace tests, format check, strict Clippy | No normal cross-platform PR CI |
| Frontend | Control-plane and Mail TypeScript typecheck/build plus container health/config routes | No real-browser desktop/mobile automation or visual regression test |
| Core Message | Store/API/CLI tests plus real Proxy/Controller/Host restart, DAG, ack, idempotency, policy, migration, log, and workspace-isolation E2E | No multi-Proxy/NATS Message routing E2E, retention/export/delete contract, or attachment support |
| Plugin boundary | Manifest validation, 11 positive/negative static-boundary tests, credential withholding, capability denial, and official-runner E2E | Same-UID hostile-code isolation, uninstall, and automatic state migration are not implemented |
| Mail and Telegram | Python unit suites, Mail frontend build, fake CLI/API fixtures, real brokered Mail OAuth/migration and Telegram reply/restart E2E | No real external Telegram canary, active-active plugin mode, webhook mode, or browser automation; an ambiguous Telegram send can duplicate externally |
| Architecture | Crate boundaries, shared protocol types, and first-party plugin source scan | No general Rust dependency-boundary lint |
| Documentation | Indexed maintained docs and mechanical link check | No freshness or source-claim automation |
| Release publishing | Native four-platform builds carry commit/platform metadata and checksums; Node tests cover complete artifact sets, deterministic manifests, detached signatures, and prepared-release immutability | No updater signature enforcement or automated R2 artifact rollout test |
| Operations | Structured tracing plus buffered directional machine traffic counters | No local metrics/traces harness or end-to-end performance assertions |
| Event and cluster distribution | Event-envelope, Message-specific transactional outbox, lease/snapshot separation, durable projection replay, and two-Proxy command/terminal/network integration tests | No generic transactional outbox, multi-Proxy Message E2E, or automated NATS failure CI |
| Security | Explicit trust tier, Message/plugin policy checks, static plugin boundary, and source-level tests | Allow-all policy when no document exists, same-UID plugin exposure, and no production isolation backend |
| Accounting | Transactional organization-management audit, best-effort lifecycle events, and hourly directional machine traffic | Runtime audit has no transactional outbox; no quota or billing ledger |

This is a backlog of evidence gaps, not a promise that all items belong in the
next release. Promote an item into an implementation plan when it becomes a
repeated bottleneck or blocks the current product tier.

## Review triggers

| Change surface | Minimum focused evidence |
| --- | --- |
| Shared protocol or frames | Round-trip/unit tests plus all affected endpoints |
| Proxy auth, membership, or routing | Authorization and cross-workspace isolation tests |
| Host mutation or Controller restart | Idempotency and process-survival tests |
| Core Message model, route, store, or CLI | DAG/visibility, per-recipient delivery, idempotency, acknowledgement, policy, outbox body-exclusion, restart, and cross-workspace tests; run `just messaging-e2e` |
| Plugin manifest, broker, install, or runner | Manifest/unit tests, credential/capability denials, `just plugin-boundary-test`, package validation, and `just messaging-e2e` |
| Mail or Telegram adapter | Package unit tests, frontend build when applicable, external API fixture coverage, mapping/ack crash ordering, migration/restart tests, and `just messaging-e2e` |
| Service identity, runtime path, or connection ownership | Hostname/server-scoping tests, generated service-manager assertions, and duplicate-Controller fencing tests |
| Runtime path logic | Working-directory containment and escape tests |
| Network namespace, DNS, SOCKS, virtual host, or public ingress | Unit/integration coverage plus the two-machine Railway Canary workflow; add focused checks for changed authentication/header, streaming, WebSocket, or containment behavior |
| Domain event or NATS adapter | Envelope/subject tests plus real JetStream persistence and two-Proxy routing checks |
| Browser interaction | Typecheck/build plus App-to-Proxy CORS, runtime config, and affected-flow validation |
| Documentation/index change | `node scripts/check-docs.mjs` |
| Release manifest, signing, channel, or upload flow | `node --test scripts/release-r2.test.mjs` plus an isolated R2 canary publish and public download verification |

Periodic maintenance should compare routes, protocol types, launch flags, and
the database schema with the maintained docs, then correct drift in small
changes. Do not grow AGENTS.md or PLAN.md as catch-all knowledge stores.
