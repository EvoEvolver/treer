# Authentication/retry author evidence (#158)

Separate successor to `87ae84cc01dc7532e9e4fcd0835210e90474baeb` in isolated
`/Users/dooms/treer-rust-message-retry-158`; accepted batching artifact untouched.

- Fixture runtime unit tests: **16/16 PASS**, including two new fixed-category
  privacy and exponential jitter/cap tests, plus receipt/batching/recovery units.
- Fixture build: EXIT 0.
- Initial combined actual-entry run: **12/12 PASS**, 49.8 seconds: four receipt/
  prompt tests, three authentication/retry tests and five batching tests.
- Final targeted entry run after making oversized-output cancellation immediate:
  **4/4 PASS**, 18.6 seconds. Covers receive auth stopping/no repeat/no diagnostic
  disclosure; auth ACK with 256 KiB stderr retaining two pending identities and
  no turn replay after fixture restart; two transient failures with 1–1.2s then
  2–2.4s delay and counter reset after successful ACK; and 17 MiB stdout with an
  intentionally lingering CLI, killed at the response limit without waiting for
  stderr EOF or the 40-second deadline. No test uses live credentials or state.
- Strict fixture all-target Clippy: EXIT 0.
- `git diff --check`: EXIT 0.

Commands use `TREER_SPIKE_BUILD_CODEX` bound to the accepted native provider path
and the author's private Cargo target in the batching worktree. Unit invocation:
`cargo test --manifest-path launchers/agent-rust/Cargo.toml --features fixture-harness --offline runtime::`.
Strict lint uses the same manifest/feature/offline options with
`cargo clippy --all-targets -- -D warnings`.
Entry runs from `launchers/agent-rust` set `NODE_LAUNCHER_ROOT` to
`/Users/dooms/treer/launchers/agent`, `RUST_SPIKE_BIN` to the author's fixture
binary, and invoke `node --test --test-name-pattern='Rust message retry' tests/contract.mjs`.
The initial combined pattern also included message batching, receipt and explicit
prompt limit. All fixture providers are explicitly bound to this source tree.

No unchanged whole gate, native release artifact, live failure injection,
lifecycle, migration, browser or running-agent acceptance. Passing source tests
do not turn a previously frozen batching runtime into this successor.
