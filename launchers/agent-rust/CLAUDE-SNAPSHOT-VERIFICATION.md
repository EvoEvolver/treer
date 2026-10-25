# Managed idle snapshot leaf, issue247

Base: origin/main `dc67211d04395cb84a3f8e04faae1a73dddd1a3f`.
Isolated worktree: `/Users/dooms/treer-launcher-claude-snapshot-247`.
Contract: [CLAUDE-TRANSFER.md](CLAUDE-TRANSFER.md).

## Author evidence

- New fixture-feature native executable built in the owned cache
  `/Users/dooms/.cache/treer-claude247-snapshot-target`, using the existing
  absolute native Codex/Claude build pins. The fixture executes only the new
  synthetic protocol producer; neither pinned real provider is invoked.
- `RUST_SPIKE_BIN=.../debug/treer-agent-rust-spike node --test
  tests/session-snapshot-entry.mjs` from `launchers/agent-rust`: **4/4 PASS,
  0 skip**. Log `/Users/dooms/.cache/claude247-snapshot-entry.log`.
  Only temporary synthetic native config/session/project files and a synthetic
  Controller quota server are used. All spawned fixtures are stopped and their
  own temporary roots removed by test cleanup.
- Actual route rejects unmarked/wrong-Agent/stale-instance/cross-origin requests,
  wrong method, duplicate JSON keys, extra path/config/cwd/session selectors and
  stale thread. Exact provenance/thread exports a no-store bundle, verifies each
  SHA and consumes it through the real offline importer into a fresh UUID and
  separate directory; the current thread is unchanged and receives no input.
- A fixture-only filesystem gate holds the actor's read window. A duplicate
  export refuses; a concurrent prompt is not sent during the read, is absent
  from the snapshot, then executes exactly once under the original session.
- A 64-event synthetic provider burst exceeds the channel capacity. Observing
  the final task ID before releasing the read gate proves the event pump is
  active. That changed snapshot refuses, the provider stays alive and a following
  prompt completes exactly once. Existing terminal-task retention remains
  bounded to its normal 20 rows; the test does not expect all64 to be retained.
- Busy input, queued input/running task, missing explicit config, unmanaged entry
  and a symlinked native session file refuse without returning a bundle. Source
  filesystem errors are mapped to a fixed error, without leaking private paths.
- Strict macOS default and fixture all-target Clippy `-D warnings`, new-module
  rustfmt, producer/test syntax and diff checks: PASS. Logs
  `/Users/dooms/.cache/claude247-snapshot-clippy-{default,fixture}.log`.

Initial fixture startup failed because managed private-root env was not bound;
the harness now supplies its own root. Initial producer text assumed a scalar
instead of actual content blocks, and its burst assertion assumed64 retained
tasks rather than the existing20-row retention limit. These harness failures
were retained in the turn history and corrected without weakening product
behavior. Final evidence above is attributed only to the corrected run.

## Delivery and remaining work

This is the independently usable snapshot/export leaf, not complete managed
transfer. No native checkpoint, new runtime launch, credential copy, old-session
shutdown or runtime receipt/queue import is implemented. `sessionTransfer`
remains null. New-session checkpoint/bootstrap and launch wiring is the concrete
next leaf; the current offline importer alone cannot satisfy Rust resume's
supported-history checkpoint requirement.

New ordinary production native is required; this debug fixture image must never
ship. All13 public assets, dispatcher/installer, Cargo/build/provider pins and
existing settings/recovery code are unchanged. Windows snapshot/transfer remains
explicitly unsupported. No repeated goals/images/tasks/old transfer matrices,
production build, install, push, real provider/history/credentials access or live
Agent/Host/Controller lifecycle was performed.
