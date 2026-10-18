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

M2 file routes pin a canonical workspace directory descriptor at
startup, then open every resolved relative component with `openat` and
`O_NOFOLLOW` (`O_DIRECTORY` for intermediate components). An intermediate or
final symlink swapped after path admission is refused. A replaced workspace
pathname never redefines the pinned root; already opened directories remain
capabilities to those original objects even if renamed. In-workspace symlinks
are still supported by canonicalizing their target before the descriptor walk,
including directory links back to the workspace root (metadata uses the pinned
root descriptor itself).
This is read confinement, not isolation from same-account writers, hard links
or mounts. Directory tree listing uses an independent descriptor-based stream
and obtains child metadata with descriptor-relative `statat` without following
final symlinks or opening FIFO/device entries. Canonical path resolution still
selects permitted in-workspace symlink targets, but enumeration, metadata and
file bytes never reopen an admitted absolute path. A concurrent rename can
produce a partial/stale listing; it cannot retarget the descriptor to a new tree.
Directory enumeration and path resolution share two blocking-worker slots
(429 when occupied), retained by the worker even after request cancellation.
Enumeration scans at most 4096 non-dot entries, including
hidden/unsupported entries; larger directories return an explicit 413 rather
than silently truncating. Cancellation retains the worker's slot until it exits.
The entry point canonicalizes the configured cwd once, so stable aliases such
as macOS `/tmp` are accepted. Only the resulting canonical path is passed to
the descriptor opener; a component swapped to a symlink after canonicalization
is refused rather than canonicalized again. Resolution/open/type failures use
the fixed startup error `workspace_file_root_unavailable`, without path details.

M2 transcript admission retains at most 10,000 provider turns and 16 MiB of
serialized turn content (excluding the outer array separators). Each turn
replacement is counted without allocating a second serialized history and is
admitted atomically. Exceeding either limit preserves the previous transcript,
persists the dispatch fence, stops the directly owned provider/process group,
rejects further mutations, and leaves state and receipt reads available.
It never settles an oversized completion as success or replays a running intent
on restart. The synthetic many-turn regression samples only its own launcher's
RSS with a 384 MiB regression ceiling. On macOS, setting
`RUST_SPIKE_HEAP_PROFILE=1` also samples aggregate live allocations with
`heap -s --noContent` at baseline, after 16 turns, at the admission limit and
after repeated snapshot reads. The sampled fixture has a 128 MiB live-heap
ceiling and permits less than 16 MiB of retained growth across those reads.
The profiling variant has a separate 150-second deadline for four heap probes;
the normal regression retains its 60-second deadline. These are finite-workload
regressions, not an OS quota or proof against leaks over an unbounded lifetime.
Provider frames/channels, the durable journal and HTTP snapshots have
separate bounds, so the 16 MiB content budget is not a total RSS promise.
Missing or non-string provider turn IDs are rejected rather than assigned a
synthetic identity. During notifications this preserves accepted history and
receipts, persists the dispatch fence, stops the provider and reports a fixed
owner-visible error without echoing provider content. Startup rejects such
history without rewriting the journal or falling back to a fresh thread.
Recovery requires operator inspection and repair of the provider thread; this
launcher does not repair the malformed history or clear the dispatch fence
automatically. A malformed resumed thread will be refused on each restart.

Native macOS **does not guarantee detached-descendant cleanup**. A provider or
tool can immediately call `setsid` or reparent outside the verified process
group. Periodic scans, process names, cwd, UID and inherited environment markers
are not ownership proofs and are not used to broaden cleanup. The general M2
descendant-reaping criterion remains unmet; a future supervised OS containment
boundary and its native tests are required before claiming that guarantee.
The snapshot's `runtime.cleanup` distinguishes direct-provider exit confirmed
versus unverified, group termination requested but unverified, and detached
descendants not guaranteed. History-limit and startup-failure notices expose
the same distinction through the existing browser error display. Direct exit
confirmation is bounded to five seconds; timeout or kill/wait failure is never
reported as confirmed. SIGTERM, SIGHUP and SIGINT cancel even an in-flight
startup/turn RPC, stop the provider, bound direct-child wait to five seconds,
and print a fixed cleanup outcome before at most two seconds of HTTP drain.
Internal observer/notification and provider pipe-reader tasks are cancelled.
Queued and uncertain running receipts remain durable; stopping does not invent
a terminal result or replay them. This covers graceful launcher stop (including
a Controller stop delivered as these signals), not SIGKILL, abrupt machine loss,
or a guarantee about deliberately detached descendants. Agent deletion and
private-state lifecycle remain Controller-owned; the launcher deletes no state.
Resuming an already oversized thread fails with an owner-visible history-limit
reason and no fresh-thread fallback. The durable journal remains byte-for-byte
unchanged; this implementation does not silently select a partial history.
An operator must use a launcher supporting that thread's history; there is no
automatic state deletion, migration or fresh-session recovery.

M1 compatibility increments preserve the separate browser and AIS contracts:
`/api/prompt`, `/api/settings` and `/api/interrupt` return the full browser state
with the requested `history_limit`; prompt responses also include the submission.
The `/v1` mutation routes continue returning their AIS receipts. This increment
does not yet establish full browser parity or authorize a team launcher switch.
Accepted user text and Controller-attested sender metadata are projected from
the durable submission journal into history, including after sparse completion
events and restart. This does not resolve or replay uncertain dispatches.
Journal recovery validates the schema, dispatch fence, record identities,
metadata and payload fingerprints before starting the provider. Invalid state
refuses startup with `private_state_invalid`, preserving the journal rather than
dropping records or creating a fresh session.
Read-only reconciliation uses one bounded background `thread/read` operation.
Only an explicit terminal turn matching a known accepted running submission can
settle its receipt. Missing turns, an idle flag and read failures never complete
a submission or clear the persisted uncertainty fence.

The next M1 increment exposes native Codex goal create/edit, token budgets,
pause/resume and clear through the browser state contract. A bounded goal-edit
journal checkpoints native mutation intent and a separate idempotent prompt
delivery. A lost native acknowledgement is reconciled by reading the goal;
an unmatched result is retained as an error rather than replaying the mutation.
This is still partial goal parity: non-null round budgets, slash commands and
managed Agent goal-exit requests remain unsupported.
Managed Codex sessions support PNG, JPEG, WebP and GIF attachments through a
separate `rust-images` directory under the Controller-provided private root.
Container framing, CRC where applicable, dimensions, truncation and appended
payloads are checked; this is not compressed pixel decoding. Uploads require
Controller quota reservation and commit, with bounded rollback/release on error.
Files are exclusive 0600, reads refuse symlinks/hardlinks and verify SHA-256;
only opaque image metadata reaches browser state. Sent images are retained with
the submission journal for reload and restart, including image-only prompts.
Limits are 10 MiB per image, 8 images/24 MiB per prompt, 256 MiB local storage,
8,192 entries, and two combined upload/download slots. Dimensions are bounded
to 16,384 per side and 100 million pixels, matching Node. Retained entries never
expire; reaching either storage bound rejects new uploads without evicting sent
images. Response buffers remain
under a slot until consumed or disconnected. Drafts expire after one hour on
the next upload; a missing expired draft drops only its journal entry, while
unsafe storage still fails closed. Controller quota requests time out after five
seconds. Orphan cleanup and native Windows storage verification remain M2 work;
native Windows image capability is off. This POSIX spike does not migrate Node
attachment state.
The [dated dependency advisory query](DEPENDENCY-AUDIT.md) records the exact
lockfile checked and the limits of that result.
Plan snapshots and updates normalize Codex's `in_progress` to the browser's
`inProgress`. HTML carries the existing permissions policy and CSP; script and
CSS assets declare UTF-8 without an HTML document policy.

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
tests may use `TREER_RUST_SPIKE_STATE_DIR`. No cwd fallback is permitted.
Journal publication or placement failure makes the runtime read-only, blocks
dispatch and stops the directly owned provider. It does not automatically resume
after storage repair. An unavailable store cannot durably record this new stop:
the prior journal, including any already-persisted running intent, remains the
restart authority. Error responses and cleanup status do not expose OS paths.
A process-lifetime nonblocking file lock rejects a second Rust writer with
`private_state_in_use`; its private 0600 inode is retained after exit rather
than unlinked. This is an advisory Rust-only lock: Node does not participate;
never run Node and Rust concurrently on the same state. A saved Rust thread cannot be overridden with a different resume
ID (`private_state_session_mismatch`), and refusal precedes provider startup.
Node-to-Rust migration remains unsupported: finding the selected Agent's legacy
`.treer/agents/ID` directory (including `standalone`), a linked/non-directory
ancestor, or known Node state in the private root refuses startup with
`legacy_state_migration_required`, without opening, parsing, modifying or
discarding that state. No resume override bypasses this check. This is a
fail-closed refusal, not a completed migration or a tested bidirectional rollback.
Do not delete or relocate legacy state to bypass it: queued/uncertain receipts,
acknowledgments, notification batches, goals and attachment bindings need a
reviewed converter. Keeping Node's files untouched alone does not make a
rollback after new Rust work lossless. Such a switch remains blocked.
The main state file is
`rust-spike.json`, atomically replaced with mode 0600 in a pre-existing 0700
directory. Journal reads recheck placement, open without following the final
symlink and use nonblocking mode before rejecting non-regular files, so a FIFO
cannot stall startup. A failed write/sync/rename removes only that attempt's
exclusively created temporary file; cleanup failures use a fixed diagnostic and
never replace the primary failure. Other temporary files are never swept.
A crash can still leave a temporary file. A UUID-shaped name and private mode
alone do not prove ownership; automatic orphan recovery remains unimplemented.
Managed image storage uses a separate journal and Controller quota
client as described above; standalone images are disabled. Queue metadata
has a 32 MiB hard journal bound; quota integration for other private bytes is a
port prerequisite.

The loopback HTTP/1 transport admits at most 128 live connections, including
upgraded WebSockets; excess accepted sockets close immediately. Every request
header has a ten-second absolute read deadline, at most 64 headers and a 32 KiB
buffer. Body-consuming routes keep their existing ten-second/size limits.
Accept errors retry after 50 ms rather than ending the provider/server.
The write-idle wrapper does not time out reads: request-body routes enforce
their own ten-second deadline. Silent upgraded WebSockets have no read-idle
deadline and occupy a connection slot until disconnect or launcher shutdown;
upgrade uses the same local Host/forwarding admission checks as other routes,
not a new credential-based authentication boundary. Outgoing WS writes have
their separate five-second deadline.
File and image responses hold their respective two-slot budgets through
backpressure. Both use the operator-configurable download total duration cap
(default one hour) and connection write-idle timeout (default 60 seconds).
The total-duration timer is polled with the response body: a transfer already
blocked on a socket write may retain its slot past that timer until reads resume
or the write-idle timeout fires. The image regression proves truncation on
resumed reads, not independent wall-clock eviction. Directory cancellation
slot retention is established by worker ownership in source; the HTTP regression
currently covers entry rejection and concurrent state reads, not cancellation.
These limits bound admission; they do not authenticate local callers or isolate
a same-account filesystem writer. A blocked OS filesystem operation can still
outlast the HTTP request; the two directory worker permits remain held.

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
`tests/default-build.mjs` scans the default release artifact for HTTP fixture
hooks and verifies override refusal in a disposable directory, without starting
the native provider. Its required input is `RUST_SPIKE_RELEASE_BIN`.
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

Known gaps: complete goals and goal recovery parity; attachment orphan cleanup;
session export/import; task-output APIs; Node's relay-error
reconciliation/backoff; exact batched notification semantics; per-user resource
admission; slow HTTP header/connection limits; containment of detached descendants;
minimal harness environment; crash-dump policy; complete model-setting and item
mapping parity; dependency advisory audit. These are reasons not to migrate a
team Agent yet. Unknown GET routes serve the UI shell, matching Node; unknown
mutation APIs fail closed.

See [experiment record](EXPERIMENT.md) for baseline and the pending comparison,
and [P2 preflight](P2-PREFLIGHT.md) for the verified build inventory and pending
run prerequisites. [Proposed creation and teardown commands](P2-CREATE.md) are
for leader/security review; they have not been executed.
