# Message batching focused checks

Base: `b40e68d4df946ed378ad66a9291decc582331a24`.
Isolated worktree: `/Users/dooms/treer-rust-message-batching-158`.
All provider/CLI entry checks use synthetic local fixtures and owned ephemeral
processes; no running managed Agent, private provider file or live journal read.

Author results:

- `cargo test --features fixture-harness --offline runtime::`: **10 passed**
  (8 message admission/recovery tests and 2 existing journal/import tests).
- `cargo clippy --features fixture-harness --offline --all-targets -- -D warnings`:
  **EXIT 0**.
- `node --test --test-name-pattern='Rust message batching' tests/contract.mjs`:
  **5 passed, 0 failed**, 20.7 seconds. Uses this worktree's fake Codex producer,
  the existing installed Node `ws` dependency, and the newly built fixture binary.
- `git diff --check`: **EXIT 0**.

Entry coverage: three same-poll messages produce one provider turn; four
messages across busy polls retain one queued receipt and its original retry
fingerprint; an explicit queued prompt is a barrier; same message/new delivery
adds a checkpoint without dispatch; remote ACK success followed by a lost local
checkpoint retries the same ACK operation after restart without replaying the
uncertain turn; failed journal publication neither dispatches nor ACKs. The CLI
fixture independently checks that every ACK already has a durable checkpoint.
Ten 210,000-byte bodies (over 2 MB total) admit as ten bounded previews and retain
all ten IDs while another explicit turn is active.

Unit coverage additionally checks 120-message busy coalescing and journal reload,
256-checkpoint split, Unicode/quoted control framing, whole-poll identity conflict
refusal, queue pressure, exact legacy receipt reconciliation and ambiguous legacy
refusal. Removing additive batch metadata leaves the old reader's record shape
valid; this is not a claim that arbitrary state rollback is safe.

Failed development checks were corrected, not counted as passes: one unit test
omitted required synthetic journal envelope fields; the first entry run resolved
an older fake provider via the external Node dependency root, causing completion
and resume deadlines; the first large-output fixture used `process.exit` before
stdout drained. The final fixture explicitly binds this base's producer and
awaits stdout completion. Clippy also caught two style issues, corrected before
the final run. No production predicate was relaxed to satisfy these checks.

No unchanged complete gate, live acceptance, lifecycle operation, provider
upgrade, or managed runtime update was performed. Native artifact pinning and
leader-owned target update are separate operational steps.
