# Real Controller dispatch to native worker (#111/#222)

## Missing boundary and this leaf

The accepted e705 paired native/HTTP/PG fixture uses a synthetic command actor
and a private binding-file transport. Its real native result remains valid,
but it cannot establish the normal Controller websocket command handler path.
This leaf replaces that transport with the production Proxy router's authenticated
`/agent/connect`, the real installed Controller's `ProxyClient`, typed command
handler, existing native preparation/handoff/worker and operation projection.
The HTTP test uses a real session cookie and Origin checks, not an injected
session. The same operation is compared in native evidence and PostgreSQL.

No second updater, browser UI, manager or network distributor is introduced.
Artifacts are the existing local independently pinned fixture images. This is
explicitly **not** a repeat of the HTTPS staging or systemd matrix and not an
enrolled-machine update. Old40169160667/PID907 evidence is unchanged.

## Explicit test entry and ordinary boundary

Only `native-update-acceptance` builds contain the new private configuration
selector `hosted_native_dispatch:true`. The parent native fixture writes it to
its exact `tn-<16 hex>/xdg/treer/agent-servers/srv_native_acceptance-controller.json`.
The selector is checked before normal service environment cleaning can erase
managed-caller evidence. It requires the existing hosted/non-root effective
account/private HOME/root contract, Linux x86_64, exact workspace/server/root/
Host socket, retained private config/readiness readback, exact loopback listener
and machine-token binding and the installed pinned image.
Absent/false selectors cannot enable it; ordinary `native_accepted()` remains
hardfalse. No environment-only switch enables dispatch, no remote field can
select a root or artifact, and no default/release image contains this adapter.

The dedicated feature Controller advertises the test capability through the
normal registration and inspector. Its actual typed dispatch validates the
full provisioned current/target/scope/revision/canonical plan, writes an EXCL
once-only admission record, then calls the same prepare/handoff implementation.
Only artifact sourcing is a local pinned test adapter. A second dispatch is
refused and leaves a repeat marker that fails acceptance. The record is evidence
of a command already executing, never an executable trigger or transport.
The replacing target is an ordinary image: capability goes back to unavailable,
while its existing read-only operation handler remains able to reconcile the
previous operation. No production capability is enabled by passing this test.
Receipt-query transport is separate from fresh update availability:
`ControllerUpdateOperation` does not require the mutation capability. The normal
HTTP route still authorizes the session, workspace membership and machine.exec
Policy, then loads only that user's durable machine-bound operation before
querying; native reads still check the workspace/server submission binding.
Inspection and dispatch remain capability-gated, and native admission remains
off. An unsupported older decoder can refuse a query; that is not authority to
submit an update or release its slot.

## Single fresh Release command contract

Reuse provisioning geometry and image-size/pin controls, never old account/root/
binding/operation. Build current Controller+CLI with `native-update-acceptance`,
target Controller+CLI and Host **without** it, all against this frozen source
and independently retained hashes. Ordinary Host bytes/source behavior need no
change. Keep the existing prepared launcher payload and real recipe health path.
Use a new disposable PostgreSQL database, not production. Keep all fixture
credentials private; do not upload `controller-proxy-ready.json` or service
configs containing the newly generated machine credential.

Both tests run concurrently once, sharing the fresh actual non-root HOME/root
and existing `TREER_DISPOSABLE_RUNNER=1`, `GITHUB_ACTIONS=true`,
`RUNNER_ENVIRONMENT=github-hosted`, `TREER_ACCEPTANCE_ROOT`,
`XDG_CONFIG_HOME=root/xdg`, `TREER_STATE_DIR=root/state`; Proxy also gets its
disposable `TREER_TEST_DATABASE_URL`. No workload/operator credentials.

```text
self_update::hosted_acceptance::fixture_tests::hosted_controller_dispatch_native_fixture --exact --ignored
api::controller_update_tests::http_db::native_db::websocket_native::authenticated_websocket_dispatch_runs_native_worker_and_reconnects_same_operation --exact --ignored
```

The Proxy test seeds its disposable machine through normal enrollment credential
creation/hashing, renaming only the generated fixture ID before any use. It
publishes private listener readiness, never a dispatch binding. The native parent
starts the real Host/Controller and captures the original Controller image before
allowing HTTP confirmation. Real cookie/Origin refusals must submit nothing.
HTTP prepare/confirm then dispatch normally. The test closes the owned fixture
websocket and recreates the actual router/state at the same listener address;
the real Controller reconnects. Repeated confirmation only returns the existing
PG operation; only GET observes the actual worker projection. No actor fabricates
results or calls `complete_command` in this new test.

A test-only counter at the real Proxy typed `send_command` boundary runs before
connection/capability refusal. Retained old and replacement AppStates must show
exactly one and zero dispatch attempts respectively after repeated confirmation
and operation GET. This also detects a replay refused by the ordinary replacement
before native admission; the native repeat marker alone cannot establish that.

Keep the Proxy alive through final native image/Host verification. The parent
waits for the successful PG/native full comparison before stopping its own Host;
the Proxy test then waits for that parent's final cleanup record. Require both
exact tests one PASS/zero skip/exit0, full operation equality, one EXCL binding,
old/new dispatch-attempt counts 1/0, no repeat marker, final target image/old Controller exit/retained Host checks and
`fixture-cleanup.json` success. A native result written before final parent
cleanup is insufficient. Preserve failures without mutation retries. Retain
bounded safe result/exit/hash evidence, not raw private service/token records.

The new safe leaf is `proxy-websocket-native-result.json`; reuse
`native-acceptance-result.json`, `fixture-cleanup.json`, native diagnostics and
the exact source/image pins. Existing PID evidence helper may classify bounded
collection evidence, but must not turn same UID into task ownership or restart
the old cleanup investigation. Do not rerun this operation after an unknown.
`proxy-websocket-observation.json` additionally records the latest safe GET
projection, native operation and both dispatch counters before comparisons.
Only bounded read-only polling follows temporary disconnects; no confirmation
or mutation is retried. Include this leaf even if final comparisons fail.

## Author verification

Four new controls PASS: no permission in an ordinary feature process; absent/
false selector cannot opt in; exact private loopback/credential binding rejects
foreign URLs, userinfo, query/fragment/path and token changes; full dispatch
binding rejects changed scope, target, revision and canonical plan. Command:

```text
cargo test -p treer-agent-server --bin treer-agent-server --locked --offline --features native-update-acceptance self_update::hosted_controller_dispatch::tests -- --nocapture
```

Both owning crates' test targets pass strict Clippy with the acceptance feature
selected only on Agent Server; the ordinary Agent Server test targets also pass
strict Clippy. Proxy `cargo check --tests`, formatting and diff checks pass.
Logs are `/Users/dooms/.cache/treer222-realws-author-unit.log`,
`treer222-realws-final-feature-clippy.log` and `treer222-realws-default-clippy.log`
in that same cache. The first compile exposed an invalid error conversion in
the new input validator; it was corrected before the passing checks. These
local checks do not establish actual websocket/native acceptance.

The independent e246734b review found the missing pre-refusal dispatch witness
described above. Its test-only successor passes Proxy test-target strict Clippy
(`cargo clippy -p treer-proxy --tests --locked --offline -- -D warnings`),
formatting and diff checks. Log:
`/Users/dooms/.cache/treer222-realws-dispatch-witness-clippy.log`.
No old native/HTTP/PG/manager execution was repeated for this correction.

Hosted whole execution is NOT_RUN locally; Release owns the one subsequent
actual job after one independent source review. No installed files, live Host,
Controller or Agent were touched. Ordinary capability remains off; Windows,
macOS activation and production enrollment activation are outside this leaf.

## First actual run and read-only transport correction

Release run40175124657 on7fdef1c2 failed both exact entries (exit101). Its safe
native operation is Succeeded, but the Proxy result is absent and the parent
cleanup says native_result_available=false. Global cleanup also has unreadable
process links, not proof of task ownership or absence. The original private
test panic/source line was not retained, so this run cannot identify a unique
assertion or be relabelled as paired acceptance. Old evidence is preserved.

Source inspection independently found that operation queries still required the
mutation capability at the Proxy command boundary, so an ordinary capOFF target
could not return its receipt. The successor separates only that read-only query
and covers capOFF query forwarding with inspection refusal and zero dispatch.
It also follows the real read-only polling contract and retains the safe
observation before comparisons. No security/auth/Policy/receipt validation or
native mutation gate is relaxed, and no old operation/root is replayed.

Successor author checks: exact protocol gating unit1PASS; exact Proxy
`state::tests::controller_receipt_query_survives_capoff_without_enabling_update`
unit1PASS; Proxy+protocol test-target strict Clippy/fmt/diff PASS. The Proxy unit
uses a closed in-memory command channel to prove forwarding/refusal only, not
native acceptance. Initial invocations selected the Proxy library rather than
binary target and ran zero tests; only the subsequent `--bin treer-proxy` exact
one-test result is counted. Logs under `/Users/dooms/.cache/`:
`treer222-capoff-receipt-bin-test.log`, `treer222-capoff-protocol-test.log`, and
`treer222-capoff-receipt-final-clippy.log`. No actual stack was repeated locally.

The existing process collector normally requires subject UID == effective UID;
plain `sudo ... --uid ORIGINAL_UID` is invalid and must not bypass that guard.
An explicit `--hosted-root-observer` successor permits only real/effective root
on Linux with `GITHUB_ACTIONS=true`, `RUNNER_ENVIRONMENT=github-hosted`,
`TREER_DISPOSABLE_RUNNER=1`, and `TREER_ACCEPTANCE_ROOT` exactly matching the
new physical `tn-<16 hex>` root. The subject UID must remain nonzero and that
root must still be owned by it, mode0700. Release may invoke the same read-only
helper with those explicit env bindings under the hosted runner's supported
sudo custody, preserving the original subject UID. It never changes process
ownership, signals a process, opens credentials or relaxes classification.
Evidence records observer UID and explicit mode. Any remaining inaccessible
identity/link still prevents absence proof. The old failed cleanup remains
unproven; this contract is only for a future new root, not recovery of old PIDs.
Two new mocked observer-admission controls PASS (normal UID/root-fallback
refusal, complete explicit root-mode binding and missing-field negatives), log
`/Users/dooms/.cache/treer222-hosted-observer-controls.log`; no actual root
observer or process fixture was run locally for this change.

## Actual receipt reconciliation: scoped leaf passed

Release run40176787046 executed exact78456d5c with wrapperc789b94f. Both exact
entries passed one test/zero skips/exit0. The normal authenticated HTTP router
and real machine websocket dispatched to the Controller/native worker; native,
Proxy/PG and the safe GET observation contain the same full operation
`306003d4-ecb3-449d-90bd-b62ca7c87db2`. Old/new dispatch attempts are1/0. The
parent's final replacement-image/retained-Host checks passed, and its cleanup
records Host reaped/socket absent plus native_result_available=true. This is
real HTTP API acceptance, not an actual browser-button claim or synthetic actor.

The whole GitHub job still FAILED: the additional non-root collector reported
no holders but live PID900/1814 with denied exe/cwd, so broader root-reference
absence remains UNPROVEN. Do not infer task membership from UID, ignore denied
evidence, retroactively run b05's observer against that root, or repeat the
accepted native/PG leaf. Original40175124657 and its unknown first panic remain
unchanged. b05's explicit root-observer mode was reviewed but was not in this
executed source; no actual root-observer pass is claimed.

Safe receipt:
`/Users/dooms/treer/.treer/reviews/release222-readonly-20261025/ACTUAL-RESULT.json`,
SHA256`d2d1f966aec969e558ec96ea4a5eab5c56e91088e90c461c7ab8e6461bdfeb50`.
Core independently verified all22 safe-index hashes, full operation equality,
dispatch1/0 and parent cleanup without rerunning any test. Ordinary capability
stays off and no enrolled-machine/Host lifecycle was performed.

This leaf is complete within those limits. Remaining queued work is distinct:
actual isolated browser interaction with this backend; optional future
cleanup-only proof of the explicit observer contract on a new root; ordinary
platform activation/release qualification. None is permission to enable the
capability or touch enrolled machines. Windows/macOS activation is unproven.
