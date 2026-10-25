# Native scheduled invocation regression (#247)

Base:75dcc36df584a7490b30c08c28ae3bf31aa86545. No UI/installer/Cargo changes.

## Root cause evidence

Expert generation14 loaded the ordinary75dcc diagnostic image (native SHA
f64f94270098b91a2b22c60a0f1aa27c8ffd488eb24276a1e275be6f1ac09a82), same
session/cwd, original37 ordered receipt records preserved. At22:02Z its public
state reported claude_unowned_stream, activeInput=false, taskFollowup=false,
taskRuns=0, recovery integrity_failure with no unresolved receipts. Public tool
metadata contained a previously successful CronCreate recurring */40 schedule.
The diagnostic deployment itself is not the fix. Its initial generation13
failed before provider launch with private_state_in_use; confirmed exit and
absence of an expert lease holder preceded one forward start to generation14.
No lock was removed and no receipt/input was cleared, disposed or replayed.

A fresh task-owned real Claude2.1.306 session created exactly one non-recurring
every-minute CronCreate with a harmless text-only prompt. On old75dcc, its initial
user input completed, then the scheduled invocation's first stream caused the
same claude_unowned_stream/activeInput=false/taskRuns=0 fence. The original receipt
remained done. A separate fresh direct producer capture completed both results:
the scheduled invocation has no result origin or input UUID, but is bracketed
by matching command_lifecycle started/completed. This distinguishes it from the
already-supported task-notification origin, and is a real producer counterexample,
not a fabricated mock or a claim to reconstruct every past expert interruption.

Only task-owned synthetic cwd/session/native output was captured. No existing
Claude private history/configuration/credential files were read or copied. Own
provider/runtime processes were terminated after capture; one-shot jobs are not
recurring jobs on a working Agent.

## Focused verification

- Adapter units cover matching-session/init/completion scope, duplicate refusal,
  result UUID/origin refusal and interrupt epoch protection for waiting input B.
- Four actual fixture entries cover completed display-only output while B stays
  running, B completion exactly once, restart without replay, missing command,
  borrowed input UUID and wrong completion ID. Three adjacent task-followup
  controls retain their previous behavior. All seven pass.
- Final-source units6/6, scheduled entries4/4, strict all-target Clippy,
  documentation and diff checks PASS. The final additional pre-write steer
  guard is covered by the invocation unit; no old full feature matrix rerun.
- Real Claude2.1.306 on the changed runtime PASS twice in fresh owned sessions:
  one ordinary CronCreate input, one scheduled output, one subsequent ordinary
  prompt; exactly3 native results (2 distinct input UUIDs),2 done/error-null
  receipts,3 display turns, idle/open with no recovery unresolved. The final
  source run is treer247-own-protocol-tkRqLB; the previous run mgGdJx separately
  predates only the added pre-write steer guard. Both owned runtimes were closed.
- Initial fixture assertion incorrectly counted B's pending display turn; it
  was corrected to assert absence of a scheduled record before command completion.
- Initial changed-source real probe exposed native local `queued` lifecycle
  metadata; it was retained as a failed probe and corrected to be non-authoritative.
  It was not counted as a PASS or used to settle that probe's uncertain receipt.
- Default cargo test without tool pins refused at build.rs as intended; tests
  use fixture-harness with a private Cargo target. No default guard was bypassed
  and that binary is not a shipping artifact.

Author evidence lives in ignored .treer/reviews and task-owned temp directories.
Shipping requires a new ordinary native image; unchanged13 public assets retain
their existing attribution. No other Agent/default/Host/Controller was changed
by these checks. Full browser, goals/images/settings/Codex matrices and PostgreSQL
gate were not rerun for this adapter-only boundary. Console transfer acceptance
and sessionTransfer=null remain separate from this fault.
