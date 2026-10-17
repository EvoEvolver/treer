# Windows self-update: first native checkpoint

Issues #111, leader3158, security3159 and launcher3161. This is a proposed
Windows-specific contract for review, not implementation or live-probe approval.
The [transaction design](2026-10-16-controller-self-update.md) still defines
verification, receipts, rollback and acceptance. Windows does not depend on the
Unix socketpair/SCM_RIGHTS/pidfd work. Darwin remains unavailable for Rust
authority-bearing self-update; its existing updater is unchanged.

## Bounded breakaway probe

First deliver a standalone native Rust diagnostic helper, separate from updater
dispatch. It does not parse update inputs, access enrollment credentials, acquire
transaction locks, replace files, change ACLs/Jobs/services or start an installer.
Its only child is the same verified helper image. No shell/CIM/PowerShell fallback.

Proposed fixed interface: `treer-breakaway-probe.exe --probe`. Its private child
mode is exactly `--probe-child`; other arguments fail before spawning. There are
no executable, command, path, PID, duration or creation-flag overrides. The runner
installs the frozen helper and records its SHA256 in the explicitly approved
`%LOCALAPPDATA%\Treer\self-update-probe-111` temporary directory. The helper
derives LocalAppData through the native Known Folder API, ignoring environment
overrides, and refuses images outside that fixed directory. It retains open
non-reparse ancestor directories and its own image without delete sharing,
rejects image hardlinks and compares final handle paths, size and SHA256. After
spawn it opens only the kernel-reported child image path matching its own path
and requires identical volume serial/file index to the pinned image. Same-account
replacement before the approved run remains a residual; the runner checks the
reviewed artifact hash. The helper itself writes no files. The runner
records the actual Host binary version separately before running it through the
real exec-origin Job; a terminal run cannot establish that Job's breakaway policy.

The parent queries its own Job membership, resolves its own image and starts that
exact absolute application with CreateProcessW, fixed child argv, no inherited
handles and an explicit empty Unicode environment block, with
CREATE_UNICODE_ENVIRONMENT. No workload environment is inherited. Request
CREATE_BREAKAWAY_FROM_JOB first, then in a second fixed attempt
CREATE_BREAKAWAY_FROM_JOB | DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP; no
CREATE_NEW_CONSOLE, shell launch or Job-policy changes. The flags are a request,
not proof. Stop at the first failure; this is a diagnostic sequence, never a
policy-changing retry. Exception required by security3182: if CreateProcess is
denied, run one fixed control with only CREATE_BREAKAWAY_FROM_JOB removed. This
control can distinguish a breakaway-specific refusal from general image launch
denial; it cannot make the probe succeed. Verify its image/liveness and clean it
up through its owned handle too. Report `breakaway_denied` only when this control
starts and is fully cleaned up; otherwise preserve `child_create` and the original
Win32 error, with a nested control report. Control cleanup uncertainty takes
precedence as `cleanup_unconfirmed`. Stop after this diagnostic; do not try the
next breakaway flag combination. The child only waits for ten seconds and exits; it spawns nothing,
prints nothing and never enters updater code. There is no child-ready assertion
from a message or PID file.

Retain the actual PROCESS_INFORMATION process handle, close the thread handle,
and use that process handle to query Job membership, PID and creation time while
checking the child is still alive. IsProcessInJob with a null Job handle checks
membership in any Job. Success requires the child is alive, every query succeeds,
the parent was actually in a Job, and the child is outside all Jobs. Parent not
in a Job is `not_exec_job_evidence`, never deployed-Host acceptance. A failed
create or child still in a Job cannot authorize activation.

Always terminate only the owned child process handle, then wait at most five
seconds for that same handle to signal. Never reopen a PID for cleanup. An
already-exited child is still waited on through its retained handle. Query or
cleanup failure produces a fixed failure, never a successful report. The child's
ten-second self-exit bounds its lifetime if the parent dies; this does not claim
the production supervisor can survive a real Controller restart. The launcher
also imposes a twenty-second overall probe deadline and reports an interrupted
probe as unknown, not success.

Emit exactly one bounded JSON report on parent stdout: schema version, result,
parent PID, nullable child PID/creation time, requested flags, nullable parent
and child Job membership, cleanup-confirmed boolean, fixed error stage and
nullable numeric Win32 error. No paths, argv, environment, credentials, raw child
output or exception strings. Only `breakaway_observed` with confirmed cleanup
exits zero; all other results exit nonzero. Freeze the precise field/result
whitelists with the implementation before live use.

Development implementation: standalone example
`crates/treer-platform/examples/treer-breakaway-probe.rs`, with Win32 FFI isolated
under its `support` module. This is not linked into a Controller command. The
runner records Host version using the existing read-only Host query and joins
that evidence with the helper report; the helper emits `host_version: null` and
`host_version_source: runner_existing_host_query`, not a fabricated version or
an ambient-environment value. The report also includes this helper's own SHA256
and size. Parent Job member enumeration is bounded to 1024 entries; an incomplete
query fails closed and no other process IDs are output. A failed parent limit
query has the fixed `parent_job_limits_unavailable` stage with null flags. Access
denied at CreateProcess is `breakaway_denied` only after the fixed control
starts and is cleaned up; otherwise it remains `child_create`. Termination
failure or an unconfirmed wait is
`cleanup_unconfirmed`, never retried through another API. The current Job may be
one level of a nested tree, so its limit flags are not claimed to enumerate all
ancestor limits; the independent child query still checks membership in any Job.

The implementation report keys are fixed: `schema`, `result`, `parent_pid`,
`host_version_source`, `host_version`, `windows_build`, `helper_sha256`,
`helper_size`, `parent_in_job`, `parent_job_limit_flags`, `attempts`,
`error_stage`, `win32_error`. Each attempt has `flags`, `child_pid`,
`child_creation_time`, `child_in_job`, `child_in_parent_job`, `cleanup_confirmed`,
`result`, `error_stage`, `win32_error`, `control`. The nullable `control` has the
same process fields without another `control`, with result `control_started` or
`failed`; it never reports `breakaway_observed`. Root/attempt results are `failed` or
`breakaway_observed`; diagnostic failures are distinguished by fixed error stage
(including `not_exec_job_evidence`). Output is below 8192 bytes; stdout failure
returns nonzero without a raw error message. Invalid argv fails before hashing
or spawning. A Windows build and native tests remain mandatory before live use.

Development verification commands (use the one agent-owned Cargo target and
DEBUG=0): `cargo test -p treer-platform --example treer-breakaway-probe` runs the
portable report whitelist tests on macOS/Linux and additionally exercises native
owned-child cleanup/create failure on Windows. On a native Windows worker
outside any ambient Job, explicitly run
`cargo test -p treer-platform --example treer-breakaway-probe windows::tests::native_job_matrix -- --ignored --exact`
for the isolated no-Job/allow/deny fixtures. These fixtures can change their own
test Job only; their code is absent from the production example. The production
build is `cargo build -p treer-platform --example treer-breakaway-probe --release`;
live argv is only `treer-breakaway-probe.exe --probe`. A native artifact hash,
full author gate, reviewed source SHA and exact #98 paths are still required
before any live run; cross-compilation checking alone is insufficient. Run the
synthetic Job fixtures only on a hosted/disposable Windows worker, never on the
user's win_native machine. That machine receives only the reviewed production
helper's fixed `--probe` command.

The runner must use the actual Host exec account's native Known Folder location,
not its own account's LocalAppData. Native fixtures intentionally differ only in
approved directory (their own executable directory) and child argv (the isolated
test-harness sleeper); those differences are compiled out of the production
example and must not be treated as coverage of the production placement/argv
checks. Keep the release helper small (image bound 32 MiB) and record its size;
the ten-second child budget includes hashing/inspection under load. The runner,
not this helper, enforces the twenty-second total deadline. Injected OS query/
termination failures and actual parent death still require acceptance evidence;
the existing native fixtures do not claim those cases covered.

Validate on native Windows with no Job, a synthetic Job allowing breakaway and
a synthetic Job refusing it; synthetic fixtures may create their own Jobs, but
the live probe never modifies the deployed Host Job. Test malformed argv, create
failure, early child exit, query failure, bounded cleanup and parent death. A
hosted synthetic success does not replace the authorized deployed-Host run.

API basis: [creation flags](https://learn.microsoft.com/en-us/windows/win32/procthread/process-creation-flags),
[CreateProcessW](https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-createprocessw),
[IsProcessInJob](https://learn.microsoft.com/en-us/windows/win32/api/jobapi/nf-jobapi-isprocessinjob),
[TerminateProcess](https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-terminateprocess).
Termination is asynchronous; waiting for the owned handle is mandatory.

## Accepted lifetime scope; first test increment

Leader3236 and security3237/3238 select a supervisor **inside the stable Host
Job**, launched by the Controller daemon rather than an Agent PTY process.
The bounded deployed probe3230 refused breakaway (Win32 error 5, root limit
0x2000); its same-Job control succeeded and was cleaned up. That diagnostic is
complete, not permission to repeat it or change Host policy. No BREAKAWAY_OK,
SILENT_BREAKAWAY_OK, WMI/CIM production fallback or Host upgrade is needed for
the selected scope: replacement of Controller/CLI only. Host updates remain
separate, user-approved operations. A transaction requiring survival of Host
exit is ineligible for this path.

Controller-only restart must leave the supervisor in the existing root Job.
Host exit intentionally kills the supervisor and installer writers. After Host
or OS loss, startup must classify the durable journal (no consumed permit,
synced decision, or partial publication), preserve evidence and never resume an
installer automatically. Death is not a rollback receipt. Publication still
requires individually atomic, staged and checked replacement steps and a
Host-death-at-each-phase acceptance matrix; none is implemented by this increment.

The test-only integration target
`crates/treer-platform/tests/windows_host_job_lifetime.rs` models a disposable
Host owning the root KILL_ON_JOB_CLOSE Job. It kills/waits only its Controller
child and starts a replacement, matching the current Host restart ordering.
It checks retained supervisor/installer process handles remain alive and the
installer continues writing. Separate cases then terminate the fake Host or
supervisor: root closure must kill both descendants; supervisor closure must
kill its nested installer while Host remains alive. Installer creation is
suspended, nested Job assignment precedes resume, and membership checks verify
root membership and exclusion from a sibling Agent PTY Job. Inheritable Job,
pipe and file sentinels exercise `bInheritHandles=false`; Controller death must
leave pipe EOF and release its exclusive file handle while supervisor lives.
The file sentinel represents a handle suitable for stdio; this is not a real
Console/PTY stdio integration test. All roles use CREATE_NO_WINDOW, no inherited
environment except three fixture inputs, and bounded lifetimes.

Only actual CreateProcess process handles are duplicated into the outer
observer; no descendant is reopened by PID for cleanup. The fixture opens its
observer PID solely for DuplicateHandle instrumentation, not production
authentication. The observer owns no Job handles, and short-lived membership
query handles close before the termination phase. Temporary files coordinate
only these owned test roles; they are not updater IPC or identity evidence.
This fixture models the Host ordering; it does not execute the real Host RPC
handler, and future Host tree-kill changes require renewed coverage.

Run explicitly on a disposable native Windows worker outside every ambient Job:
`cargo test --locked -p treer-platform --test windows_host_job_lifetime native_host_job_lifetime -- --ignored --exact --nocapture`.
Do not run it on win_native. A disposable Windows 11 build 22631 worker and the
oldest supported Windows must provide native evidence before acceptance;
cross-target checks and Server 2022 evidence alone cannot claim that coverage.
The operator guide currently names Windows 11 x64 as the initial native target.
This increment changes no production launch path, permit, lock or transaction
behavior. Security reviews its lifetime scope and native evidence before the
second increment wires launch/handoff.

## Increment 2a: owner-side launch and connection primitives

Leader3262 and security3259 accept the Server 2022/20348 and Server 2025/26100
passes for `351bbc02` as development evidence. Build 22631, the supported-version
floor, the real Host restart path and Console/PTY stdio remain final acceptance
items before enabling activation, installer authority, deployment or retirement
of the PowerShell updater. No new live-machine test is part of this slice.

`windows_update_bootstrap` is an unwired, private, test-build-only platform adapter.
Downstream crates cannot call it until directory trust and the daemon-owned
handoff have been reviewed and accepted. It
pins a non-reparse local fixed-drive NTFS image and every ancestor without delete sharing,
refuses hardlinks/alternate data streams, checks the expected SHA256 and bounds
the image to 128 MiB. The expected digest's authenticity,
selected installation/enrollment and provenance of the old Controller copy must
come from a future trusted preparation coordinator; this adapter cannot prove
them from a path/hash supplied by a caller. It does not use a downloaded image
or PATH implicitly.

The immediate job directory and image must both be owned by the current token's
user SID. `GetSecurityInfo` reads their owner and DACL through the retained,
non-reparse handles. A null DACL, invalid ACL, unsupported ACE type or any allow
ACE granting mutation rights to a SID other than that user or SYSTEM is refused.
Mutation includes generic write/all, file write/append/EA/attributes, child
deletion, deletion and owner/DACL changes. The conservative check also rejects
foreign inherit-only grants and grants denied elsewhere; it does not approximate
Windows effective-access evaluation. The directory must contain exactly the
pinned image, with no extra files or subdirectories. These checks run again
before launch and connection proof. They do not isolate the current account or
SYSTEM: either can change its own ACL/content between checks. The adapter never
repairs permissions on a supplied path.

The future preparation coordinator must create the job directory atomically with
`CreateDirectoryW` and explicit `SECURITY_ATTRIBUTES`: owner is the current token
user SID, with a protected DACL granting only that SID and SYSTEM. Creating it
first and repairing security later, as the disposable test fixture does, is not
a production preparation strategy. Set the owner explicitly even for elevated
tokens, whose default owner may be the Administrators group. Default inherited
ACLs under `%LOCALAPPDATA%` may grant other groups write access and will correctly
fail validation. Choose a location where this exact private descriptor can be
installed; do not loosen validation to accommodate inherited permissions.

The supervisor must not write scratch files into its image/working directory:
the one-image content invariant remains active at later verification points.
Any future scratch storage needs a separate job-private directory with the same
ownership and DACL validation and an explicitly reviewed content policy.

Native fixtures create a separate current-user-owned directory with protected
current-user/SYSTEM DACLs and one copied helper image. Cases reject world-write
grants on the directory or image and an extra DLL entry, including a change after
pinning. Fixture cleanup names only its two exact files and empty directory;
there is no recursive deletion. Native results are still required for these
checks; cross compilation is not execution evidence.
Additional caller-slice negative coverage remains required for Administrators
and Authenticated Users write grants, conditional/callback ACEs, a null DACL,
foreign directory ownership and a deny-then-allow grant (which must still refuse).

The owner endpoint generates a 128-bit selector using the Windows system CSPRNG
and derives one fixed private pipe name from its 32-character lowercase hex
encoding. Fixed-selector binding is private for adversarial fixtures. The selector
is visible in the child's command line and is not a secret or authorization token.
Existing pipe names fail (FIRST_PIPE_INSTANCE);
the existing native pipe adapter supplies the current-SID/SYSTEM-only DACL,
rejects remote clients and never impersonates them. The endpoint consumes
itself on one launch/connection attempt, including refusal or cancellation.
A same-account process that learns the selector can connect first and force an
abort. This is an accepted availability limit, not account isolation; never add
reconnect/retry logic that weakens the single-connection identity check.
The fixed production child argv is `--self-update-supervisor SELECTOR`; there
is **no such Controller entry point yet**. CreateProcess receives a fixed
absolute image, its pinned parent as cwd, an empty Unicode environment,
`bInheritHandles=false`, and CREATE_SUSPENDED/CREATE_NO_WINDOW. No breakaway,
shell, WMI/CIM, inherited observer handle or temp-file handle publication exists
in the adapter.

Before resume, and again on connection, the owner checks the actual retained
process handle's creation time, liveness, image volume/file identity and digest.
Bounded current-Job membership enumeration proves that the child is in the
caller's Job; KILL_ON_JOB_CLOSE is required and both breakaway flags refused.
This is **not proof that the caller is the Controller daemon rather than an
Agent PTY process**. Only the future daemon-owned coordinator may call it;
no CLI/request-handler call is wired. A five-second deadline is not renewed
after launch. Image opening, launch-time pin checks and post-connect image
verification use Tokio `block_in_place` on an explicitly required multithread
runtime; a single-thread runtime is refused before work starts. Other tasks and
timers can progress on a replacement worker. Native filesystem/hash calls cannot
be preempted, but expiry is rechecked before returning any connection proof.
Ownership stays on the calling task throughout the blocking section: cancellation
is observed at the next yield, with no detached worker that could spawn a child
after cancellation or outlive its cleanup guard. A bad client
PID, timeout or cancellation terminates/waits the actual owned child handle.
Drop cleanup is best effort, not a receipt; explicit abort confirms cleanup
or returns a fixed failure. No PID is reopened for cleanup.

The connection proves only that the owner's pipe client is its exact launched
image/process. It has no raw-stream, permit, ACK or dispatch accessor. Child-side
server/OS-parent attestation, mutual job/receipt binding, supervisor-owned locks
and synced once-only decision are still required before a complete handoff can
exist. These are the next reviewed slice, not implied by this type's name.

Windows native unit tests cover pipe-name squatting and image-digest conflict.
They also assert the exact UTF-16 production command-line bytes independently
of the minimal libtest helper argv.
The ignored `windows_update_bootstrap::tests::native_owner_round_trip` additionally
exercises verified connection/abort, a same-SID wrong-PID pipe client, and
cancellation while the actual owned child remains alive; retained duplicate
observation handles must signal after refusal/cancellation. It creates its own
root Job and must start outside ambient Jobs on a disposable worker, never
win_native. Test-only child argv invokes a libtest helper via `--skip SELECTOR`
with the same empty environment; that helper connects or waits, spawns no work,
and exits within ten seconds. The test root Job is intentionally held until
the isolated harness process exits. This models no production mutual handoff.

## Increment 2b1: inert child-side parent attestation

Security3281 accepts the exact `a2196422` hosted results for development-only
increment 2a. Leader3283 permits further inert, test-only increments on security
acceptance plus hosted native evidence. Authority-bearing callers, activation,
installer authority, a first live-machine run and deployment remain separate
decision points. No production caller is exposed by this increment.

The private child adapter queries `GetNamedPipeServerProcessId` and compares it
with the current process's OS parent from one process-only ToolHelp snapshot.
Enumeration is bounded to 65,536 entries and refuses failures without retry.
It opens that OS-derived parent PID exactly once with query/synchronize rights,
never a PID received in a frame. The opened parent's creation time must precede
the child's creation time: a reused PID after the real parent's exit cannot pass.
The parent handle and its non-reparse, read-pinned executable file remain owned
by the proof. There is no PID reopen, process termination or impersonation on
the child side.

The parent's **installed** image handle permits `FILE_SHARE_READ |
FILE_SHARE_DELETE`, while continuing to deny write sharing. It must not block
the later publication rename of the installed Controller. The immutable job-copy
handle still denies delete sharing. A native ordinary fixture renames a separate
disposable image while the parent-image handle remains open, checks the retained
file identity and digest through the renamed path, and verifies that writing to
the renamed file still fails. This is file-sharing evidence, not a real running
Controller publication test. A removed/replaced lookup path during later
registration re-verification still refuses; this primitive does not reconstruct
a session after publication or authorize retries.

Registration comparison requires exact equality of parent PID, creation time,
image volume/file identity and SHA256. It then rechecks the retained process's
liveness and image identity/digest, including liveness after hashing. This proves
only that registration fields match the kernel-attested parent at the check;
it is not an authenticated transaction, mutual receipt or execution permit.
ToolHelp parentage does not isolate other processes in the current account.
It records the creator-selected parent PID: a creator with the necessary process
rights can set a parent-process attribute. The trusted Controller creation path
and private image directory are therefore essential; parentage alone is not
stronger isolation from the current account.

The native fixture uses one fixed 56-byte registration observation and one-byte
success marker, with absolute five-second deadlines, solely to exercise this
adapter. It retains the proof during the inert child lifetime. Five rejection
cases independently alter PID, creation time, volume, file identity and digest;
each must disconnect without a success marker, after which the owner confirms
cleanup using its original process handle. An ordinary test rejects a pipe
server in the child's own process instead of its OS parent. These tests need
new hosted native execution; cross compilation alone is not runtime evidence.

This fixture transport is not a production protocol. The next sub-increment
still needs attempt/intent/registration digest binding, fresh one-shot challenge,
bounded typed frames and rejection of unknown/duplicate/out-of-order frames.
The production registration must match the exact durable registration record
and its digest, not just an owner's report over the pipe. Parent liveness and
identity must be verified again at permit acceptance and immediately before ACK;
the state-machine fixtures must test both points.
Supervisor-owned locks, durable once-only permit/decision and installer Jobs
remain subsequent reviewed work. The additional directory negatives, atomic
production preparation and daemon-only construction gate in security3281 remain
required before exposing any caller.

## Authority contract for the subsequent implementation review

- Owner/supervisor communication uses a local private named pipe, restrictive
  current-SID/SYSTEM DACL, remote clients refused and FILE_FLAG_FIRST_PIPE_INSTANCE.
  Verify kernel client/server PIDs against retained process handles and creation
  times, plus image file identity/hash. A pipe name or message PID is no proof.
  Keep bInheritHandles false; do not add anonymous inherited handles casually.
- The owner retains CreateProcess's process handle for lifetime identity. The
  supervisor starts inert inside the stable Host Job, outside every Agent PTY
  Job, before any transaction
  mutation. It acquires installation then enrollment locks itself before staging;
  the owner must not claim that duplicating a mutex transfers thread ownership.
  Moving Windows lock acquisition into the supervisor requires review before
  implementation; it is not an exception to existing Unix handoff rules.
- After verified permit and synced/read-back decision, spawn installer work
  suspended, assign it to a supervisor-owned Job with KILL_ON_JOB_CLOSE and no
  breakaway, then resume. Do not inherit that Job's handle into descendants.
  Confirm zero live Job processes plus closed writers before claiming noexec or
  quiescence; closing a handle or receiving one process exit is insufficient.
- Preserve the once-only permit, bound durable decision before ACK/dispatch,
  absolute deadlines, evidence on uncertainty and no gate reconstruction/resend.
  Native checked image rename, sharing-denial handling, DACLs, lock lifetime and
  restart/rollback need separate small diffs and real NTFS/Job tests. The probe
  grants none of these authorities and never supplies a reusable gate token.

No public Windows self-update caller, updater removal, signed-channel cutover or
Host restart is approved by this note. Breakaway refusal keeps the fixed
`activation_requires_host_terminal` pre-mutation outcome in the intended updater.
