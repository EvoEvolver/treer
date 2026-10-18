# Rust launcher experiment (#158)

This is a separate, unpublished Cargo workspace. It does not replace
`launchers/agent`, enter the production bundle, change a saved profile, or change
Core behavior. No production Agent is eligible for this experiment.

The first implementation covers one Codex app-server session: loopback AIS,
HTTP/WebSocket state, prompt/queue/abort, transcript paging, operation-id receipts,
completion notification and Treer message receive → durable queue → ack. It
serves the existing React assets from `../agent/public` unchanged. The semantic
manifest is read from [the existing contract](../agent/interface-contract.json).
This is a compatibility target, not a claim that all production tests pass.
The completed experiment is assessed in [P3](P3-REPORT.md): continue a staged
port, but do not switch team launchers. The real-browser send failure, missing
durable user items and detached-child reaping are explicit port blockers.

Build and focused checks (a private `CARGO_TARGET_DIR` is required). Default
builds require native tools: if `codex` on PATH is an npm script wrapper, set
`TREER_SPIKE_BUILD_CODEX` at **build time** to the absolute native Codex binary.
The same native binary must be selected by the disposable Node comparator.

```sh
cargo build --locked --features fixture-harness --manifest-path launchers/agent-rust/Cargo.toml
cargo test --locked --manifest-path launchers/agent-rust/Cargo.toml
cargo clippy --locked --manifest-path launchers/agent-rust/Cargo.toml --all-targets -- -D warnings
cd launchers/agent-rust
RUST_SPIKE_BIN=/absolute/private-target/debug/treer-agent-rust-spike \
  NODE_LAUNCHER_ROOT=/absolute/checkout/launchers/agent node --test tests/contract.mjs
COMPARE_NODE=1 NODE_LAUNCHER_ROOT=/absolute/checkout/launchers/agent \
  node --test tests/contract.mjs
```

The common tests reuse the production fake Codex, private-state fixture and
hermetic HTTP/environment helpers. Five common tests run against either launcher.
Rust-only checks cover its separate journal, uncertain dispatch (including a real
60-second dropped-response timeout), immutable receipts/dynamic queue positions,
slow-notification output/abort isolation, and the manifest registration probe.
This is a focused suite;
the production browser/image/goal/session-transfer suites are not substituted
by these checks.

## State and authority

Managed execution requires the Controller's explicit
`TREER_AGENT_PRIVATE_STATE_DIR`, an available `agent.private-state.v1` status
from the bound loopback Controller, and private physical ancestors outside the
working directory and VCS. The status is rechecked before HTTP prompt admission;
local placement is rechecked before each journal write. Standalone hermetic
tests may use `TREER_RUST_SPIKE_STATE_DIR`. No cwd fallback or import/migration
of Node's journal or session files is implemented. The only state file is
`rust-spike.json`, atomically replaced with mode 0600 in a pre-existing 0700
directory. There is no image quota client: images are disabled. Queue metadata
has a 32 MiB hard journal bound; quota integration for other private bytes is a
port prerequisite.

The inherited Agent identity is deliberately delegated to Codex and its Treer
tools as in Node. It is never returned to the browser or written to the journal.
`RUST_SPIKE_REGISTER=1` opts the **disposable Agent only** into AIS registration.
The actual source/target/instance header names and browser checks mirror
`interface-identity.ts`; native loopback clients remain trusted. This is not an
OS sandbox. Codex uses the existing full-authority no-approval launch policy.

The shared contract currently names only seven capabilities (submit, transcript,
state, abort, queue, notify, queue read); it does **not** name images, goals or
session transfer. Registration/manifest use an explicit implemented list of
these seven instead of copying arbitrary future contract additions. Unsupported
harness features remain false/null in state.

Authorization matrix (same existing Node policy, checked against both launchers):

| Routes | Unmarked native loopback | Browser without valid forwarding | Valid proxy-ui | Valid controller-api |
| --- | --- | --- | --- | --- |
| `/v1/prompts`, `/v1/queue*`, `/v1/submissions/*` | Reject in managed mode | Reject | Allowed; sender/notify rejected | Allowed; attested sender/notify |
| POST `/api/prompt`, `/api/interrupt`, `/api/settings`, `/v1/abort`; DELETE `/api/queue/*` | Allowed; legacy prompt ignores sender/notify | Reject | Allowed; legacy prompt ignores sender/notify | Allowed; legacy prompt ignores sender/notify |
| State/transcript/manifest/static and WS | Allowed | Reject | Allowed | Allowed (no Origin) |

All rows still require exact local Host or the valid Controller's
`agent-interface` Host. Any present stale/invalid provenance is rejected.
Native loopback trust and forgeable provenance headers are pre-existing security
limitations, **not client authentication**. Do not expose this port publicly.

Default builds pin canonical absolute Codex/Treer executable paths from the
build host's PATH in `build.rs` (or `TREER_SPIKE_BUILD_CODEX` for native Codex).
Default builds reject script entry points to avoid PATH-dependent interpreters;
runtime PATH cannot redirect either executable.
`CODEX_BIN`/`RUST_SPIKE_CODEX_ARGS` overrides cause default-build startup to fail.
Only the explicitly enabled `fixture-harness` test feature permits overrides and
a fake Treer CLI from PATH. P2 must use a default release build, record resolved
tool paths and all three binary SHA-256 hashes, and not set harness overrides.
The port fixture file is created exclusively as 0600 and never follows a link.

One current-thread Tokio executor, bounded command/event channels and watch
snapshots avoid an unbounded WebSocket backlog. HTTP bodies: 128 KiB/10 seconds;
WS input: 16 KiB, writes: 5 seconds; Codex JSON lines: 4 MiB; message CLI stdout:
1 MiB/40 seconds. The current history budget is 16 MiB/10,000 turns; exceeding
it stops prompt admission and interrupts the active turn rather than silently
discarding transcript. Initial thread/resume is subject to the 4 MiB RPC line
limit. These bounds are experiment limits, not production parity.

Operation IDs are durable before 202. A conflicting retry is refused. Running
submissions after restart become explicit uncertain failures and are not
automatically re-dispatched. Persisted dispatch intent blocks **all** further
dispatch if the start/steer result is missing, times out, disconnects, omits a
turn identity, or cannot be durably checkpointed. Restart and late provider
events do not clear this lock. Read/observe/abort/cancel remain available; new
queue admissions remain queued. Automated reconciliation/unblocking is not
implemented: stop this disposable run and inspect its transcript; do not use a
restart to continue the comparison after uncertainty. Admission receipts are
immutable; submission state and queue positions are computed separately.
Completion sends retry with a stable key through one bounded worker (one pending
job and one result); a slow CLI does not occupy the state/abort/event actor. The
watcher acknowledges only after a persisted submission. It does not yet coalesce
message polls or notifications by native turn like the mature Node queue.

The provider starts in a new owned POSIX process group. Shutdown/error/drop kills
that group and the direct child; a fixture checks descendant cleanup. Deliberate
detachment escapes this group: this is not process containment. The P2 stop plan
must still inventory the actual tree and verify no descendants remain.

## Remaining port and review work

Before any disposable managed run, Core/security review this first checkpoint
and the exact experiment command. Before any team-Agent switch, both a full
security review and the leader's decision are required. No team switch is part
of P0–P2.

Known gaps: goals and goal recovery; attachments and quota reservations;
session export/import; file browser/task-output APIs; Node's relay-error
reconciliation/backoff; exact batched notification semantics; per-user resource
admission; slow HTTP header/connection limits; containment of detached descendants;
minimal harness environment; crash-dump policy; complete model-setting and item
mapping parity; dependency advisory audit. These are reasons not to migrate a
team Agent yet. Unknown routes fail closed instead of serving guessed APIs.

See [experiment record](EXPERIMENT.md) for baseline and the pending comparison,
and [P2 preflight](P2-PREFLIGHT.md) for the verified build inventory and pending
run prerequisites. [Proposed creation and teardown commands](P2-CREATE.md) are
for leader/security review; they have not been executed.
