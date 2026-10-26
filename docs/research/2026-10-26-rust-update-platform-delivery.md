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

The binding's digests are claims until a future admission layer independently
verifies signatures and opens/pins the physical installation. This leaf does
not provide that admission, image replacement or a CLI/capability entry. There
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
cleanup. That native execution is pending; no full matrix or installed updater
is required.

## Remaining ordered leaves and waiting boundaries

1. Native NTFS store acceptance is complete. The single-use intent schema above
   is the next leaf; its native reservation check is pending. Following that,
   connect verified admission and add phase-specific evidence for actual image
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
