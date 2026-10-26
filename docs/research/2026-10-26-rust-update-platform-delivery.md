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

### Trusted state-root admission still required

Current `main.rs` supplies `ControllerConfig.startup_store_path` from
`service::startup_state_path(server_id)`; `controller.rs` loads exactly that
path into its live StartupStore. On Windows the source resolver uses
`TREER_STATE_DIR` when configured, otherwise `LOCALAPPDATA/Treer/state`, followed
by the normal server filename. Those are resolver rules, not observations of a
deployed Controller's environment. The isolated Windows backup API instead takes
an internal caller-supplied directory. It verifies its physical/private/NTFS
properties and the normal server filename, not that live configuration fact.
No production updater entry currently connects those two contracts.

Before production admission, the coordinator must obtain the selected running
Controller's authoritative resolved startup path/configuration and Server/workspace
identity, bind that observation to this operation and pin its physical parent.
It must use that configuration rather than recomputing a default from the
updater's environment or accepting an arbitrary empty private directory. Older
deployed status/ServerInfo responses do not attest the runtime config binding;
the old Mariana inventory gap remains. This source integration is independently
actionable when the production coordinator is wired; it is not a publisher,
missing-package or live-activation prerequisite for the isolated file leaf.
Named live scope, shared-installation impact and recovery custody remain separate
requirements. Ordinary capability stays OFF and Host is untouched.

## Remaining ordered leaves and waiting boundaries

1. Native NTFS store, single-use intent, signed pinned-input admission, lease,
   install-wide claim and verified-image backup acceptance are complete within
   their stated scopes. Private startup snapshot/backup/evidence acceptance is
   also complete within the limits above. Future production admission must bind
   the state root to trusted installation/runtime configuration; publication
   evidence must follow real effects, never a generic setter.
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
