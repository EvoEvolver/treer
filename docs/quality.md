# Quality and maintenance

How to verify a change, what CI covers, and where evidence is still missing.

## Verify a change

Run the focused checks for the boundary you changed; reserve the full gate for
release preparation or an explicit request. Record what you ran and what you
skipped (for example the PostgreSQL-backed tests when Docker is unavailable).

```bash
just test-db-up       # PostgreSQL 17 on 127.0.0.1:55432 (--shm-size=1g)
export TREER_TEST_DATABASE_URL=postgres://treer:treer@127.0.0.1:55432/treer_test
just check            # everything CI runs
```

`just check` runs the release-tooling and docs checks (`just scripts-test`),
Console typecheck/build/e2e, updater tests, the Agent launcher tests, and the
Rust workspace build, format, tests and Clippy with warnings denied. Each
database test uses its own schema. Never point `TREER_TEST_DATABASE_URL` at a
shared or production database. If PostgreSQL fails with "could not resize
shared memory segment", recreate the container with
`just test-db-down && just test-db-up`.

Set `TREER_TEST_NATS_URL` to an isolated broker for cross-Proxy tests; without
it those tests return early and are not distributed-path evidence.

| Change | Minimum focused evidence |
| --- | --- |
| Shared protocol | Round-trip tests, capability negotiation, unknown-command connection survival, affected endpoints |
| Proxy auth, membership, Policy | Authorization, revocation and cross-workspace tests (`cargo test -p treer-proxy`) |
| Core Message | `cargo test -p treer-proxy message_`, `just messaging-e2e` |
| Host/Controller lifecycle | `just service-canary`, `cargo test -p treer-agent-server recovery`, idempotency and process-survival tests |
| Network, ingress, workload proxy | `cargo test -p treer-agent-server network`, `... workload_proxy`, `cargo test -p treer-proxy network`, Canary |
| CLI transport | `cargo test -p treer-cli --test session` |
| Controller update | `cargo test -p treer-agent-server -- controller_update release_update` |
| Console | `cd web && pnpm typecheck && pnpm build && pnpm test:e2e`, `pnpm run check:contrast`, desktop and 390px checks |
| Agent launcher / AIS | `just agent-launcher-test` |
| Workspace Apps | `just app-test` or the App's own `just <app>-test` |
| Self-host updater | `just updater-test`, Proxy admin update tests |
| Release tooling, install scripts | `just scripts-test` |
| Single-container image | `just single-image-smoke` (heavy; not part of `just check`) |
| Documentation | `node scripts/check-docs.mjs` |

Some subprocess fixtures are ignored by default because they need freshly built
sibling binaries:

```bash
cargo build -p treer-agent-host -p treer-agent-server -p treer-cli
cargo test -p treer-agent-server recovery_real_host_cold_restart -- --ignored
cargo test -p treer-agent-server real_service_environment_boundary -- --ignored
cargo test -p treer-cli --test controller -- --ignored
cargo test -p treer-agent-server curl_uses_inherited_proxy -- --ignored   # needs curl
```

Console, launcher and App browser screens also follow the
[UI skill](../skills/treer-ui/SKILL.md).

## CI

[`ci.yml`](../.github/workflows/ci.yml) runs on pull requests and pushes to
`main`: the Linux Rust job (format, build, tests against a PostgreSQL 17
service, Clippy), a Node 22/pnpm Console job (typecheck, build, Playwright
user journeys on Chromium), the launcher checks, and the release-tooling and
docs checks. It needs no deployment credentials and publishes nothing.
`build-release-artifacts.yml` builds and tests native artifacts for Linux,
macOS and Windows; `publish-ghcr.yml` publishes self-host images. See
[Release process](releases.md).

## Native Windows

Cross-compiling or running Clippy for Windows on macOS establishes only that it
compiles. The Windows job in `build-release-artifacts.yml` is the evidence for
local IPC and owner checks, private file replacement, ConPTY argv/input/resize/
stop, detached Host idempotency and survival across Controller replacement,
the Agent launcher's unit tests and native text-queue/restart/lease entry,
the installer, and install plus the native, registration-lock and real Host
tests as a standard (non-administrator) user. Real enrollment,
service routing, App deployment, reboot recovery and in-place update on Windows
still need a native acceptance run before claiming them verified.

## Known gaps

| Area | Gap |
| --- | --- |
| Rust | No native macOS/Windows pull-request gate |
| Console | No screenshot visual-regression suite |
| Core Message | No retention/export/delete, attachments, or load and failure suite |
| Distribution | No automated network-partition tests; physical sleep/wake is checked manually |
| Host supervision | No macOS service-manager or Apple container machine CI |
| Release | No downgrade or freshness enforcement for signed releases |
| Self-host update | No unattended Compose apply against live Docker |
| Apps | No live Telegram canary; no unattended Mail browser audit |
| Terminal output | Host replay is bounded and in memory, not a durable log archive |
| Security | Default Policy allows; isolation is operator-owned ([security model](security.md)) |

## Documentation rules

- Documents describe current behavior. Git history keeps earlier designs; do
  not add dated plans, change logs or ticket numbers to maintained docs.
- Update the closest document in the same change as the behavior: commands in
  the [operator guide](operator-guide.md) and the embedded
  [Treer skill](../skills/treer/SKILL.md), components and protocols in
  [architecture](architecture.md), trust claims in [security](security.md).
- Run `node scripts/check-docs.mjs`; it checks required entry points, the
  embedded-skill contract and every repository-relative Markdown link.
