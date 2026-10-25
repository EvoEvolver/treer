# Claude images verification (#247, second batch)

This batch follows the separately reviewed steering commit `6e30e937`, delivered
on main as `27dba1bb`. It changes no shared UI/public assets, installer, provider
build pins, Controller, Host or Codex protocol. The direct base64 dependency was
already in the lockfile; only the launcher's dependency edge changes.

## Checks

- Actual Claude entry: a valid two-MiB GIF (comment extension blocks) is uploaded
  three times through the existing private attachment API. Image-only initial
  input, working image+text steering and queued image-only input arrive as
  native base64 blocks with the exact decoded size/MIME/SHA. The native echo
  exceeds the old two-MiB frame cap. Transcript/receipt attachments retain only
  ID/MIME/size; restart retains downloadable bytes and sends no input again.
  Invalid bytes, missing IDs and duplicate IDs refuse before dispatch.
- The old model243 fixture binary fails this new entry control at the false
  image capability; the new binary passes.
- Actual shared UI test passes at1280px and390px. Upload and image-only Send,
  followed by working image Steer, reach the real Rust HTTP/static entry and
  protocol producer. Two inputs, one native image each, no overflow.
- Claude units:14 passed, including MIME/format/base64/count/path refusals,
  large-image echo followed by another frame, nonimage size refusal, and the
  previous held-writer/result race and fatal stalled-write controls.
- Codex focused controls:3 passed (existing attachment quota/validation/durable
  restart, failed quota commit cleanup and identical-text steering).
- Final full Claude entry suite:28 passed, zero failures, three opt-in browser
  controls skipped. The new image browser control was run separately as above.
- Both default and fixture all-target strict Clippy, JS syntax and diff checks
  passed.

Commands use `cargo ... --locked --offline`, the task-owned
`CARGO_TARGET_DIR=/Users/dooms/.cache/treer-claude247-target`, the existing
native Codex darwin-arm64/Claude2.1.306 build inputs and explicit native
Node22.23.3 for producer RPC units. Entry tests use its debug fixture-feature
binary through `RUST_SPIKE_BIN`; browser tests resolve existing Playwright via
`NODE_LAUNCHER_ROOT=/Users/dooms/treer-launcher-ci92198-fix/launchers/agent`.
Browser opt-in is `CLAUDE_IMAGES_BROWSER=1`. All fixture processes/state are
owned and cleaned up; no online Agent or provider-private history is involved.

## Limits and corrected test setup

The raw Claude frame cap is34MiB with60-second framing deadline; it is not a
process peak-memory guarantee. Raw bytes, JSON strings, a bounded single-image
decode and compact serialization may coexist. All frame types may buffer to
that raw cap before classification. Only a user image echo may exceed2MiB
semantic admission, with the same eight-image/24MiB decoded limits; nonimage
residual must fit2MiB. Echo image bytes are discarded and never establish input
or terminal authority. Ordinary RPC remains128KiB, fatal write/flush5s.

Initial restart controls omitted the managed private-root environment on their
second launch and failed before port publication. The corrected invocation
keeps that explicit root; production admission was unchanged. An initial echo
unit expected no events, overlooking the existing local turn-start event; the
corrected assertions require no terminal/user addition and active turn retained.
Both failures are setup/assertion corrections, not relaxed product guarantees.

Production delivery requires a new non-fixture native artifact. Windows image
storage remains unsupported; capability is false when attachment storage is
unavailable. Background tasks, goals, transfer and effort remain separate batches.
