# Loaded Store writer custody for mutation preparation (#111)

The previous same-Holding path can create complete new/restore candidates but
its ordinary loaded Store CAS does not freeze subsequent normal writers. This
increment adds the explicit `Preparation::compare_write_startup_fenced` path,
reusing the normal confirmed reservation/runtime/Trust/backup prerequisites and
the existing compare, monotone v2 denial-fence validation, persistence, readback
and memory commit. The ordinary CAS entry retains its prior unfenced behavior.

Under the SAME loaded StartupStore Arc/Mutex, the fenced variant consumes its
attempt, compares revision/raw/physical state and entries, validates replacement
scope and fences, then records the operation owner before any possible write.
Persistence/readback failure leaves both the owner and unknown status. Success
retains the owner with the new revision/snapshot. All normal writer entrypoints
already pass through the same mutex gate and refuse while an owner is present.
Rejected normal-writer callbacks do not run; no credential/startup wire schema
is changed.

The non-Clone/non-wire committed owner is saved in the SAME coordinator before
any worker acceptance await. The original connection carries the existing
bounded snapshot transition; the worker still independently checks state and
old backup fences. Later coordinator work checks its retained owner and the
SAME worker can create complete candidates from held signed inputs and old
backups. Original sender/worker pins, lease, job, claim and restoration custody
remain. A failed/late/cancelled acceptance or later candidate failure stays
unknown; close/Drop do not silently unfreeze ordinary writers.

Owner snapshot inspection verifies exact operation, owner, revision and physical
raw snapshot. A foreign operation query is harmless refusal. A real state/read
failure permanently poisons a fenced owner; restoring bytes does not repair it.
There is deliberately no generic release setter, reload/adoption or Drop-based
recovery. Releasing writer custody needs the future reconciled durable outcome.

Focused new selector `windows_update_mutation_fence_` has three portable controls
for successful write/clone writer exclusion/Drop, distinct pre-write and unknown
IO boundaries, and permanent raw-state poison. Two native controls compose the
normal saved-installation/Trust private worker with fenced CAS then complete
candidate preparation, and a post-CAS candidate conflict that preserves both
unknown and the loaded writer fence. Native controls are NOT_RUN at freeze and
require ONE independent review then one sole exact-source Release execution.
No old selector/matrix/full gate or live runtime operation is required here.

This fence is on one loaded instance only and is not persisted in startup JSON.
It does not synchronize separately loaded Stores, other processes, direct disk
writes or a successor Controller. It cannot prove durable transfer, creator-exit
lifetime or permission to release sender image pins. Those remain explicit
prerequisites before worker pair publication, selected Controller-only restart
and health, fence-preserving whole-pair rollback, durable outcome and safe claim
retirement. Public capability remains OFF; Host/live/publisher are unchanged.

Actual receipt: Release's ONE independent source review passed for frozen
`e07c77ded817f65038f2f9cf9011ca49ac7330dd` (#111:9902). Sole Windows
run `40432538642`, wrapper `bf51bcda595687d48dce7ff1312bb16f1f993d5f`,
completed successfully on Windows2022/Rust1.90/Fixed NTFS image20261022.344.1.
All five new tests passed (three portable, two native), with zero failures or
ignored tests, 367 filtered, and build/dev-build/test-build/direct-test exits0.
The same-source development and uniquely resolved test images retained their
sizes and SHA256 values. Only the direct test child used the private TEMP and
matching catalog markers; the fixture child was not precreated. Remaining
entries were empty, cleanup was proven, and the empty root was removed.
Core independently read RESULT/platform/tests and checked RESULT SHA256
`9b0ee38ce6962e5458b618090253fcf9680c815da0c2df0097c656073fd764ef`.
Receipts remain under ignored
`.treer/reviews/windows111-mutation-fence-e07c77de/run-40432538642/`.
Author Mac portable3, focused strict Server acceptance/tests Clippy, formatting,
documentation and diff checks passed. No old updater selector, matrix, full
gate, live operation or additional review layer was run. The actual controls
prove this finite loaded-instance fence and same-session preparation only;
the complete transaction and lifetime limits above remain unchanged.
