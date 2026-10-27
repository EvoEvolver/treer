# Original job holder lifetime (#111)

The successful single-use intent reservation now returns the original open
UpdateStore instead of dropping its exclusive receipt.lock and physical ancestor
handles. ReservedInputs owns that same store alongside installation lease,
claim, images, selected runtime and confirmed-plan custody. Existing worker and
Holding session owners already retain ReservedInputs; no new session, message,
permission or release token is introduced. Controller-state checks read and
compare the exact intent through the retained store. The file-only publication
path moves the same store into PublishedInputs, across explicit pair restoration,
and verifies the intent before effects and during current-pair verification.

Dropping an owner or returning an error releases its OS holder, but leaves the
durable intent/installation claim and any partial evidence. Reopening for
inspection does not permit another reservation, even with identical binding.
This is original sender job-holder lifetime, not independent receiver ownership,
creator-exit survival, durable transfer or permission to release sender custody.
It does not make two renames atomic or grant runtime restart/health authority.

Two new native controls target only windows_update_job_holder_: the original
job remains exclusively held through backup, file publication and explicit
restoration; failed backup drops the holder while preserving intent/claim fences
and rejecting replay. Existing live inspection uses the retained store rather
than opening another exclusive owner. Native execution remains NOT_RUN at
source freeze. Author checks and ONE independent review are recorded separately.

Author checks: Server tests with native-update-acceptance strict Clippy, fmt,
docs and diff checks pass on the author platform. These do not compile/run the
Windows-only controls; sole new native acceptance remains required. No old
selector, package matrix, feature fixture executable or live target is rerun.

Remaining normal transaction work includes independent durable worker/job
ownership and mutation/lifetime constraints, explicit sender release, worker
pair publication, selected Controller-only conditional restart/health,
whole-pair rollback preserving monotone StartupStore fences, durable outcome and
safe claim retirement. Public Windows capability stays OFF. No Host/live,
installation, restart, publisher or release operation is authorized by this leaf.

Actual acceptance: ONE independent source review (#111:9875) passed for
`7a88c4e7acbe0109f1b5ef3b42149fa1733ef22f`. Release's sole Windows run
`40424293013`, wrapper `d7891d377268d9187fbdaa06f82d56f9b8e06f6c`,
completed with build/test exits zero. Windows 2022/Rust 1.90 Fixed NTFS image
`20261022.344.1` ran both new controls: 2 passed, 0 failed, 0 ignored,
340 filtered. Core independently checked RESULT/platform/build/test receipts:
one exact local Server test binary selected, before/after size/hash unchanged,
matching private fixture markers with no precreated child, remaining entries
empty, cleanup proven and empty root removed. RESULT SHA256:
`27c52d6dc74bd8adca5c50231618c6774e1f66f374249ca2b12db8dc2562db62`.
Ignored receipts remain under
`.treer/reviews/windows111-job-holder-7a88c4e7/run-40424293013`.
No old selector/matrix or feature fixture binary build was run. These controls
prove original job-holder lifetime only, not full worker transfer or updater
acceptance; the remaining boundaries above are unchanged.
