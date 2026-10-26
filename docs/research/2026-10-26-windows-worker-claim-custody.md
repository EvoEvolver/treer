# Independent receiver claim custody (#111)

The next durable-transfer prerequisite retains the fixed installation claim
with the same signed-input receiver. Previously the sender could inspect the
claim, and the receiver independently held images/state/installation lock, but
the receiver did not keep a claim file handle. It now does so after independent
normal configured-trust verification of the signed whole inputs and before the
first receipt. Core encodes the existing canonical full Binding; no offer key,
new record schema, arbitrary claim path or new permission switch is introduced.

Platform opens only the existing fixed private regular single-link claim under
the pinned installed pair. The non-Clone ClaimPin owns a read-only handle without
write/delete sharing and the bounded expected bytes. It checks volume and full
bytes, then reuses the same handle for later comparisons, without reopening a
path. The original Holding receiver keeps this guard alongside all prior input,
lock and backup guards. Normal inspection, backup and startup acceptance checks
therefore keep the claim pinned and recheck its full binding. Unknown errors
retain the parent reservation and forbid replay; absent/partial/foreign records
are never repaired or adopted. Explicit close/session exit drops receiver guards
but leaves the claim file and original sender custody.

This pins an existing durable denial record; it does not write a durable
ownership/takeover record, prove worker survival after creator exit or permit
sender release. Future transfer must independently address job ownership,
worker lifetime/Job constraints and an explicit release contract. Pair
publication, selected Controller-only restart/health, whole-pair rollback with
monotone startup fences, durable outcome and safe claim retirement remain
separate. Public capability stays OFF; no Host/live/installed-image/publisher
operation is performed.

Author checks: Windows GNU Platform all-target strict Clippy, Server tests with
the existing acceptance feature strict Clippy, fmt/docs/diff. Two new native
`windows_update_worker_claim_` controls are NOT_RUN at source freeze: the normal
saved installation/Trust same-source worker pins the claim throughout inspection
and backup until explicit close, and missing/partial/foreign operation claims
cannot yield reception or replay. The first control proves write-open succeeds
before worker creation and fails while that independent receiver holds its pin;
it also attempts deletion only inside its private test fixture. No old selector
or matrix is rerun. ONE source review and sole new Release native execution
will record actual results separately.

Actual acceptance: ONE independent source review (#111:9867) passed for
`5554a0ec0e60db6927158b6b7d2a211e138e951c`. Release's sole Windows run
`40422874249`, wrapper `68bf1359beaa9e99c1855a30c30ccb21df09f0b9`,
completed with dev build, test build and direct test exits all zero. On Windows
2022/Rust 1.90, Fixed NTFS image `20261022.344.1`, both new native controls
passed (2 passed, 0 failed, 0 ignored, 357 filtered). Core independently read
the RESULT, platform, compiler artifact and test receipts: exactly one local
Server test executable was selected; both dev/test image sizes and SHA256
digests were unchanged; the isolated catalog child was not precreated; remaining
entries were empty, cleanup was proven and the empty TEMP root was removed.
RESULT SHA256 is
`6800b91c4ab000f36a4414cd5d59eebbd29977685ef1761fb63b3e4a09a244dc`.
Receipts remain under ignored
`.treer/reviews/windows111-worker-claim-5554a0ec/run-40422874249`.
This verifies only the claim custody boundary described above. It grants no
sender release, durable takeover, lifecycle authority or complete updater claim.
