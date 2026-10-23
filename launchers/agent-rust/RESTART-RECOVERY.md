# Restart recovery

On startup the launcher resumes the exact configured provider thread and validates
its history and current status before changing any receipt. A running receipt
with a retained provider `turn_id` is reconciled by that exact identity:

- `completed`, `failed`, and `interrupted` settle the original receipt;
- `inProgress` stays running and is observed until its terminal event;
- old failed receipts bearing the launcher's dispatch-uncertainty error are
  reconciled the same way, repairing journals written by earlier launchers.

The same operation/submission identity, immutable input, notification and Core
Message acknowledgement obligations remain intact. Recovery never sends the old
input again. Once reconciliation commits, previously queued work continues in its
original order. A previously completed ordinary receipt never reopens.

A turn outside the recent window is looked up by its exact ID through the
existing bounded streaming `thread/read` selector. This retains normal memory,
history authority, thread identity and private-publication checks. At most 256
recovery lookups are admitted per startup. Events remain serviced during reads;
receipt lookup uses immutable submission/turn IDs rather than mutable positions.

A missing ID, missing provider turn, or unknown turn status remains uncertain.
Idle alone, matching text, or the absence of a turn never establishes completion.
History/trust validation and private publication checks remain mandatory.

## Recovery while running

Codex's one-pending read-only observer runs even when dispatch is fenced. Every
two seconds it selects one acknowledged unresolved turn by exact ID, rotating
through the bounded pending set, including turns outside the recent window.
Explicit fresh terminal evidence settles only matching receipts; fresh
`inProgress` means acknowledged-running, never resend. Ordinary historical failed
or interrupted receipts do not fence new work. Local-only display turns are
marked `projectionOnly`; their apparent terminal status is not provider authority.
Real provider terminal identity/content conflict checks remain enforced.

Responses are bound to the provider incarnation and dispatch/event generation.
A read begun before another dispatch/completion/disposition cannot supply a new
idle proof or change the new operation. Failed journal publication restores the
last committed receipts and keeps dispatch fenced before any successor starts.

For EOF or RPC/stdin transport timeouts/errors, the launcher confirms its owned
old provider child has exited, then attempts at most three new provider
connections, with 2/4/8-second retry delays. Each connection only initializes and
resumes the SAME thread, followed by bounded exact-ID reads. It never calls
`turn/start` or resends an input. Reads have a 12-second deadline, one attempt a
30-second deadline, and event buffering is limited to 64. All retained responses
and events together are admitted against 32 MiB of serialized bytes and 500,000
JSON nodes per attempt; exceeding either bound refuses recovery. Every failed attempt
must confirm its own child exit within five seconds before another attempt;
unproven exit stops retries. Launcher shutdown cancels the attempt and requests
owned-child/group termination; detached descendant cleanup remains unguaranteed.
All normal dispatch still waits for durable reconciliation. State reads and
launcher shutdown remain independent of a slow recovery worker.

Integrity/history admission and storage failures are distinct sticky failures,
not eligible for automatic transport reconnect. Claude saved turns remain display
checkpoints, not fresh recovery authority: automatic reconnect/history settlement
is unavailable for Claude until a supported authoritative query exists.

## Explicit unknown-outcome disposition

The existing authenticated, same-origin UI API exposes `POST /api/recovery`.
This is not an unauthenticated provider endpoint. `runtime.recovery` in the state
response reports the classification, read/attempt counts, unresolved submission
and turn IDs, and whether fresh history is supported.

`{"action":"recheck"}` schedules bounded read-only recovery again (including
resetting an exhausted transport retry budget). It cannot bypass an integrity or
storage refusal, or resend any input.

If an operator explicitly accepts an unknown operation's possible prior effects,
the following disposition can opt past that ONE receipt without replay:

```json
{
  "action": "acknowledge_unknown",
  "thread_id": "exact-current-thread",
  "submission_id": "exact-unresolved-submission",
  "expected_turn_id": null,
  "expected_error": "exact-current-receipt-error",
  "accept_unknown_outcome": true
}
```

Use the actual expected turn ID when one exists. For Codex the actor requires a validated
provider idle read from the current generation within five seconds, no different
active turn, exact current receipt binding, and successful private publication.
Missing fields, stale values, active/unknown provider state and storage/integrity
failure refuse. Repeating the identical disposition is idempotent. Other
unresolved receipts stay fenced; previously queued inputs keep their order.

For Claude, explicit disposition instead requires a ready, initialized current
adapter, an open transport, no current input request and no pending interrupt.
That current-process fact is queried under the adapter lock at disposition time;
it is not derived from cached turns, nor evidence about the old operation's
effects. A live active adapter refuses. After a launcher restart an operator can
explicitly dispose the old unknown receipt, retaining its outcome as unknown;
the exact old cached active slot becomes a locally interrupted display entry
with disposition metadata, so it cannot block all future input. No historical
input is replayed. `idleEvidence` distinguishes this evidence from a fresh Codex
idle read. Claude's unsupported automatic historical reconciliation stays honest.

The receipt becomes locally `interrupted` with an explicit **user disposition**
error, not a provider-confirmed interruption. Its durable `recovery_disposition`
retains original state/error, action/time, `providerOutcome:"unknown"` and
`replayed:false`; any subsequently emitted completion notification includes this
metadata (an already-sent notification is not resent). This does not
assert that the operation had no effects. Original operation/input/receipt IDs
remain unchanged and duplicate submissions still return their original receipt.

Focused validation: `cargo test --features fixture-harness runtime::recovery`,
and `tests/contract.mjs` selected with `restart recovery reconciles|runtime recovery`. The latter
uses the actual Rust HTTP entry, a synthetic provider, a private Controller
fixture, real journal persistence and two launcher processes; it checks that the
accepted input is sent once, the queued input follows it, and deduplicated retries
retain their submission ID.
