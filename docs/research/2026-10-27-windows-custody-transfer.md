# Same-Holding bounded custody transfer (#111)

This increment advances the existing normal confirmed preparation success path
past candidate preparation. It neither restarts a closed session nor adds an
identity, Policy, downloader or trust resolver. The original operation, selected
loaded runtime, normal configured Trust, fenced loaded Store and all signed
inputs remain the prerequisites.

The explicit same-connection transfer command includes strict full Binding,
whole-offer digest/challenge, accepted physical/raw startup snapshot and an
absolute expiry no more than 120 seconds away. The receiver rechecks its original
claim/job/raw state/verified backup/four complete candidates, creates the fixed
job record `worker-transfer.bin` once, flushes and
reads back complete bytes. A readonly duplicate retains the SAME file object
without a reopen gap; original sharing denies new writes/deletes. It retains
record, independent job/lease/claim, target/state, backups and candidates before
ACK, consuming only its old installed-image handles. Record existence alone
never creates an execution owner or permits adoption/retry.

Sender transfer checks the original Child's live observation, normal
Trust and loaded fence before exchanging; it reads the exact durable record
after the sequenced ACK and retains original image custody. Image release is a
SEPARATE synchronous call: selected runtime, operation, loaded owner, normal
Trust/catalog/claim, original live Child and exact held record are checked
before consuming the reservation's old image pins. Saved inventory duplicates
are also consumed in the same non-await continuation, while physical parents,
config snapshots, original job/lease/state and target remain. No public digest,
ACK, receipt, inspection, close or creator-exit observation is that call.

Private pinned worker creation preserves its existing normal Job policy; it
does not request breakaway, change Job limits or escape stable Host cleanup.
Original Child liveness is checked before/after exact record verification.
Independent custody here means retained file/job-record/lease/claim guards,
not independence from enclosing Windows Job or Host lifetime. Job/Host/OS
termination may end the worker; that remains unknown, never a replay permit.
An explicitly
transferred worker no longer requires creator liveness for read-only custody
checks. Pipe error/EOF retains existing guards until the accepted finite expiry,
without mutation or reconnection; explicit close ends the finite session. This
is bounded ownership, not unlimited daemon lifetime, reboot recovery, power-loss
durability or autonomous restart/health authorization. Deadline expiration and
all unknown paths leave the intent/claim/transfer evidence and loaded fence.
The writer fence belongs to the original loaded Store Arc; this is not a
durable reconstruction of that in-memory owner after process restart. Loaded
owner reconciliation remains part of the later terminal outcome work.
Both owners also arm a local monotonic deadline once. It is omitted from wire
data and cannot be reset by another command. The receiver retains against that
same deadline after EOF/error, so wall-clock changes cannot extend retention;
sender release checks its own monotonic bound as well as the absolute contract.

New focused selector `windows_update_transfer_` has two portable controls for
full context/state/deadline/strict schema and cancelled/foreign ACK refusal;
four native controls compose the same-source signed worker with shared saved
inventory, fenced CAS, backups and candidates, then prove explicit image release;
conflicting transfer evidence preserving unknown/original pins; and disconnected
custody retaining the original Child and independent lease/job. Test-only
teardown targets only that original owned Child, not Host or installed runtime.
The fourth control stops only the original owned fixture Child after a real
transfer ACK/record, then proves sender release/replay/close are refused while
original pins, claim and loaded writer fence remain held.
The sole initial Windows run `40435958374` at `ca849e5f` built both images
successfully and passed the two portable controls. All three native controls
failed at `Preparation.start`, before backup/CAS/candidates/transfer assertions;
the fixed receiver refusal did not retain the underlying cause. Cleanup was not
proven. This remains a failed run, not transfer acceptance. A test-only diagnostic
successor records only the numeric OS creation code/error kind and a static
creation-stage marker; it prints no OS message, path, arguments or configuration.
Its sole run `40437220092` again built both images, passed portable2 and failed
native3 at creation with OS code 5/PermissionDenied; cleanup was not proven.
That code alone does not uniquely attribute the cause. The next correction
removes this increment's global forced-breakaway launch regression and its
extra no-Job transfer precondition, while preserving original live Child
checks, all file custody and bounded retention. A new actual result is required.
That correction's run `40438285802` built both images and advanced all native
cases past startup, backup, fenced CAS and candidate preparation. All four
native cases then failed because the tests addressed the staging root instead
of its fixed `job` subdirectory for the transfer record or exclusive job lock.
The actual layout is established by `windows_update_preparation::stage_pair`;
the fixture successor corrects those four locators only. The failed run and
unproven cleanup remain recorded; it is not six-test acceptance.

The sole corrected-source [Windows run 40439231255](https://github.com/EvoEvolver/treer/actions/runs/40439231255)
at `a54993ad39de67961cc9c5f7a4275577188cf033` passed all six controls
(portable2/native4, zero failed or ignored) on Windows2022/Rust1.90/FixedNTFS.
Both same-source builds and test execution exited zero. Both dev/test image
hashes and sizes were unchanged; the private TEMP was empty and removed.
The result receipt SHA256 is
`b72e87ff6ecf92843614a970c6d0ecf28ed8f5e5ed0526168a70e68002abc114`.
It verifies this bounded transfer/release contract, including the fixed job
record's denial of writes, conflict refusal, original Child exit refusal and
independent receiver job/lease retention after disconnect. The disconnect
control observes 100ms of retention and explicitly stops only its owned Child;
it does not demonstrate a real creator-process exit, full 120-second expiry,
Job escape, Host survival or power-loss durability. The prior three failed
runs and their unproven cleanup remain failures.
No old selector/matrix/full gate is required by this increment.

Remaining complete transaction: use the transferred worker's candidates for
publication with durable phase boundaries; compose selected Controller-only
restart and health within the finite lifetime; preserve v2 deletion/denial fences
through whole-pair/startup rollback; record terminal durable outcome and reconcile
the loaded writer fence; retire claims only after safe terminal proof. No image
publication, restart, rollback, outcome or retirement is added here. Public
Windows capability remains OFF; Host/live/publisher are untouched.
