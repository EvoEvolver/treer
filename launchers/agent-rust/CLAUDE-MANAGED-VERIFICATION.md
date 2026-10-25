# Prepared target persistent startup (#247)

Base: canonical `355cfeaea05a9af1fa2e738a184b6517f823054f`.
Author worktree: `/Users/dooms/treer-launcher-claude-managed-247`.
The Web owner supplies its separate prepared-Create UI commit and browser
evidence; the combined source preserves that attribution.

## Boundary

`session run` uses the installed dispatcher/native entry and ordinary Controller
Create/startup persistence. Initial bootstrap is once-only; subsequent identical
argv requires the same Agent, root inode/path and real Claude checkpoint/session.
Missing checkpoint after intent never triggers bootstrap. The resumed runtime
keeps normal recovery controls/fences; it is not subjected to the fresh-import
idle gate. Auth remains explicitly preconfigured environment, never argv/secrets
copied from a source session. No capabilities were enabled.

Normal Core creation may retain a failed/reserved Agent; this is explicitly not
an atomic create-plus-AIS transaction. No AIS registration or partial bootstrap
checkpoint is published on initial provider refusal. Unknown creation is not
retried or undone through delete/recreate.

## Author checks

Private Cargo target: `/Users/dooms/.cache/treer-claude247-managed-target`.
Build pins are the unchanged existing Mac Codex native 0.163.0 path and Claude
`/Users/dooms/.local/share/claude/versions/2.1.306`. Fixtures override the provider
only under the existing fixture feature. No production binary was built here.

```sh
cargo build --manifest-path launchers/agent-rust/Cargo.toml --locked --offline \
  --features fixture-harness --bin treer-agent-rust-spike
RUST_SPIKE_BIN=/Users/dooms/.cache/treer-claude247-managed-target/debug/treer-agent-rust-spike \
  node --test --test-name-pattern=persistent tests/session-launch-entry.mjs
cargo clippy --manifest-path launchers/agent-rust/Cargo.toml --locked --offline \
  --all-targets -- -D warnings
cargo clippy --manifest-path launchers/agent-rust/Cargo.toml --locked --offline \
  --features fixture-harness --all-targets -- -D warnings
```

The entry command runs from `launchers/agent-rust`. Environment includes explicit
`TREER_SPIKE_BUILD_CODEX`, `TREER_SPIKE_BUILD_CLAUDE` and private `CARGO_TARGET_DIR`.
Logs: `/Users/dooms/.cache/claude247-managed-{build,entry,clippy-default,clippy-fixture}.log`.

Five focused actual-entry cases:

1. One prepared target, identical persistent argv over three launches, native
   context grows, one follow-up input only, ordered receipts and intent retained;
   concurrent same-root run refuses through the ordinary lease.
2. Initialization refusal leaves intent but no checkpoint/port, and the next
   invocation starts no additional provider or input.
3. Actual process cwd mismatch refuses before intent; copied private checkpoint,
   wrong session and deleted checkpoint refuse before another provider.
4. The task-owned installed dispatcher/native fixture pair requires Claude,
   ignores a parent's port observation, registers the initialized target and
   resumes with the same argv. An additional refused-initialization target never
   calls even the task-owned registration CLI or publishes a checkpoint.
5. A task-owned persisted uncertain receipt remains fenced on resume with AIS
   reachable, same receipt identity and no replay or launcher-induced shutdown.

Result: 5/5 PASS, 0 skip. Final installed-dispatcher initialization-refusal
increment plus the original once-only launch compatibility control: 2/2 PASS
in `/Users/dooms/.cache/claude247-managed-final-entry.log`. Both strict Clippy
variants PASS. These are author results, not an independent repeat.

Every source/config/native file, private state, synthetic identity, quota server,
provider process and registration CLI belongs to the test. Each case captures
real synthetic provider-created JSONL through managed export/prepare. No current
Claude history, credential files or actual Controller/Host lifecycle is used.
Fixtures and synthetic manifests are never shipping artifacts.

JS syntax, changed Rust module formatting and `git diff --check` are checked.
Public13/Cargo/build/provider pins remain unchanged. New ordinary native and
dispatcher must be delivered together; no previous image is relabelled.
Old images fail the existing offline harness/cwd guard rather than silently
starting an unrelated runtime.

Not repeated: completed goal/task/image/transfer full matrices, full Core gate,
production build, Windows native entry (this leaf remains POSIX-only), deployed
browser headers or real provider authentication. Web's synthetic browser evidence
does not establish installed Console or real Claude acceptance.

Remaining: full snapshot/import/prepare UI orchestration, per-target secret
custody and ready-gated creation require additional supported contracts; rich
native-history rendering and Windows transfer remain absent. `sessionTransfer`
stays null. Source completion does not mean any existing Agent was transferred.

## Web owner contribution

Web `7c2ff6d76088ae6f0aa3ec26d758345f55e57471`, parent355, supplies only nine
Web paths. Existing Create sends fixed run argv once after target-bound explicit
acknowledgement. Sent attempts, including unknown results, lock prepared mode
for that page lifetime and retain the ordinary reservation-recovery handling.
Success reports saved startup/process only; no readiness/capability claim.
The existing source UI and public13 launcher assets are not rebuilt by this leaf.

Web owner evidence: seven focused synthetic Chromium controls PASS, desktop390/
1280 success/unknown, en/light and zh/dark/keyboard; typecheck/build, contrast64,
docs and diff checks PASS. Screenshots and exact logs live under
`/Users/dooms/treer-web-claude-prepared-create-247/.treer/reviews/web247-prepared-create/`.
Initial label harness failure was corrected and retained by Web. The final
ignored output-directory-only test config change was syntax/list checked;
product/assertion bytes match the tested ones. No real Agent was created.
The Launcher integration reads the exact Create request/argv/error paths and
checks tree identity rather than rerunning Web's unchanged browser matrix.
