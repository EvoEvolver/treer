# Rust updater platform delivery (#111)

This records current implementation and the next isolated work, not production
activation approval. Ordinary release downloads and update transactions are
different acceptance scopes.

## Distribution is complete

#252:9394 delivered v0.1.16, source
`fef8e276680311eab94f820e1a1ddcfd1f9e61e5`, for Linux x86_64/aarch64,
macOS x86_64/aarch64 and Windows x86_64. Ordinary artifact run `40275176289`
succeeded. Independent canonical HTTPS verification covered 21 objects, three
official signatures and 15 binary hashes. Receipt SHA-256:
`a2a3e8f7280354349edb39493f51e7cc06c6ff359a1cc9a393e3cc6cc59b8546`.
Publisher custody, public key and missing platform packages are not blockers.
Those checks did not install or execute an updater on any machine.

## Implementation versus acceptance

| Platform | Existing implementation | Actual transaction acceptance | Next independent action |
| --- | --- | --- | --- |
| Linux x86_64 | Signed `update` replaces Controller/CLI and reactivates selected services. Separately, Unix native worker has private journals, installation/enrollment locks, authenticated handoff, rollback and launcher arbitration. | The isolated native worker, real HTTP/WS/PG/browser same-operation flow and systemd topology have passed in their recorded scopes (#222); those are not a production `update` command run. | Reuse those results. Prepare a named installation's supported inventory/impact/rollback contract before any live operation. Do not rerun that matrix. |
| Linux aarch64 | Same Unix source, ordinary binaries distributed. | No aarch64 whole-transaction run established by #252. | A single disposable aarch64 transaction entry using the existing native fixture, after a sole executor has a suitable isolated runner. No new implementation merely for CPU architecture. |
| macOS aarch64/x86_64 | Same Unix transaction engine and ordinary signed `update`; Darwin image identity, filesystem and socket-peer code exists. | Focused Darwin primitives/source checks do not prove the complete launchd-owned worker survives Controller replacement. Intel artifact construction is not that acceptance either. | Add the launchd topology to the existing isolated native fixture; prove original Host identity, old Controller exit, new identity, worker ownership, no manager respawn and complete cleanup in a disposable runner. Do not use the current user's enrolled service. |
| Windows x86_64 | Signed wire verification compiles, but `service::update` explicitly refuses activation. `self_update` and its image/worker/activation modules are Unix-only. The Windows envelope is test-only. | No Rust Windows whole transaction has passed. Prior PowerShell/ConPTY/file-transfer checks are not Rust updater acceptance. | Implement native storage, then two-rename image publication/rollback, detached ownership handoff and health reconciliation; execute each leaf on Windows before exposing a transaction entry. |

The browser/native capability remains hard OFF on ordinary installations on all
platforms (`controller_update::native_accepted`), independently of the existing
signed CLI command. No change here opens that gate. The proposed unified
`self-update` CLI is not yet a shipping command. The Python/PowerShell engines
remain until feature parity and native acceptance justify their retirement.
Ordinary signed channels also lack a durable sequence/expiry anti-rollback floor;
the operator catalog's revision checks do not supply that protection to the
ordinary signed updater.

## First implemented Windows leaf

`treer-platform::windows_update_store::UpdateStore` supplies private bounded
opaque record publication on local fixed NTFS only. It opens an existing
physical directory, retains non-delete-sharing ancestor handles, refuses a weak
owner/DACL rather than repairing it, and serializes writers with a stable
exclusive file handle. New files have current-user/LocalSystem protected DACLs
from creation and non-inherited handles. A next record is flushed before the
same-directory Win32 rename; readback checks its contents. Empty/oversized data,
reparse objects, hardlinks, unreadable state and an unresolved next file refuse
publication. Errors preserve evidence; elapsed time does not authorize retry.

This is a storage primitive, not an update receipt schema or recovery authority.
It does not interpret phases, authenticate content, bind operations, replace an
executable, transfer a lock, spawn a process or enable updates. It provides atomic
record visibility, not a claim of power-failure durability. Same-account code is
trusted. Separate journal schema/phase validation must precede engine use.

Author Windows GNU target strict all-target Clippy checks compilation only.
The four `windows_update_store_` tests require native Windows NTFS; their exact
entry is:

```powershell
cargo test --locked -p treer-platform --lib windows_update_store_ -- --nocapture
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
```

This entry creates only its own fresh temporary directories. It never opens an
installed Treer store or invokes Host/Controller/Agent commands. One Release
executor records exact source, native test count/exit and cleanup; a cross-target
check must not be relabeled as native success. No publisher build or five-platform
matrix is needed for this leaf.

Native acceptance is complete for this storage scope: Windows Server 2022 run
`40279041664`, source `1bdf13f801178e32633de737e4993f62117fcdaa`, wrapper
`96a842a5dec5e9d892e508c2046f2198ff4ef9a5`, on fixed NTFS. The four named tests
passed with zero failures/ignored tests and exit zero. Independent post-test
inventory found no remaining fixture entries; cleanup was proven. The retained
`RESULT.json` SHA-256 is
`714a4b188429c8d1caf29964ad59b3b9f7568d3b8a4d59651072ab260df856e9`
(#111:9404). Core independently matched that hash, platform record and test log.
The single source review is #111:9401. These results do not establish executable
replacement, worker survival, health reconciliation or a complete Windows update.

## Single-use Windows intent

`windows_update_intent` adds a separate `TRWININT` schema version 1, capped at
4 KiB and using only the `reserved` state. It binds the RFC UUIDv4 operation,
workspace/server, installation volume/file identity, old and target Controller
and CLI hashes, and signed-manifest hash. Unknown/duplicate fields, malformed
identifiers, other versions/phases and mismatched independently supplied inputs
refuse. This is not the earlier test-only `TRUPDWIN` draft envelope.

On Windows, `reserve` consumes one exclusive `UpdateStore` holder and publishes
only when no record exists. A matching existing intent is also refused, so it
cannot become a replay entry. Read-only `inspect` checks the entire binding;
it supplies no continuation, execution or rollback permission. A write error
retains its unknown outcome and all evidence; callers must not retry. Existing
unresolved staging records remain refused by the underlying store. These are
atomic-visibility semantics, not a promise of power-loss durability.

The binding's digests alone are claims; they require the signed admission layer
below to independently verify signatures and open/pin the physical installation.
The intent schema itself does not provide that admission, image replacement or a CLI/capability entry. There
is deliberately no generic phase setter, success marker or recovery executor.
Portable schema controls can run on the author machine; the additional
`windows_update_intent_native_single_reservation_and_read_only_reconciliation`
test requires a fresh private Windows NTFS temporary directory. Its native
result is not supplied by the earlier four storage tests.

Author checks: two portable schema tests passed on macOS; strict default
all-target Controller Clippy, workspace formatting, docs and diff checks passed.
Windows cross-target compilation was not run locally (the Controller dependency
graph needs an unavailable cross C compiler). The single native entry for this
new leaf is `cargo test --locked -p treer-agent-server --bin treer-agent-server
windows_update_intent_ -- --nocapture`: expect three tests on Windows, including
the reservation test, with zero ignored/failed tests and independent fixture
cleanup. Native execution completed in the sole run `40281612432`, source
`720c522f1a3991313bcd00d003c383b5d12583a7`, wrapper
`a60592d91045ead2e345c87758c097445205c436`: all three passed, zero failed/ignored,
exit zero, on fixed NTFS (Windows 2022 image `20261022.344.1`). Independent
temporary-directory inventory was empty and cleanup proven. Core matched the
platform record, test log and `RESULT.json` SHA-256
`8a7abe78cb9551de6fbb6ea2d6c3fa56dde7a4b886a5d228fe1eeb8b9248b359`
(#111:9418). The single source review is #111:9412. This accepts the intent/schema
leaf only, not signature admission, image replacement or a complete transaction.

## Signed Windows input admission

`windows_update_admission` reuses the ordinary Rust consumer's existing
Ed25519 channel/manifest validation, manifest hash binding and unique
`windows-x86_64` Controller/CLI artifact selection. The new offline document
entry checks all four metadata buffers against the existing 1 MiB bound;
its verifying key is separately trusted input, never supplied by the manifest.
No network, downloaded code execution or new channel is needed for admission.

`treer-platform::windows_update_inputs::PinnedPair` opens existing private,
physical, fixed-NTFS directories and the fixed `treer-agent-server.exe` and
`treer.exe` names. It retains all ancestor handles and single-link, non-reparse,
private file handles with no write/delete sharing. Admission seeks each held
file to zero, streams bounded full-file hashes, matches target size/hash against
the authenticated manifest, and obtains installation volume/file ID from the
retained directory handle. Staging and installation cannot be the same physical
directory. Existing weak ACLs are refused rather than repaired.

The privately constructed `AdmittedInputs` owns all four pinned files. Consuming
it for one intent returns `ReservedInputs`, which retains the handles but has no
second-reservation, publication, rollback, health or execution method. The
installed images are only observed. No worker/Job
handoff is claimed; these read pins must be explicitly reconciled with future
rename mechanics, not dropped and replaced by an unchecked path reopen. This
leaf also does not authenticate the old pair's publisher or assert its running
process identity, target PE loadability, freshness or power-loss durability.
Only target bytes are authenticated against the separately trusted signer.

The two portable admission controls and eight affected ordinary signed-consumer
regressions passed on the author Mac. Controller default all-target strict
Clippy and platform all-target strict Clippy for Windows GNU and Linux GNU
passed, as did formatting/docs/diff checks. Controller Windows cross-compilation
was not run locally because its cross C compiler is unavailable. The initial
test-fixture borrow-check error was corrected before the test pass. Two further
native controls require Windows: held files refuse write/rename and reserve the
exact physical binding without replacement; corrupt/same-root/hardlinked/weak
ACL inputs refuse before creating an intent.
The single focused entry is `cargo test --locked -p treer-agent-server --bin
treer-agent-server windows_update_admission_ -- --nocapture`, expecting four
passed, none failed/ignored, exit zero, and independent task TEMP cleanup. The
earlier store/intent native runs do not establish this admission result.

Release executed that entry once in run `40284733737`, pinned to source
`aaa633664038d93863c68f2f341d031fcb32cb49` and wrapper
`2af1b8448592e7cc122e099a638968ece3d63612`. Windows Server 2022, Rust 1.90
and fixed NTFS image `20261022.344.1` produced four passed, zero failed/ignored
and exit zero. Core independently checked the result, platform and four named
test log entries. The task TEMP had no remaining entries, cleanup was proven,
and the empty root was removed. The result receipt SHA256 is
`e1939bfaed6f1673afdcd40ff4c67e0e176ff14cb64ff190843ab3c00e75c0b4`.
This establishes signed pinned-input admission through one reservation only;
image replacement, mutation locking, Job ownership and health remain untested.

## Installation-wide Windows lease

Signed admission requires a `LockedInstallation` before it
admission hashes the installed pair. It consumes an existing `PinnedPair` and
opens the fixed `.treer-controller-update.lock` in that pinned physical private
NTFS directory. The lock uses the same checked private, regular, single-link,
non-reparse, share-zero file opener as the receipt holder. Only an absent name
may be exclusively created; an existing name is neither truncated, deleted nor
ACL-repaired. The receipt holder's acquisition is factored into this shared
helper without changing its behavior.

`AdmittedInputs` and then `ReservedInputs` retain this lease with the installed
pair, so another participating admission targeting the same physical install
cannot proceed merely by choosing another task receipt directory. Target image
pins remain read-only. This is OS lifetime exclusion only, not a durable
operation claim, crash fence, worker ownership or authority to resume after an
owner exits. Same-account code that does not use this contract remains outside
the cooperating-updater guarantee. No caller can release only the lease while
retaining the admitted/reserved value.

Two new Windows-only `windows_update_lock_` controls cover exclusion before and
after reservation across distinct job directories, refusal of lock write/delete/
rename, stable-name retention after lease release and existing-intent refusal;
and hardlinked/weak-ACL lock refusal without repair, intent or image mutation.
Prior admission four-test acceptance above remains attributed to its original
source, not this successor.
Author checks passed: Controller macOS all-target strict Clippy, platform
Windows GNU and Linux GNU all-target strict Clippy, formatting, documentation
and diff checks. No unavailable local cross C toolchain was installed. The
focused entry is `cargo test --locked -p treer-agent-server --bin
treer-agent-server windows_update_lock_ -- --nocapture`, expecting exactly two
passed, none failed/ignored, exit zero and independent task TEMP cleanup.

Release ran that entry once in `40286956540`, exact source
`8c1d77c4e3da7fec4a7bd1f449346f5e378b7cbe` and wrapper
`e6b8ffbd38ad08f5208c07d56b51f64abdeec8e8`. Windows Server 2022/Rust 1.90,
fixed NTFS image `20261022.344.1` produced two passed, zero failed/ignored and
exit zero. Core independently checked both named log entries, platform and
result. TEMP remaining entries were empty, cleanup was proven and the empty
root was removed. Result SHA256:
`27c4ff6798ea1692e54365705d25eac9f96d0633b31a76e28bb5015de9234be5`.
This is native lease acceptance only; no image publication, durable crash
fencing, Job ownership, health or installation authorization is established.

## Installation-wide single-use claim

The source successor closes the different-job reservation bypass after OS lease
release. Before a task intent can be reserved, `AdmittedInputs::reserve` encodes
the already authenticated full `Binding` using the existing strict intent schema
and exclusively creates `.treer-controller-update.claim` in the physically
pinned installation. Its lease and image pins remain held throughout creation,
write, flush, same-handle bounded exact readback and subsequent task reservation.
The platform layer only stores opaque bytes, bounded to 4 KiB; no signing or
trust decision moves into it.

Any pre-existing claim name refuses creation, including identical bindings,
partial/invalid records, reparse points and unreadable names. An error preserves
the claim and task evidence; neither branch overwrites, deletes or retries. In
particular a claim created before a task intent failure is still consumed.
`inspect_installation_claim` permits bounded checked read and exact schema/full
binding comparison only. A matching observation cannot resume a job. No claim
retirement/reset/recovery-execution API exists yet: later safe terminal
retirement must be implemented before an engine can perform subsequent updates.

This is a process-exit fence for cooperating callers while the installation and
record remain intact, not power-loss/volume durability, rollback, an anti-rollback
high-water mark, protection from other same-account code or update success.
Production capabilities and the missing worker/image engine remain unchanged.
Native controls are pending under `windows_update_claim_`: a child test process
exits without Rust destructors after reservation, then a fresh owner must reject
both same-binding and different-binding submissions through a different task
root; a task-intent failure preserves the consumed install claim; and partial,
hardlinked or weak-ACL claims refuse without repair or task/image mutation.
Bounds refuse before file creation. None of the earlier accepted native leaves
is relabeled as acceptance for this successor.
Author Controller macOS all-target strict Clippy, platform Windows GNU and Linux
GNU all-target strict Clippy, formatting, documentation and diff checks passed.
The Controller's Windows native compilation is pending with the focused entry:
`cargo test --locked -p treer-agent-server --bin treer-agent-server
windows_update_claim_ -- --nocapture`, expecting three passed, none failed/
ignored, exit zero and independent TEMP cleanup. Its child process is only the
same filtered test executable on fresh task-owned files, never an installed
Controller or Host. No unavailable cross C compiler was installed locally.

## Remaining ordered leaves and waiting boundaries

1. Native NTFS store, single-use intent, signed pinned-input admission and lease
   acceptance are complete within their stated scopes. The install-wide claim
   successor above is implemented with native acceptance pending. Next
   add phase-specific evidence for actual image
   publication, never a generic setter that could manufacture completion.
2. Implement Windows verified Controller/CLI image backup and two-rename
   publication with locked-image refusal and whole-pair rollback. Never treat
   restoring files alone as successful rollback.
3. Implement Windows original-worker ownership/Job escape and Host-owned
   Controller-only restart plus original workload/epoch/identity checks. Native
   isolated acceptance is required; no Host restart or blind retry on lost ACK.
4. Complete the macOS launchd fixture and the distinct aarch64 native acceptance
   entry. Existing Linux accepted layers remain reused.
5. Add signed sequence/expiry verification with a durable local high-water mark,
   then unify the supported CLI/coordinator and retire legacy engines only after
   platform parity. Do not bypass the current capability gate as a shortcut.
6. For a real installation, obtain a named target, shared-binary impact scope,
   recovery custody and one executor. That live-operation selection is the only
   user dependency; it does not block the source and disposable-fixture leaves
   above. No production activation is requested by this document.
