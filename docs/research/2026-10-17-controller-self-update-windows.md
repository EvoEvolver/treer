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
installs the frozen helper and records its SHA256 in one explicitly approved
Treer-owned temporary directory. The helper itself writes no files. The runner
records the actual Host binary version separately before running it through the
real exec-origin Job; a terminal run cannot establish that Job's breakaway policy.

The parent queries its own Job membership, resolves its own image and starts that
exact absolute application with CreateProcessW, fixed child argv, no inherited
handles and no inherited workload environment. Request only
CREATE_BREAKAWAY_FROM_JOB | DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP; no
CREATE_NEW_CONSOLE, shell launch or Job-policy changes. The flags are a request,
not proof. The child only waits for ten seconds and exits; it spawns nothing,
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
