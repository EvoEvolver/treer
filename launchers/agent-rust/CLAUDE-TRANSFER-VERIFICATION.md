# Offline Claude transfer leaf, issue247

Base: origin/main `f959e77af0f7a814f11605fbfa3774124659e2eb`.
Isolated worktree: `/Users/dooms/treer-launcher-claude-transfer-247`.
Contract and remaining live-transfer work: [CLAUDE-TRANSFER.md](CLAUDE-TRANSFER.md).

## Author evidence

- Default (no fixture feature) native executable built with existing absolute
  Codex/Claude build pins and private cache
  `/Users/dooms/.cache/treer-claude247-target`: PASS. No provider is started by
  these commands. Invalid runtime cwd/identity environment in every test proves
  the offline branch does not initialize the ordinary runtime.
- `RUST_SPIKE_BIN=.../debug/treer-agent-rust-spike
  TREER_TRANSFER_NODE_LOADER=.../tsx/dist/loader.mjs node --test
  launchers/agent-rust/tests/session-transfer-entry.mjs`: **8/8 PASS, 0 skip**.
  Log `/Users/dooms/.cache/claude247-transfer-final-entry.log`.
  Fresh task-owned synthetic native main/subagent JSONL, native/project memory
  and instructions only; UUIDs/cwds are generated inside temporary roots.
  Both Node bundle -> Rust restore -> Node export and Rust bundle -> Node import
  execute the actual maintained Node helpers against separate synthetic homes.
  Test cleanup removes only those generated temporary roots.
- Actual entry covers allowlist exclusion of an unreadable synthetic credential
  and arbitrary project file; unchanged source bytes; UTF16 project key with an
  astral character; new UUID/cwd/payload rewrite, private file modes and isolated
  config; canonical base64/SHA; unsafe paths, duplicate files/JSON keys, absent
  main/source-ID mismatch and incomplete JSONL. Existing empty/nonempty/link
  destinations refuse unchanged. Symlinked files/ancestors, hardlinks, FIFO and
  writable native directory refuse. Sparse oversized native/input files and a
  decoded-total overflow refuse. A file/directory collision fails after staging
  writes and leaves neither target nor owned stage. Four concurrent actual
  importers publish exactly one complete target, with no overwritten destination.
- Actual installed dispatcher with a synthetic bound bundle and real default
  image forwards `session export`, with no provider bindings or provider launch.
  A changed executable hash refuses without Node fallback. This is an isolated
  dispatcher test, not a default installation or shipping artifact.
  Its defensive runtime-only cwd/harness is also run through the ordinary main
  path (no `session` argv), which refuses before runtime/provider initialization.
  This closes an author-found old-image pairing hazard: old images ignore argv,
  so blindly forwarding `session` under inherited session selectors was unsafe.
  No historical image is relabelled as implementing the new command.
- Strict macOS default all-target Clippy `-D warnings`: PASS, log
  `/Users/dooms/.cache/claude247-transfer-clippy.log`.
  Windows GNU default binary cross-Clippy: PASS, log
  `/Users/dooms/.cache/claude247-transfer-windows-clippy.log`; compile-only
  evidence for the explicit unsupported branch, not Windows transfer support.
- New module rustfmt, dispatcher/test JS syntax, docs and diff checks: PASS.
  Cargo/build/provider pins, UI source and all13 public assets are unchanged from
  f959. No broad existing goals/images/tasks/recovery entry matrices were repeated.

Initial six-entry author run passed, then added real Node interoperability and
bound dispatcher controls (eight-entry pass). The final run also includes
directory trust, parent identity and post-publication stdout-error handling.
No observed product failure was relaxed to pass these controls.

## Scope and handoff

New ordinary native executable plus changed dispatcher are required for delivery;
the previous effort image cannot provide this command. Public13 closure remains
the exact f959 closure; no UI rebuild is required. This author executable is a
debug validation artifact, not a production image and must not ship.

Offline CLI leaf complete; live `sessionTransfer` remains null. Authenticated
runtime snapshot/idle custody, explicit new-session launch wiring and Windows
private-file backend remain concrete next implementation items, documented in
the contract. No real Claude history/config/credentials, managed state/history,
live provider, Agent/Host/Controller lifecycle, install, production build or push
was performed. Import never copies credentials or restores queued tasks/goals;
it preserves current provider files and does not mutate a running session.
