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
Native controls under `windows_update_claim_` cover a child test process that
exits without Rust destructors after reservation, then a fresh owner must reject
both same-binding and different-binding submissions through a different task
root; a task-intent failure preserves the consumed install claim; and partial,
hardlinked or weak-ACL claims refuse without repair or task/image mutation.
Bounds refuse before file creation. None of the earlier accepted native leaves
is relabeled as acceptance for this successor.
Author Controller macOS all-target strict Clippy, platform Windows GNU and Linux
GNU all-target strict Clippy, formatting, documentation and diff checks passed.
The focused Windows entry is:
`cargo test --locked -p treer-agent-server --bin treer-agent-server
windows_update_claim_ -- --nocapture`, expecting three passed, none failed/
ignored, exit zero and independent TEMP cleanup. Its child process is only the
same filtered test executable on fresh task-owned files, never an installed
Controller or Host. No unavailable cross C compiler was installed locally.

Release executed this entry once in run `40290264517`, exact source
`030921bc042298031b4504edcd44079ba70477ba` and wrapper
`8f63f00c2018c9028e9dc09a8a5c75a2d92c1e74`. Windows Server 2022/Rust 1.90,
fixed NTFS image `20261022.344.1` produced three passed, zero failed/ignored
and exit zero. Core independently checked all three named log entries, platform
and result; TEMP remaining entries were empty, cleanup proven and the empty
root removed. Result SHA256:
`3c1e4e4d43b05591fb2023d25c4ecec242d0b8284a42c0d1c1603b9efd96d90f`.
Acceptance remains limited to process-exit claim retention and refusal, not
power loss, a real worker, safe claim retirement or a complete update engine.

## Verified Windows old-image backups

The next source increment consumes `ReservedInputs` to copy only the observed
old Controller and CLI images from their already-held no-write/no-delete-sharing
handles. Each stream seeks to zero, is bounded to 128 MiB and must match its
reserved old-image SHA256. The destination is an existing private physical
fixed-NTFS directory on the installation volume, distinct from both installed
and target directories. Fixed `controller.backup` and `cli.backup` names use
exclusive private creation; existing evidence or image names are never adopted
or overwritten. A second-file failure may leave a partial first file, which is
retained along with the installation claim, never cleaned up for a retry.

After both writes, the platform flushes the files and uses reduced-access
`DuplicateHandle` copies of the same file objects. It drops the writable handles
without any path reopen or sharing gap. The returned backup handles have read
access only and retain the original exclusive sharing. Core independently
hashes these held backup bytes before publishing `backup.evidence`: a strict
bounded record containing only `images_backed_up`, the full reserved binding,
physical backup directory/file IDs and sizes. Exact readback is required.
`BackedUpInputs` keeps installation lease, all source pins and backup pins;
read-only inspection rechecks bytes and evidence. There is no generic phase
setter, recovery/adoption, install replacement, rollback-success or health API.

This records old-image copies, not an authenticated old publisher, startup-store
backup, power-loss durability, running-process identity or rollback success.
Startup backup, actual pair publication/rollback, safe claim retirement and
worker/health remain missing. No production entry or capability is enabled.
Two portable copy controls cover full bytes/digest/bounds and read/write refusal;
two native controls cover copied bytes/physical binding/held access/readonly
duplicate rights, plus alias, weak ACL and partial destination refusal.
The two portable controls passed on the author Mac, as did platform Windows GNU
and Linux GNU all-target strict Clippy. Controller macOS all-target strict
Clippy, formatting, documentation and diff checks also passed. The focused entry is:
`cargo test --locked -p treer-agent-server --bin treer-agent-server
windows_update_backup_ -- --nocapture` (four passed, none failed/ignored, exit
zero and independent TEMP cleanup). No old native leaf or publication matrix
needs another run for this boundary.

Release executed that entry once in `40293422724`, exact source
`275c779010858f91d64e51e8593f66f9a9743ced` and wrapper
`9b2d5e911a40658d08698cd77455859d4ce62be9`. Windows Server 2022/Rust 1.90,
fixed NTFS image `20261022.344.1` produced four passed, zero failed/ignored and
exit zero. This includes actual reduced-access duplicate write refusal and
sharing exclusion. Core independently checked all four named log entries,
platform and result. TEMP remaining entries were empty, cleanup proven and the
empty root removed. Result SHA256:
`4b9e8c988d31775840479513703b6a9999453e27a8f97b8bc70cc6a22bdca7b3`.
This establishes old-image backup evidence only; startup backup, durability,
pair replacement, rollback, worker/health and retirement remain outside scope.

## Startup-store format compatibility before Windows backup

The maintained startup store writes version 2 whenever it retains denied Agent
IDs, including a store with no remaining startup entries. The existing Unix
updater backup previously accepted version 1 only. Preparation would therefore
refuse a legitimate denial-fenced store before the update transaction. The
backup parser now accepts versions 1 and 2, bounds and validates denial IDs,
rejects duplicate/overlapping fences and rejects version 1 with nonempty fences.
It still copies the complete credential-bearing bytes into the private job;
the existing index schema and safe entry projection remain unchanged. Its raw
SHA256 binds the format and every fence, so fence-only edits or backup tampering
are detected even with an empty entry list. Comparison never restores startup
state or removes a later legitimate fence.

Focused coverage includes a version-2 file produced by the real `StartupStore`
writer, reopening its captured backup with that loader, a later deletion fence,
backup tampering, mixed entries/fences and malformed/duplicate/overlapping or
oversized denial sets. This compatibility correction belongs to the existing
Unix backup path. It does not establish Windows startup capture, a cross-process
startup-store mutation lock, rollback authorization or a full updater result.
The author's macOS focused entry
`cargo test --locked -p treer-agent-server --bin treer-agent-server
self_update::startup_backup::tests -- --nocapture` passed all nine tests,
zero failed/ignored. Controller binary/test strict Clippy and formatting passed.
No hosted job, previously accepted native leaf or live update was rerun.

## Windows private startup snapshot after image backup

`BackedUpInputs::backup_startup` consumes the image-backup holder and an existing
private physical fixed-NTFS state directory on the installation volume, distinct
from installed, target and backup directories. Only the reserved server's normal
`<server_id>-agent-startup.json` is read. A checked readonly, single-link,
non-reparse file handle supplies one snapshot of at most 16 MiB; only NotFound
means absent. That source handle closes after reading, allowing later normal
Controller writes. Its directory/file identity is recorded as an observation,
not a continuing source pin or a promise of unchanged current state.

Core decodes real `StoredAgentStartup` entries, checks workspace/server and
generation, bounds entries and denial IDs to 4096 each, validates versions 1/2
and rejects duplicate/overlapping/invalid fences. Credentials and unknown fields
stay in the complete raw backup, never the evidence. An empty entries array with
denial fences remains a present version-2 snapshot. Absence creates no data file.

Fixed `startup.backup` and `startup.evidence` names refuse any existing object.
After raw EXCL creation, flush and reduced-access duplication of the same file
object, Core reads and validates those held backup bytes again. Only then is
strict schema-1 `startup_backed_up` evidence created and read back. It contains
the complete reserved binding, physical state/backup directory identities,
optional source/backup file IDs and optional full raw SHA256/size. The returned
holder keeps image/source pins, installation lease and readonly startup-backup
handle. Inspection rechecks image and startup backups plus exact evidence. It
never compares current startup state as a CAS, restores a snapshot, retires a
claim, replaces installed images or authorizes rollback/health success. Failed
capture consumes its holder and retains existing claim/partial evidence; there
is no automatic retry or cleanup.

Two portable format controls cover version 1/2, mixed and denial-only stores,
full-byte digest changes, scope/fence/format/bounds refusal and safe metadata.
Two Windows native controls use the real StartupStore writer for absent, v1,
denial-only v2 and mixed v2 files, check exact bytes/normal later writes/retained
lease and readonly backup pins, and reject unsafe, hardlinked, malformed,
oversized, aliased or pre-existing backup inputs. Their isolated entry is
`cargo test --locked -p treer-agent-server --bin treer-agent-server
windows_update_startup_ -- --nocapture`: require exactly four passed, zero
failed/ignored, exit zero and independent task TEMP cleanup on fixed NTFS.
The two portable tests passed on the author Mac; Controller binary/test strict
Clippy, Windows GNU platform all-target strict Clippy, fmt, docs and diff checks
passed. This adds no production command or capability. Windows Controller
compilation and the four-test native entry were subsequently accepted below.

The first Windows entry, run `40299042663` on exact `07273d83` with wrapper
`ea50cf9a`, failed compilation with E0507: a mutable pattern binding attempted
to move `File` out of a shared `Option` reference. Exit was 101 and none of the
four tests ran; this is not a zero-test pass. The successor matches explicit
`Option<&File>`/`Option<&StartupDigest>` values through `as_ref()`, retaining the
same file object and readonly rights. No clone, reopen or sharing change is
introduced. That run's TEMP was empty, cleanup proven and root removed. Result
SHA256: `378c9ce697c95cfe7c33fb43e9d1a635e434295471f0c649b1ad72e9b3814547`.
The failed run remains failed; the changed-source entry below retained the
same four-test contract.

Release's sole changed-source run `40300048784` succeeded on exact source
`c7b9a7d0f6e3a0fb029f1ed2235223b4a6180bc5`, wrapper
`311d32ad61371f37df6da6284525d928a27c4c5c`. Windows Server 2022/Rust 1.90,
fixed NTFS image `20261022.344.1` produced four passed (two portable, two native),
zero failed/ignored and exit zero. Core independently matched all four named
test log entries, platform/source and result. TEMP remaining entries were empty,
cleanup proven and the empty root removed. Result SHA256:
`dccac76bcabc57b45c1fa35ae213499dff854d81c37a02f165148600a89efe08`.
This establishes private physical startup snapshot/backup/evidence binding and
the stated format/fence/refusal controls. The state directory remains an
internal caller-supplied observation; this does not attest that it is a running
Controller's resolved configuration root. Production admission still needs that
trusted installation/runtime binding. No restore, CAS, safe claim retirement,
pair publication, health, full transaction or power-loss durability is claimed.

## Windows pair file publication and restoration

This leaf starts only from the consumed `StartupBackedUpInputs` holder. Core
checks its existing evidence, refuses pre-existing publication/restoration
records, and copies four fixed CREATE_NEW candidates in the pinned installation:
new Controller/CLI and old Controller/CLI restoration copies. Each copy is
bounded, hashed against the admitted binding, flushed and independently hashed
from its held handle. Target, backup and startup pins remain held.

The platform retains the physical installation parents and exclusive lease,
then releases only its original installed image pins immediately before file
replacement. It uses `SetFileInformationByHandle(FileRenameInfo)` on the held
candidate with DELETE rights; there is no source-path reopen and no POSIX or
delete-sharing bypass flag. The API contract is documented by Microsoft in
[FILE_RENAME_INFO](https://learn.microsoft.com/en-us/windows/win32/api/winbase/ns-winbase-file_rename_info).
Existing external no-delete-sharing readers must make replacement refuse. The
two names are replaced sequentially: a transient mixed pair is possible, so
this is explicitly not a pair-atomic operation or an activation API.

Only known physical identities authorize the next step. First-step refusal
requires both original file IDs still present. Second-step refusal requires the
known new Controller/original CLI pair before compensating the Controller from
its old copy. Both current files are then pinned and Core verifies their whole
old hashes. Unknown identities, observation errors or failed compensation stop
without another rename attempt or success evidence. Partial names and the
installation claim remain; owner exit does not authorize replay.

After two successful replacements, Core verifies both current pinned hashes and
IDs before EXCL `publication.evidence` with one of `files_published`,
`files_restored` or `publication_refused`. Current pins have reduced readonly
rights. A published holder supports one explicit `restore_files` operation using
the previously verified old pair: it verifies current evidence, refuses an
existing restoration record, releases its current pins, replaces both names,
then revalidates both old hashes/IDs before new EXCL `restoration.evidence`.
Original publication evidence remains historical. If only part of restoration
succeeds, the result is unknown and no restored-pair evidence is published.
Neither files-restored state nor a file-only refusal is runtime rollback/health
success. No installed image is executed, no claim retired, no new Controller
started, and there is no recovery/adoption or automatic retry command. A future
activation handoff must deliberately release/transfer current pins and prove
worker ownership and health; this leaf does not supply it.

The new isolated Windows entry is
`cargo test --locked -p treer-agent-server --bin treer-agent-server
windows_update_pair_ -- --nocapture`, requiring exactly four native tests,
zero failed/ignored, exit zero and independent task TEMP cleanup on fixed NTFS.
It covers authenticated Core publication/explicit full file restoration, actual
no-delete-sharing first/second-name refusal and compensation, stale candidates
and evidence, and a platform-kernel partial-restoration failure produced by a
retained duplicate of the new CLI pin. The last control is explicitly a platform
file-effect test with opaque synthetic claim, not a signed Core operation or
health test. Native execution and Windows Controller compilation passed in sole
run `40304321706`, exact source `1e638b1353f1fba9bf87b6399b630c0d74a7dc08`
and wrapper `c6ad57b7e3b5a5361ac277ce869b23c6c62d979f`, after one independent
source review (#111:9484). Windows 2022/Rust 1.90, fixed NTFS image
`20261022.344.1`: four passed, zero failed/ignored, exit zero; independent TEMP
inspection found no remaining entries and proved cleanup. Core independently
checked the four test names, summary, platform and RESULT SHA256
`15abcfdb8f7a9582993e561562ae225071bded1b74328782ef7ff6c63ecd04b9`.
Receipts are under
`.treer/reviews/core111-windows-pair-1e638b13/artifact/` (`RESULT.json`,
`platform.json`, `tests.log`). This is file-effect acceptance only: no image
executed, no runtime rollback/Job/health, claim retirement or production
state-root admission was accepted. Windows GNU platform all-target strict Clippy
and author macOS Controller binary/test strict Clippy cover local compilation;
no accepted storage/intent/admission/backup leaf or platform matrix is repeated.

### Runtime-owned state-root admission

Current `main.rs` supplies `ControllerConfig.startup_store_path` from
`service::startup_state_path(server_id)`; `controller.rs` loads exactly that
path into its live StartupStore. On Windows the source resolver uses
`TREER_STATE_DIR` when configured, otherwise `LOCALAPPDATA/Treer/state`, followed
by the normal server filename. Those are resolver rules, not observations of a
deployed Controller's environment.

The next source leaf connects the internal admission to the selected live
`ControllerRuntime`. Its `windows_update_state_for_operation` derives the path
from the SAME already-loaded StartupStore and checks the requested operation
(UUIDv4), workspace and Server against that runtime. The configured path must be
absolute with the exact normal Server filename and no parent traversal. On
Windows capture opens/pins its physical private fixed-NTFS parent. A new opaque
non-Clone/non-Deserialize object retains that runtime, operation/scope, Controller
instance UUID and Host epoch digest. Safe evidence cannot reconstruct authority.
It neither accepts a state path nor resolves ambient defaults.

`prepare_for_controller` consumes that authority with the verified image inputs,
refuses installation/target aliasing and wrong volume before any claim/intent,
and derives its scope from the selected runtime. The backup destination cannot
alias this state root. `backup_configured_startup` only snapshots the held
configured directory; it records the physical directory and runtime-instance
evidence alongside the existing complete operation binding. Authority and the
directory pin survive reservation, startup backup and file publication/explicit
restoration. Whole evidence comparison checks against the retained authority,
not just a decoded record. Existing direct `prepare`/`backup_startup(directory)`
entries are now cfg(test) fixtures only. StartupStore writes are not frozen:
this remains one bounded raw snapshot, not a restore/CAS or quiescence claim.

Three new author portable controls passed (path refusal, scope binding and
retained distinct runtime instances). An initial fixture used inconsistent Host
metadata and was correctly refused by the existing identity guard; fixing that
fixture's config/metadata source produced the three passes without guard changes.
The new Windows entry is `cargo test --locked -p treer-agent-server --bin
treer-agent-server windows_update_state_ -- --nocapture`, exactly four tests
(three portable, one native), zero failed/ignored and exit zero, plus fixed NTFS
and independent task TEMP cleanup. The native control uses a normal from_sync
runtime with a closed fixture Host and synthetic signed inert image bytes:
configured-directory pinning, signed admission, exact configured startup backup
instead of a same-volume private decoy, and pin retention across file publication.
It never executes those image bytes or proves deployed configuration. Sole run
`40310583031` passed all four tests (zero failed/ignored, exit zero) on Windows
2022/Rust 1.90/fixed NTFS image `20261022.344.1`, exact source
`9e3921607b0219af0cdee9c738eadbcb68096495`, wrapper
`370cb3945c8551b6a39fb36fd22183137e807cf4`, after independent review #111:9495.
Core checked all four names, summary, platform and empty TEMP/cleanup evidence;
RESULT SHA256 is
`5dbf3114306486b65dda92222b257becaed7c6a96a80d880402540ea23f98a66`.
Receipts: `.treer/reviews/core111-windows-state-9e392160/artifact/`
(`RESULT.json`, `platform.json`, `tests.log`). This accepts the internal runtime
binding/snapshot scope only. Author macOS binary/test strict Clippy passed.

No production updater command or capability entry is wired. Its future
coordinator must call this internal selected-runtime entry after normal
authentication/Policy and selection; the context itself grants neither dispatch
nor restart permission. It is not transferable to a child worker via JSON.
Older deployed status/ServerInfo responses still do not attest runtime config;
the old Mariana inventory gap remains. This is not a publisher or missing-package
dependency. The next boundary is worker custody and a Controller-only handoff
bound to this operation, the original Controller instance and surviving Host
epoch. That requires real child/process ownership, exit and identity checks,
not an argument string or decoded evidence posing as an owner. No worker/Job
escape, restart, health or safe claim retirement is supplied by this leaf.
Named live scope, shared-installation impact and recovery custody remain separate
requirements. Ordinary capability stays OFF and Host is untouched.

### Accepted leaf: read-only Windows process observation

`treer-platform::windows_update_process::ProcessObservation` retains a single
query/synchronize OS process handle, its creation time and a read-only handle to
the OS-reported executable pathname. Zero-time process waits bracket queries;
an exited process is refused even if its exit code is 259 (`STILL_ACTIVE`).
Creation identity and reported-path file IDs are rechecked against the same
retained handles. Any-Job membership uses that process handle and errors remain
unknown; `require_outside_jobs` refuses membership in any Job rather than trying
breakaway. No PID-file/argv/environment lookup, spawn, signal, restart, wire
constructor or authority to adopt a process is provided.

This is deliberately observation, NOT worker ownership or proof of the originally
mapped executable's file identity. Windows reports a pathname that may already
have been replaced before capture. Opening that pathname cannot authenticate
the old mapped image. The future parent/child handoff must retain the actual
created Child/process handle and pre-spawn authenticated image custody, bind
that relationship to the operation/runtime authority, then prove readiness and
Host/Controller-only restart semantics. A reported-path match or a Job boolean
must not be substituted for that contract.

The three new native controls cover current identity/reported-file mismatch,
an owned test child exiting with 259 while its observation handle remains, and
a real Job-bound owned child refusing escape. The test child only runs a fixed
ignored helper in the same platform test executable with cleared environment,
reads its own stdin and exits; its parent retains Child and kills/waits only that
child on errors. The helper name does not match the parent entry filter.
Focused entry: `cargo test --locked -p treer-platform --lib windows_update_process_
-- --nocapture`, exactly three passed/zero failed/zero ignored/exit zero, plus
independent task TEMP cleanup. Sole run `40311536049` passed all three tests
(zero failed/ignored, exit zero), exact source
`e675c6299844c7f98fefa739d8751ce43699e560`, wrapper
`ed7de23ff98b97580abc5ccab4b31b936b522d1b`, after independent review #111:9501.
Windows 2022/Rust 1.90/fixed NTFS image `20261022.344.1`; Core checked all three
names, summary and platform, empty remaining entries and cleanup evidence.
RESULT SHA256:
`07ea4bf36c9b71171c866d0e11279a9165c99c7a3c319f44946c796d8d97503f`.
Receipts: `.treer/reviews/core111-windows-process-e675c629/artifact/`
(`RESULT.json`, `platform.json`, `tests.log`). This does not extend the accepted
scope to mapped-image attestation, actual worker ownership, Job escape or
restart/health. Windows GNU platform all-target strict Clippy also passed; no old platform or
Controller acceptance selector is repeated. The initial cross-check used the
wrong windows-sys constant spelling; using its actual `PROCESS_SYNCHRONIZE`
constant fixed that source compile error without changing access rights.

API references: [QueryFullProcessImageNameW](https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-queryfullprocessimagenamew),
[IsProcessInJob](https://learn.microsoft.com/en-us/windows/win32/api/jobapi/nf-jobapi-isprocessinjob),
[WaitForSingleObject](https://learn.microsoft.com/en-us/windows/win32/api/synchapi/nf-synchapi-waitforsingleobject).

### Accepted leaf: created Child custody bound to selected runtime

`ReservedInputs::spawn_owned_child` consumes the existing reservation, checks its
runtime-owned state against the selected Controller instance, Host epoch,
workspace/server and full operation binding BEFORE `Command::spawn`, then owns
the actual returned `std::process::Child`. The opaque `ControllerUpdateChild`
retains the entire reservation (image pins, installation lease and state-root
pins), runtime and binding while the owner exists. Successful teardown is
observed before normal release; dropping after an unknown outcome releases OS
custody without proving child exit. It cannot be
constructed from a PID, process observation, JSON receipt or reported pathname.
Neither a matching operation ID alone nor a second runtime with the same server
and workspace is sufficient to query or stop it.

Scoped `poll` uses the retained Child; scoped `stop` makes at most one teardown
attempt using that Child. The stop latch precedes observation/signalling and is
not cleared on failure. Drop cleans up only an owned child with no previous stop
attempt; it does not retry an unknown observation/kill/wait. A failed spawn
consumes the reservation and leaves its durable claim intact, without a retry
entry. No PID reopening, process-name kill, Host operation or restart RPC occurs.
Dropping the owner after an unknown outcome releases OS handles, not the durable
claim; it does not establish quiescence or safe claim retirement.

This is an INTERNAL custody primitive, not a production worker execution entry.
The Command is still constructed by trusted in-process code; its executable and
arguments are NOT authenticated by this leaf. The future coordinator must bind
verified pre-spawn executable custody, restricted command/IPC and normal
identity/Policy before invoking it. The image hashes in the retained reservation
do not authenticate an arbitrary Command. No endpoint, capability enablement,
Job escape, Controller restart, workload continuity or health evidence is added.

Two new portable controls run a fixed ignored helper in the SAME test executable
with cleared environment: foreign runtime/operation/workspace/server/digest
refusal before spawn or stop, and retained Child exit-259 handling with repeated
stop refusal. Author macOS focused entry passed both; binary/test strict Clippy,
fmt, docs and diff checks passed. Controller Windows cross-compilation remains
unavailable locally because its C dependencies lack a Windows cross-compiler;
the native entry must establish Windows compilation as well as execution.
The Windows-only third
control consumes actual signed fixture admission/reservation and verifies pins,
state-root denial and durable claim retention while owning the helper and after
its termination. The admitted fixture image bytes remain inert and are never
executed. Sole focused entry:

`cargo test --locked -p treer-agent-server --bin treer-agent-server windows_update_child_ -- --nocapture`

Sole run `40314283329` passed all three (zero failed/ignored, exit zero) on
Windows 2022/Rust 1.90/fixed NTFS image `20261022.344.1`, exact source
`c688223b9b9e0672c7ce7f45980dea284313465c`, wrapper
`bcadca01c1ef19fb5b907aadebfdc1f4c59210b2`, after the single independent
review #111:9513/9516 and author wording correction. Core independently checked
the three names, summary, source, platform and empty TEMP/cleanup evidence.
RESULT SHA256:
`04cfd2f9a99fa073bc252119ea529217ccc723d13d4e114f4dffe37cd6dc8abd`.
Receipts: `.treer/reviews/core111-windows-child-81277da4/artifact/`
(`RESULT.json`, `platform.json`, `tests.log`). Earlier source81277/88e2 inputs
were never dispatched. No previous acceptance selector is repeated. This accepts
Child custody only, not executable authentication, Job escape or restart health.

### Accepted leaf: pinned signed-target launch and fixed worker command

The non-test `ReservedInputs` creator now accepts only the selected runtime;
the caller no longer supplies a mutable Command. It revalidates the retained
runtime scope, rewinds and hashes the SAME held target Controller file against
the digest established by signed admission, then uses the pinned target pair's
fixed `treer-agent-server.exe` path. The platform reopens that name read-only and
checks physical file identity against the retained pin while all physical
ancestor/file pins remain held through creation. It does not infer the image
from a child's reported pathname. The low-level platform spawn is mechanics,
not signature authority; authentication and argument selection remain in Core.

Core supplies exactly `--controller-update-worker OPERATION_UUID`, with cleared
environment, pinned target directory as cwd, null standard streams and no shell
or PATH lookup. No caller argv, executable override or inherited workload
credentials are accepted by this internal creator. A worker-attempt latch is
set before image checks/creation and never reset, even on failure. The parent
file-publication method refuses a reservation after a worker attempt rather than
letting an uncertain child race a second parent transaction. The previous
arbitrary-Command custody fixture constructor is now test-only. Normal pins,
lease, durable claim, selected-runtime checks and unknown-outcome limits remain.

This still does NOT supply a production Windows worker entry, inherited/peer-
authenticated IPC, Job escape, worker ownership transfer, Controller restart or
health. Windows currently rejects the private worker switch instead of falling
through to ordinary Controller startup. Production capability remains OFF.
The launch target is the signed staged Controller, not an attested running
old-image worker. Readiness, child-side operation/runtime/Host-epoch proof and
survival across the Controller-only handoff are subsequent requirements.

New focused entry:
`cargo test --locked -p treer-agent-server --bin treer-agent-server windows_update_launch_ -- --nocapture`

The two portable controls check whole-image hashing despite an end-of-file
cursor, modified/empty bytes and failed seek/read refusal. The two Windows
controls use a closed signing fixture: a copy of the SAME Rust test executable
as real PE bytes reaches owned Child creation and rejects the unsupported private
worker switch (deliberately NOT ready), while signed inert bytes fail creation,
consume the attempt, refuse parent file publication and preserve the installation
claim without a second spawn or changing installed bytes.
No production worker or installed Controller is launched. Sole run
`40316916344` passed all four, zero failed/ignored and exit zero on Windows
2022/Rust 1.90/fixed NTFS image `20261022.344.1`, exact source
`65bcd5d1f1dcf373749f9a6615d2822b110b0f60`, wrapper
`4b42f0d227dd9c45749f01f4423ec2b9d1d711e4`, following the single independent
source review #111:9524. Core independently checked the four names, summary,
source, platform and empty TEMP/cleanup evidence. RESULT SHA256:
`ddfcb423c22dea2564ad00a063c87b6be7284a8fffc866ad6a16eb4d61eaa877`.
Receipts: `.treer/reviews/core111-windows-launch-65bcd5d1/artifact/`
(`RESULT.json`, `platform.json`, `tests.log`). This accepts the restricted launch
boundary, not worker readiness, Job escape or restart health.
Author macOS new portable two-test entry,
Controller binary/test strict Clippy, Windows GNU platform all-target strict
Clippy, fmt/docs/diff checks passed. The local Windows Controller C toolchain
limitation remains; the sole native run established compilation and execution
for this leaf. No earlier
selector/matrix is repeated.

### Accepted leaf: original-Child Job refusal boundary

The internal custody owner can make one scoped Job check on the ORIGINAL
`Child` handle returned by creation. The platform uses zero-time process waits
before and after `IsProcessInJob(handle, NULL, ...)`; any Job membership,
exited process (including application exit code 259), or unconfirmed query
refuses. It does not reopen a PID or convert `ProcessObservation` into authority.
Core validates the selected runtime instance, Host epoch and full operation
binding before the query, then consumes the query attempt before calling the
OS. Success and failure both forbid repeating this check on that owner. A
foreign scope does not consume its check. Pins/reservation remain held and
the existing one-attempt original-Child teardown remains available.

This is an instantaneous negative safety prerequisite, not a durable outside-Job
proof, readiness or permission to restart. Another same-account process could
assign a Job after a successful check; a future handoff must address that race
and continued ownership. This leaf neither sets breakaway flags nor alters the
Host's kill-on-close Job. It does not implement authenticated worker IPC or
call the check from a production coordinator. Ordinary capability remains OFF.

New focused entry:
`cargo test --locked -p treer-agent-server --bin treer-agent-server windows_update_job_boundary_ -- --nocapture`

Two portable controls cover foreign scope before any query, single use after
success/error and preservation of normal teardown. Two Windows controls query
an actually created, task-owned helper assigned to a test Job, and a naturally
exited original Child with exit code 259. They require refusal without escape
or a second query. No admitted image, installed Controller or Host is run.
Sole run `40318713225` passed all four, zero failed/ignored and exit zero on
Windows 2022/Rust 1.90/fixed NTFS image `20261022.344.1`, exact source
`c38deb6f4e48aed6fef307136dc99a74d273df73`, wrapper
`b81cb2c581f45249a82d5bafcd4814184fff1dbb`, following the single independent
source review #111:9533. Core independently checked the four names, summary,
source, platform and empty TEMP/cleanup evidence. RESULT SHA256:
`43b0dcf322555c609d8902e78ab7da45c0089415421f6641032b564efc74bfb5`.
Receipts: `.treer/reviews/core111-windows-job-boundary-c38deb6f/artifact/`
(`RESULT.json`, `platform.json`, `tests.log`). No previous acceptance selector
is repeated. This does not establish a successful outside-Job handoff or complete
Windows updater acceptance.
Author macOS new portable two-test entry, Controller binary/test strict Clippy,
Windows GNU platform all-target strict Clippy, fmt/docs/diff checks passed.
The local Windows Controller C toolchain limitation remains; the sole native
run established compilation and execution for these integration controls.

API contracts: [IsProcessInJob](https://learn.microsoft.com/en-us/windows/win32/api/jobapi/nf-jobapi-isprocessinjob),
[WaitForSingleObject](https://learn.microsoft.com/en-us/windows/win32/api/synchapi/nf-synchapi-waitforsingleobject).

## Remaining ordered leaves and waiting boundaries

1. Native NTFS store, single-use intent, signed pinned-input admission, lease,
   install-wide claim and verified-image backup acceptance are complete within
   their stated scopes. Private startup snapshot/backup/evidence acceptance is
   also complete within the limits above. Runtime-owned state-root admission is
   implemented with the four focused native-entry controls accepted above;
   publication evidence must follow real effects, never a generic setter.
2. The Windows Controller/CLI file-pair publication and restoration leaf above
   has passed the four focused native controls above. Never treat restoring
   files alone as successful runtime rollback.
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
