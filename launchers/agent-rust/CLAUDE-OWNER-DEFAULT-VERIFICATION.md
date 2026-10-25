# Owner-default authentication transfer verification (#247)

Native base: `0bb476d24d176fe6d38718217da31d352129b01a`. This batch
does not change steering, task result ownership, model/effort control, goals,
the standalone dependency lock, installed defaults or any live Agent.

## Author controls

On macOS, using only a task-owned temporary HOME, native JSONL, fake private-state
Controller and synthetic stream-json provider:

```sh
CARGO_TARGET_DIR=/Users/dooms/.cache/treer-claude247-managed-target \
  cargo build --locked --offline --features fixture-harness \
  --manifest-path launchers/agent-rust/Cargo.toml
# Run from launchers/agent-rust:
RUST_SPIKE_BIN=/Users/dooms/.cache/treer-claude247-managed-target/debug/treer-agent-rust-spike \
  node --test tests/session-owner-default-entry.mjs
```

Four entries PASS, zero skipped:

1. Actual native source input writes its own native file; authenticated/provenance
   idle export requires the explicit owner-default selector. Hash-bound inspect
   writes nothing; prepare returns the same receipt counts/bytes and publishes
   both roots. Persistent target initializes the imported new UUID, displays
   native history without receipt authority, executes one new input exactly
   once and resumes its own checkpoint after native-file growth. Source hash,
   source UUID and synthetic settings sentinel remain unchanged. No API key or
   OAuth-token variable is supplied; the fixture models path selection, not
   successful remote authentication.
2. Bad hash, existing native project (including empty target ownership), symlink
   project and explicit conflicting config refuse without target publication.
   An isolated-source runtime cannot use the new selector to read default-root
   history. Existing source files remain unchanged.
3. Fixture-only failure after provider-root publication retains the intent and
   complete provider project without publishing the target. Read-only
   `session publication` reports partial/unknown; a repeated prepare refuses
   and leaves content unchanged. There is no automatic repair or deletion.
4. Provider initialize refusal creates no Launcher checkpoint or prompt. The
   exact launch intent is retained and a later same-argv attempt refuses before
   a second provider invocation. Original source remains unchanged.

A fifth focused canonical-config-alias entry PASS after the Web receipt
binding check: an existing owner `.claude` symlink resolves to the same held
canonical owner root, receipt binds that physical root, and provider still
receives no config override and resumes through its normal default path.
Only this new boundary was run, not the four unchanged entries again.

The existing environment-auth orchestration entry was run once separately:
`--test-name-pattern='orchestrated snapshot inspect/hash'` in
`tests/session-launch-entry.mjs`: one PASS, zero skipped. This covers the old
receipt shape, isolated config and persistent startup; no other parity matrix
was rerun.

Distinct old-image control: the same first new entry against fixture image
`3c3f95385108f9454dab9808bfc27245848a596c1a18dfba57550b7b98a19ff7`
fails on its actual source HTTP response400 `snapshot_exact_thread_required`.
The new source accepts the explicit selector and completes the entry. This is
a missing-contract counterexample, not evidence about a user's private session.

Strict fixture all-targets Clippy, focused rustfmt, Node syntax and diffcheck
PASS. The final help text was added after the runtime controls; its rebuilt
`session --help` is checked separately. No production native was built by the
author; Release owns the new paired production artifact. No fixture may ship.

## Limits

No real Claude provider transfer or managed Agent creation is claimed here.
No real `.claude` directory, credential, Keychain, private history, default
pointer, in-use Agent, Host or Controller was read or mutated by these tests.
The temporary settings sentinel is synthetic and is checked only for unchanged
bytes. The fixture is not proof that another machine's owner login works.
Owner hooks/settings remain normal provider behavior; this is not config
isolation. Real user-flow/continuation acceptance and production artifact
binding remain separately attributed. `sessionTransfer` stays null.
