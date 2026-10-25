# Claude native goals, first batch (#247)

Base: fresh origin/main10d5d495. Worktree:
`/Users/dooms/treer-launcher-claude-goals-247`.

## Delivered boundary

Explicit native initialize command discovery; idle set/replace/clear via the
existing durable FIFO journal; exact local-command acknowledgement separate
from normal turn receipt completion; exact-condition native completion. Shared
UI uses `pause:false,budget:false`. A desired objective is desynced until native
ACK, not an accepted provider mutation. An ACK pumped during input write is only
a candidate until the successful local dispatch publishes its exact turn ID;
failure/uncertainty never gains goal authority. A bounded pending completion
stays attached to that ACK candidate when received during the same write wait.

Unknown command outcome remains goal-specific and does not poison normal prompt
transport or cause input replay. Further goal changes/reconcile/retry explicitly
refuse that unresolved outcome; native query/disposition recovery is not part
of this first batch. Busy mutation/clear, pause, budget and agent-exit requests
refuse before sending; no implicit interrupt. Restart retains local desired
state with explicit desync, never claims that saved state is current native
state and never reapplies a goal. Session transfer and extra settings remain
separate. See CLAUDE.md for the operator-visible limits.

## Final author checks

All fixtures use own temporary sessions, synthetic native stream-json producer,
loopback AIS and private state. No current Agent, private provider history,
foreign workspace or credentials are inspected. The fixture image is not a
shipping artifact.

- Focused Claude actual entry:6 PASS. Covers explicit discovery/unavailable,
  invalid budgets/pause/objective refusal, slow ACK vs desired UI state,
  idle clear, busy clear without interrupt, FIFO/exactly-once ordinary prompt,
  duplicate operation, missing ACK, exact attachment and Stop-hook completion
  vs unrelated condition, acknowledged-goal restart desync, and restart during
  pending command retaining the uncertain receipt/fence without replay.
- One entry uses the real30-second goal deadline, then a native ACK at31 seconds:
  goal stays unknown/desynced, queued successor runs once after normal result,
  no provider termination/resend. No deadline overrides.
- Shared GoalEditor browser:1 PASS at1280 and390. Real AIS, managed fake
  Controller provenance, set/save/clear; no budget or pause controls and no
  horizontal overflow. Existing public assets are unchanged.
- Native goal envelope/completion units:2 PASS. Wrong args/model, sidechain,
  missing discovery, Stop-hook output/errors/impossible result do not acquire
  completion authority.
- Codex goal focused actual entries:2 PASS: existing budget/pause/context delivery
  and restart dedup; uncertain native mutation reconciles without repeat.
- Strict all-targets Clippy: default and fixture Mac PASS; Windows GNU fixture
  cross-check PASS (compile-only, no native Windows execution claim).
- New module rustfmt, JS syntax and diff checks PASS.

The final seven-entry run (six native controls plus one browser control) used
the rebuilt final Rust tree. Existing full Claude/Codex/240/241 matrices were not
repeated. No just check, production build, installation or live lifecycle.

## Corrected initial checks

Initial WIP reused `sent` before native ACK and did not set `applying`, which the
existing goal journal correctly rejected on restart. Fixed by retaining its
existing applying -> applied -> sent invariant; only ACK completes the edit.
An initial test sent DELETE JSON without explicit HTTP Content-Length and had
an intermittent empty400 on the following request; the goal test client now
frames every body explicitly. Final focused run7/7 PASS. Initial strict Clippy
also rejected a nested else/if; corrected before final checks. These initial
failures are not production incident or provider compatibility claims.

## Delivery

No UI/public/installer/Cargo/build pin changes. All13 public assets remain the
92198 closure. Requires a new ordinary native production image owned by Release;
never ship the debug fixture. One Core narrow source review follows this freeze;
leader owns main push. No prior tasks/images/steering handoff is repeated.
