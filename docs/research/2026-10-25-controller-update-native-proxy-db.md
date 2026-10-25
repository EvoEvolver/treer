# Paired native worker and authenticated Proxy database fixture (#111/#222)

This increment connects the existing native complete fixture to the existing
production HTTP router, cookie/Origin middleware and a disposable PostgreSQL
database. It does not enable an enrolled installation or repeat the accepted
systemd-manager fixture. Both new entry tests are ignored by ordinary runs.

The Proxy process owns the preview and `DispatchBinding`. It verifies the
durable PG claim before publishing one private dispatch intent and atomically
publishing its binding. A second dispatch refuses the EXCL intent. The native
fixture accepts only the exact provisioned current/target, workspace, server,
catalog revision and canonical plan digest. Its original installed image invokes
the existing native acceptance submit entry once, with that same operation ID.
No new updater, worker executable, production route or recovery API is added.

The test transport deliberately loses the dispatch acknowledgement. HTTP retains
its real Accepted operation; the fixture never fabricates an Accepted/Succeeded
native reply. Native execution continues after all Proxy memory is dropped.
Only the real worker's checked read-only operation projection is returned after
rebuilding Proxy state against the same PG database. The test compares the full
operation stored in PG, releases its slot through normal observation, and joins
the reconnect actor with queued-command draining so its assertions cannot be
lost in a background panic. Cookie/Origin failures submit nothing.

## Dedicated Release execution inputs

Release retains the existing hosted-native provisioning contract: a fresh
non-root effective OS account, its real private HOME, a private physical
`HOME/tn-<16 lowercase hex>` root, exact independently pinned current/target
Controller+CLI images, pinned Host, SUMS and existing launcher prerequisites.
The root's `acceptance-input.json` is unchanged. Use Linux x86_64 only for this
slice. Supply a disposable `TREER_TEST_DATABASE_URL`; never use an enrolled or
production database. Both processes must share this one account/root, with
`TREER_DISPOSABLE_RUNNER=1`, `GITHUB_ACTIONS=true`,
`RUNNER_ENVIRONMENT=github-hosted`, `TREER_ACCEPTANCE_ROOT`, matching
`XDG_CONFIG_HOME=root/xdg`, `TREER_STATE_DIR=root/state`. Do not pass Agent,
workload or operator credentials.

Build test binaries from the frozen source with separate task-owned target
directories. The Agent Server test binary needs `native-update-acceptance`;
the Proxy test binary uses its ordinary test configuration. Run these exact
tests concurrently once, without serializing one behind the other's completion:

```text
self_update::hosted_acceptance::fixture_tests::hosted_proxy_database_native_fixture --exact --ignored
api::controller_update_tests::http_db::native_db::authenticated_http_pg_uses_same_native_worker_receipt_without_redispatch --exact --ignored
```

Retain both exit codes, native diagnostics, `fixture-cleanup.json`,
`native-acceptance-result.json`, `proxy-native-db-result.json`, and the private
dispatch intent/binding. The success operation ID and all identities must match
in native and PG results. The Host fixture remains the native process owner's
cleanup responsibility; Release must retain its existing worker/claim/account
teardown checks on either process failing. Do not retry either mutation path or
replay a binding after a partial/unknown run.

## Evidence boundaries

Source/compile and small private-file controls can run locally. The paired
actual native/HTTP/PG execution requires the dedicated hosted account. Its
recorded result and remaining cleanup qualification follow below. Previous c56 manager, 55eb
HTTP/PG and 0a459 HTTPS staging evidence retain their separate scopes.

The Controller connection here is a private **test transport**. It admits the
test's capability on the Proxy side and changes the checked native inspection's
`native_ready` only in that adapter. The real inspector must report false,
ordinary `native_accepted()` stays false and no production capability is
advertised. This is not acceptance of production dispatch activation, remote
Controller websocket replacement, signed publication, Windows or macOS update
activation. Those remain separate finite product/release leaves.

## Local source checks (2026-10-25)

The new native binding negative control passed (one test), including wrong
workspace/server, target, catalog revision and canonical plan digest. Its first
run exposed that the bridge checked digest syntax but not digest equality;
the bridge now verifies the canonical digest before accepting a binding.
The Proxy private-file control passed (one test): complete atomic publication,
EXCL intent refusal on a second dispatch, and retained original binding.
These controls run without PostgreSQL, installed discovery or Host lifecycle.
They do not substitute for the paired hosted execution above.
Both owning test crates passed strict Clippy (`--tests -- -D warnings`), with
`native-update-acceptance` selected only for the Agent Server test crate.
Formatting and diff checks passed. No installed binary was changed.

## Actual paired hosted result (2026-10-25)

Release run `40169160667`, source
`e705f2078069e2e0531ea8c3c9e3c64e61ad56c9`, wrapper
`099bdac8dac493f591cf14ce37e15208415ddd27` executed both exact entries once.
Native and Proxy each passed one test and exited zero. The full native/PG
operation matched, with operation ID `41e4cd04-3cde-4145-a55c-1630c3e91505`.
The native parent finished its final installed-image/Host checks; the Host was
reaped, its socket and the web claim were absent, and the DB slot-release
assertion passed. Disposable containers/network were removed. No manager
fixture was repeated, capability stayed off and no enrolled system changed.

GitHub's overall conclusion was **failure**, separately from those passed
entries. The additional Release process scan found no task-root executable/cwd
holders, but received a permission denial for same-account PID907 without
recording enough identity to classify it as task-related or unrelated. Broader
final process cleanup is therefore **UNPROVEN**, not a product failure diagnosis
and not a full cleanup PASS. The removed hosted environment cannot retroactively
supply that missing observation. Do not rerun this consumed operation/binding.
Any future cleanup-proof increment must first define the missing PID identity
and classification evidence; production dispatch activation remains unaccepted.

Safe receipt: `.treer/reviews/release222-native-pg-20261025/ACTUAL-RESULT.json`,
SHA256 `7c8d62ca557e4d3f712231d7943a6b4908e425a2667ac50d59409a3af3590b41`.
This documentation reconciles that receipt; it adds no execution or test result.
