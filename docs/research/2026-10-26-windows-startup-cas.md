# Loaded StartupStore CAS on the same Holding session (#111)

This continuation follows explicit backup preparation on the original Holding
worker. It neither reconnects a closed consumer nor treats a snapshot match as
compare/write authority. Preparation retains its original confirmed selection,
operation, normal configured Trust, image pins, installation lease and claim.
The original confirmed reservation supplies a non-wire permit to the selected
runtime's actual loaded StartupStore Arc. The worker never loads a Store by path.

One token captures the loaded instance revision and original physical/raw
snapshot. Every normal writer uses the same mutex and advances the revision.
Under that mutex, without an intervening unlock/await, CAS checks revision,
disk snapshot, loaded memory equivalence, entry workspace/server scope and the
monotone denial set. It calls the existing normal persistence implementation,
checks readback, and commits memory. This preserves v2 deletion/denial fences
and refuses a v1 replacement that would discard them. Identical bytes written
by a normal writer still invalidate an earlier token through revision changes.

Any attempt consumes its token. After persistence can have effects, IO/readback
failure marks the loaded instance unknown, so normal mutators also refuse;
read-only access is not a success acknowledgement. There is no automatic reset,
path reload or retry. Normal reads can still observe the last committed memory.
This in-memory fence is not a durable recovery ledger.

After commit, a bounded 512-byte transition carries the old and new snapshots on
the SAME sequenced/context-bound connection. The receiver requires its explicit
backup owner, reads only the existing pinned state directory, verifies the new
physical/raw state and preserves every denial in its original backup. It then
adopts the new expected snapshot for future inspection while retaining the old
backup and input/lock custody. The parent checks the exact committed operation,
revision and disk snapshot before and after acceptance. Wrong/late/lost replies
or cancellation leave the parent unknown and cannot replay the write or reopen
the session. A refusal before transport take may retain the pipe until teardown;
the operation latch still prevents retry. Existing session/exchange deadlines
remain; synchronous filesystem calls cannot be forcibly interrupted.

This CAS serializes writers of this actual loaded instance only. Arbitrary
external file writers and separately loaded Stores do not share its mutex.
Later normal writes are allowed; a race with receiver acceptance or the parent
postcheck can make the operation unknown after a successful disk write. The
finite public consumer does not request this internal mutation, and no browser
raw-state mutation API or new Policy/config switch is added.

Author validation covers five new portable controls: normal cloned Store success
with entries and persistent fences; competing normal write and logical ABA;
fence regression, wrong scope, duplicate entries and raw divergence; injected IO
or readback unknown fencing normal mutators; ordered transition and wrong-reply
single use. The nine existing normal StartupStore tests are also required
because their shared commit path gains revision accounting. Two new Windows
controls use normal saved installation/Trust and the same-source real private
worker: successful commit followed by same-session inspection/close with old
backup retained, and fence-regression/concurrent-writer/missing-grant refusal.
Native acceptance is NOT_RUN at author freeze and will be recorded separately.

The author found a Windows-cfg visibility error after the initial freeze:
`startup_store_update` referenced Platform's internal `open_record`. The narrow
successor uses the existing public `BackupDirectory::startup_snapshot` checked
physical/raw observation instead. Directory observation does not reload a Store
or supply write authority; the same loaded Arc/mutex remains authoritative.
The first run and its actual result remain separate from any changed-source
acceptance, with no automatic replay or reuse of its temporary root.

Remaining full-transaction work is durable mutation/lifetime transfer and an
explicit sender release contract; installed pair publication; selected
Controller-only conditional restart and health; whole-pair runtime rollback;
durable outcome and safe claim retirement. Neither this acceptance ACK nor
creator exit authorizes sender release. No live Host, service, capability,
publisher or installed image is operated; public Windows capability stays OFF.

## Exact-source acceptance

Release's ONE scoped source review of `5f550245` passed (#111:9853). The
original sole run `40419287278`, wrapper `bf2762b2`, failed dev compilation with
E0603 at the private Platform call: dev exit 101, no test build/execution, no
test root created and cleanup_proven=false. Seven tests were NOT_RUN. Core
independently read the original RESULT and dev error; those facts remain
unchanged. RESULT SHA256:
`c57a80d0801a35819c7ecb71d3d4d80152f62a36154cb289c2fac7323e5f326c`.

The same review closed the narrow reader fix at exact
`257cbdbfb6577a03f29f76ade6855992dd4455b4` (#111:9858). Sole changed-source run
[40420113292](https://github.com/EvoEvolver/treer/actions/runs/40420113292),
wrapper `9156d5c1eca2a8578a4fa342577ac9b42277f548`, passed dev build, JSON
test build and direct test execution, each exit zero (#111:9861). On Windows
2022/Rust 1.90/fixed NTFS image `20261022.344.1`, exactly seven new tests passed,
none failed or ignored, 350 filtered, in 2.42 seconds. This includes actual
loaded Store commit followed by inspection on the same real private worker
session, original backup retention, and the three native refusal scenarios.

Core independently read RESULT, platform, tests and the unique local Server
bin/profile.test compiler artifact. Dev size 37,088,256 / SHA256
`01bd690696ec70fe192a3f563fe4a0fc930b942db85abee111bcd94da5879dea`, and test
size 45,359,104 / SHA256
`d5400a79a51c98644e662238f63b4bc6ad3096c7ad1ea9dc392fd795788a739a`,
were unchanged before/after. Only direct tests received the isolated TEMP/TMP
and matching catalog/LOCALAPPDATA markers; the child was not precreated.
Remaining entries were empty, cleanup proven and the empty root deleted.
RESULT SHA256:
`ac5f2a465f5ccd72fe79ad86d0083714d04fff217507cc9cd87a13a39c1ea997`.
Receipts: `.treer/reviews/windows111-startup-cas-257cbdbf/run-40420113292/`.
No binary or environment was uploaded. Existing unrelated provider-cli warnings
do not become a Windows strict-Clippy claim. No old selector, matrix, live Host,
installed-image, capability or publisher operation was run.

Delivery combines the two reviewed commits on latest main with identical Rust
source to tested `257cbdbf`; only this acceptance record is added. This closes
the loaded-instance CAS and same-session acceptance leaf, not the remaining
full transaction described above. waiting_on stays null while autonomous
source work remains; production activation has not been requested.
