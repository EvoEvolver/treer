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

## Console composition and its evidence

The combined Web tree is byte-identical to Web owner `ce90ef7b`, following
`d77eda0b` (both based on main `3e8f5237`). The owner used normal synthetic
fixture login and APIs, without a live workspace, native transfer or credential:

- Eight distinct initial orchestration controls passed: desktop/mobile full
  flow, ambiguous upload/prepare/receipt/instance, close-before-late-export and
  Chinese dark mobile keyboard/focus. The seven existing prepared-Create
  controls passed. Setup/reopen/focus failures and their fixes are retained in
  the owner's evidence rather than counted as passed tests.
- The two-file successor adds declared export-size rejection and UUID-shaped
  source selection. Its three focused controls passed: declared oversized
  response stops before upload, and desktop/mobile full ready observation.
  The existing streamed response limit remains. This adds one distinct negative
  control; the repeated positive ready observations are not additional coverage.
- Owner TypeScript/build/contrast64/diff checks passed. Evidence is in
  `/Users/dooms/treer/.treer/reviews/web247-session-copy/CHECKS.json` and
  `SIZE-SUCCESSOR.json`, with JSON reporters and screenshots. The integrating
  author read these receipts, checked the exact Web tree and viewed the 390px
  Chinese dark screenshot; the unchanged browser matrix was not repeated.

After composition the integrating author also ran the final `npm run build`
(TypeScript plus Vite singlefile build) successfully: `web/dist/index.html`
1,289,342 bytes, SHA256
`98a90830b4d1c58d8e8d8ffd5244b4c82ac70e6314c1f299dacd14dbe00acada`.
Final documentation and diff checks passed. Generated HTML is ignored, not
committed; the Launcher public13 closure and Cargo/build/pins remain unchanged.

Source status is checked before/after export and bound to the snapshot's exact
Agent/interface/session. Target upload path/byte count/hash, fixed exec argv and
exit/deadline/truncation fields, native phase/hash/ID/path/count receipt, explicit
prepare confirmation and existing Create request are independently checked.
Readiness requires a fresh normal Agent record and matching interface status,
Claude harness, exact new session, health readiness and idle runtime. A Create
200 alone cannot display ready. Closing a dialog cannot erase the write latch;
ambiguous results retain the planned snapshot path for inspection.

## Remaining acceptance

The only remaining acceptance for this environment-auth workflow is its complete
supported deployment with a real Claude provider and explicitly preconfigured
target authentication. Synthetic browser API control and native entry tests
are separate evidence, not a claim of a real managed transfer. Native and Console
production distributions must match this new code; old images must not be
relabelled. `sessionTransfer` remains null. Per-target secret delivery, atomic
Create-plus-AIS, Windows and rich native historical rendering are explicit scope
limits rather than promised follow-up leaves necessary for this workflow.
