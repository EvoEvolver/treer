# Native-read bootstrap and explicit new runtime, issue247

Base: origin/main `320933a5eb42c76032f214c3b28c91ebc8465f25`.
Worktree: `/Users/dooms/treer-launcher-claude-launch-247`.
Contract/remaining limitations: [CLAUDE-TRANSFER.md](CLAUDE-TRANSFER.md).

## Author checks

- New fixture native built in owned cache
  `/Users/dooms/.cache/treer-claude247-launch-target`, existing native provider
  build pins unchanged. Build log `/Users/dooms/.cache/claude247-launch-build.log`.
- `RUST_SPIKE_BIN=.../debug/treer-agent-rust-spike node --test
  tests/session-launch-entry.mjs` from `launchers/agent-rust`: **4/4 PASS,
  0 skip**. Log `/Users/dooms/.cache/claude247-launch-entry.log`.
- Full actual entry: task-owned initialized source runtime creates synthetic
  native JSONL via real input/result protocol; authenticated-provenance snapshot
  -> atomic prepare with fresh UUID/isolated config -> explicit target launch
  -> native file-order display and real private checkpoint -> first target
  prompt exactly once -> ordinary same-target restart, with no replay. Target
  producer verifies `--resume`, new cwd/config, preserved native context and the
  synthetic environment credential. Source native SHA, session and input count
  stay unchanged while target starts, receives input and restarts.
- Bad bundle SHA leaves no published target. Missing auth, nonempty private
  root and changed target native hash refuse before provider launch or endpoint
  publication. Existing private marker is unchanged. No secret is copied to
  the bundle/plan or logged; the auth fixture logs only a boolean match.
- Initialize refusal with a preceding task event verifies no partial private
  checkpoint or port publication. The durable once intent remains; a new private
  root cannot retry that same prepared launch. Provider receives zero user
  inputs; original source remains unchanged. Initialization events are pumped
  without prematurely persisting bootstrap state.
- Actual installed dispatcher from a separate synthetic bound bundle refuses a
  missing Claude binding, then starts the exact prepared target using the native
  image when the binding exists. A task-owned synthetic CLI records only the
  normal AIS registration argv; no actual Treer service/credential is contacted.
  The bound debug image/tool manifest is a fixture, not production provenance.
- Strict macOS default and fixture all-target Clippy `-D warnings`: PASS. Logs
  `/Users/dooms/.cache/claude247-launch-clippy-{default,fixture}.log`.
  Changed standalone Rust module rustfmt, JS syntax and diff checks PASS.

Initial three-entry loop passed; the fourth adds installed dispatcher provider
binding and normal registration. Subsequent source review by the author tightened
port preflight/shutdown and disallowed publishing a ready-but-blocked startup.
Final4/4 evidence includes those changes. No previous goals/images/tasks/transfer
or recovery full matrices were repeated.

## Delivery boundary

New ordinary production native **and dispatcher** are needed; do not ship the
fixture or relabel old native images. Public13/Cargo/build/provider pins are
byte-unchanged from the base. No production build, CI dispatch, install, Agent
creation/migration/restart, Host/Controller operation, real provider operation,
real Claude private history/config or credential-file access occurred.

The implemented loop is deliberately explicit and POSIX only. Historical
envelopes are display-only, not provider terminal/receipt authority. API/env auth
mode is selected, not remotely verified by initialize ACK. Native rich history
rendering, persistent-startup/Agent-creation/shared UI orchestration and Windows
transfer remain absent. `sessionTransfer` remains null. Original sessions/files
and user startups are unchanged; later ordinary startup of the new target uses
the successful checkpoint rather than retrying the once-only launch operation.
