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

## Authority contract for the subsequent implementation review

- Owner/supervisor communication uses a local private named pipe, restrictive
  current-SID/SYSTEM DACL, remote clients refused and FILE_FLAG_FIRST_PIPE_INSTANCE.
  Verify kernel client/server PIDs against retained process handles and creation
  times, plus image file identity/hash. A pipe name or message PID is no proof.
  Keep bInheritHandles false; do not add anonymous inherited handles casually.
- The owner retains CreateProcess's process handle for lifetime identity. The
  supervisor starts inert and outside the Host Job before any transaction
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
