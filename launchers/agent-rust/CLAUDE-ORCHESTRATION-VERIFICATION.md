# Claude source-to-target orchestration (#247)

Native author boundary, based on main `3e8f5237`: a read-only `session inspect`
and hash-bound `session prepare` reuse the existing bounded parser, allowlist,
native JSONL rewriting and atomic publication. No new server filesystem API,
credential transport, provider mutation or automatic retry is introduced.

## Author native checks

Executed on macOS arm64 with the task-owned target
`/Users/dooms/.cache/treer-claude247-managed-target`, existing compile-time Codex
and Claude pins, and only task-owned synthetic native/config/private files:

```sh
cargo build --locked --offline --features fixture-harness \
  --bin treer-agent-rust-spike
RUST_SPIKE_BIN=/Users/dooms/.cache/treer-claude247-managed-target/debug/treer-agent-rust-spike \
  node --test --test-name-pattern='actual entry: inspect|orchestrated snapshot' \
  tests/session-transfer-entry.mjs tests/session-launch-entry.mjs
cargo clippy --locked --offline --all-targets --features fixture-harness -- -D warnings
cargo clippy --locked --offline --all-targets -- -D warnings
```

Final changed-source actual entry: **3 passed, zero skipped**. Both strict
Clippy variants, changed-module rustfmt and diff check passed. An initial build
invocation used the nonexistent `fixture-provider` feature and refused before
compilation; the corrected, actually tested feature is `fixture-harness`.

1. Inspection creates no target/stage; byte changes between inspection and
   preparation refuse even if they remain valid JSON. Exact reviewed bytes
   produce the same receipt with phase `prepared`; a duplicate prepare refuses.
2. Inspection rejects traversal, entry hash mismatch, missing native history,
   duplicate options, absent/new-source UUID misuse, nonprivate input file or
   parent, and an already existing empty target without changing it.
3. A fresh real runtime with synthetic provider accepts one source input,
   exports via the authenticated-provenance route, then its actual bytes are
   inspected/prepared and started with persistent `session run`. The new session
   reaches actual health/state readiness and accepts its one explicit subsequent
   prompt exactly once. The original native file hash, source session and source
   input count remain unchanged. No old input is replayed. Target capability
   remains `sessionTransfer:null`.

The third check models the existing upload boundary with a private exclusive
local file. It does **not** run a real Proxy login, normal managed Create,
Controller/Host lifecycle or real Claude authentication. Console tests must
separately cover the authenticated request orchestration, Create reservation
and independent readiness states. No production image was built here, and a
fixture image must never ship. Existing goals/tasks/images matrices were not
repeated; public assets, Cargo/build files and pins are unchanged.
