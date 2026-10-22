# Receipt successor author evidence (#158)

Base: accepted batching `748062502878567230429802565e1e421d0ba960`.
Isolated worktree: `/Users/dooms/treer-rust-receipt-pressure-158`.
Build outputs use the author's private `.cargo-target` in the batching worktree;
the sealed batching native candidate is a separate immutable copy.

Checks on this source increment:

- `cargo test --manifest-path launchers/agent-rust/Cargo.toml --features fixture-harness --offline runtime::`: EXIT 0, **14/14**. Includes four new pruning/pressure unit tests and the accepted batching/recovery tests.
- `cargo build --manifest-path launchers/agent-rust/Cargo.toml --features fixture-harness --offline`: EXIT 0.
- `NODE_LAUNCHER_ROOT=/Users/dooms/treer/launchers/agent RUST_SPIKE_BIN=/Users/dooms/treer-rust-message-batching-158/.cargo-target/debug/treer-agent-rust-spike node --test --test-name-pattern='Rust (receipt|explicit prompt limit|message batching)' tests/contract.mjs`: EXIT 0, **9/9**, 38.6 seconds. Four new receipt/prompt entry tests plus five batching entry tests. Fake provider is explicitly bound to this worktree; local Controller, journal and children are disposable fixtures.
- Strict fixture all-target Clippy (`cargo clippy --manifest-path launchers/agent-rust/Cargo.toml --features fixture-harness --offline --all-targets -- -D warnings`): EXIT 0.
- `git diff --check`: EXIT 0.

Development failures preserved in this account: the first entry run was 6/9.
Two assertions incorrectly expected internal error strings despite the existing
content-free HTTP `operation_refused` envelope. The third reused the generic
completion injection, which matches the first old completed receipt during
startup. Assertions now check the actual public error code and no admission /
dispatch; the new fixture-only `completion-prune` phase targets the synthetic
new operation after window eviction. No production error disclosure or startup
semantics were changed to satisfy these tests. Initial strict Clippy also
flagged a test-only range loop; it now enumerates the retained rows.

Not run: unchanged whole gate, native-provider live turn, lifecycle operations,
new migration, browser check, new native release artifact. No current startup,
journal, fence, shared runtime pointer or previously frozen artifact was edited.
This is source evidence for a narrow Core delta review, not rollout acceptance.
