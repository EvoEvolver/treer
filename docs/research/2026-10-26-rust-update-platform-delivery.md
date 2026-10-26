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

## Remaining ordered leaves and waiting boundaries

1. Finish native NTFS acceptance of this store (one source review, one focused
   Windows job), then bind versioned receipt/marker semantics and durable intent.
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
