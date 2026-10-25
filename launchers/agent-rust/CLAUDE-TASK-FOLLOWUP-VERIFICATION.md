# Task completion followup regression (#247)

Base: origin/main343bc180. No public assets, installer, tools, default pointer,
Host/Controller or in-use Agent changes. This increment fixes the real c0ebb
acceptance failure recorded in #247/9009, not transfer authentication.

## Distinguishing evidence

One task-owned direct Claude2.1.306 producer invocation, fresh UUID/cwd, normal
default authentication, captured89 stdout frames. No private history/config or
credential was inspected. Frame39 was the original input result. Frames41–43
updated/notified a completed task, with run ID. Frame44 was a new system/init
without user_message_uuids. Frame46 began a stream. Frame89 was another result,
with origin kind=task-notification, producer=session-task, matching runId.
The original adapter's active input was already cleared at frame39; frame46
therefore hit claude_unowned_stream. This is distinct from a task metadata event
or a new input ACK. The direct owned child was terminated and awaited afterward.

The synthetic producer preserves this shape and gates the task result separately
from the waiting user's result. Old fixture image SHA256
3c3f95385108f9454dab9808bfc27245848a596c1a18dfba57550b7b98a19ff7
FAILS the new positive entry (deadline after protocol refusal). Final new image
SHA2568b3d0d49401f44fa6e1d99e1f73f6d4b183ee4ce4e06b97e587a574caad51b80
passes the focused controls below; fixture image is never a shipping artifact.

## Final checks

- Two focused task_followup_tests PASS: unowned task result cannot settle local
  input; buffered output is not idle proof; duplicate run cannot rearm output.
- Four actual native fixture entries PASS,0skip: independently gated task and
  queued-input results (receipt still running after task result), unchanged model,
  exact two input writes, display-only persistence/restart without replay; wrong
  origin refusal; unsolicited-stream refusal; existing task metadata/output/
  clear/FIFO/restart control. Other entry matrices were not repeated.
- Final-source real Claude2.1.306 isolated native runtime PASS: fresh task-owned
  cwd/session/private root, background sleep8/printf, completed exit0, independent
  task followup display, transport open, then another prompt completed exactly
  once. Exactly two user receipts are done; three display turns include one
  projectionOnly native-task-notification entry. No user Agent was created or
  migrated; the owned runtime exited0 after SIGTERM and was awaited. Its shutdown
  reports provider_exit=confirmed, process-group termination requested but
  unverified, detached descendants not guaranteed. Native
  generated session532b2081-7183-435f-a4bf-ed3de9c9a00c is recorded in the evidence
  JSON, rather than inferred from a PID. The real operation used existing owner auth
  normally, without reading/copying credentials.
- Strict feature Clippy all-targets, node syntax and diffcheck PASS.

Evidence root:
`/Users/dooms/.cache/treer247-real-claude-acceptance-20261025`:
TASK-WIRE.json (own producer only), fixed-native-final/{initial.json,
observations.json,task-final.json,task-queue.json,final.json,final-queue.json,
cleanup.json}; real-fixed-task.py is the actual driver. Earlier fixed-native
results preceded the final unowned-result guard and are kept separately.

Build qualification: canonical base's standalone Cargo.lock omits libc from the
current treer-platform dependency list, so initial --locked refused. Focused
build/test/Clippy used an offline local resolution adding only that edge, then
restored the tracked lock unchanged. The initial ordinary test command also
refused unpinned tool discovery before execution; feature-harness commands were
used for all successful checks. An intermediate new unit exposed recursive
origin admission in the buffered mapper; final code validates origin once at
the outer boundary and passes the admitted result to the inner mapper. Final
units/entries and real provider execution all use that correction.

## Scope

One additional bounded transcript is buffered, not an execution queue. Its final
origin validation never supplies receipt or recovery authority. Bad/missing
origin and ambiguous interleaved init fail closed. Unsupported older producer
shapes remain refused rather than guessing chronology. Model/effort/steering/
image/goals matrices, deployment and transfer remain outside this narrow check.
Release must build a new ordinary native image; public13 assets remain unchanged.
