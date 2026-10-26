# Same-session worker backup preparation (#111)

The minimal successful mutation continues the SAME still-Holding coordinator,
not a previously closed read-only session. `Preparation::prepare_worker_backup`
checks the original selected installation/runtime and poisons its continuation
before awaiting. Coordinator rechecks normal Trust and borrows a backup-only
permit from the original confirmed reservation. That permit checks the full
runtime binding, operation, current fixed catalog/confirmed plan and retained
installation claim. No offer key, arbitrary path or new Policy switch is added.

A distinct command/reply binds the original whole-offer receipt and monotone
sequence. Both sides consume the one backup attempt before effects/await. The
worker keeps its independently verified input pins, inherited installation lock
and runtime-derived raw state in the same owner. Only after the explicit grant
does it exclusively create `worker-backup` beneath the pinned operation target,
copy and verify the old Controller/CLI, preserve exact raw startup bytes and
denial fences, seal read-only backup handles, and publish/read back bound evidence.
Those handles remain held through subsequent inspection until explicit close or
session exit. An acknowledgement proves this finite preparation, not permission
to drop sender pins/lease/claim. Sender guards remain held on either outcome.

Existing destination, changed raw state, wrong reply, cancellation or any partial
write cannot replay or adopt outputs. The original total 30-second session and
three-second exchange budget apply. Synchronous filesystem calls are not forcibly
interrupted; a late result cannot acknowledge success. Errors keep original
sender custody while that owner lives; destruction is not proof of child exit or
a durable lifetime transfer. Existing claim denial remains.

Author validation: the two new portable `windows_update_backup_transition_`
tests pass (ordered explicit command/single-use and wrong-reply/cancel refusal).
Strict Server Clippy including tests and `native-update-acceptance`, formatting
and documentation checks pass. The new Windows native success/failure controls
were NOT_RUN at author freeze; exact-source native acceptance is recorded below. They use
normal saved-service installation, normal trust and the same-source private dev
worker. Prior consumer/native selectors are not rerun.

Release's ONE scoped source review of
`c365327adf9e91bf682a9952db1531cb75cf2c71` passed (#111:9843). Sole Windows run
[40415548945](https://github.com/EvoEvolver/treer/actions/runs/40415548945), wrapper
`6bae7e6280763f99176d8ac45a68802f3b077702`, passed same-source dev build,
JSON test build and direct test execution, each exit zero. Exactly four new
`windows_update_backup_transition_` tests passed (two portable, two native), none
failed or ignored, 346 filtered, on Windows 2022/Rust 1.90/fixed NTFS image
`20261022.344.1`. The success case used normal saved installation/configured
trust, the real private same-source dev worker and original Holding connection:
no backup existed after reception/inspection; explicit grant produced the old
pair and exact raw startup backup, kept independent read-only backup handles,
and allowed further inspection/explicit close while sender guards stayed held.
The native failure case refused a pre-existing destination without adopting it,
and a normal loaded StartupStore denial change before any backup, retaining
unknown/no-replay and original sender custody.

Core independently read RESULT/platform/test logs and the unique local Server
compiler-artifact. Dev executable size 37,048,832 and SHA256
`e7dc8e843a4905cecc6d72804e59ed5065d38e78172144100d9131ef6ec69be6`, and test
executable size 45,142,016 and SHA256
`5335e63a2201e4b19a294dee40929885fb44d7f0de9785ef86b622b2b4e2ff20`, were
unchanged before/after. Only direct tests received matching private catalog/
LOCALAPPDATA markers and isolated TEMP/TMP; the child directory was not
precreated. Remaining entries were empty, cleanup was proven and the empty root
removed. RESULT SHA256:
`8165bcecf016fd28a8bc27406f5b3d0aaa54e50e0fb8a280a7d5fc6749878910`.
Receipts are under
`.treer/reviews/windows111-backup-transition-c365327a/run-40415548945/`.
Windows build warnings were confined to unrelated existing provider-cli source
and tests; this result is not a Windows strict-Clippy claim. No old selector,
matrix, live Host, installed image, publisher or capability was operated.

The public consumer's finite read-only session is unchanged and still closes
explicitly; it cannot be restarted to request mutation. The new internal method
is a continuation seam for a future full transaction. Public capability remains
OFF. Remaining work is explicit durable mutation/lifetime transfer and sender
release, synchronized StartupStore CAS/fence handling, installed pair publication,
selected Controller-only restart/health, whole-pair runtime rollback, durable
outcome and safe claim retirement. Backup preparation alone supplies none of
those contracts. No installed images, Host, live service, publisher or capability
are operated by this change.
