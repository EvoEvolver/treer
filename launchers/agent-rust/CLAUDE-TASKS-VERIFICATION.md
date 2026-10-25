# Claude background-task batch (#247)

Base: origin/main47f9606f (includes model243, recovery240/241, steering27dba,
images886276 and browser248 assertion fix). Isolated worktree
`/Users/dooms/treer-launcher-claude-tasks-247`.

## Scope and authority

Native task system events update bounded auxiliary metadata, independently of
turns/receipts. Existing shared TasksPanel is reused unchanged. POSIX output is
a registered-task lookup followed by pinned, component-wise no-follow file
opening and a bounded same-descriptor tail. Windows output explicitly refuses.
Clear finished removes records only. Running task observations are volatile;
terminal records retain the existing validated checkpoint schema. No task
execution/replay, input resend, interrupt, provider restart or new turn authority
is introduced. Goals, session transfer and extra settings remain separate.

No UI source/public assets, installer, Cargo/build pins, Core or CI files change.
All13 public assets remain the existing92198 closure. Shipping requires a new
ordinary production native image; the debug fixture binary must never ship.

## Actual entry evidence

Own synthetic stream-json producer and temporary private state only; no native
Claude home/history/config/credentials or foreign workspace data.

- Full Claude suite:33 tests,29 PASS/4 opt-in browser SKIP. Includes model alias,
  refusal/slow ACK/timeout provenance, image admission/steer/FIFO, uncertainty,
  interrupt, imported21 terminal records and restart/no replay.
- New task entry: task_started, task_updated patch, task list and notification;
  turn finishes while task remains running; a later idle task notification
  completes only that record without input/turn creation. Three prompt inputs
  remain FIFO/exactly once. Restart restores same session and terminal records
  only, never task work/input replay. Clear leaves all running records and files.
- Output returns the requested tail up to64KiB; outside, parent traversal,
  symlink, hardlink, unknown ID, invalid tail and foreign origin refuse.
- Actual TasksPanel browser:1 PASS,1280 and390; managed fake Controller
  provenance, native task rows, keyboard expansion, output and close, no overflow.
- Codex focused contract:3 PASS (identical steer, private images/restart,
  200-event backpressure/early completion).

Full-suite evidence predates the final narrow short-route overlap guard and
UTF-16 project-name alignment; final task-only entry/browser checks run against
the rebuilt final source. No unchanged full-suite matrix was repeated.

## Focused source checks

- Task metadata units2 PASS: status/patch/exit-code mapping, terminal schema,
  running retention,100-record atomic refusal and20-finished retention.
- Confined output unit1 PASS: bounded tail, symlink/parent/outside refusal,
  exact-session native temp path, pinned cwd surviving pathname replacement;
  final increment additionally verifies FIFO refusal without blocking.
- Existing Claude units12 PASS, plus RPC writer/steer race tests2 PASS with
  explicit TREER_TEST_NODE. First invocation omitted that required fixture
  variable:12 passed/2 setup failures; only those two were rerun successfully.
- Strict all-target Clippy: Mac default and fixture, WindowsGNU fixture PASS.
  Cross-check is compilation/lint evidence, not actual Windows entry validation.
- JavaScript syntax, new-module rustfmt and git diff checks PASS.

Setup failures retained: initial task FIFO test omitted explicit queue mode and
second input became a legitimate steer; corrected test to request queue. Initial
unmanaged browser lacked interface provenance and found no task button; corrected
to the same managed fake Controller/header setup as existing UI tests. No product
guard, queue policy or production timeout was relaxed for either setup issue.
The final short-route negative initially received the generic SPA fallback200;
the task route now rejects unsupported task paths explicitly rather than slicing
overlapping prefixes or returning an HTML success page.

No production build, live install, Agent/Host/Controller lifecycle or real Claude
provider validation was performed by this implementation batch.
