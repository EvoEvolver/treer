# Claude native goal authority

## Read-only reproduction

Leader's loopback UI was inspected with GET `/v1/state`, process start metadata,
its goal usage checkpoint and only goal command metadata from the specified
session JSONL. No prompt, clear, restart or credential operation was performed.
The listener started at 2026-10-13 21:40:50Z, before `0fff26e` (22:04Z),
`89968a8` (22:14Z) and `ac914fe` (22:22Z); those fixes were not loaded by it.
UI memory and its checkpoint both contained a non-null `status: cleared` goal.
The latest native goal commands (Claude 2.1.296) instead reported `No goal set`,
including 2026-10-14 01:28:09Z. Objective and transcript bodies are omitted here.

## Cause and convergence

The adapter retained cleared lifecycle records, and the UI renders every
non-null goal. Native absence was recognized only for `args: clear`, and resume
could overwrite native history with a cached goal or pending exit. JSONL polling
stopped for terminal goals, so a later external clear could also be missed.

Native `/goal` local-command output is authoritative: `Goal cleared` or
`No goal set` removes the goal and deferred edit, writes a null checkpoint and
updates UI/state. Ordinary assistant prose has no such authority. UI DELETE
succeeds on native absence. JSONL polling continues after completion; resume
reconstructs existence/objective/status from native history and never replays
a mismatched cache. Verified same-goal checkpoints still supply budgets/usage.
Formal exit reasons remain durable notifications, separate from native goal
existence; a confirmed clear cannot resurrect a goal for display.

## Verification and activation

Hermetic fake-Claude tests cover UI DELETE receiving `No goal set`, bare `/goal`
absence, residual active/cleared/blocked caches after native clear, mismatched
cached objectives, external JSONL clears after completion, and ordinary assistant
prose isolation. Existing deferred-edit races, budget/loop protection, formal
exit notifications and crash recovery remain covered. No paid model was called.

Existing sessions need a user-approved Agent-only restart into the updated
launcher, preserving their saved session ID and startup command. Native JSONL
then removes stale cached goals on resume. No Agents are restarted by this fix.
