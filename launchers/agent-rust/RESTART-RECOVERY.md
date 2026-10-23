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

A missing ID, missing provider turn, or unknown turn status remains uncertain and
keeps dispatch paused. Idle alone, matching text, or the absence of a turn does
not establish completion. History/trust validation and private publication checks
remain mandatory. This change does not implement arbitrary operator resolution
of genuinely ambiguous unacknowledged dispatches.

Focused validation: `cargo test --features fixture-harness runtime::recovery`,
and `tests/contract.mjs` selected with `restart recovery reconciles`. The latter
uses the actual Rust HTTP entry, a synthetic provider, a private Controller
fixture, real journal persistence and two launcher processes; it checks that the
accepted input is sent once, the queued input follows it, and deduplicated retries
retain their submission ID. The scenarios include journals from the older
launcher that had already marked an accepted receipt uncertain, including an
explicitly interrupted provider turn followed by queued work.
