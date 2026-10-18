# #158 P2 preflight — not a run authorization or completed experiment

Current proposal is the corrected second attempt, source 3bfa138b: security
3544 and Core 3547 pass the delta (Core independent encoded-path tests 2/2).
First attempt stopped on receipt-path integration failure;
P2-RUN.md preserves its actual IDs and evidence. Do not restart that old pair.
The snapshot/hash below now identify the corrected build. Original snapshot
remains unchanged and retained for evidence. No new Agent has been created.

No second-attempt Codex session or managed experimental Agent has been started. Ticket
activities 3487, 3490 and 3493 require the follow-up static review and the full
run inventory before P2. Security 3490 also requests a human-created disposable
Agent; activities 3509/3513 accept the user's delegation to the leader to create
the two Agents after command review. Their actual
Controller-assigned private roots are not known yet. Do not substitute a team
Agent or import its credentials, journal or native session.

## Verified build inventory

Host: mac_m2, macOS/arm64; effective UID 501. Toolchain: rustc/cargo 1.90.0.
The Cargo target is `/Users/dooms/.cache/treer-target/launcher-rust-158`, jobs=2.
Release build has **no** `fixture-harness` feature. It pins these native tools:

| Artifact | Absolute path | SHA-256 |
| --- | --- | --- |
| Rust launcher | `/Users/dooms/.cache/treer-target/launcher-rust-158/release/treer-agent-rust-spike` | `23cfdc129a671f28b5dfdd4be256791c1e8b2e2284483cf35347a796824699e3` |
| Codex native | `/Users/dooms/.local/lib/node_modules/@openai/codex/node_modules/@openai/codex-darwin-arm64/vendor/aarch64-apple-darwin/bin/codex` | `74c6a6d263f40d65f99c27a815275f5b4ed0bcc2baecc396b4cf3255498bdfc0` |
| Treer CLI | `/Users/dooms/.local/bin/treer` | `87e2fd520714fbbc468cc5a5f8d01dda0cf97459f6d94da1ee7383d804e15d65` |

The npm `codex` entry point resolves to a script, so it is deliberately **not**
the pinned executable. Both comparators must use the native binary above.
Rehash immediately before running; changed binaries require an updated inventory.
Default-build negative checks for `CODEX_BIN=/fixture-must-not-run` and
`RUST_SPIKE_CODEX_ARGS=["must-not-run"]` both exited 1 before private-state,
registration or provider startup. Test builds and credentials were synthetic.

## Still required before the first real invocation

Security 3522 closed security source items 1–5 and acknowledged Node's production
CODEX_BIN. Security 3524 accepts leader 3521's use-in-place auth decision;
Core 3525 closes all 3487 blockers on source 0c53b246 with four independent fault
tests. Subsequent commits only update these experiment documents.
Rust's fixed 0555 binary snapshot (0700 parent), verified with the same release
hash, is `/Users/dooms/.cache/treer-158-tUql8b8E/treer-agent-rust-spike-3bfa138b`.
Do not rebuild the Cargo target or replace pinned artifacts during the run.

- Record the human-created Node/Rust disposable IDs, exact startup argv, selected
  model/effort, actual Unix user, empty non-secret cwd, default Codex home in use,
  Controller-assigned `TREER_AGENT_PRIVATE_STATE_DIR` for each, port-file paths,
  evidence output paths, Node source/dependency hashes, and unchanged UI assets.
  No placeholders in the final ticket inventory.
- Leader 3521 accepts full-account exposure and directs both runs to use the
  existing default Codex home in place (unset CODEX_HOME), new native threads
  only, no authentication copying or session import/resume. Security 3524
  acknowledges this revision. Codex will read its normal
  auth/config and write shared history; that home is never a cleanup target.
  Do not read/list/copy/print its contents. Record the two native thread IDs
  from app-server responses for the user to archive; they remain after teardown.
  Measure only process metrics and counts/sizes of our own journals.
- Explicitly remove inherited `AGENT_UI_SESSION_ID`, `CODEX_BIN` and
  `RUST_SPIKE_CODEX_ARGS` from the Rust startup. Keep the new disposable Agent's
  own Controller-injected identity; do not copy another Agent's environment or
  saved credentials. Node uses the same pinned native Codex path above.
- Keep `AGENT_UI_MESSAGE_WATCH=0`. Do not publish a service or port. AIS binds
  loopback/port 0 and registers only the disposable interface. Record actual
  ports after startup; browser requests use the existing authenticated bridge.
- Fix identical benign prompts, cadence, UI polling and model settings; no tools,
  downloads, destructive commands or reads outside the empty experimental cwd.
  Use new sessions, a three-hour maximum, and report idle/turn/many-turn history
  sizes with RSS/CPU for launcher and provider tree separately. Capture Node
  heap/native/buffers only inside the disposable Node process.
- Stop on uncertainty, error, resource/history bound, unexpected tool activity,
  provenance failure or elapsed time. No automatic replay/unlock. A stopped or
  capped run is not a successful matched comparison.
- Record and monitor run generation; stop both Agents on any unexpected
  exit/restart or generation change. Human observation of the process tree and
  transcript tool items is required during the first prompts. Post the exact
  matched prompt script before inference and rehash all pinned binaries/assets
  at creation and teardown.
- Stop procedure must target only the recorded disposable launchers/Agents.
  Rust SIGTERM drops its owned provider process-group guard, kills that group and
  its direct child, and aborts its CLI worker. A sealed test proves cleanup of a
  non-detached descendant. Record the process tree before/after and verify no
  owned descendants remain; separately detached children are not contained.
  Never signal the caller's inherited process group or kill by a name pattern.
  Save evidence for review before Agent deletion. Controller's verified Agent
  deletion purges only those two managed roots; never manually remove a managed
  root recursively, and never change existing team Agents.

Browser acceptance, matched measurements and the multi-hour run are pending.
The source review, explicit paths/argv and stop plan must be accepted before
inference begins. A successful P2 is not permission for a production switch.
