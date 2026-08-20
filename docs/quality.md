# Quality and maintenance

- Status: maintained
- Last reviewed: 2026-08-18

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
node --test scripts/release-r2.test.mjs
pnpm --dir web typecheck
pnpm --dir web build
cargo fmt --all -- --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
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

At base revision `72921f1`, the full gate passed with 118 Rust tests plus the
frontend typecheck and production build. The source uses shared protocol crates,
forbids unsafe Rust workspace-wide, and treats Clippy warnings as errors.

| Area | Present evidence | Remaining gap |
| --- | --- | --- |
| Rust behavior | Workspace tests, format check, strict Clippy | No normal cross-platform PR CI |
| Frontend | TypeScript typecheck, production build, and standalone container health/config routes | No checked-in browser workflow or visual regression test |
| Architecture | Crate boundaries and shared protocol types | No dependency-boundary lint |
| Documentation | Indexed maintained docs and mechanical link check | No freshness or source-claim automation |
| Release publishing | Node tests cover version validation, complete artifact sets, deterministic manifests, detached signatures, and prepared-release immutability | No cross-platform artifact provenance, updater signature enforcement, or automated canary rollout test |
| Operations | Structured tracing plus buffered directional machine traffic counters | No local metrics/traces harness or end-to-end performance assertions |
| Event and cluster distribution | Event-envelope, lease/snapshot separation, durable projection replay, and two-Proxy command/terminal/network integration tests | No crash-safe domain-event outbox or automated NATS failure CI |
| Security | Explicit trust tier and source-level tests | Allow-all policy and no production isolation backend |
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
