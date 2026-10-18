# Telegram-only candidate provenance (#159)

Integration base: `64a8cb0136efcfb2fc790b7dbfa4d3be2d778d32`.
The P2 history through original `88779477` was rebased on that base; original
SHAs below identify content sources, not the rewritten candidate commit IDs.
The separate `message-endpoint-159` branch retains unfinished P3 HTTP work and
must not be deployed as this slice. This candidate has exactly two Rust binaries.

Every changed tracked file relative to the integration base is listed below.
“Review N” refers to security's static verdict in #159 comment N; it does not
approve subsequent edits or the extracted candidate. Baseline rows without a
matching individual review pointer explicitly require fresh coverage. No verdict
in this inventory establishes live Telegram delivery, actual Policy isolation or
cross-platform correctness. Author checks and the frozen SHA belong on #159.

| File | Source / extraction | Prior coverage; candidate still needs review |
| --- | --- | --- |
| `apps/message-endpoint/.gitignore` | New extraction change | preserve ignored reports; ignore dependency symlink; fresh review pending |
| `apps/message-endpoint/Cargo.lock` | Exact content from `87b4d343` | Review 3922 |
| `apps/message-endpoint/Cargo.toml` | P2 `88779477`, last original change `038b4aef` | Review 3775; fresh slice review pending |
| `apps/message-endpoint/DEPENDENCY_AUDIT.md` | Exact content from `87b4d343` | Review 3922 |
| `apps/message-endpoint/README.md` | New extraction change | Telegram-only scope, check instructions and release handoff; fresh review pending |
| `apps/message-endpoint/TELEGRAM-SLICE.md` | New extraction change | this provenance inventory; fresh review pending |
| `apps/message-endpoint/dist/app.css` | P2 `88779477`, last original change `47f7548a` | Review 3794; fresh slice review pending |
| `apps/message-endpoint/dist/app.js` | Exact content from `fc822e43` | Review 3994 |
| `apps/message-endpoint/dist/index.html` | P2 `88779477`, last original change `038b4aef` | Review 3775; fresh slice review pending |
| `apps/message-endpoint/package-lock.json` | P2 `88779477`, last original change `038b4aef` | Review 3775; fresh slice review pending |
| `apps/message-endpoint/package.json` | P2 `88779477`, last original change `038b4aef` | Review 3775; fresh slice review pending |
| `apps/message-endpoint/scripts/build-ui.mjs` | P2 `88779477`, last original change `038b4aef` | Review 3775; fresh slice review pending |
| `apps/message-endpoint/scripts/check-ui.mjs` | P2 `88779477`, last original change `038b4aef` | Review 3775; fresh slice review pending |
| `apps/message-endpoint/scripts/ui-build.mjs` | P2 `88779477`, last original change `038b4aef` | Review 3775; fresh slice review pending |
| `apps/message-endpoint/src/activation_state.rs` | P2 `88779477`, last original change `d63cc7fb` | Review 3893; fresh slice review pending |
| `apps/message-endpoint/src/archive.rs` | P2 `88779477`, last original change `d13b48ff` | Review 3898; fresh slice review pending |
| `apps/message-endpoint/src/bin/treer-endpoint.rs` | P2 `88779477`, last original change `31ead75a` | Review 3608; fresh slice review pending |
| `apps/message-endpoint/src/bin/treer-message-hub.rs` | P2 `88779477`, last original change `038b4aef` | Review 3775; fresh slice review pending |
| `apps/message-endpoint/src/cli_integrity.rs` | P2 `88779477`, last original change `94add5a5` | Review 3862; fresh slice review pending |
| `apps/message-endpoint/src/configuration.rs` | Selected file patch from `a8d95a2d` on P2 | Review 3992; extraction needs fresh coverage |
| `apps/message-endpoint/src/configuration/activation.rs` | Selected file patch from `a8d95a2d` on P2 | Review 3992; extraction needs fresh coverage |
| `apps/message-endpoint/src/core.rs` | P2 `88779477`, last original change `94add5a5` | Review 3862; fresh slice review pending |
| `apps/message-endpoint/src/failures.rs` | P2 `88779477`, last original change `d13b48ff` | Review 3898; fresh slice review pending |
| `apps/message-endpoint/src/handoffs.rs` | P2 `88779477`, last original change `d63cc7fb` | Review 3893; fresh slice review pending |
| `apps/message-endpoint/src/holds.rs` | P2 `88779477`, last original change `d63cc7fb` | Review 3893; fresh slice review pending |
| `apps/message-endpoint/src/hub.rs` | Selected file patch from `a8d95a2d` on P2 | Review 3992; extraction needs fresh coverage |
| `apps/message-endpoint/src/hub_runtime.rs` | Selected file patch from `c7e5ad17` on P2 | Review 3997; extraction needs fresh coverage |
| `apps/message-endpoint/src/hub_runtime_fixture.rs` | Exact content from `b6bae7ca` | new joined relay/Telegram fixture; fresh review pending |
| `apps/message-endpoint/src/lib.rs` | P2 `88779477`, last original change `d63cc7fb` | Review 3893; fresh slice review pending |
| `apps/message-endpoint/src/notices.rs` | P2 `88779477`, last original change `e2cfa6ef` | Review 3686; fresh slice review pending |
| `apps/message-endpoint/src/oauth.rs` | P2 `88779477`, last original change `0b34f0c8` | Review 3761; fresh slice review pending |
| `apps/message-endpoint/src/owner_actions.rs` | P2 `88779477`, last original change `d63cc7fb` | Review 3893; fresh slice review pending |
| `apps/message-endpoint/src/owner_auth.rs` | P2 `88779477`, last original change `d63cc7fb` | Review 3893; fresh slice review pending |
| `apps/message-endpoint/src/private_store.rs` | P2 `88779477`, last original change `a33e415f` | Review 3588; fresh slice review pending |
| `apps/message-endpoint/src/protocol.rs` | P2 `88779477`, last original change `47951834` | Review 3658; fresh slice review pending |
| `apps/message-endpoint/src/recovery.rs` | P2 `88779477`, last original change `7e945b69` | Review 3850; fresh slice review pending |
| `apps/message-endpoint/src/refetch.rs` | P2 `88779477`, last original change `94add5a5` | Review 3862; fresh slice review pending |
| `apps/message-endpoint/src/relay.rs` | P2 `88779477`, last original change `94add5a5` | Review 3862; fresh slice review pending |
| `apps/message-endpoint/src/scheduler.rs` | Selected file patch from `c7e5ad17` on P2 | Review 3997; extraction needs fresh coverage |
| `apps/message-endpoint/src/settings_http.rs` | P2 `88779477`, last original change `d13b48ff` | Review 3898; fresh slice review pending |
| `apps/message-endpoint/src/store.rs` | P2 `88779477`, last original change `a33e415f` | Review 3588; fresh slice review pending |
| `apps/message-endpoint/src/telegram.rs` | P2 `88779477`, last original change `d63cc7fb` | Review 3893; fresh slice review pending |
| `apps/message-endpoint/src/telegram_http.rs` | P2 `88779477`, last original change `7835d89e` | Review 3903; fresh slice review pending |
| `apps/message-endpoint/src/telegram_workers.rs` | P2 `88779477`, last original change `7835d89e` | Review 3903; fresh slice review pending |
| `apps/message-endpoint/src/transfers.rs` | P2 `88779477`, last original change `cdc74196` | Review 3650; fresh slice review pending |
| `apps/message-endpoint/src/verification_gate.rs` | P2 `88779477`, last original change `210ab181` | Review 3751; fresh slice review pending |
| `apps/message-endpoint/tests/RUNTIME-SMOKE.md` | P2 `88779477`, last original change `721a456d` | Review 3532; fresh slice review pending |
| `apps/message-endpoint/tests/TELEGRAM-SMOKE.md` | Exact content from `b6bae7ca` | new release checklist; fresh review pending |
| `apps/message-endpoint/tests/app-runtime-smoke.py` | P2 `88779477`, last original change `201408b3` | Inherited P2; no per-file verdict inferred; fresh slice review pending |
| `apps/message-endpoint/tests/browser.mjs` | Exact content from `fc822e43` | Review 3994 |
| `apps/message-endpoint/tests/http_fixture.py` | P2 `88779477`, last original change `7835d89e` | Review 3903; fresh slice review pending |
| `apps/message-endpoint/tests/measure_relay_rss.py` | P2 `88779477`, last original change `88779477` | Inherited P2; no per-file verdict inferred; fresh slice review pending |
| `apps/message-endpoint/tests/positive_runtime_fixture.py` | Exact content from `b6bae7ca` | new recipient-separated fake Core bus; fresh review pending |
| `apps/message-endpoint/tests/protocol.rs` | P2 `88779477`, last original change `47951834` | Review 3658; fresh slice review pending |
| `apps/message-endpoint/tests/relay_executable.rs` | P2 `88779477`, last original change `c2fc8b41` | Review 3665; fresh slice review pending |
| `apps/message-endpoint/tests/run.py` | New extraction change | explicit hermetic Cargo tool paths; two test threads; fresh review pending |
| `apps/message-endpoint/tests/runtime-smoke-policy.json` | P2 `88779477`, last original change `e99c350d` | Review 3516; fresh slice review pending |
| `apps/message-endpoint/tests/test_http_runtime.py` | New extraction change | allow unittest discovery without mistaking argv for executable; fresh review pending |
| `apps/message-endpoint/tests/test_production_boundary.py` | New extraction change | preserve diagnostic scans and support unittest discovery; fresh review pending |
| `apps/message-endpoint/tests/test_runtime_smoke.py` | P2 `88779477`, last original change `201408b3` | Inherited P2; no per-file verdict inferred; fresh slice review pending |
| `apps/message-endpoint/ui/index.html` | P2 `88779477`, last original change `038b4aef` | Review 3775; fresh slice review pending |
| `apps/message-endpoint/ui/src/api.ts` | P2 `88779477`, last original change `1d7e61e2` | Review 3797; fresh slice review pending |
| `apps/message-endpoint/ui/src/app.css` | P2 `88779477`, last original change `47f7548a` | Review 3794; fresh slice review pending |
| `apps/message-endpoint/ui/src/main.tsx` | Exact content from `fc822e43` | Review 3994 |
| `apps/message-endpoint/ui/tsconfig.json` | P2 `88779477`, last original change `038b4aef` | Review 3775; fresh slice review pending |
| `docs/research/2026-10-17-message-endpoint-hub-design.md` | P2 `88779477`, last original change `2bd0b4eb` | Review 3470; fresh slice review pending |

## Runtime and evidence boundaries

The production source contains no public HTTP lifecycle module, public-http
subcommand, hub_http_ table, HTTP client pairing store or RPC listener. The
private settings HTTP server and Telegram Bot API HTTP client remain necessary
and are not public HTTP channel support. Binary scans must accompany the exact
release build; a clean source grep alone is not binary evidence.

The new joined fixture exercises the actual Rust Relay implementation with the
assembled hub runtime and recipient-separated fake Core CLI, plus a fake Telegram
HTTP server. It covers pre-resume no-delivery and both directions through the
endpoint identity. It is not a live Telegram bot test or a separate OS-process
relay test; the separate relay_executable integration tests cover that executable
boundary. Browser fixtures exercise wildcard and embedded paths with synthetic
identities. The operator gate remains false by default.

See [the disposable acceptance checklist](tests/TELEGRAM-SMOKE.md) for release's
real-bot, negative controls, drain, rollback and token-revocation obligations.
