# Claude steering verification (#247)

Base: `92198d88cc3a059a956500ca296e7df9dc259bb9`. Only the Claude
adapter, its RPC bridge, capability declaration, focused tests and docs change.
The shared UI, all thirteen public assets, Cargo/build inputs and Codex product
code are unchanged. A new production native image is required; the tested
fixture-feature binary must never ship.

## Final checks

- `cargo test --locked --features fixture-harness claude`: 12 passed, including
  an actual Node producer with the stdin writer held while the old turn result
  arrives. The old terminal excludes the pending steer; successful later write
  returns unknown, without another turn or retry.
- `node --test tests/claude-entry.mjs`: 27 passed, one existing opt-in model
  browser test skipped, zero failures. Working steering, two equal-text inputs,
  queued FIFO, idle steering, concurrent ordinary prompts, restart/no replay,
  model ACK/unknown, interruption and import refusal boundaries pass.
- The separately added `CLAUDE_STEER_BROWSER=1` / `Claude shared UI` test:
  1 passed, exercising both 1280px and 390px against actual Rust HTTP/static
  assets and the protocol producer. Each button submission receives
  `disposition:steered`; no horizontal overflow, lost draft or duplicate input.
- Focused Codex `contract.mjs` selection: 6 passed (identical-text steering,
  optional model timeout/late/burst/refusal, and backpressure/early completion).
- Strict all-target Clippy, both default and fixture features: passed.
  JavaScript syntax and `git diff --check`: passed.

The binary used was the task-owned
`/Users/dooms/.cache/treer-claude247-target/debug/treer-agent-rust-spike`.
Build inputs were the existing native Codex darwin-arm64 executable and
`/Users/dooms/.local/share/claude/versions/2.1.306`; synthetic producers override
them only under the fixture feature. RPC tests explicitly used
`/Users/dooms/.local/opt/node-v22.23.3-darwin-arm64/bin/node`.
The browser test resolved existing Playwright from the separately owned
`treer-launcher-ci92198-fix/launchers/agent` dependency tree. All processes and
private state were newly created fixtures and cleaned up; no working Agent,
Host, Controller, provider history or credentials were accessed.

## Distinguishing evidence and limits

The pre-change binary fails the new working-steer capability/admission test;
idle steering already passed. Core found an intermediate false-settlement race
when retaining the steer before write completion. The final implementation
defers retention, rechecks the exact active turn and conservatively preserves
an unknown receipt when result wins. This is local write acceptance, matching
Node's contract, not a provider consumption ACK per UUID.

Two setup/assertion failures were corrected without weakening product checks:
the first RPC unit invocation named an absent Node path (10 passed, two setup
failures); a first full entry run retained the obsolete `steer:false` assertion
(26 passed, one failure, one skipped). The corrected final invocations above
pass. The opt-in UI test was then added and run independently; its addition does
not change the already tested runtime bytes.

Remaining parity batches are listed in `CLAUDE.md`. This increment does not
claim image, live background-task, goal, session-transfer or effort support,
nor installed-provider acceptance or an online rollout.
