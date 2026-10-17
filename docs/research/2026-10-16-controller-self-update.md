# Controller self-update transaction

Design and P1 implementation notes for Issues #111, 2026-10-16. The Unix engine
remains internal WIP, without a public self-update CLI or full activation gate.
Baseline: `2bb65dbe323457e8be6779c36d7469885e2433d5`. This is not a claim that
Windows self-update already works.

## Scope and reuse

One Rust transaction engine in `treer-agent-server self-update` replaces the
Python/PowerShell machine updater. The coordinator submits a job and polls its
receipt; launcher installation continues to use the existing embedded launcher
installer, not a second implementation. Never replace or restart the Host;
record its PID, creation identity and inventory before/after activation. Running
Agents retain process identity, generation, output and launcher assets.

Existing code to extract and reuse:

- `crates/treer-agent-server/src/release_update.rs`: locally provisioned SPKI
  Ed25519 key, strict detached signatures, channel/manifest/artifact bindings,
  duplicate rejection, bounded downloads and binary digest verification.
- `service.rs::update`, `stage_executable`, `activate_controllers` and
  `restart_controller_at`: verify both binaries before executing either, close
  write handles before validation, Unix replacement, epoch-based hot activation
  and rollback. Current update explicitly rejects Windows and has no durable
  transaction journal or anti-rollback state; these are new work.
- `treer-platform`: private files/directories, atomic `replace_file`, process
  identity and locks; extend platform mechanics here. Current helpers alone do
  not prove a whole path is free of symlinks/reparse points: validate ancestors
  and use checked handles for mutation.
- `launcher.rs`: verified embedded payload, version/capability contract, trusted
  absolute Node 22+ selection, profile preservation and rollback.

CLI proposal: `self-update --workspace W --sha FULL_SHA --sums-sha256 DIGEST
--artifact-base HTTPS_URL --node ABS_NODE [--accept-shared-binaries]`, or mutually
exclusive signed `--channel stable --release-base-url HTTPS_URL --public-key KEY`.
`--status JOB_ID` reads a local receipt; there is no arbitrary receipt-file or
internal worker executable override. Require exact enrollment selection when
ambiguous. Derive destinations from the selected installed configuration and
validate them; do not select binaries from ambient PATH or workload `TREER_BIN`.
No wire change is required for initial local CLI execution/status polling.

Pinned mode verifies exact SHA256SUMS bytes against the supplied digest, then
strict expected filenames, size bounds and every installed binary digest.
Distinguish release SHA from artifact source SHA when releases reuse native
artifacts: compare binary identity with the authenticated artifact identity,
not blindly with the release wrapper SHA. A bare SHA alone is not provenance;
the caller supplies a trusted checksum pin/receipt. Signed mode keeps its local
key trust root. Neither mode fetches a trust key from the distributor. Both use
no ambient proxy, no redirects, no URL credentials/query secrets, bounded
timeouts/body sizes, and production HTTPS. Loopback HTTP is fixture-only.

## Transaction and Windows handoff

The activator image preparation primitive now copies the selected installed
Controller under the selection locks into an exclusive job-private `activator`
file. It hashes the bytes actually copied against the journal's original
Controller digest, syncs them before setting mode 0500, closes the writable
handle, and retains the reopened inode for later process attestation. It never
uses PATH, `current_exe`, or downloaded replacement bytes for this old-version
worker. Existing or failed partial copies are retained, not adopted or replaced;
bad content never receives executable permissions. Before use, the retained
image and its directory entry must still match in inode, digest and exact mode.
The full job receipt must match, allowing only its single ordinary transition
from Preparing to Prepared. A changed job/image or later phase is refused.
The isolated ownership fixture launches this prepared native test image in its
normal case, attests its running inode/digest through the connected peer, and
checks exclusion survives release of the parent's original lock handles.
The platform session primitive calls `setsid` after exec without a shell or
pre-exec callback and refuses an existing process-group/session leader rather
than falling back. The parent can verify the owned process's session ID and
process-group ID both equal its PID, rechecking its OS creation/image identity
around those queries. The isolated ownership child now enters that session;
the parent checks it before lock transfer. Separate owned-process tests reject
a process group without a new session, repeated detach, and an exited identity.
The production endpoint now also enforces the session check after accepting a
connection and before authenticating it into a transfer-capable peer; merely
polling before the child connects does not abort startup. An attached owned
child is rejected with a fixed error and permanently aborts that endpoint,
leaving the journal, ownership record and the parent's locks unchanged. The
ownership fixture exercises this rejection without sending any lock handles.
The child-side `RunningActivator` primitive now checks the exact Prepared
receipt and selected enrollment, the OS-reported running executable's inode
against the job-private 0500 image, and both file digests against the original
Controller pin before calling `setsid`. It then rechecks the receipt, image and
private session before returning a proof. The normal isolated ownership child
uses this sequence before connecting; an identical executable at another inode
is rejected before changing session or connecting, leaving the parent's locks
and journal untouched. A different enrollment is rejected as well. This is an
internal bootstrap primitive, not yet a production worker dispatch path.
An additional isolated acceptance fixture now executes the private native image
with all three stdio descriptors pointing to `/dev/null`, completes the durable
handoff, and exits the submitting process without running its destructors. After
reaping that parent, the outer observer verifies the worker's creation identity,
running inode and private session, exclusion of both shared-install workspaces,
and a new child-side proof/lock probe. Explicit release then terminates the worker
and makes the locks available; the full Prepared receipt remains unchanged.
This proves survival of an ordinary Unix parent exit in that fixture, not escape
from a cgroup/service manager's termination scope. Wiring the synchronous
production worker entry, stdio and the complete owned-child lifecycle remain
pending, as do activation and rollback acceptance under service managers.

The worker-side private endpoint connector now checks the physical directory
and fixed socket entry before and after a single nonblocking connect. Pending
connects have one three-second poll budget; queue exhaustion and other errors
fail without reconnecting or removing the socket. Successful streams are
blocking, CLOEXEC and have three-second read/write timeouts. The stream still
requires exact parent peer authentication before any lock reception. On Darwin,
where socket flags cannot atomically set CLOEXEC, invoke it in synchronous
startup before any concurrent thread can fork. The parent-exit fixture uses this
connector; platform regressions cover alias rejection, a closed listener, and
an unconsumed full backlog without false connection success or an endless wait.

The private parent bootstrap now records the complete Prepared receipt and the
submitting process's OS creation identity and executable device/inode, only
after proving it is executing the selected installed old Controller under the
selection locks. The bounded, exclusive 0600 record contains no paths or
credentials; an existing or partial record is never adopted or overwritten.
After verifying its own running activator, the worker obtains its direct parent
PID from the OS and checks that record, live creation identity, executable inode
and original Controller hash before connecting. Socket peer authentication is
still mandatory; this file alone grants no ownership. The parent-exit fixture
now executes its submitting parent from the isolated installed Controller and
uses this bootstrap instead of an environment-supplied parent PID. It rejects
changed schema, parent PID, creation, inode, full receipt, unknown fields,
malformed and oversized records before socket traffic. Synchronous production
dispatch and the complete activation/recovery engine remain pending.
The production submitter must itself execute the selected installed old
Controller; invocation from another inode, including a copied maintenance
binary, intentionally fails this proof. A crash after saving the parent record
but before spawning can leave that record without an ownership ACK. Recovery
must treat it as uncertain evidence alongside `ownership.json`, recheck the
recorded process creation identity, and never delete it or retry spawning merely
because ownership is absent. Record presence alone proves neither a live worker
nor permission to replay activation.

`WorkerBootstrap` now combines the pre-runtime worker sequence: revalidate the
running private activator, load and attest the OS parent, connect once to the
fixed endpoint, authenticate that exact peer and recheck the complete Prepared
receipt. Construction sends no bytes. Its consuming ownership method closes the
stream on every error, including uncertain ACKs, and returns the existing sealed
lock-owning activation value only after durable handoff. No parent PID, image or
socket is supplied by the caller. The isolated parent-exit fixture uses this
production bootstrap; it still uses fixture barriers around expensive debug
image hashing, separate from the three-second framed exchange budget. The
production dispatch must exit on bootstrap error and must complete this sequence
before creating runtime threads or child processes; that dispatch is not wired
yet, and no public self-update command is exposed.

The worker resource resolver accepts only a canonical job UUID. It opens the
existing private `self-update/jobs` directory below the local service state
directory, requires a Prepared receipt, and resolves the selected enrollment
from that receipt's workspace and server binding. It does not accept receipt
paths, workspace overrides, executable paths or parent PIDs from argv. Inventory
hostname discovery uses the OS API without launching a PATH helper. Resolution
does not create directories, acquire replacement locks or mutate installed files.
Shared-binary selection here is only resource discovery, not consent: the parent
must have obtained consent during preparation, and execution still requires its
authenticated full-receipt handoff and the same installation lock descriptions.
The consuming synchronous entry verifies the running private image, session,
parent and endpoint peer before taking ownership. Failures return a fixed
bootstrap diagnostic; there is no ordinary Controller fallback or reconnect.
Only the resulting owned capability permits async execution.
Tests cover malformed/missing/unprepared jobs before inventory enumeration,
workspace/server mismatch, aliased directories and a wrong running image without
ownership creation or installed-file changes. This is not yet wired to main or
parent spawning; launcher completion and worker-death recovery remain pending.
Production dispatch acceptance must additionally inject an early return after
successful bootstrap: it must reach the phase-appropriate RollbackFailed or
`update_recovery_required` path with evidence retained, never silently exit after
claiming ownership. This regression remains pending with production dispatch;
the resource resolver does not claim to implement or test that failure path.

The owned worker now offers a one-shot Core coordinator. Synchronous
`begin_core` permanently marks the attempt before constructing the asynchronous
run; progress defaults to the fixed `update_recovery_required` label immediately
after ownership. The run mutably borrows the owner instead of consuming its
locks. Dropping an unpolled run or cancelling a polled run leaves the owner,
locks and recovery-required status intact; it never authorizes another attempt.
The production engine calls the complete Core activation proof, and only an
exact, bound RollingBack restoration result authorizes one rollback coordinator
call. Errors, foreign/stale results and RollbackFailed all require recovery,
without diagnostic text escaping. A proven CoreHealthy result requires launcher
work, not Complete; only independently proven RolledBack reports rollback success.
The coordinator does not invent journal transitions on error. Its private test
engine verifies orchestration, including cancellation after publication, while
the existing activation tests cover the underlying live-proof rules. This is
not a main-entry early-exit or worker-death recovery test: a process exit still
releases OS locks, and production main must explicitly report/handle this status
instead of silently returning. Launcher execution, production dispatch and crash
recovery remain pending. No network failure authorizes restarting the Host.

Launcher report comparison is now a separate bounded metadata parser, with no
process execution or receipt-transition capability. Install and independent
status outputs must identify the same `sha256-` content version and authenticated
native artifact source SHA (not a reused release wrapper SHA). Both compatibility
reports must be compatible, the installed runtime portable, the active entry
integrity valid, and the current version present exactly once. Older retained
incompatible versions may lack provenance without invalidating the current one.
Duplicate known JSON fields, duplicate version IDs/profile names, missing or
wrong fields, empty profiles, more than 32 profiles or 1024 retained versions,
and either output over 4 MiB fail with a fixed diagnostic. Extra path/Agent
metadata is ignored rather than retained or logged. The result retains only
content version, source SHA and profile count; it is explicitly NOT install or
live-health proof. The future executor must supply bounded exit-zero outputs
from the reverified new Controller under original ownership, preserve the
launcher's own profile transaction/readback checks, and independently recheck
Core/Host/workload health before Complete. Neither arbitrary caller JSON nor a
replayed report can authorize a journal transition through this parser. Actual
launcher process lifecycle, failure-to-Partial handling and main dispatch are
still pending; these parser tests do not claim an isolated launcher install.

An independent Unix platform helper now bounds trusted installer stdout and
execution time, with real null stdin/stderr and a dedicated process group.
Normal completion, errors, deadline expiry and future cancellation clean the
group before reaping its leader. `waitid(WNOWAIT)` retains that leader's PID
until group signalling, preventing a reap-then-signal PID/PGID reuse race. Darwin
can return EPERM for a zombie-only group; only an independently exited owned
leader plus a complete libproc inventory containing exactly that leader permits
the singleton exception. Other permission failures remain errors. The small
libproc wrapper uses initialized fixed-capacity storage and rejects truncated
or failed inventories. Isolated subprocess tests cover normal exit with a live
descendant, timeout, cancellation, excessive output, null streams, retention and
reaping of the leader, and the Darwin singleton/multiple-member distinction.

This helper authenticates no executable, argv, environment or receipt. The
authenticated version probe now uses its strict output path, but the self-update
worker and launcher executor are not yet wired to it. The caller must retain the
transaction locks and map uncertain/cancelled execution to recovery or Partial,
never completion. Descendants must cooperate by retaining their inherited group;
this is not a sandbox against a same-account process calling setsid/setpgid.
The supervisor must be the child's exclusive reaper. Killing its group does not
survive the supervisor dying, and synchronous spawn or kernel-uninterruptible
kill/reap cleanup has no hard wall-clock guarantee. Worker-death recovery and
the actual authenticated launcher executor remain pending. No real service or
installed launcher is exercised by these lifecycle fixtures.
Group signal submission plus leader reaping does not prove every descendant has
finished kernel teardown. The caller must retain locks and establish the needed
quiescence/recovery evidence before rollback or release; this helper alone does
not authorize either operation.

The optional `output_quiescent` path now retains the unreaped leader through
group signalling and bounded OS state observation, and only then reaps it and
returns a private-constructor `QuiescentOutput`. A live, stopped or unreadable
member does not count as quiescent; only exited/zombie state does. Darwin uses
a bounded group inventory and fixed-size initialized `proc_bsdinfo` output with
the documented zombie-inclusive argument; changed/disappearing members require
another inventory. Linux bounds `/proc` enumeration and each `stat` read, parses
the PID/group/state without decoding or retaining the opaque process name, and
requires the reserved owned leader in the observed group. Linux procfs enumeration
is not an atomic group snapshot: the observation follows group SIGKILL, relying
on Linux group-signal/fork locking and the trusted cooperating-child contract.
All descendants must remain in their inherited group and remain signalable by
the caller. This is not containment of another same-account actor or an escaping
or credential-changing installer. Permission errors still fail, except for the
existing narrow Darwin exited-singleton exception.

Linux also enumerates every matching process's `/proc/<pid>/task/<tid>/stat`:
a zombie thread-group leader alone is insufficient, because sibling threads may
still run after the main thread exits. Every observed thread must be Z/X with
matching TID and process group; empty, vanished or unavailable task inventory
never grants a witness. A deterministic procfs fixture covers a zombie main
thread with R/D/S/T/I siblings, then Z/X siblings, missing inventory and identity
mismatches. This tests the observation boundary without inducing real stuck
threads or relying on a scheduler race after SIGKILL.

The original execution deadline also covers quiescence; failed, cancelled or
expired observation returns no witness and cannot authorize rollback or lock
release. A slow synchronous inventory cannot return a late successful witness;
kernel-stalled inventory I/O still has no hard wall-clock bound. Zombies cannot
issue further user-mode writes, but this does not prove all kernel teardown or
previous asynchronous I/O has finished. Authenticated command construction,
durable child-start/worker-death recovery, no-global-reaper integration guards,
final live health and journal completion are still separate pending obligations.
The strict lifecycle fixture does not claim a launcher installation or whole P1
completion, and no production self-update dispatch uses it yet.

The version probe retains a checked executable inode as an internal
`VerifiedImage`, alongside its authenticated digest and physical installation
parent. It rechecks the same inode, digest, executable permission and physical
parent immediately before execution and after strict group quiescence. A
same-byte replacement with a different inode fails. This is path-based execution,
not an atomic verify-and-exec primitive or isolation from a malicious same-user
writer. The probe supplies only `--version`, null input/error streams, a cleared
environment with C locale and a checked working directory. Version text is
accepted only after strict completion and image revalidation; OS errors and
child diagnostics are reduced to fixed labels. Timeout and cancellation clean
the owned process group without granting a result. One absolute ten-second
deadline now starts before the first authentication hash and covers all image
revalidation, execution, quiescence and version parsing. Synchronous work checks
the deadline before and after each stage; the child receives that same absolute
deadline, not a fresh budget. Exhaustion refuses spawn or any late result, but
synchronous image hashing and kernel operations still have no hard wall-clock
interruption bound. Stdout stays bounded at 1 KiB; the platform marks excess with
`ErrorKind::FileTooLarge`, mapped to the fixed probe output-limit label without
matching human-readable error text. Isolated fixtures
cover descendant late writes on success/timeout/cancellation and a valid-version
script replacing its own inode. They do not run a real launcher installation,
persist child-start evidence or cover worker death. The future launcher executor
must still bind both installed images to the owned receipt and build its fixed
command/environment under the original locks.

The read-only `launcher_preflight::check` now binds both installed Controller and
CLI images to that same sealed ownership and a CoreHealthy receipt, requiring
Installed/Unchanged publication records. It authenticates both files before
executing either fixed version probe, retains both original inodes across both
probes, then rechecks the images and exact journal revision/content. The probes
borrow these same retained `VerifiedImage` objects directly, deriving their
executable paths from them; there is no independently reopened inner proof.
An isolated regression succeeds once, replaces that executable with identical
bytes on a new inode, and proves the same retained object rejects a second
version execution before its sentinel is written. This is an execution-boundary
fixture, not installer or worker-death acceptance. Matching
version text must agree between binaries and with the receipt's native artifact
source, which may differ from the release wrapper SHA. A single absolute
twenty-second budget covers both probes and synchronous authentication checks;
late synchronous work is rejected rather than forcibly interrupted. Tests use
only isolated scripts and synthetic ownership/health fixtures: wrong CLI digest
prevents either execution; foreign ownership/phase, source/version disagreement,
journal corruption and a CLI replacing the already-probed Controller with
identical bytes all fail without releasing the original locks. Expired budgets
execute neither image. Success returns no reusable execution capability and
does not advance the receipt. No installer is started: authenticated fixed
installer argv/environment, durable child-start/identity and worker-death
reconciliation, spawn/completion revalidation and final live health remain
required before production dispatch or whole-P1 acceptance.

`launcher_invocation::Invocation` now supplies a read-only recipe under borrowed
original ownership and an exact CoreHealthy journal snapshot. Its only operations
are `install` and `status`; program/cwd and explicit CLI path come from the locked
selection, with explicit Node/workspace/Controller URL arguments and server ID.
The URL is the saved numeric Controller listener, not the remote Proxy URL. It
preserves the launcher's exact listener selector (including wildcard spelling),
unlike the health transport's loopback normalization. HOME comes from the
effective OS account database, canonicalized and checked as an owned installation
directory, never the updating Agent's environment. XDG_CONFIG_HOME is recovered
from the selected `<config-home>/treer/agent-servers` path; unrepresentable layouts
fail instead of falling back to another configuration. Node resolution reuses
the launcher's trusted fixed OS locations without PATH fallback.

The future child must clear its environment before applying the recipe's nine
allowlisted entries: HOME, XDG_CONFIG_HOME, fixed system PATH, C LANG/LC_ALL and
the selected workspace, Controller URL, server ID and job-private TMPDIR. No ambient credentials,
workload identity, loader/Node hooks, proxy settings or state-directory override
are copied. This intentionally uses the OS account's default launcher state
location; it does not inherit an Agent's private state override. A recipe binding
check rejects changed receipt content/revision or temporary-directory identity.
Public recipe preparation requires a borrowed `LauncherTemporary`, retaining
the original owner, locks and directory descriptor. It neither authenticates image
bytes nor starts any process, changes a receipt or grants execution authority.
Installer-time enforcement, durable child-start/identity and recovery, actual
env-clear/spawn/completion enforcement and final live health remain executor
work; no real installation or whole-P1 acceptance is claimed here.

`launcher_temporary::LauncherTemporary` now creates exclusive 0700 storage using
the original retained job-directory handle. Before creating anything it requires
the job locator's UUID filename and physical directory identity to match that
handle. The fixed `launcher-tmp` child and parent are synced; existing entries
are never adopted, chmodded or removed, and errors/Drop preserve evidence for
explicit recovery. There is no shared `/tmp` fallback. Revalidation reopens both
physical locators without following symlinks and checks their retained inodes,
owner and private permissions. Recipe binding requires the same selection,
owner and journal instances and exact receipt snapshot; TMPDIR receives only
the revalidated path. Isolated synthetic fixtures cover existing files,
directories and symlinks, wrong/aliased job locators, directory replacement,
permission and receipt changes, and copied-journal rejection. No installer runs.

Executor review requirements (security2527) still include keeping this storage
and its identity through child quiescence/recovery. Actual spawn wiring, durable
child-start recording, recovery after worker death and cleanup only after proven
quiescence remain pending. The current CoreHealthy snapshot binding must be
integrated with validated executor phase transitions; it does not prove a child
started or permit installation. Retain Node's checked file descriptor and
physical identity (device/inode), accepting only root/current-account ownership
and no group/other write permissions on the file and trusted ancestor chain.
Recheck identity, permissions and the selected path before spawn and after
strict completion; a same-bytes inode replacement must fail. This OS dependency
does not acquire a release-artifact digest pin merely by residing at a fixed
path. Preserve the separate Controller/CLI receipt-digest checks.

The Unix platform `retained_executable::RetainedExecutable` now provides the
underlying OS-dependency identity mechanism, not executor wiring. Its public
entry opens a physical absolute path from `/` using handle-relative no-follow,
close-on-exec traversal, checking every directory plus the regular executable
file for root/current-account ownership and no group/other write permissions.
Privileged file mode bits and non-executable files are rejected. It retains all
directory/file descriptors; fresh traversal must match every device/inode as
well as recheck current permissions/types. This detects a replaced ancestor
even when a hard link preserves the executable inode. It never canonicalizes a
new target during revalidation, changes permissions or executes a process.
No content digest is checked: in-place writes by a trusted owner are outside
this identity-only guarantee. Source-only/package/network and real Node22
acceptance, installer spawn/completion wiring and durable child lifecycle remain
pending, so this helper does not authorize installation.

Public `Invocation::prepare` now retains this Node proof immediately after
fixed-location resolution, builds `--node` from its exact physical path and owns
the proof for the recipe lifetime. Binding verifies that argv names that same
path; every subsequent `verify_binding` also rechecks the retained file and
ancestor identities and current permissions. It never resolves or adopts a
replacement inode during revalidation. Isolated invocation regressions use
inert files in exclusive directories within the private Cargo target, allowing
the production walker to start at `/` without changing system installations.
They cover identical-byte replacement, repeated refusal without adoption,
wrong-path proof binding, symlink/missing/non-regular paths and permissions
changed between proof creation and binding. No fixture interpreter is executed.
The existing trusted-location resolver canonicalizes package-manager symlinks
before retention. A regression uses that same resolver with an isolated symlink,
then retargets the link: the existing recipe still names and verifies the
original physical target, while replacing that target's inode is refused.
No test changes `/opt/homebrew`, `/usr/local` or `/usr`.
`Invocation::check_node_version` now probes this same retained interpreter with
only fixed `--version`, a cleared environment with C LANG/LC_ALL, and the
physical interpreter parent as cwd. It borrows the recipe and its original
owner/locks throughout the call, checks the full binding directly before spawn
and after strict process-group quiescence, and never opens a replacement proof.
One absolute ten-second deadline covers checks, execution, quiescence and
parsing; synchronous checks account for elapsed time on both sides. Stdout is
bounded to 1024 bytes and never exposed in errors. Only a bounded stable numeric
`vMAJOR.MINOR.PATCH` with major >=22 is accepted; extra text, prereleases,
control characters and older versions fail. Probe timeout/output-limit/failure
and invalid version use fixed diagnostics, distinct from retained-node change.
Tests execute only isolated shell stand-ins: fixed argv/environment, journal
preservation, wrong version, nonzero exit, excessive output, timeout and a
replacement of the retained executable before successful output are covered.
These are version-probe tests, not a real Node or installer acceptance test.
The call does not advance a receipt or return reusable installation authority.
Version output is the executable's own claim: this is a compatibility check,
not authentication of interpreter contents or release provenance. Neither
receipts nor diagnostics may label the interpreter authenticated based on this
probe. Retained uid/mode/inode checks remain distinct from a digest/signature.
The no-file-write version probe deliberately receives no TMPDIR; its isolated
environment test asserts that absence. Actual installer execution must instead
receive the revalidated job-private TMPDIR. Executor acceptance must assert
these two environments separately; installer-side enforcement/tests are pending.

The future executor must still enforce Node and Controller/CLI checks directly
at installer spawn and after proven completion, integrate expected phase
transitions and record durable child-start identity/recovery before installation.
The standalone version probe does not supply that lifecycle authority, production
dispatch or a worker-death recovery path; the installer must not start yet.

`Invocation::record_start_intent` now provides the pre-dispatch durable marker
only. A fully bound recipe (original owner/locks, exact CoreHealthy receipt,
retained Node and job-private TMPDIR) writes one fixed install/status marker
exclusively through the original journal directory handle. The private schema
contains only schema version, the full original receipt, typed operation and
job/operation-derived ID; no argv, paths, environment, output, PID or claim that
a child started is stored. The bounded 64KiB record is 0600, file-synced then
directory-synced, reopened without following links, compared to the original
file inode and strictly decoded/compared to the expected record, with recipe
binding checks around readback. No journal phase/revision is advanced.

Any existing marker, including an empty/corrupt record, directory or symlink,
or an uncertain persistence/readback result yields `update_recovery_required`;
the marker is never replaced, adopted or removed on error/Drop. A binding error
before creation leaves the directory untouched. Isolated regressions verify
both operation records, exact receipt/ID, private mode and bounded/no-path
contents, repeated refusal with evidence unchanged, all conflicting entry kinds,
and incomplete/changed-receipt refusal before writing. This is not child-start
identity, a completion record or permission to spawn, and readback does not
authorize replay. Actual pre-spawn image authentication, expected phase
transitions, durable attested child identity and worker-death/accepted-before-ACK
reconciliation still must be wired before any installer dispatch. An intent
without completion remains ambiguous, never proof that no child ran.

Fresh intent evidence now returns `RecordedIntent`, with private fields and no
Clone, deserialization, file accessor or public constructor. It borrows the
original invocation (and thereby its locked owner), retaining the newly created
file handle and exact synced/read-back bytes. `verify` rechecks the recipe/job,
private named entry, original dev/inode and byte equality; `sha256` first performs
that same check and exposes only the exact-byte digest for future registration.
It cannot be reconstructed by opening an old marker, and dropping it never removes
recovery evidence. Tests reject byte-identical inode replacement, same-inode
JSON whitespace changes, symlink/missing entries, changed journal revision,
replaced Node and moved temporary directory. These are verification snapshots,
not atomic protection from a trusted same-account writer, supervisor registration,
single-gate ownership or installer execution authority; those remain unwired.

`RecordedIntent::register_supervisor` now consumes that fresh intent and writes
one exclusive install/status supervisor record through the retained job directory.
It binds schema/job/operation/attempt, exact intent SHA256 and dev/inode, plus
OS-captured PID/creation identity, private SID/PGID, image dev/inode and SHA256.
It revalidates the intent and private process session immediately before writing
and after file/directory sync, then checks bounded exact-byte readback and the
registration inode. Existing entries, nil attempts and non-private sessions fail
closed without replacement. `RegisteredSupervisor` retains the intent, process
image and record FD; verification and binding access recheck those records and
live image/session. Drop preserves evidence. Isolated Rust subprocess tests cover
valid registration, replacement/mutation, changed intent, child exit, existing
entries and refusal to replace a registration even with a fresh intent/attempt.
This is durable identity metadata, not attestation of owned-child provenance or
an authenticated connection: the caller must supply the coordinator's owned
child image. Registration alone authorizes no permit/noexec/ACK or installer
execution. Its private in-memory gate is described below; the real supervisor
entry, authenticated lifecycle connection and recovery/death matrix remain
future work. Sync-failure/crash-point
injection for registration is also still required. Creation identity includes
Linux kernel boot_id plus process start ticks, or macOS kern.boottime plus the
libproc process start seconds/microseconds. Missing/invalid OS boot data fails
closed. A regression forges only the retained boot component while preserving
the real PID/start fields/image and verifies rejection. This is not cryptographic
boot attestation; an equal OS-reported boot identifier is trusted within the
existing local trust model, and clock changes may conservatively invalidate it.

Fresh registration now owns one private `Arbitration`, constructed with a
non-Clone `RegistrationGate` capability minted only inside the exclusive
registration leaf. The ordinary `Arbitration::new(Binding)` constructor is
test-only. No token, mutable gate, reset or gate-extraction API is returned;
copying public Binding metadata cannot create another production gate. Existing
registration files still prevent minting another gate after drop/restart, even
with a fresh intent/attempt. A source tripwire checks the token's definition,
impl and sole literal remain in the private leaf and that it has no child
modules, and that the only `Arbitration::registered` call remains in that leaf;
this is not a Rust parser or a compile-fail test. Keep that sole caller on the
lifecycle wiring review checklist.

Loss of the in-memory gate while durable registration remains is always
registered-needs-quiescence / recovery_required. Recovery must never recreate
a gate from that file or resume accepting permits. A fixture drops the entire
RegisteredSupervisor, retains its durable evidence, and verifies that both the
original intent and a deliberately substituted fresh intent cannot construct
another accept-capable value (even with the original attempt ID). This models
loss of memory authority; it is not a real process-restart or death-matrix test.

`RegisteredSupervisor` reads an independently authenticated owner frame through
its own gate and revalidates the durable supervisor registration again before
acceptance. The permit owner and registered supervisor have separate identities;
requiring equal PIDs would reject the intended protocol. Tests authenticate the
test-parent owner while registering a different owned child's private session:
valid binding reaches Accepted once, repeat reads cannot reset it, registration
mutation after a successful read forces Uncertain, and restoring exact bytes
cannot reopen that gate. The wrapper performs full image hashes at read and
acceptance boundaries as well as metadata access; it is not for polling.
This closes metadata-only gate construction for current production callers,
but still supplies no real supervisor entry, owned-parent provenance proof,
sealed cancellation/writer-closure evidence, durable ACK/decision or executor.
An in-memory Accepted value alone must never dispatch the installer. Those
lifecycle and crash/recovery acceptance gates remain outstanding.

Lifecycle wiring must enforce this hard ordering: durable intent, spawn with
birth-lock inheritance, capture ProcessImage from the owned Child, complete
exclusive registration sync/readback, then permit authenticated connection
reading or permit issuance. Any failed/interrupted registration after spawn is
registered-needs-quiescence/recovery_required, never evidence of not-started;
partial files must not be removed to retry. RegisteredSupervisor::verify and
binding currently perform a full image hash on every call as the safe default.
Use these at bounded lifecycle transitions, not a polling hot path; frequency
and cost must be assessed during wiring. No inode/mtime/size-only hash cache is
implemented or sufficient for this check.

Status intent policy (security2596): this is the updater's one-shot post-install
status verification, not an intent requirement for ordinary `treer launcher
status`. Its conservative marker is intentional: a supposedly read-only command
still starts an interpreter and may leave descendants. Until a separate durable
completion/quiescence record exists, a second updater status probe for the same
job is refused. Explicit recovery may archive its validated intent and related
storage only after proving either that the closed execution gate never opened
and no unregistered runner remains, or that the registered child/group is
quiescent. Preserve the outcome and archive disposition durably; a subsequent
explicit attempt must use a new attempt identity, never replay the old operation
ID. Normal successful completion should eventually record this disposition
without requiring manual recovery. None of that completion/archive lifecycle is
implemented by the current exclusive intent writer.

Proposed birth-to-execution protocol (security2596; NOT implemented or accepted
as recoverable startup yet): use a trusted Rust supervisor with a closed,
one-shot execution gate. A discoverable environment string alone is not an
ownership or quiescence proof. The implementation must establish this order:

1. Under the original job/installation/enrollment ownership, create an exclusive
   attempt and acquire its dedicated OS birth lock. Persist the intent first.
   Use a process-shared lock whose same open-file description is inherited by
   the runner atomically at fork, before it can run user code. Merely locking
   after exec or sending the lock later over IPC leaves the original gap open.
2. The trusted runner starts with the gate closed and the inherited birth-lock
   descriptor retained, using a clean environment and fixed private bootstrap.
   It authenticates the job/intent and its parent through OS evidence and the
   private one-shot channel. It must not launch Node, execute installer code,
   release the birth lock or create unregistered descendants at this stage.
   Exec/bootstrap failure closes the inherited descriptor; EOF/timeout or an
   invalid permit makes the runner exit without executing the installer.
3. Before any permit, persist a strict child identity record bound to the exact
   intent/attempt and OS boot, PID, creation identity, session/group and verified
   supervisor image. Establish the intended group/session first, then validate
   those facts against the OS. Sync the identity file and parent directory and
   read it back under retained handles; an acknowledgement alone is insufficient.
   Record format, authentication and integration of descriptor inheritance still
   need implementation and Mac/Linux evidence, not a child-self-reported PID.
4. Only after that durable identity and current image/receipt checks may the
   parent send the one-shot execution permit. The runner verifies the binding
   again before opening the gate. Its identity is already durable if the parent
   dies after permit delivery but before observing an ACK. Neither side retries
   a permit or installer launch after an ambiguous result. Retaining the birth
   lock until this boundary prevents an unregistered runner escaping recovery.
5. Recovery first proves the original owner cannot issue another permit. For an
   unregistered attempt it must acquire the same birth lock exclusively; failure
   means a possibly live owner/runner, so keep `update_recovery_required`. A lock
   acquired by the parent and inherited at fork covers the crash window even
   before the child executes its first instruction. Successful acquisition plus
   validated closed-gate records can prove no unregistered runner can execute;
   lock availability or absence of a PID/record alone cannot. Corrupt, partial,
   contradictory or unavailable gate/identity evidence remains unresolved.
6. With a durable identity, including after a lost ACK, reconcile actual OS
   identity and the complete group without replaying startup or capturing a new
   baseline. Require a quiescence witness before completion, archive, cleanup or
   rollback. If the supervisor dies while descendants may remain, retain the
   durable identity/evidence and fail closed until the platform can prove the
   group safe; disappearance of the leader is insufficient.

Required isolated fault tests must kill the worker/runner at each boundary:
before fork, before child registration, during partial identity write/sync,
after durable identity before permit, after permit before ACK, and while a
descendant survives its leader. Assert that no installer sentinel is written
before the durable gate, missing identity cannot authorize replay/cleanup,
an inherited birth lock blocks unregistered-runner recovery, and lost-ACK
reconciliation never starts a second child. Include status completion/archive
and repeat-attempt disposition. These tests and the protocol are outstanding;
`record_start_intent` alone is not a recoverable launch prerequisite or permission
to start the installer. The existing generic bounded subprocess helper does not
implement this startup handshake or atomically inherited birth lock.

The separate platform `birth_lock::BirthLockInheritance` primitive now duplicates
an already-held lock with atomic CLOEXEC, retaining the same open-file
description. Only the intended child's post-fork `pre_exec` callback clears
CLOEXEC using `fcntl`; the parent's descriptor remains CLOEXEC. The configured
inheritance value owns its duplicate until dropped or consumed by `spawn`, which
also consumes the `Command`; neither can be reused for a second launch. The
parent's duplicate closes on return, including spawn failure. Callers must prove
the original lock is held, authenticate the supervisor and restore CLOEXEC before any installer
exec. The returned descriptor number is only a bootstrap locator, never authority.
Isolated Mac/Linux tests check inheritance across exec, lock exclusion after all
parent copies drop, release after normal child exit or exact owned-child SIGKILL,
and release when an unspawned inheritance drops or spawn fails. A compile-fail
test checks that the inheritance value cannot be reused after spawn. Concurrent
unrelated forks can briefly inherit CLOEXEC descriptors until exec; release
assertions allow that bounded window, never treating availability as authority.

State and job directories must reside on a local filesystem with native `flock`
semantics. Network filesystems (including NFS flock emulation) are unsupported;
their exclusion/inheritance behavior is not liveness evidence for this protocol.
The platform `local_filesystem::verify` now probes the exact retained file or
directory using `fstatfs`; `PrivateDirectory::verify_local_filesystem` exposes
that check without reopening the path. Its conservative allowlist is APFS/HFS
with Darwin `MNT_LOCAL`, or Linux ext2/3/4 (shared magic), XFS and Btrfs. Network,
FUSE, overlay, memory, device and unknown filesystem types fail closed with a
fixed diagnostic, as do probe errors; no mount/path details are disclosed.
This type check is only a prerequisite: it does not attest backing-device
durability, mount options, flush behavior or lock ownership. Synthetic tests
cover both platform allowlists and deny network/unknown/nonlocal cases; real
Mac handles check local temp storage and reject devfs, and Linux rejects procfs.
No network mounts are created. The lifecycle must still invoke this preflight
for every retained state/job/lock storage handle before relying on its evidence;
that production integration remains unfinished.

Storage compatibility for the replacement updater (not enabled in production):

| Filesystem | Storage preflight |
| --- | --- |
| macOS APFS/HFS with `MNT_LOCAL` | Accepted prerequisite |
| Linux ext2/3/4, XFS, Btrfs | Accepted prerequisite |
| NFS/SMB, virtiofs/9p/FUSE | Rejected |
| overlayfs/container writable root, tmpfs, device/proc filesystems | Rejected |
| F2FS, ZFS, bcachefs, unknown types | Rejected pending separate flock/fsync review |

Failures use fixed code `update_storage_unsupported_filesystem` with a fixed
instruction to place Treer state on a supported local volume. Prefer a local
ext4/XFS/Btrfs/APFS volume; for Linux containers a dedicated named volume backed
by an accepted filesystem can avoid overlayfs. Volume names do not attest the
driver or underlying filesystem, so every retained handle must still be checked.
Do not widen the allowlist without reviewing each filesystem's flock/fsync
semantics and adding evidence.

Static container-default audit: `service::state_dir` selects `TREER_STATE_DIR`,
then `$XDG_STATE_HOME/treer`, otherwise `$HOME/.local/state/treer`; job storage is
its `self-update/jobs` child. That path policy does **not** ensure an accepted
filesystem in arbitrary Linux containers: without a dedicated mount it can be
on overlayfs. The Apple container setup skill uses guest `HOME=/home/<user>`
and keeps Treer state out of the Mac's `/Users/<user>` virtiofs share; this avoids
that specific shared-directory default, but the image and skill do not attest
the guest disk's filesystem type. Thus supported default storage is **not
confirmed** for every container deployment. Production preflight must check it;
no running guest, real enrollment or mount was inspected or changed for this
audit. Operators must arrange accepted local storage before an authorized setup
or maintenance, rather than bypassing a rejection or relocating live state.

The single-use adapters consume either a standard `Command` or Tokio's
`Command`; the latter preserves the callback through `as_std_mut`. Isolated
tests cover both paths through the same Rust test supervisor, which restores
CLOEXEC before starting a Rust descendant probe. That probe checks the descriptor
is absent or refers to a different device/inode after exec (allowing descriptor
number reuse). A positive control first confirms the probe detects the same lock
before CLOEXEC is restored. Parent readiness is withheld until both bounded
probes succeed.
This tests the adapter and substitute-supervisor mechanics, not the unfinished
production supervisor or an actual installer. Production lifecycle integration
must still enforce that ordering and preflight the local filesystem restriction.
This descriptor-only primitive has no production caller and does not implement registration, permit arbitration,
recovery or the required worker-crash fault matrix above.

Lifecycle acceptance invariants (security2598; required before implementation
can be accepted):

- **Negative evidence:** a held birth lock is positive evidence of possible
  liveness; obtaining it is never proof of safety. A `never-started` decision
  requires both proof that the original owner can no longer issue permits
  (attested ownership takeover or proved death) and a validated durable
  `exited-without-exec` record from the authenticated supervisor. That record
  must bind the exact intent digest, unique attempt, registered supervisor
  boot/creation/image identity and terminal reason (for example owner-channel
  EOF). The trusted supervisor must irreversibly close its execution gate and
  permit receiver BEFORE writing/syncing/readback of this record, then exit
  without executing installer code. A permit once accepted precludes this
  record. Its only writer is the authenticated supervisor through retained
  private handles, never the installer or an unauthenticated client. Confirm
  quiescence as well before archive/cleanup. If the supervisor dies before
  this record becomes durable, do not infer `never-started`: valid registered
  identity requires group reconciliation; missing/partial/unverifiable identity
  or terminal evidence means `update_recovery_required`, even when a test knows
  no installer actually ran. Lock availability cannot repair missing evidence.
- **Channel and descriptor ownership:** only the original owner and trusted
  supervisor may possess the private one-shot permit channel; no inherited
  duplicate in Node, installer, unrelated child or reconnecting client. Bind
  it to the exact intent/attempt and authenticate actual peer process identity
  using Linux SO_PEERCRED or macOS LOCAL_PEERPID plus the existing OS
  creation/image proof. Do not treat credentials cached on a pre-fork socketpair
  as proof of the newly forked child: use an authenticated post-fork private
  connection or separately attest the actual sender. No environment variable
  or user-writable bootstrap location determines supervisor behavior. Keep the
  inherited birth-lock descriptor in the supervisor, set CLOEXEC for installer
  execution, and explicitly close it in any forked installer child before
  execution. The supervisor retains its own copy. Do not pass ownership evidence
  to Node. Channel EOF forbids execution, but is not itself durable negative
  evidence until the supervisor records the closed-gate terminal result above.
- **Process identity and containment:** Linux identity includes `/proc` start
  time and boot_id, with a pidfd when available; macOS includes libproc start
  time and kern.boottime. A PID alone never authorizes observation or signalling.
  Reconcile both session ID and process-group ID with bounded complete scans,
  because setpgid may leave the original group while remaining in the session.
  Enumeration/permission errors or ambiguous reused identities fail closed.
  Delegated Linux cgroup v2 may provide additional containment when available,
  but baseline correctness cannot depend on its availability. A descendant
  escaping with setsid/double-fork cannot be proved absent by session/group
  scans alone. Document this cooperating-child limitation in operational
  recovery-required guidance; do not silently assume the risk is zero. Suspected
  escape or unprovable containment remains unresolved and prevents cleanup or
  rollback based merely on leader disappearance.
- **Fault evidence:** implement deterministic test-only crash checkpoints
  (`cfg(test)`, no production crash controls), AND isolated substitute-supervisor
  tests using actual SIGKILL of precisely owned/attested test processes. Cover
  every fork/registration/write/sync/permit/ACK/descendant boundary with both
  styles; no broad kill-by-name or real-service targets. Assert
  the recovery classification and that no operation is replayed, using the
  durable/OS observations available to recovery rather than the test's knowledge
  of where it killed a process. Until these pass on Mac and Linux, the intent
  remains storage only, not a recoverable startup mechanism.
- **Disposition:** status uses the same evidence rules despite being read-only.
  Archive/completion outcome is durable and bound to the original attempt;
  another attempt needs a new identity and explicit authorization. Never remove
  a conflicting marker merely to make the current operation ID runnable again.

The minimum crash matrix must encode these expected decisions, rather than
inferring success from elapsed time or a child PID:

| Observed durable/OS evidence after failure | Required decision |
| --- | --- |
| Intent only, worker died before fork or runner registration | `recovery_required`; no identity/terminal evidence, no replay |
| Unregistered runner still holds birth lock | `recovery_required`; no permit, no archive/cleanup |
| Partial/corrupt identity or uncertain sync/readback | `recovery_required`; retain evidence, no permit/replay |
| Registered identity, owner dead, runner durably records closed gate and no exec after EOF, then becomes quiescent | `never-started`; eligible only for explicit recorded disposition |
| Registered identity, runner also dies before durable no-exec terminal record | `registered-needs-quiescence`; never infer no exec |
| Registered identity, permit delivered, owner dies before ACK | `registered-needs-quiescence`; reconcile actual group, never send a second permit |
| Registered leader dies with descendants in its group/session | `registered-needs-quiescence`; group/session must be accounted for |
| Escape, incomplete inventory, unprovable identity/terminal evidence | `recovery_required`; no cleanup or rollback on an absence claim |

These names describe required recovery classifications, not implemented receipt
phases, CLI output or a claim that the current code can produce these proofs.

Permit/noexec arbitration (security2612): supervisor state has a single serialized
decision point, with mutually exclusive terminal branches `accepted` and
`closed-without-permit`; neither may transition to the other. The owner treats
the permit as accepted only after receiving the supervisor's bound acceptance
ACK, never merely after a successful write. No ACK is an ambiguous outcome,
not evidence that the supervisor did not accept or execute.

Before writing noexec evidence, the supervisor must irreversibly close the
receiver and establish that no unread permit bytes were discarded. Establish
that fact while the receiver still exists: after proving the owner cannot send
again and all writer copies are gone, drain the authenticated channel to EOF
under a bounded read and track every pending byte/frame. Only the zero-pending,
no-accepted-permit branch may close the receiver, commit the irreversible closed
state and persist noexec. Closing/shutting down the descriptor first and then
assuming its buffer was empty is forbidden. A buffered complete permit, partial
frame, read error or uncertain writer ownership prevents this negative proof;
preserve the registered identity for quiescence/recovery instead. Thus a write
may succeed without acceptance, but no acceptance ACK plus validated noexec
evidence can establish never-started only with the other required owner and
quiescence proofs. A received acceptance ACK and noexec record for the same
attempt are contradictory evidence: fail closed, never choose one as truth.

Add both deterministic and real-process races interleaving permit write,
supervisor receive/accept/ACK, owner death/EOF, receiver close and terminal
record sync. Assert mutual exclusion, no replay, and that unread/partial permit
data cannot be hidden behind noexec. Also reject stale or foreign noexec
evidence: its unique attempt, intent digest and registered identity must match
the CURRENT attempt exactly, even for the same job/operation. Recovery must use
the fixed refusal `update_launcher_attempt_mismatch` for attempt mismatch;
test previous-attempt and other-attempt records against a new explicitly
authorized attempt. The full authenticated/durable arbitration protocol remains
a requirement, not implemented behavior or new receipt fields.

The standalone `launcher_arbitration` module now implements only its serialized
in-memory decision rule. Metadata binds a unique attempt UUID, exact intent
digest and registered identity-record digest; stale/foreign bindings return
`update_launcher_attempt_mismatch` without echoing values. Only a pending gate
can accept once or select closed-without-permit after an observation of empty
authenticated EOF with all writers proved gone. Buffered/partial input, read
errors, unknown writers or binding mismatch make a pending decision uncertain;
terminal decisions never reopen or switch branches. Deterministic tests
enumerate all 625 length-four interleavings of permit, empty EOF, uncertainty,
buffered permit and partial-frame observations, plus stale bindings/read errors.
The noexec observations/transitions above are now private test-only model
inputs: production has no way to select ClosedWithoutPermit until actual
writer-closure evidence exists. Permit acceptance now consumes the authenticated
connection-bound frame described below. Constructing metadata or selecting a
decision grants no execution, receipt or recovery authority. There is no
production lifecycle caller, ACK handling, persistence or actual supervisor in
this module. Real-process races and the complete fault matrix remain outstanding.

The raw `launcher_arbitration::transport` reader now separately bounds private
permit framing: a fixed 152-byte frame plus one overflow-detection byte, under a
three-second absolute deadline that fragments cannot renew. A complete frame
requires EOF; malformed, extra or repeated frames fail closed. Empty and partial
EOF are distinct, and only a complete valid frame contains binding metadata.
The private observation wrapper borrows the still-open receiver exclusively;
neither its metadata nor EOF is authenticated authority. Socketpair fixtures
deliberately retain a live writer after write-half-close to exercise this limit.
An owned Rust subprocess fixture uses a unique private Unix socket for complete,
partial and empty streams, waiting for peer exit before reading buffered bytes;
deadline tests cover an open writer and slow fragments.
Nonblocking reads and `poll` over the input and a private cancellation socket
enforce the deadline without setting socket timeouts after peer exit. Interrupted
polls recompute the original remaining budget; input errors and deadline expiry
never synthesize an EOF observation. Cancellation data, write-half-close, peer
disappearance or descriptor errors return `update_launcher_permit_read_cancelled`.
Cancellation takes priority when both sockets are ready in the same poll, and a
final nonblocking cancellation check runs after EOF, before returning a raw
observation. This is a local interruption signal, not a durable ACK, proof of
permit-peer death, or a promise about a cancellation racing after that check.
Socketpair tests cover all three cancellation signals with buffered complete
frames and empty EOF, plus interruption of open and partial input streams while
retaining the receiver. No 1ms polling loop remains in the reader.
This reader has no production caller, peer authentication, writer-disappearance
proof, arbitration transition, ACK or persistent record. It does not satisfy the
sealed authenticated proof requirement below or authorize installer execution.

Additional wiring prerequisites (security2685): attempt and digest fields are
public metadata without a MAC. Production code must reject unauthenticated
connections before calling `read`. Authenticate the same accepted connection
that is subsequently read, using the private 0700 endpoint, OS peer credentials
and creation-time/image evidence; authenticating a pathname or another connection
does not satisfy this requirement. Any read failure, including timeout, I/O error
or invalid framing, must call `invalidate()` on the pending arbitration, making
it `Uncertain`. Never retry ambiguous input as `Pending` or as empty EOF. Add
integration tests for all three failure classes and for rejection before reading
an unauthenticated connection when this wiring exists; current raw fixtures do
not establish those guarantees. Wire the existing cancellable poll wait to the
supervisor's exclusively owned cancellation channel, retaining the same absolute
deadline and fail-closed arbitration requirement.

Cancellation wiring prerequisites (security2706): the cancellation write endpoint
must remain exclusively inside supervisor signal/shutdown handling; neither the
owner nor another untrusted party may hold or write it. Test endpoint ownership
when the real caller is added. `update_launcher_permit_read_cancelled` must
invalidate pending arbitration to `Uncertain`, just like other read failures.
A complete permit may still be buffered after cancellation; cancellation can
never justify `ClosedWithoutPermit`. These are required integration guards,
not guarantees implemented by the current raw reader.

The separate `AuthenticatedPeer::read_launcher_frame` entry now revalidates the
same connected socket's OS peer identity before reading, allows only one read
attempt per wrapper and requires pending arbitration. Identity/read failures
(including cancellation), repeated reads and non-pending input invalidate any
pending arbitration; terminal decisions remain unchanged. This connects the
existing peer verifier to raw framing, not to execution: success leaves
arbitration pending and returns a connection-bound observation borrowing the receiver.
After a successful raw read it now checks the retained process creation
identity/image again (security2748); the receiver's exclusive borrow prevents
descriptor replacement and its connection UID/PID credentials were checked
before reading. Same-PID exec or peer exit makes this check fail and invalidates
pending arbitration rather than returning the observation. Peer exit is never
interpreted as writer closure: the raw drain remains separately available for
the future sealed death/writer-proof path. There is no attempt/digest
attestation and no all-writers-gone or noexec proof.
The supervisor's private endpoint, cancellation ownership and lifecycle caller
remain unimplemented. Socketpair regressions cover malformed framing, timeout,
cancellation, non-retry and raw empty EOF. The real owned Rust peer fixture
leaves a byte buffered before exiting: identity rejection leaves that byte
unconsumed and invalidates pending arbitration. A real OS read-error injection
is not covered by these integration fixtures.
The same real exited-peer fixture separately drains raw empty EOF and applies
the post-read image guard: it must return `update_handoff_peer_changed` and
invalidate pending arbitration. This checks the guard with real stale OS
identity and separately tests the post-read guard; it does not test exec during
the integrated read.

An additional owned Rust subprocess regression now covers same-PID exec during
the integrated read. It authenticates the original image on a unique private
Unix socket. A test-only duplicate observes the shared `O_NONBLOCK` flag change
performed by raw read after pre-verification, before releasing the child's
stdin barrier. The child then execs a copied test image with a different inode,
deliberately retaining its own socket across exec; the target restores CLOEXEC
and sends a complete valid frame on that same socket, half-closes writes and
remains alive. A separate connection confirms target readiness; OS evidence
confirms the same PID and changed executable inode. The actual authenticated
reader must return `update_handoff_peer_changed` and leave arbitration
`Uncertain`, with no observation returned. No production test callback, unsafe
descriptor ownership conversion, external CLI, installer or fixed port is used.
Existing nix `fs` support is enabled only as a Unix dev dependency for fixture
descriptor flags. This establishes the image-change read-window regression;
sealed lifecycle integration remains unimplemented and requires its own combined
tests.

The authenticated read entry now also compares a complete frame's attempt,
intent digest and registered-identity digest with the pending arbitration's
binding, after the post-read image check. A mismatch returns the fixed
`update_launcher_attempt_mismatch`, invalidates Pending to Uncertain and cannot
be retried or subsequently accepted with current metadata. Matching input still
leaves Pending and returns raw data, without ACK, persistence or execution.
Integration regressions authenticate actual same-PID socketpair peers and send
valid matching, previous/future-attempt, changed-intent and changed-identity
frames. This closes metadata equality at the read boundary only: the binding is
still caller-constructible, not derived from an authenticated durable supervisor
registration. That derivation and sealed writer-closure proofs remain required.

The returned `LauncherFrame` now has fields private to the dedicated leaf
`handoff::launcher_frame` module, no public constructor and no Clone
implementation. Even sibling ownership/worker modules and the parent handoff
module cannot initialize its fields. It wraps the raw observation and retains both the
actual receiver's exclusive borrow and the authenticated process image borrow.
The public `Arbitration::accept` consumes that frame by value; Binding metadata
or a raw transport observation alone cannot be passed to it. Acceptance checks
the process image again (covering a caller delay after reading), rejects empty or
partial observations, rechecks the exact pending binding and preserves terminal
decisions. Metadata-only acceptance is private to the arbitration module; its
pure transition tests still exercise the state machine independently. Integration
tests cover successful acceptance, empty/partial rejection, a replaced binding,
invalidation after reading, and two independently authenticated frames competing
for one gate, where only the first can succeed. This does not establish that a
supplied binding came from durable supervisor registration, prevent a caller
from constructing another arbitration with the same metadata, or prove writer
closure/owner ACK/durability. The real lifecycle must exclusively own one gate
per registered attempt. A source-audit regression scans self-update Rust files
for explicit frame construction and requires the sole site to remain in the
authenticated leaf reader; it also guards against introducing ordinary child
module declarations into that leaf. This is a grep-style tripwire, not a Rust
parser or a full compile-fail test: the private-field compiler boundary enforces
external construction refusal. Source spelling tricks or new construction APIs
still require code review.
Lifecycle integration review must retain both construction-site and leaf-module
checks together. Outside handoff, imports/references to the private
`launcher_frame` module must not introduce non-type access; consumers use the
re-exported frame type and the authenticated reader/acceptance boundaries.

A real owned Rust child fixture sends a valid matching frame, half-closes its
socket and blocks on stdin. The parent authenticates and successfully reads
the frame while the child is alive, then either releases stdin for normal exit
or sends SIGKILL to that exact owned child, and reaps it before acceptance.
Consuming the retained frame must return `update_handoff_peer_changed`, leave
Uncertain and refuse retry, despite the earlier successful read. Unique private
0700 directories/sockets and cleared child environments are used; no deployed
process or installer is touched.

The owned exec fixture also tests the separate delay between successful read
and acceptance. In this mode the original image sends a valid matching frame,
half-closes writes, then waits for the parent's stdin byte before exec. The
reader thread retains the actual `LauncherFrame` and signals successful read;
the parent confirms the original image is still current before allowing exec.
The copied target restores CLOEXEC on its retained original socket, reports
readiness through a separate connection and stays alive. After OS evidence
confirms the same PID now has the copied executable inode, a second channel
barrier lets the reader consume the frame. Acceptance must return
`update_handoff_peer_changed` and leave Uncertain. Thus both exec-during-read
and exec-after-read cases reject a live peer with a changed image, at their
respective guards. Channel/stdin/OS barriers establish ordering; no production
hooks or timing sleeps manufacture the acceptance window. This still does not
implement supervisor registration, ACK/durable decisions or writer-death proof.

Required before lifecycle integration (security2665): replace caller-constructible
EOF observations with a sealed value whose constructor is restricted to the
authenticated transport/drain module. Tie it to the actual still-open receiver
and exact registered attempt; only the path that proves authenticated empty EOF
and all writers gone may construct it. Similarly restrict construction of the
verified binding. Add compile-fail or equivalent source-boundary checks that
other modules cannot fabricate either proof. The current ordinary data model
does not satisfy this requirement and must not be connected as an authority.

Recovery must distinguish three independent facts: the supervisor's in-memory
decision, the owner's observation of a bound acceptance ACK (or lack/ambiguity
of that observation), and the decision record's verified durable sync/readback.
`Accepted` describes only the first. It means neither owner-ACKed nor durably
committed; writing an ACK is not receiving it. Model pending/verified/uncertain
persistence separately, and do not expose a memory-only decision as durable
evidence. A failed or ambiguous write/sync/readback remains unresolved. The
future record and recovery classifiers must preserve these distinctions,
including lost ACK and process death between decision, persistence and ACK.
No such persistence or recovery classifier is implemented by the current model.

Known permission boundary (security2545): these checks inspect Unix owner/mode
bits only. They do not inspect macOS/POSIX ACL-granted write access or platform
file flags (including Linux immutable flags). A passing check is not proof that
an ACL grants no other account write access; operators must enforce that policy
with the OS. This primitive does not add an ACL isolation guarantee.

Recipe preparation now distinguishes initial trust-validation refusal
(`update_launcher_node_untrusted_location`) from failure to revalidate a retained
identity (`update_launcher_node_changed`), without exposing raw paths or errors.
The latter means revalidation failed, not proof of an attack. The invocation
layer implements these mappings; the platform helper still returns one fixed
diagnostic. Future executor checks must preserve the same distinction.
Resolved Homebrew/admin-group-writable directories and group-writable
`/usr/local` ancestors are examples that fail the unchanged `0o022` check, even
when they are intentional local package-manager layouts. A machine administrator
must provision Node22+ in a supported fixed location with trusted resolved
ancestors and audit/remove unintended group/other write permissions; intentional
shared-write layouts remain unsupported by this updater. Neither the updater
nor an Agent should chmod those directories automatically, bypass the check or
substitute an ambient interpreter to make installation proceed.

Tests exercise the same walker under a private fixture-root handle, avoiding
system-directory writes and a real Node dependency; only the private test seam
can choose that root, while the public entry always starts at `/`. They cover
same-bytes replacement, ancestor replacement with the original file inode,
unsafe modes on the root/ancestor/file, file/ancestor symlinks and non-regular
files (Unix sockets on Mac/Linux and a FIFO on Linux), with fixed diagnostics.
The owner predicate also explicitly covers root/current/foreign IDs; no real
chown or installed Node was exercised. This is not process-execution acceptance.

Static package-manager audit: the Rust launcher uses `install.mjs
--controller-request`, which dispatches directly to `controller-install.mjs`,
not the legacy git/npm installer `main()`. Its fixed self-update argv omits
`--allow-network` and `--npm-cli`; Rust rejects a source-only install before
starting the JS installer, and controller-install also refuses non-portable
payloads without explicit network permission. Its sole npm branch requires a
non-portable payload and explicit permission; portable install copies verified
bundled assets, while status returns before that branch. Profile operations and
contract queries still invoke the selected CLI and may use the authenticated
control plane: this is not a claim of zero network traffic. Actual executor
acceptance must assert no npm/git/package-manager child or dependency-network
access on install/status (including refusal of a source-only payload), using
isolated HOME/configuration and process/network evidence. This source audit
does not replace those execution tests or prove that no user config was read.

The worker-side `ImageBatch` now requires the sealed `OwnedActivation` and an
exact matching Prepared receipt. It preflights both images' backups, original
modes/current bytes and executable stages before recording Activating or making
the first rename. The consuming publication operation preserves the existing
per-file intent/completion journal and publishes Controller then CLI. Failure
immediately attempts verified reverse-order image restoration; an unrecognized
backup or destination fails closed and records RollbackFailed when possible,
retaining evidence and the caller's locks. Rollback binds the original job,
source, target hashes, original hashes and timestamps, allowing only monotonic
progress fields to change. A file-only restoration remains RollingBack: it
does not prove that the old Controller is running or healthy. No Host request,
startup-store overwrite, launcher installation or terminal success is performed
by this layer. Isolated tests use a cfg(test)-only ownership constructor and
synthetic image bytes; they do not constitute real handoff/activation acceptance.
Deterministic interruption tests reopen the journal at all seven publication
boundaries (Activating, then intent/rename/completion for each image), and on
both sides of each reverse-order restoration rename. They verify convergence
to both original bytes and modes without consuming unpublished stages, continued
lock ownership and idempotent RollingBack receipts. These tests retain the
original sealed ownership and locks: they cover durable file reconciliation,
not recovery after worker death or reacquisition of abandoned ownership.

The forward `activate_core` orchestration now reconnects to the saved Host
baseline before publishing, then issues the existing durable one-shot
Controller-only restart request. It never retries a lost/error ACK. A separate
30-second total observation budget (250 ms between attempts, after the existing
bounded restart request) checks the actual live Controller image, source commit,
new epoch, verified Host parent and unchanged workload/output baseline through
`observe_live`. ACK alone cannot advance CoreHealthy; lost ACK can still be
resolved by that full independent observation. Failed or stalled health restores
the two images and leaves RollingBack, without claiming the old Controller is
running. Ownership remains with the caller. Tests substitute only the private
runtime seam to cover ACK/health disagreement, lost ACK, a stalled observation,
and rejection of a second activation. The old-Controller restart/health half is
now composed separately by rollback_core (described below). Real Host activation
acceptance, production command dispatch, launcher work and post-worker-death
recovery are still pending.
Only fixed known startup-transition errors are retried. Explicit Host identity,
workload/output continuity or binding failures, and unknown errors, immediately
enter verified image restoration; they do not wait out the observation budget.
The fixed 30-second budget is an availability limit, not a performance guarantee:
a correct Controller whose cold startup exceeds it can trigger a safe rollback.
Reassess that budget using release acceptance measurements, not retry-until-green.
Restoration can race a previously accepted restart whose ACK was lost or whose
execution is still pending. That request may then launch the restored old image;
it is not evidence of rollback health. The rollback activation layer uses the
independent `controller-rollback-intent.json` / `-rollback` operation ID at most
once, and reconciles the live result instead of replaying the forward request.
It proves the old running image and unchanged Host/workloads before writing
RolledBack; ACKs cannot settle either direction.

Unix lock handoff now has a bounded SCM_RIGHTS transport (one to three lock
descriptors). The parent retains its handles until durable ownership ACK; the
child receives references to the same open file descriptions, so closing the
parent's copies does not create an unlock/relock gap. Core reopens checked stable
lock names only for inode comparison, adopts each received exclusive lock, and
rechecks the selected enrollment/shared-installation inventory. A different
workspace's enrollment lock is rejected. Descriptor count/header/truncation
errors fail closed and close received handles; no ambiguous-send retry. Linux
receives with atomic CLOEXEC. On Darwin receive must run before activator threads
or subprocess creation, because CLOEXEC is applied immediately after receipt.
The socket must be dedicated blocking I/O with bounded deadlines. This transport
does not authenticate its peer or establish a durable ownership ACK: the future
activator entry must authenticate the private socket's peer/job/verified image
and commit handoff before the parent reports accepted. No public activation
command exists yet.

The process-proof wrapper for this transport now obtains UID/PID from the exact
connected socket (Linux SO_PEERCRED; Darwin LOCAL_PEERPID/getpeereid), requires
the expected owned process's creation identity and the held verified image inode
and digest, and rechecks that process before and after sending/receiving locks.
The wrapper exposes no raw socket accessor. Same UID or same binary alone is
insufficient; another PID, an equal-content different inode, a wrong digest or
an exited process is rejected. Sender descriptors must already be CLOEXEC.
The endpoint is now exclusively created as `handoff.sock` under the retained
0700 prepared-job directory, checking the pathname still names that directory
and the socket inode has not changed. It never replaces or unlinks an existing
entry, including on drop; stale endpoints require explicit recovery. An overly
long Unix socket pathname fails closed, with no public temporary-path fallback.
Acceptance checks the exact prepared receipt (including job and revision) before
and after authenticating the connected child. Accepted sockets are blocking with
three-second I/O bounds; listener and authenticated sockets require CLOEXEC.
The parent directory supplies privacy, not the socket file's umask-derived mode.
Pathname checks do not provide atomic bind-at semantics against another process
of the same OS account. This wrapper is not a same-account sandbox.
The one-shot ownership exchange now binds the complete Prepared receipt on both
sides before transferring descriptors. The child adopts the checked locks, then
exclusively creates `ownership.json` with that receipt and its OS PID/creation
identity, syncs the file and directory, and only then sends its ACK. The parent
requires an exact ACK/durable-record match to its authenticated live owned child,
and an unchanged journal. Its final confirmation lets the child continue without
racing the parent's journal check. Both APIs consume and close the channel;
the child retains the received locks in `OwnedActivation`. The parent's API only
borrows its locks: errors do not drop them, and acceptance permits their release.
Frames are bounded to 96 KiB, deny unknown fields, and use one three-second
deadline per complete frame (partial reads do not restart it); descriptor I/O
retains three-second bounds. Reads poll the dedicated single-reader socket with
the remaining deadline. A Mac regression proved that changing SO_RCVTIMEO after
the peer closes returns EINVAL even while its complete final confirmation is
still queued; polling drains that frame without requiring the sender to linger.
The earlier timeout hypothesis was disproved: no longer timeout is needed for
this failure. Disk sync/process authentication themselves are not claimed to be
interruptible by I/O deadlines. The future coordinator still needs an overall
owned-child startup/activation deadline. The owned-process fixture synchronizes
authentication before beginning its ownership exchange, keeping unoptimized
test-executable hashing outside that exchange's I/O budget.

An existing ownership file, including an incomplete one, is never overwritten or
removed on failure. A lost ACK/final confirmation is uncertain and requires
explicit recovery, not another submission, automatic retry or PID-file takeover.
Child connection loss before final confirmation returns an error and drops its
received locks; the parent retains its originals. Tests include an actual owned
subprocess retaining exclusion after parent-handle release, plus forged ACK,
missing/different durable record, changed journal, uncertain-record retention,
malformed/oversized/truncated/stalled frame and cross-enrollment rejection.
This is still not the full updater: owned child launch/detachment, integration of
parent-release/activation/recovery, launcher installation and real isolated
upgrade acceptance remain to be wired and tested.
The activator must close the handoff socket before any further
fork/exec: CLOEXEC alone cannot prevent another child inheriting it on fork.
The preparation coordinator must use `Endpoint::mark_prepared`: it validates the
host's Unix socket address length and the private locator before writing Prepared,
returning the fixed `update_handoff_endpoint_unavailable` code on failure without
creating a socket. Binding repeats the check; preflight does not reserve a name.
The ordinary journal `advance` rejects Prepared. Only the endpoint module can
construct the preflight proof consumed by the journal; the complete receipt must
still match when that proof is consumed. Journal-only tests have a separate
fixture helper that is absent from production builds.
The handoff deliberately fails loud on any authentication/endpoint/journal error:
the endpoint latches aborted and refuses further accepts. The future coordinator
must record the fixed error while retaining the parent's locks; it must not retry
or report ownership accepted. A same-account process racing the child can thus
abort this job, an availability limitation of the single-account trust domain.
Immediate lock-release proofs run in isolated test subprocesses: CLOEXEC closes
descriptors on exec, not fork, so an unrelated parallel test's fork/exec window
can transiently retain another lock reference. The tests remove that unrelated
fork window rather than retrying a failed release assertion.

1. Acquire an installation-wide cross-process lock (shared binaries span
   workspaces), then selected-enrollment activation lock in that order. Enumerate
   enrollment names without exposing credentials; require explicit shared-bin
   acknowledgment if another installation shares the destinations. Activate only
   the selected Controller; warn that other workspaces use new binaries on their
   next start. Recheck paths, free space, running Host identity and capability.
2. Create a UUID job below the configured Treer state root, outside the workload
   root/shared TEMP. Unix directories/files are 0700/0600; Windows DACL is current
   SID plus SYSTEM. Reject symlink/reparse chains and unexpected existing files.
   Download, verify, stage and smoke-validate before any destination replacement.
   Executables get required executable mode after verification, before launch.
   Close and sync files; same-volume sibling staging is needed for atomic rename.
3. Back up Controller, CLI and startup store privately; hash each and record the
   startup store revision/hash. A backup contains credentials and never travels
   through Proxy, stdout or the public receipt. Do not modify startup specs as
   part of Core activation. Save durable `prepared` receipt before replacing files.
4. Start a verified, job-local activator (a copy of the installed Rust Controller,
   with matching hash) with sanitized service environment, null stdin and private
   bounded logs. The activator owns the transaction lock for the full operation;
   parent and child use a ready/ownership handshake, not an unlocked interval.
   Parent returns accepted/job ID only after durable handoff. A lost exec response
   is ambiguous: poll the existing job, never automatically submit another one.
5. On Windows use `CREATE_BREAKAWAY_FROM_JOB | DETACHED_PROCESS |
   CREATE_NEW_PROCESS_GROUP`, no inherited handles, and verify the activator is
   outside any Job before acknowledging handoff. These flags are a request, not
   proof: existing Host Jobs may prohibit breakaway. If prohibited, fail before
   mutation with `activation_requires_host_terminal`; use a trusted host terminal
   or a separately reviewed Host-owned activation mechanism. Do not fall back to
   CIM/PowerShell, silently alter Host Job policy, or claim arbitrary `treer exec`
   can always survive its Controller's termination. This limitation is an explicit
   P2 acceptance question for the existing deployed Host, not a reason to restart it.
6. Activator writes `activating`, retains verified old Controller/CLI under unique
   `.prev-<sha>-<job>` names, then places verified replacements. Windows uses checked
   native rename/replace operations: running image sharing/antivirus denial can
   fail; bounded OS-sharing retries cannot bypass failed verification. Journal
   intent and completion for each individual rename so a crash between old-image
   rename and new-image placement is recoverable. Unix renames staged siblings
   over destinations and syncs parents. No two-file atomicity is claimed.
7. The **existing Host** receives its ordinary restart-controller request and
   starts the configured installed Controller, not a second manually spawned
   Controller competing for the machine lease. The detached activator remains
   alive as watchdog. Check new Controller epoch, expected binary source identity,
   Proxy connection, and unchanged Host identity plus workload IDs/PIDs/generations.
   Missing old-Controller health/contract capabilities fail preflight explicitly.
8. Invoke verified new Controller `launcher install --workspace W` with trusted
   Node/CLI paths. Verify manifest capabilities, profile readback and installed
   version. Do not restart Agents or rewrite customized profiles without consent.
   A separate smoke Agent requires explicit test authorization, is deleted only
   by its returned stable ID, and is not confused with preserving existing Agents.
   Core healthy but launcher failed yields `partial`, never `complete`; preserve
   receipts/backups and require an explicit follow-up or rollback decision.

The internal activator entry is not an authorization boundary: a same-account
process can already replace these binaries. Nevertheless it accepts only a
validated private job ID, revalidates hashes/paths/state transitions, and cannot
become a generic command execution or process-breakaway API. Policy `machine.exec`
remains the remote submission gate. One OS account is one trust domain.

## Receipts, rollback and crash recovery

Receipt schema v1: `job_id`, monotonic `revision`, `workspace_id`, `server_id`,
`release_sha`, `artifact_source_sha`, source kind, verified binary hashes,
`phase`, start/update timestamps, bounded fixed `error_code`, per-step outcomes,
before/after Controller epochs and Host/process invariants. Phases:
`preparing -> prepared -> activating -> core_healthy -> launcher_installing ->
complete`; failures terminate as `failed`, `rolled_back`, `rollback_failed` or
`partial`. No URL secrets, argv, config content, environment values, raw child
output or startup credentials. Private transaction metadata additionally stores
checked destinations, backup hashes and rename progress; only fixed safe fields
are exposed to the coordinator. Receipt reads never authorize arbitrary file read.

Write temporary receipt in the same private directory, close/sync, native atomic
replace, sync parent where supported. On NTFS validate effective ACL/owner and
protection after replacement (equivalent duplicate ACEs are not permission
expansion). Retain previous receipt until replacement succeeds; never delete it
first. Faulted writes must not produce success or a missing receipt. Power-loss
durability limitations of each OS/filesystem must be documented from tests.

On Core activation failure restore both verified old binaries and ask the same
Host to restart only its Controller; report `rolled_back` only after old epoch,
connection and workload invariants pass. If restore/activation fails, retain
backups and report `rollback_failed` with an explicit manual recovery command.
Do not overwrite a concurrently changed startup store: normally it was never
modified; any necessary restore requires revision/hash compare-and-swap under
the recovery lock. Refuse incompatible persisted-schema changes before activation
unless forward/backward compatibility is proven. Launcher failure does not
blindly restore a stale startup backup over legitimate concurrent edits.

P1 implementation constraint: the existing startup store has only an in-process
mutex, not a global store revision or a writer-shared OS lock. The first backup
primitive therefore only captures private original bytes and compares their
hash/revision projection; it provides **no restore write API**. An updater-only
lock or a preceding hash comparison is not a CAS against a live Controller.
Any eventual restore must exclude all store writers (including cached state
which a live Controller could later flush), or fail explicitly without writing.

Unix live observations bind the UID/PID to the same socket used for Host Sync.
Linux uses boot ID/start ticks and the opened `/proc/PID/exe` inode. Direct-child
enumeration requires the kernel's `CONFIG_PROC_CHILDREN` support; without its
per-thread `children` files, the running Controller cannot be proven and update
fails closed. On macOS,
`proc_pidpath` alone is insufficient: after replacement its saved name can open
the new disk file while the old process remains running. Compare the first
file-backed executable region's kernel vnode identity, creation time, and checked
opened file; bounded libproc reads fail closed for unsupported layouts/protected
processes or an unavailable original image. Never repair this by trusting the
replacement filename. A Controller successor must be a direct child of the same
Host executing the selected installed inode, in addition to its scoped new-epoch
HTTP health and unchanged Host/workload/output checks. These observations do not
attest every mapped code page or isolate a malicious same-account writer.

The Unix restart primitive now requires the selected installation/enrollment
locks, a matching job revision and scope, completed image publication (or
completed restoration for rollback), independently checked installed digests,
and an unchanged live Host/workload observation. It creates and syncs an
exclusive private restart-intent file before the ordinary Host request. An ACK
only means accepted; it is not proof of activation or health. A crash, lost
reply, malformed response, or an existing/partial intent never triggers an
automatic resend. Activation and rollback have separate job-bound operation
IDs and intent files. Recovery must reconcile actual state before deciding any
follow-up; the detached coordinator, lock handoff and recovery entry are still
unimplemented, so this primitive is not a usable self-update command.

A crash after durable intent but before transmission is deliberately ambiguous;
do not delete the marker or infer that the request was never sent. Recovery may
accept activation only through `observe_live`: the expected installed and running
Controller image, a new scoped epoch, the same parent Host, and unchanged
workloads/output must all be proven. Otherwise retain the evidence and choose a
separately journaled rollback or explicit manual abandonment, never automatic
restart replay. The activator acceptance tests must include an ACK followed by
failed live observation: it must enter rollback, not `core_healthy` or `complete`.
This is a required integration test, not coverage supplied by the current
request-only fixture.

The private pre-update Host baseline is now an exclusive 0600 job file, bounded
to 8 MiB and bound to the job/workspace/server and expected artifact identities.
It retains only Host creation/image identity, build/epoch, workload stable IDs,
PIDs/generations/status/stream counters and output hash witnesses, plus the
independently observed previous Controller epoch. It contains no cwd, command,
environment, workload credential or raw output. A partial/existing file is not
overwritten. Recovery loads and validates this original baseline, connects to
the selected Host with same-socket OS peer attestation, and verifies a cursor
Sync against the saved observations. It never substitutes a freshly captured
inventory. Missing, malformed, aliased, cross-job or changed evidence fails
closed. The restart primitive additionally requires its in-memory observation
to match this saved original; persisting a baseline is required before mutation.

Saving the baseline now requires a typed initial Controller observation, not a
caller-supplied epoch string. While the job is Preparing, the monitor finds the
selected original image as the verified Host's actual OS child, reads bounded
credential-free health at the selected literal address, and checks workspace,
server, nonempty bounded epoch/build, source-SHA syntax and Proxy connectivity.
It rechecks the Host, live process and installed image, then binds the observation
to the job and original image digest. Saving rechecks this process on both sides
of Host Sync. Recovery tests require the exact HTTP-observed old epoch, distinct
from a caller default. The private baseline format is now schema 2: it retains
that original source SHA alongside the epoch and binds the original Controller
digest as well as the replacement digests. Missing/malformed old build evidence,
schema 1, or an altered original digest is rejected; this unshipped private
format has no migration/default that could invent rollback evidence. A later
rollback health check must use this saved old source SHA and original digest,
never the replacement source SHA or a newly captured post-failure baseline.
The rollback coordinator now consumes this saved evidence while retaining the
same ownership and locks. It accepts only an already RollingBack job, rechecks
the verified image restoration and recovers the same saved Host/workload
baseline. It uses the separate durable rollback restart intent/operation (never
retries a Host mutation after a lost ACK or an existing intent), then observes
the original installed/live image inode and digest, the old source SHA, a new
epoch distinct from the original epoch, the same Host parent, and unchanged
workloads/output. Only this full proof advances to RolledBack; observation
failure/timeout advances to RollbackFailed with original files and evidence
retained. A journal change during observation is refused, not overwritten. The
forward restart can still be in flight and may itself launch the restored old
bytes: its ACK is not rollback proof. The private observation seam is shared by
forward and rollback coordination; production cannot replace it with HTTP-only
or ACK-only success. These paths remain library work, not a production command
or worker-death ownership-recovery entry point; a real isolated upgrade and
service-manager survival gate are still required.
If rollback setup/recovery fails (for example, the saved Host was replaced or
its baseline cannot be proved), the call can return an error with the receipt
still RollingBack. This is deliberately not terminal success. The production
caller must report the fixed status `update_recovery_required`, preserve the
receipt/evidence and original lock ownership, and stop automatic activation;
it must not loop spawning workers, restart the Host, or label the files-only
restoration RolledBack. Any later manual recovery entry point needs explicit
authorization, the full original receipt and attested lock ownership; none is
provided by this increment. Worker-death recovery acceptance must include death
after a forward restart was accepted but before its ACK, with that request then
launching restored old bytes: reconcile durable intent and actual live identity
without replaying either restart or inventing a new baseline.
The five retryable observation diagnostics are shared constants at their
production sources and the explicit allowlist. Unknown/new/renamed diagnostics
remain permanent by default, including familiar prefixes or wrapped text;
negative tests guard that boundary.
An old build string is an observed claim, not signed
release provenance. The detached coordinator and end-to-end activation gate
remain outstanding.

The HTTP-to-process binding is indirect: a response at the selected loopback
address and a simultaneously verified Host child image do not prove that this
child owns the listening socket. Loopback TCP provides no peer process
attestation here. Another process under the same OS account could occupy the
health port and forge an epoch, causing an incorrect health decision. This is
within the documented single-account trust domain, not an authorization grant.
Later live observation still requires the target image, a changed epoch and the
same verified parent Host; those checks do not make the HTTP response itself
process-attested.

After activator death, the next explicit `self-update --recover JOB_ID` reconciles
journal, actual hashes and live process identities under the same locks. Never
infer success from a PID or an old receipt alone; never restart the Host or replay
a partial launcher/smoke operation implicitly. Keep completed backups/receipts
under an explicit retention policy: keep the latest three completed transactions
by default, never evict an unresolved transaction automatically. An explicit
cleanup command removes only journal-registered, validated exact job directories;
retain any job whose safety cannot be established. Backup paths live in the Treer
reserved namespace denied by machine file transfer.

TMPDIR recovery requirements (security2569): an existing `launcher-tmp` remains
a refusal, not permission to retry creation or adopt its contents. The explicit
recovery entry must report `update_recovery_required`, retain evidence and stop
automatic activation while reconciling the original journal, attested ownership
and durable child identity, including an accepted start whose ACK was lost.
Only after proving all relevant children quiescent may the controlled recovery
flow move the exact validated directory to a private, journal-recorded archive
under the same retained job handle, sync the move and record its outcome. Never
infer quiescence from worker death or a missing PID. An ambiguous archive move
must itself be reconciled before another mutation. A later explicitly authorized
attempt may create new exclusive storage only after recovery has validated the
job phase and recorded that disposition; it must not reuse the archived inode
or replay the previous installer implicitly. Unprovable ownership/quiescence
keeps the job unresolved and exposes the fixed recovery-required status for
operator intervention, rather than looping or silently discarding evidence.
This recovery entry and archive protocol are requirements, not implemented by
`LauncherTemporary::create` or Drop.

The state-tree TMPDIR may contain payload copies, installer logs or other
sensitive material. Treat it and recovery archives as equally sensitive as
credential-bearing backups: keep private owner/permissions, exclude contents
from receipts/diagnostics and apply the same protected backup policy. Count
archives and TMPDIR bytes toward both per-job and aggregate retained-job disk
budgets. Before production installer dispatch, enforce finite configured quotas
covering active and failed jobs, and refuse new work at exhaustion with evidence
preserved. A free-space precheck alone is not a quota or protection against
growth during installation. Completed-job cleanup includes these directories
and archives within the latest-three-transaction retention limit; unresolved
jobs are never automatically evicted to make room. They continue consuming the
aggregate budget and require explicit safe recovery/cleanup. Quota enforcement,
bounded cleanup accounting and its exhaustion/recovery tests remain unimplemented
acceptance gates; the current storage primitive does not bound installer writes.

Signed-channel P4 adds a durable highest accepted release sequence per trusted
key/channel and signed monotonic metadata; current version strings do not supply
that property. Reject replay/downgrade before executing staged code. Transaction
rollback to its verified previous image is distinct from accepting an old network
release and must not lower the high-water mark. Legacy metadata lacking the new
capability fails clearly once protection is enabled; key rotation/recovery needs
an explicit local trusted procedure. P1/P2 must not claim anti-rollback protection
until P4 ships. Owner rescue/Provider authorization are unchanged.

## Security review questions and refinements (comment 1676)

Use existing HTTP/Ed25519/SHA-256 dependencies; no additional network client.
Staging must stream into exclusive no-follow files and hash incrementally.
Windows verified files remain protected from concurrent write/delete through
checked handles until publication; receipt/binary mutation uses those verified
identities, not a later unchecked reopen. Test Host executable bytes as well as
PID/creation identity unchanged. Bind launcher workspace, server and URL explicitly.

The future signed sequence document needs an expiry, authenticated sequence and
local highest accepted value; advance the value only after healthy activation,
never lower it on rollback. Explicit same-version/downgrade recovery is local
operator break-glass, recorded in receipts and audit. Pinned SHA has no intrinsic
ordering: require an explicit `--allow-same` or `--allow-downgrade` acknowledgment
when the operator cannot establish forward order from authenticated metadata.
Audit submission and outcome with actor/mode/hash/sequence/fixed code, not paths
or credentials. Offline audit delivery must not erase the durable local receipt.

Leader decision 1684 and security approval 1687 resolve the P0 questions:

1. The target is signed-only. Release will publish every `canary_passed` build
   to a signed `canary` channel with monotonic sequence metadata. Any new dedicated
   remote endpoint accepts signed manifests only. Until that channel exists,
   exec-origin pinned updates require `--allow-pinned`, are marked unsigned in
   receipt/audit, and default to the enrolled Proxy origin. Another origin needs
   explicit `--allow-artifact-origin` acknowledgment recorded in the receipt.
   Remove pinned mode in P3; do not release GA with this transition remaining.
   No fictitious operator-only environment guard is added: authorized machine.exec
   already permits arbitrary code as that OS account, so pins trust the authorized
   caller and its release manifest, not an independent publisher signature. This
   does not resist a compromised Proxy or a same-account Agent. Do not require
   a human credential in argv/environment to create an unenforceable boundary.
2. Build/hash checks of the staged successor happen **before** old Controller
   termination; Host-owned sequential respawn remains the lifecycle authority.
   A running successor cannot first connect with the same machine credential and
   then terminate the old Controller without split-brain risk. A preflight-only
   successor may prove executable identity over local IPC but must not open a
   Proxy connection. Existing Host restart stays wire-compatible; any stronger
   authenticated handoff requires explicit Host capability negotiation. On
   startup detect unfinished transactions and refuse ordinary activation until
   deterministic reconciliation; do not implicitly replay launcher/smoke work.

Use OS advisory locks (`flock`/`LockFileEx`), released on process death, with the
installation-before-enrollment ordering above. `--recover` takes those same locks
and refuses an active activator. Job selectors are UUIDs only, never paths. Keep
activator logs private and size bounded with fixed child failure codes; no raw
child output, environment values or URL queries. P1 health assertions compare
both source commit metadata and the installed image digest, and audit-delivery
failure tests must prove the local receipt remains intact. Before P2, obtain an
authorized non-mutating breakaway probe on the deployed Host and record it in
the ticket; approval of this design is not approval of Windows implementation.

Additional fault cases: truncate downloads, replace checked parent directories,
crash during rollback, expired signed metadata and any same-version/downgrade
override without operator acknowledgment. Neither an operator flag nor a local
receipt grants new remote permissions.

## Phases and acceptance

| Phase | Deliverable / gate |
| --- | --- |
| P0 | Security1687 conditionally approves Unix P1; leader1684 decisions recorded here. Launcher coordination review remains requested. |
| P1 | Shared verifier/transaction/receipt engine + Unix activator; private fake HTTP/signatures, fault injection and real isolated Linux/macOS handoffs. |
| P2 | Windows native filesystem/ACL/lock/Job helpers; hosted NTFS tests plus authorized win_native exact official-binary apply, launcher install and smoke. Establish whether existing Host permits breakaway before promising exec-origin activation. |
| P3 | Signed Canary channel live; remove pinned mode. Coordinator submits/polls only; migrate per-machine receipts. Remove PS/Python engines only after replacement feature parity and platform acceptance; no double activation or fallback to legacy after ambiguous success. |
| P4 | Signed sequence/replay protection, key-rotation and backward-compatibility tests; clearly advertise capability. |

Dependency correction from security1690: the phase numbers do not authorize a
P3-before-P4 rollout. P3 signed-only cutover and removal of pinned mode require
P4 sequence/expiry verification plus a durable local high-water mark to be
implemented and tested first (or delivered in the same reviewed cut). The signed
Canary publisher and consumer must agree on these fields before cutover. P1
journal schema reserves nullable accepted key/channel/sequence/expiry fields;
null means protection is unavailable, never sequence zero or permission to
downgrade. Legacy signatures alone provide authenticity, not freshness or replay
protection. Until the dependency is satisfied, keep the documented transition
explicit and do not advertise the signed-only target as complete.

Required deterministic tests: bad/missing/duplicate SUMS, wrong digest/signature/
platform/source identity, redirects/URL credentials/ambient proxy; path traversal,
UNC/verbatim/ADS/reserved names/reparse replacement races; inherited NTFS ACLs,
true-null/ReplaceFile semantics, locked running images; every rename/receipt crash
point and ENOSPC; stale PID, lock contention and two workspaces; activation timeout,
old Controller rejection, failed rollback and startup-store concurrent edits;
detached watchdog survives exec/Controller exit or rejects before mutation;
unchanged Host/Agent inventory and output; Node missing/profile customization and
launcher failure produce truthful partial receipts. No timeout relaxation or
retry-until-green substitutes for these checks.

Use one Core Cargo cache, debug info disabled and hermetic private PG/NATS where
needed. Core author gate is full `just check`; release gates exact SHA and all
five platforms, then authorized real-machine receipts. Hosted shell/ACL fixtures
cannot stand in for real process activation. On win_native all writes must stay
inside leader-approved Treer paths, exact paths are posted before each apply,
changed paths and receipts afterward; no Host restart, system changes or reads of
personal files. P0 performs no machine operations.
