# Rust launcher experiment (#158)

This is a separate, unpublished Cargo workspace. It does not replace
`launchers/agent`, enter the production bundle, change a saved profile, or change
Core behavior. The scope reset 4767 was retracted by #158/4773. Current decision
4790 simplifies migration only: one-way import after byte backup, manual restore
rollback, and switching only while idle with an empty queue. Decision 4814 then
accepts Node-equivalent process-group cleanup; correction 4822 sets a local
128 MiB disk journal/log cap; no new Controller quota or Host ownership contract
is needed. Those earlier
Core designs are cancelled. This increment authorizes no team switch; the leader
owns the one-by-one rollout after the pilot day. See [M4 design](M4-DESIGN.md) for
parked design history, not an approved stronger isolation claim.
The offline library's `envelope::validate_manifest` currently validates only
manifest grammar, declared byte/entry limits and typed metadata references.
It reads no object files and grants no handoff authority. Object digests,
cross-object semantics, Node/Rust conversion, Controller quotas and proven
process ownership are outside that library. The separate local Node import
and backup restore below do not use its envelope or any new Core capability.

The first implementation covers one Codex app-server session: loopback AIS,
HTTP/WebSocket state, prompt/queue/abort, transcript paging, operation-id receipts,
completion notification and Treer message receive → durable queue → ack. It
serves the existing React assets from `../agent/public` unchanged. The semantic
manifest is read from [the existing contract](../agent/interface-contract.json).
This is a compatibility target, not a claim that all production tests pass.
Provider notifications use a bounded eight-entry queue with lossless backpressure.
The runtime continues processing events while awaiting RPC responses, prioritizes
ready responses, and preserves early turn items/completion before the response
identifies the accepted submission. Queue saturation does not close the stream.
No automatic provider retry is performed. Fixed transport reasons (`open`,
`stdout_eof`, `line_error`, `invalid_json`, `consumer_closed`, `stdin_write_error`,
`stdin_write_timeout`, `rpc_timeout`) and independently
observed child status/signal before cleanup appear in AIS diagnostics and the
shutdown line. Stream closure alone is never called spontaneous provider exit.
Outbound operations include mutex acquisition, writing and flushing within a
five-second deadline; the event-pumping RPC wait has an absolute 60-second
deadline. A write timeout can mean partial delivery. It poisons the transport,
preserves accepted-operation uncertainty and stops further dispatch without retry.
The same five-second deadline applies to every valid RPC input (at most 128 KiB),
so a slow provider can also trigger it. Oversized input is refused before writing.
Before fencing after a cancelled wait, receipts and goals are restored from the
last published checkpoint; partially handled events cannot become durable.
Raw stderr capture is disabled by default. Only the explicit launch setting
`RUST_SPIKE_CAPTURE_STDERR=1` retains the last 8 KiB in memory and saves it at
shutdown as 0600 `rust-provider-stderr.log` under the validated private root,
atomically replacing the previous tail. This file may contain sensitive provider
text: it is never served by AIS, printed to the terminal or copied into a ticket.
Only the owner may inspect it locally and supply a reviewed excerpt. An abrupt
launcher kill can lose this tail; capture failure is reported without its text.
It is diagnostic retention, not Controller quota or detached-process containment.
This separate 8 KiB file and its publication temporary are reserved inside the
local 128 MiB journal/log cap. It is not sent to Controller quota accounting.
It lives until replaced by another opted-in run or until normal Controller
deletion purges that Agent's private root; disabling capture never deletes an
earlier evidence file. No team-wide opt-in is implied by a pilot wrapper.
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
(at most one second waiting before 429), retained by the worker even after
request cancellation. Pending handlers are bounded by the HTTP connection cap;
normal navigation bursts can wait briefly without increasing the worker count.
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
Private journal decoding also refuses duplicate JSON keys (including escaped
equivalent names), more than 32 value levels or 500,000 values/keys. These limits
are enforced during decoding before recovery consumes a JSON value, under the
existing 32 MiB wire bound. Publication applies the same shape budget without
allocating a second JSON tree, so it cannot write a checkpoint recovery would
refuse. Limit failure follows the existing checkpoint rollback and dispatch stop.
Diagnostics never include the offending key/value.
This hardens existing state reads; it does not enable M4 envelope import or
replace the separate envelope schema review and Controller handoff capability.
An earlier Rust journal beyond these shape limits now intentionally refuses
startup; it is preserved for operator recovery, never truncated or repaired.
This increment does not replace the other JSON readers: the image metadata
index in attachments.rs, provider RPC replies in runtime.rs, and HTTP request
bodies in main.rs retain their existing readers and bounds. Those sites are not
covered by this private-journal decoder claim.
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
Every journal checkpoint shares one rollback path for receipts, immutable
acceptance receipts, notification acknowledgements and goal state. A failed
pre-publication write restores the last published snapshot before exposing the
read-only stop. If rename succeeded but directory sync failed, the newly
published snapshot remains visible and the runtime still stops; durability is
unverified. The extra snapshot has the same entry and 32 MiB serialized journal
bounds; Rust allocation overhead is measured separately, not equated to that cap.
Fault regressions cover submit/steer, cancel, goal edit/clear/reconcile/retry,
dispatch intent/result, completion, notification acknowledgement and internal
goal checkpoints. Image discard has its own attachment journal and quota
rollback tests; it does not call this journal's checkpoint function.

Checkpoint caller audit:

| Caller | Publication boundary |
| --- | --- |
| Startup/reconciliation | Recovered receipts and native goal observations publish together. |
| Submit/steer/cancel | New or cancelled receipts roll back together; dispatch follows durable intent. |
| Goal edit/clear/reconcile/retry | Edit, applying/applied, delivery mode, retry and sent checkpoints use the same rollback. |
| Provider completion/goal events | Receipt and goal mutations publish before subsequent work is dispatched. |
| Notification acknowledgement | Failed acknowledgement publication restores the previous flag; sending remains an uncertain external side effect. |
| History admission stop | Receipt state is preserved; a failed stop publication cannot claim a new durable fence. |

A process-lifetime nonblocking file lock rejects a second Rust writer with
`private_state_in_use`; its private 0600 inode is retained after exit rather
than unlinked. This is an advisory Rust-only lock: Node does not participate;
never run Node and Rust concurrently on the same state. A saved Rust thread
cannot be overridden with a different resume
ID (`private_state_session_mismatch`), and refusal precedes provider startup.
On the first managed Rust start without a Rust journal, the launcher discovers
only its own `.treer/agents/ID` under the configured cwd. Stop Node first.
It copies every regular file in that directory byte-for-byte into private
`node-backup-v1`, with a SHA-256 inventory bound to Agent ID and cwd. Files are
0600, the backup is 0700; no source file is renamed, rewritten or deleted.
Bounds are 32 files, 32 MiB per file, 64 MiB total, no directories/links/devices.
The backup manifest is synced last; incomplete/tampered backups refuse startup
and remain for inspection. A complete backup can be reused after a conversion
failure only if source bytes still match. Decode and publication happen after
backup; any invalid/unrepresentable queue refuses the whole import before
starting a provider. Existing Rust state is validated normally and never imports
stale Node files again.

Import is opt-in through `--import-node-state`; legacy-state refusal remains the
default. Per decision #158/4790 the leader verifies provider idle and an empty
queue before stopping Node. The importer independently refuses any queued or
running record, pending completion notification or Core acknowledgement.
This increment imports Node's `agent-ui.json` (or old `codex-ui.json`) thread ID
and at most 4,096 settled text receipts, immutable submission IDs/receipts/Node
retry hashes and sender/notify metadata. Empty terminal text is normal Node eviction and
does not fabricate a userMessage. The provider resumes the same thread, never
silently starts a new one. Native goals are read from that thread. Attachment
references, pending completion outboxes, settled notification batches and all additional sidecars still refuse
this first import increment; support is needed before switching such an Agent.
No state is discarded to make it fit the limits. These compatibility gaps are
local adapters; migration does not depend on a new Core contract per 4790.
An empty Node-created private `images` directory is eligible only after no-follow
open, owner/mode validation and descriptor enumeration finds no entries. Its
identity and emptiness are rechecked immediately before publication; any entry
(including dotfiles or a journal) refuses. Never delete state to force eligibility.

Rollback: stop Rust (and keep Node stopped), invoke the same Rust binary with
`--restore-node-backup` under the same Agent/cwd/private-root settings, then
restore the previous Node startup command through the Controller and start it.
Restore validates the whole backup before replacing its files atomically one by
one, with directory syncs; no provider is started. A durable rollback marker
precedes any replacement, so partial restore can be retried but an old Rust
journal cannot start again. Backup and Rust journal remain; no broad cleanup or
automatic switch occurs. This restores the pre-switch queue, not later Rust
work or external provider/tool effects. Accepted residual: a message handled
during the Rust period may be repeated once after a rollback, or has to be resent.
Backups alone are not a lossless bidirectional merge. The leader controls rollout
and any recovery. The same same-account/concurrent-writer limitations as Node
apply; the Rust lease does not stop a separately running Node process.
The backup contains conversation text: the retention window is until the leader
explicitly accepts that Agent's switch and closes its rollback window. Then the
owner removes that exact backup directory after confirming rollback is no longer
needed. It is never deleted automatically or on a failed switch.
Never paste its content into logs or tickets.

Per-Agent switch checklist (leader/owner only, after the reviewed increment and pilot day):

1. Verify the running Node provider is idle, the queue is empty and there are no
   pending acknowledgements, notifications, goal edits or image drafts. Stop Node
   through the Controller; verify its provider has exited. Keep the old startup.
2. Start the pinned Rust binary once with `--import-node-state`, the same Agent,
   cwd and private root. Inspect only the backup manifest hash, not conversation
   contents, and record it with the Agent, binary hash and generation.
3. On success remove `--import-node-state` from subsequent startup arguments;
   inspect thread identity/history, short soak and post-switch state before
   considering another Agent. Any failure stops the sequence; do not delete files
   or clear a fence to make the import pass.
4. A person rolls back only with both launchers stopped: restore the byte backup,
   set the old Node startup and start Node. Restore overwrites the backed-up Node
   files, losing any Node changes since backup. The owner leaves the Rust restore
   marker intact; removing it is a separate explicitly authorized fresh migration
   after reconciling the two histories, never an ordinary restart step.

On subsequent Rust starts, a prior import's Node source inventory must still
match the verified backup; divergent Node history refuses startup.
The local journal/log cap is **128 MiB per Agent** (#158/4822). The live recovery
checkpoint stays at 32 MiB and existing shape/record limits remain; no reader or
JSON decoder accepts a 128 MiB state. After publishing a live checkpoint, at most
once per five minutes per runtime, Rust samples it to private `rust-journal-history`
for optional evidence/manual inspection with a fixed 32 KiB buffer,
fsyncs a new exclusive 0600 segment and its directory, then publishes by rename
and directory sync. History is plaintext and is never loaded, decoded, replayed
or exposed as an alternate recovery source. Any future history reader needs its
own bounded decoder/review. The bounded copy is synchronous after publication;
it may briefly occupy the actor but never delays or rolls back live publication.
Browser history keeps its existing
bounded provider pagination. A segment interrupted before publication remains
disk-only history and counts toward retention on restart.

The history byte budget reserves the full live checkpoint, its 32 MiB publication
temporary, and both 8 KiB stderr-tail files (current and temporary) within 128 MiB.
This leaves just under 64 MiB for history: at a maximum-size 32 MiB live state,
only one full-size prior checkpoint fits (with remaining room for smaller ones).
At most 64 history files are retained; the oldest sequence is retired first when
either bound would be exceeded. These are redundant old checkpoints: the live
file keeps every queued/running/uncertain receipt, pending ack/notification, goal
edit and retry ID under its existing limits. Rotation never deletes live records
or widens those limits; a full live journal still refuses new work. Under the
exclusive runtime lease, only validated UUID-named orphan journal/stderr
temporaries are retired before any startup writes and during history sampling;
unsafe orphan temporaries prevent startup, unrelated names are left alone.
A bounded metadata scan refuses unexpected history files, links, unsafe
ownership/modes or excess file count. No directory tree or history payload is
read into memory. Migration backups and image payloads are separate stores and
are not journal/log retention targets.

History is best-effort: copy/rotation/fsync failure sets fixed
`runtime.journalHistory.state=journal_history_unavailable`; it never fails or
undoes the live checkpoint. Live checkpoint write/fsync failures still use the
existing read-only/dispatch-stop path. The live recovery file is never deleted
to make room. There is no Controller
quota or machine-wide free-space reserve: N Agents may retain N times 128 MiB of
journal/log data. The conservative configured-file budgets add up to
128 MiB journal/log + 64 MiB backup payload + 64 KiB backup manifest + 256 MiB
image payload + two 32 MiB image journal files (live/publication), i.e. 512 MiB +
64 KiB per Agent, before filesystem metadata, original Node cwd files and
provider/worktree data. This is not a machine-wide disk quota. A live-checkpoint
full-disk error stops the runtime; history-only failures do not. Retention lasts until oldest-first rotation or Agent
private-root deletion; conversation text remains plaintext for that duration.

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
Accept errors retry after 50 ms rather than ending the provider/server;
fixed diagnostics include an aggregate error count, at most once per five seconds.
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
resumed reads, not independent wall-clock eviction. A fixture-only bounded
worker barrier exercises directory cancellation: two cancelled HTTP requests
retain both worker slots, a third request gets 429 after the bounded wait, and
new requests succeed after those exact workers exit. The barrier is absent
from default release builds.
These limits bound admission; they do not authenticate local callers or isolate
a same-account filesystem writer. A blocked OS filesystem operation can still
outlast the HTTP request; the two directory worker permits remain held. A
stalled blocking worker can also delay Tokio runtime teardown; signal/HTTP-drain
deadlines do not bound stalled OS filesystem calls.

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
