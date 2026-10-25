# Claude effort batch, issue247

Base: origin/main29436433d4bb5db8b7c97fa0e9ca5a39584da57a.
Isolated worktree: `/Users/dooms/treer-launcher-claude-effort-247`.
No managed lifecycle, provider configuration/history/credential reads, production
build, install or input replay. Tests use owned temporary state and protocol
producers. The debug fixture executable is never a shipping artifact.

## Contract choice

Node `src/harness/claude.ts` discovers effort from initialize.models, probes
`apply_flag_settings` with an empty overlay, then commits explicit effort/null
only after the exact control ACK. Its settings tests cover aliases, old setters,
supported model levels, prompt serialization and rejection. This batch implements
that native control boundary, without a model/version table. Session transfer is
different: `session-files.ts` exports allowlisted native JSONL/subagents/memory
and project instructions, validates hashes/metadata, then imports into an empty
directory with a fresh UUID. Rust has no corresponding file bundle/import
boundary, so sessionTransfer remains null. Native resume is not transfer.

Effort selection starts unknown, including after restart. Only explicit setting
ACK establishes effort; empty probe ACK establishes setter support alone. Fifteen
seconds after successful write/flush, no setting ACK means unknown but the queue
stays live. No per-input effort override, automatic retry, interrupt or restart.
Valid combined settings are sequential: successful model ACK is not rolled back
or hidden if effort later refuses. Unsupported combined settings reject before
either control is sent. Independent model/effort publication revisions prevent
queued pre-ACK reports from reverting effort without hiding actual model reports.
The shared UI convention skill influenced the localized unknown/default choices,
visible labelled select, keyboard focus and desktop/mobile verification.

## Focused evidence

Native fixture build uses the existing explicit native Codex/Claude build pins
and private cache `/Users/dooms/.cache/treer-claude247-target`. Entry commands run
from `launchers/agent-rust` with `RUST_SPIKE_BIN` pointing to that cache's
`debug/treer-agent-rust-spike`.

- `node --test --test-name-pattern='Claude effort (?!shared browser)|Claude model catalog|Claude model ACK is not' tests/claude-entry.mjs`:
  **7/7 PASS**. Five new effort entries plus two unchanged model controls.
  Log `/Users/dooms/.cache/claude247-effort-final-entry.log`.
  Covers provider catalog/alias levels and deduplication, probe refusal and real
  five-second missing ACK/late response, unsupported mixed input before mutation,
  successful mixed settings and explicit null reset, restart unknown, slow ACK
  with64-event burst/completion pumping and concurrent prompt exactly once,
  truthful model-success/effort-refusal partial outcome, real15-second mutation
  timeout with following prompt once under actual provider effort and18-second
  late ACK unable to overwrite a newer explicit choice. No interrupt control.
- `CLAUDE_EFFORT_BROWSER=1 node --test --test-name-pattern='Claude effort shared browser' tests/claude-entry.mjs`:
  **1/1 PASS**, real Rust AIS/static assets, four English/Chinese, light/dark,
  1280/390 views. Initial unknown, keyboard focus, pending disabled without
  premature selection, ACK broadcasts, refusal preserves value, explicit Default
  clears only after ACK, no horizontal overflow. Log
  `/Users/dooms/.cache/claude247-effort-browser-final.log`; four screenshots in
  `/Users/dooms/.cache/claude247-effort-shots` (mobile Chinese screenshot inspected).
- `cargo test --locked --offline --features fixture-harness effort_`:
  **3/3 PASS** (two adapter units and real lower-RPC held-writer control).
  Covers candidate-only ACK before cancellation, forgotten response cannot
  commit, settings attempt epoch, unsupported target validation, independent
  queued selection revision, fatal five-second probe write and pending cleanup.
  Log `/Users/dooms/.cache/claude247-effort-unit.log`.
- Codex `node --test --test-name-pattern='browser mutations return full state' tests/contract.mjs`:
  **1/1 PASS**, existing settings/prompt/interrupt/full-state history window.
  Log `/Users/dooms/.cache/claude247-effort-codex.log`.
- Strict default and fixture all-target macOS Clippy, Windows GNU fixture
  all-target cross-Clippy `-D warnings`: PASS. Windows is compile-only evidence.
  Fixture and Windows logs `/Users/dooms/.cache/claude247-effort-clippy.log` and
  `/Users/dooms/.cache/claude247-effort-windows-clippy.log`.
- UI build, typecheck and check-ui: PASS,13 public files/2170014 bytes.
  Existing local chunk/worker layout remains; changed shared UI generates a new
  content-addressed closure. Native must be newly built against this source;
  prior294 goal image/public must not be relabelled. Installer/Cargo/build pins
  unchanged. Documentation check, JS syntax and diff whitespace: PASS.

The initial browser run used an obsolete `.model` trigger from the pre-composer
menu harness and failed to find it. It was corrected to the current localized
Composer actions -> Model and effort path; original log retained at
`/Users/dooms/.cache/claude247-effort-browser.log`. Initial local delimiter/helper
placement compilation mistakes and an incorrect manually typed Codex build path
were corrected before successful final checks. No product relaxation was used.

Not run: existing goal/task/image full entry matrices, whole repository gate,
real authenticated Claude-provider model/effort execution, Windows native entry,
production packaging/build/install, managed session maintenance. Existing
Codex240/241 deadline/recovery code is preserved except additive Claude effort
control routing; no replay/terminal authority is introduced.
