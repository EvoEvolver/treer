# Same Holding worker candidate preparation (#111)

This increment advances the existing confirmed normal-Trust path from independent
input/backup custody to real new/restore file preparation. It does not activate
Windows updates. The public capability remains OFF and the public consumer
continues its finite read-only session. No Host/live/publisher operation changes.

`Preparation::prepare_worker_candidates` uses the SAME still-Holding coordinator,
original Child/connection, confirmed operation and selected loaded runtime. The
existing reservation/catalog/claim checks and configured Trust mint a distinct
non-wire candidate-only permit. Backup preparation must already have completed.
An ordered full-context command consumes its attempt before awaiting a reply;
failed/cancelled/lost replies cannot retry, reconnect or close an unknown session.

The receiver checks its live original creator, independent claim/job intent,
runtime-derived physical/raw state and old backup. Platform reuses the existing
four fixed CREATE_NEW and flush/seal helpers. Worker staging independently pins
installation ancestors and duplicates only reduced rights to the original
installation lock after matching the exact fixed lock file ID. The original
sender and receiver image pins remain held. Candidate-ready custody has no
publication/conversion/release method.

The worker copies the complete authenticated target pair to the two new files,
and its independently authenticated old backup to the two restoration files.
Every complete held candidate is flushed and rehashed before ACK and retained
before the reply await. Subsequent inspections rehash them alongside existing
custody checks. A stale second destination can leave the first empty file;
neither that partial file nor the old destination is removed/repaired/retried.
Unknown retains durable claim/intent and original sender custody. Explicit close
ends receiver handles but leaves all candidate names, while the original sender
guards remain held. Neither response nor creator exit grants sender release.

New focused selector `windows_update_worker_candidates_` has one portable control
for ordering, wrong response and cancellation/no replay, plus two Windows native
controls: same-source signed private worker prepares and retains all four complete
candidates while the installed pair/job remain held; partial destination or a
changed loaded Store snapshot refuses without overwriting or replay. Native
controls are NOT_RUN at source freeze and require one exact-source isolated
Release execution after ONE independent source review. No old selector or matrix.

Actual acceptance: ONE independent source review (#111:9893) passed for
`e0a5ee017c265aa9407abef0ea2fd6152b09d558`. Sole Windows run `40429745800`
used wrapper `351d160c1ba4b5cbaf4b07a996681e466b9d0510`, Windows 2022/Rust 1.90
Fixed NTFS image `20261022.344.1`: 3 passed (portable 1/native 2), 0 failed,
0 ignored, 364 filtered. Dev build, test build and direct test exits were zero.
Core independently checked RESULT, platform and tests: one exact local Server
profile.test artifact; dev/test image sizes and hashes unchanged; matching private
catalog/TEMP markers with child not precreated; no remaining entries, cleanup
proven and empty root removed. RESULT SHA256:
`2c7fb224fccda1a5ccf04e1dcde3534c6cb910c285593bd8e25839b4553397a6`.
Safe ignored receipts are under
`.treer/reviews/windows111-worker-candidates-e0a5ee01/run-40429745800`.
This proves same-source private-worker candidate preparation and the named
refusals, not installed-image replacement or a live machine upgrade.

Remaining normal transaction work: recorded consuming mutation/lifetime transfer,
explicit sender image-pin release while lease/job/claim and verified restoration
custody survive, worker pair publication, selected Controller-only conditional
restart and health, fence-preserving whole-pair rollback, durable outcome and
safe claim retirement. The worker still requires its creator live within a
bounded session. This preparation is not durable takeover, runtime rollback,
full transaction acceptance or permission for production activation.
