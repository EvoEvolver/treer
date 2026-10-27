# Same-worker independent job custody (#111)

After normal confirmed-plan admission, the existing coordinator retains the
original UpdateStore holder through the same original Child and Holding session.
The previously accepted reservation now lends a reduced-rights lock reference
to that exact Child during its already-latched input exchange. There is no PID
lookup, ambient inheritance, stdout/stderr channel change, new process or retry.
Private wire data adds the original job directory/lock IDs and a child-local
handle value; the whole-offer receipt digest covers every field. This is not a
bearer permission, key source or lifecycle authority.

Platform DuplicateHandle supplies read-attributes/synchronize rights only, not
write/delete. The receiver borrows that numeric source and makes an independent
local reduced-rights duplicate, never takes ownership of or closes an arbitrary
source handle. It independently authenticates the signed whole inputs with
normal configured trust before matching private fixed-NTFS job directory and
lock IDs and the complete canonical intent. It opens the fixed receipt read-only
without write/delete sharing and retains every physical ancestor. All guards
remain with the same WorkerReceivedInputs; later Holding/backup/CAS checks read
the same receipt, with existing permanent poison on inspection failure.

The creator retains its original store/pins/lease/claim. A remote reference
created before an unknown exchange is intentionally left until child exit:
remotely closing a numeric handle after a possible receive could close a reused
handle. At most one is created by the consumed handshake per original Child.
Explicit close/session failure drops the receiver's owned guards, and process
exit drops its original remote reference. Neither receipt, close nor creator exit
permits release of sender custody. This remains within the same OS-owner trust
domain, not isolation from an account owner who controls the process.

Focused controls: new worker_job_ portable framing/receipt binding control;
real same-source signed private worker receives, inspects and prepares backup
while its independent receipt pin denies writes/deletion, then explicit close
drops that pin while the original sender job remains held; original test Child
rejects foreign directory/lock IDs, invalid child-local reference and partial
intent without receipt or replay. Native controls remain NOT_RUN at freeze.
No old selector, matrix or live installation is rerun.

Author verification: the new portable offer/whole-receipt control passed;
strict Platform all-target Clippy passed for installed Windows GNU and Linux
GNU targets; Server tests with native-update-acceptance strict Clippy, fmt,
docs and diff checks passed. These are not native Windows runtime acceptance.
The two new native controls require one exact-source Release execution after
ONE independent source review, with the existing same-source dev fixture,
compiler-metadata test executable selection and direct-child-only TEMP contract.

Actual acceptance: ONE independent source review (#111:9884) passed for
`443981bda6306d85300ef545fc07ccd89ab2c75c`. Release sole run `40427163724`,
wrapper `c6316d2120f670493eab0a9a6680c65a7a49e7c9`, completed on Windows
2022/Rust 1.90 Fixed NTFS image `20261022.344.1`: all 3 new controls passed
(portable 1/native 2; 0 failed, 0 ignored, 361 filtered). Dev build, test build
and direct test exits were zero. Core independently checked RESULT, platform,
compiler artifact and tests: one exact local Server test executable selected;
both dev/test image sizes and hashes unchanged; matching private markers with
child not precreated; remaining entries empty, cleanup proven and empty root
removed. RESULT SHA256:
`9419c8a8eb2ecbe8986935536dae578ffeb9b0b483380b4f79e929e9e46fe50b`.
Ignored receipts:
`.treer/reviews/windows111-worker-job-443981bd/run-40427163724`.
This is actual same-source private-worker custody acceptance, not a live machine
upgrade, sender release, durable takeover, restart/health or complete updater.

This is a product-path custody combination, not durable takeover or complete
Windows update acceptance. Remaining work: explicit mutation/lifetime and
durable transfer/release contract, worker pair publication, selected
Controller-only conditional restart and health, fence-preserving whole-pair
rollback, durable outcome and safe claim retirement. The current worker still
requires its original creator live and serves a bounded session. Public
capability remains OFF; Host/live/capability/publisher operations are untouched.
