# Controller update: real browser interaction leaf (#222 / #111)

This is a new isolated interaction acceptance entry, not a rerun or relabel of
the accepted HTTP/native leaf. Ordinary installations remain capability OFF.
No enrolled Host, Controller or user workload is a target.

## Ownership and minimum stack

Core owns the ignored Proxy test and its real router/PG/WS/native contract. Web
owns the Playwright entry and any demonstrated Machine UI defect. Release owns
the one hosted wrapper and actual execution after author checks and one
independent source review. There is no second updater or synthetic Controller
actor, and no repeated manager, HTTPS distributor or platform matrix.

The test serves the normal built Console via production `AppDistConfig`, at the
same loopback origin as the production API router. Normal browser cookie/Origin
and CSRF checks apply. A new random-password user is inserted into only the fresh
fixture schema, with verified email and membership in the task workspace. There
are **zero sessions before the browser starts**. The browser fills the normal
login form; the real password verifier/session handler issues its HttpOnly cookie.
It must not use injected cookies, storage state, human headers, mocked API routes,
an API request context, or an HTTP script to perform update writes.

The real machine enrollment token/WS authentication, test-only Controller
dispatch selector and EXCL native worker admission reuse the existing native
parent. The target image remains an ordinary capability-OFF image. Only the new
browser test is selected alongside that parent, with a new account/root/schema/
operation; the old HTTP test is not selected.

## Private coordination contract

Use the existing hosted-only `TREER_ACCEPTANCE_ROOT` contract and independently
pinned image inputs. Also set `TREER_BROWSER_DIST` to an absolute directory
containing the matching built Console. No environment workload/operator identity
is permitted. The only two Rust selectors are:

```text
self_update::hosted_acceptance::fixture_tests::hosted_controller_dispatch_native_fixture --exact --ignored
api::controller_update_tests::http_db::native_db::websocket_native::browser_native::browser_machine_update_observes_same_native_operation_after_refresh --exact --ignored
```

The Playwright process runs concurrently, once, and reads the same user's fresh
private `browser-ready.json` after it appears. This document contains only this
fixture's `origin,email,password,workspace_id,organization_id,machine_id,target_id`.
Do not upload it, browser storage/traces, raw headers or authentication screenshots.
There is no production test/control endpoint.

Through normal Console navigation, the browser opens Machine update, chooses the
provided target, inspects the preview and confirms once. Core waits for the
persisted operation and actual typed dispatch attempt, then waits for the native
`startup_backup` entry marker before disconnecting the
fixture WS and recreates its router/AppState at the same address and PG. It writes
private `browser-reconnected.json` containing the operation ID. The browser then
refreshes, reopens Machine update and observes only through GET. Reload must not
prepare or confirm again, including when the replacement advertises capability
OFF. No automatic write retry is permitted after an ambiguous response.

The bounded baseline barrier is fixture synchronization, not update authority:
`baseline` only marks entry before `observe_initial`/`save_initial`, while
`startup_backup` is emitted after both succeed. The original Controller health
requires `proxy_connected=true`; disconnecting immediately on outbound count can
race that required initial observation. The barrier refuses a recorded native
preparation error or a 60-second timeout rather than forcing disconnection or
resubmitting. All later native/PG/browser result comparisons remain mandatory.

Web publishes `browser-result.json` only after observing the real final GET:

```json
{"operation":"complete raw GET wire object, not a mapped UI object","normal_login":true,"refreshed":true,"prepare_requests":1,"confirm_requests":1,"duplicate_write_requests":0}
```

Core independently counts actual inbound prepare and confirm requests across
both router instances (including refusals), and requires 1/1. The retained real
typed outbound counters require old/replacement 1/0. It compares the complete
browser/native/PG operation, including operation ID, final identity, health,
receipt and terminal state; requires released PG slot and no repeat marker; then
signals `browser-complete.json` so the browser can close its authenticated
context, and
publishes the existing safe parent acknowledgement
`proxy-websocket-native-result.json`. The native parent independently verifies
the target image, retained Host and old Controller exit, then performs its owned
cleanup. Both Rust exits, the actual Playwright exit and final parent cleanup
must pass. A browser screenshot, worker result or intermediate DB PASS alone is
insufficient.

The new leaf does not repeat historical PID investigation. The prior
40176787046 functional PASS / overall FAIL / broader cleanup UNPROVEN and all
earlier failures remain unchanged. Any extra collector result is separately
attributed and cannot silently change this leaf's functional result.

## Author status

Core author checks: the exact new binary-test selector compiles (`cargo test -p
treer-proxy --bin treer-proxy browser_machine_update_observes_same_native_operation_after_refresh
--no-run`), `cargo clippy -p treer-proxy --lib --tests -- -D warnings` passes for
both library and binary test builds, and focused rustfmt/diff checks pass.
Clippy initially exposed a generic `Request` import difference between those
builds; the middleware now uses explicit `axum::http::Request<Body>`.
The first `--lib --no-run` command only built the small library and did not
compile/select the API fixture; the corrected binary command is the relevant
author acceptance input.

Actual browser/native acceptance is NOT RUN until the single complete hosted
entry is frozen, reviewed and executed. This author machine cannot execute the
Linux hosted-only native parent, so it does not claim a local full-stack PASS.
No ordinary capability activation or installed lifecycle occurs.

## First actual run and corrected fixture

Hosted run `40180580686` used source `09045b54` and wrapper `50c8b9a4`.
All three entries failed (native/Proxy/browser exits 101/101/1); no complete
operation or native worker receipt was retained. The parent confirmed its Host
and socket cleanup. Private logs were excluded from upload, so the first error
is unavailable. This remains a failed run with no inferred auth, capability or
replay cause; its root and operation are never reused.

Source inspection subsequently found a deterministic browser-selector defect:
`MachineItem` renders two direct buttons (open and actions), while the old test
required a count of one. The test now selects the first direct button. This
correction does not establish which stage the old run reached.

The corrected fixture also publishes two bounded diagnostic records. They are
location evidence, not successful acceptance or root-cause claims. Neither
contains raw errors, SQL, URLs, DOM, passwords, headers or logs:

- Web `browser-diagnostic.json` has exactly `schema_version`, `phase`, `status`;
  Web's fixed enum includes runner/browser setup, login, navigation and observation.
- Core `proxy-browser-diagnostic.json` has exactly those three fields, version 1.
  Phase is one of `reading_input`, `database`, `fixture_user`, `enrollment`,
  `catalog`, `router`, `controller_connection`, `custody`,
  `browser_confirmation`, `typed_dispatch`, `reconnection`, `native_result`,
  `browser_result`, `same_operation_checks`, `parent_cleanup`, `complete`.
  Status is `running`, `incomplete` or `passed`. Unwind/cancellation records
  `incomplete` without serializing its cause; an uncatchable process exit may
  leave `running`. Only explicit final parent-cleanup checks publish `passed`.

Release must validate these exact keys/types/enums before adding only these two
leaves to its safe collector. Private ready documents and raw logs remain
excluded. Any subsequent actual run requires a fresh source-bound root and one
review of the corrected source/wrapper, not a replay of the failed operation.

## Native baseline window after same-response observation

Run `40192095576` failed with browser `observing_final_operation` and Core
`native_result`. The native entry reached `baseline`, but not `startup_backup`;
no final native result was available. This does not establish its first error.
The parent proved Host reaped/socket absent, not full cleanup. The old run stays
FAIL and its operation/root are not reused.

Source inspection found the premature fixture disconnect race described above.
The acceptance-only real dispatch preparation now also invokes the existing
fixed-enum failure recorder on returned errors. Previously this path omitted
that call, so absence of `native-submit-failure.json` did not imply preparation
was still running. Health mismatch/unavailable classifications are static labels;
no raw error text is exported. Ordinary capability and production health checks
are unchanged. This correction is not retrospective attribution of the run.

Focused author checks for this correction: the exact Proxy baseline-barrier
unit and feature-enabled Controller safe-diagnostic unit each ran one test and
passed. Strict Clippy passed for Proxy binary/tests and Controller binary/tests
with `native-update-acceptance`; focused rustfmt and diff checks passed. An
initial short `--exact` selector compiled but ran zero tests; only the corrected
fully qualified one-test result counts. No PostgreSQL/native/browser stack,
historical matrix, full gate or installed lifecycle was run. The new isolated
hosted result remains pending one combined review and Release execution handoff.

## Fresh-page read reconciliation

Run `40194677490` retained a native succeeded/complete/healthy receipt, but the
browser did not publish its final result and all three entries exited nonzero.
This remains FAIL; neither full browser/PG/native equality nor full cleanup was
proved. Source inspection found that a refreshed client selected an operation
from discovery but queried its exact detail only with a caller-supplied,
in-memory operation ID. Discovery reads the PG snapshot; the detail GET performs
the bounded native observation. A fresh page could therefore poll the old PG
accepted state indefinitely despite native completion.

The client now queries the explicit ID or the same retained discovery ID before
capability lookup, validates that the detail response has exactly that operation
ID, and retains its ordinary cancellation/scope checks. It issues no prepare or
confirm request. A closed browser stale-discovery/fresh-detail/capability-OFF
control reproduced the old queued view and verified the corrected completion
view; that is not a claim about the hosted run's first assertion.

The parent field `fixture-cleanup.json.native_result_available` currently means
`outcome.is_ok()` for the entire native parent flow, including the final Proxy
acknowledgement wait. It does **not** report whether a native result file exists.
Thus false with a present native receipt is consistent with parent failure;
preserve that original field and its limited meaning rather than claiming
corruption or successful parent checks. The Proxy's 45-second browser-result
wait and native parent's 60-second final-ack wait remain unchanged. No new
timeout failure was established by the finite receipt, so this correction does
not extend those deadlines or weaken either exit/cleanup assertion.
