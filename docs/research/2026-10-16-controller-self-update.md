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
from a caller default. An old build string is an observed claim, not signed
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
