# Claude stream adapter (#231)

This isolated source successor adds a Claude `stream-json` backend selected by
`AGENT_UI_HARNESS=claude`. It does not change any running Agent, saved startup,
shared runtime pointer or existing Codex artifact. Security's current Node
launcher remains authoritative until a separately pinned migration is executed.

The backend launches the build-time native Claude binary with the same print,
verbose, stream input/output and partial-message flags as the Node adapter.
`TREER_SPIKE_BUILD_CLAUDE` selects an absolute native binary at build time;
its version and bytes must be inventoried in a future artifact manifest.
`CLAUDE_BIN` and `RUST_SPIKE_CLAUDE_ARGS` are fixture-feature overrides only.
No native history, configuration or credential files are inspected by this code.

## Turn and durability rules

Initialize and interrupt use correlated `control_request`/`control_response`
IDs. A user input is dispatched only after the existing durable receipt intent,
and its accepted turn identity is captured before waiting on stdin. Stream
message/block identities update stable items in arrival order. Tool results
enrich their original `tool_use.id`; they never introduce a phantom user turn.
Local user insertion covers providers that do not echo input. Final assistant
summaries retain completed tool output; conflicting terminal identity/content
refuses the transport. Only `result` settles the turn. An earlier system idle
notification cannot drain the next queued prompt. An interrupt marks the turn
interrupted only after its matching successful control response; refusal leaves
the provider's normal result authoritative. A late response cannot mark a newer
turn interrupted.

The existing notification batching, receipt pruning/pressure, operation-ID
dedup, durable-before-ACK and retry paths are shared. The Claude journal has an
explicit harness tag and atomically stores the bounded mapped transcript with
receipts. A different harness refuses that journal. Restart never replays an
accepted user or tool result: an unfinished running receipt becomes uncertain
and fences dispatch, while its observed tool output remains visible.

Frames are at most2MiB, with a60-second absolute deadline after the first bytes;
JSON depth/node/duplicate-key checks use the bounded decoder. A streamed text
block is at most1MiB, tool input128KiB, and a turn at most2048 items/4MiB before
the common50-turn/16MiB transcript admission. Backpressure remains lossless.
The existing128MiB on-disk journal/history policy is unchanged.

## Existing Node Claude session import

`--import-node-state` is rejected for Claude. The distinct one-time
`--import-claude-state` requires `RUST_CLAUDE_IMPORT_SNAPSHOT` naming a private
0600 supported-API snapshot envelope, captured during a natural idle checkpoint.
The source queue/thread files are backed up byte-for-byte using the existing
stopped-state import machinery. Its common Node receipt decoder preserves all
IDs/status/errors/fingerprints; it does not turn a Claude session into Codex.
Publication is atomic, after a second source inventory check. No active receipt,
pending ACK/notification, attachment reference or active goal is imported.

Envelope fields are exactly `version:1`, `harness:"claude"`, `agent`, canonical
`cwd`, `threadId`, `queueSha256` and `snapshot`. Snapshot must explicitly show
the same Claude session, ready/idle, null active turn/error/goal/goalDelivery,
empty queue/background tasks, and at most50 terminal mapped turns with unique
turn/item identities. Its queue hash binds the stopped Node source inventory.
Missing identity, active/unknown state or mismatched queue refuses; no fresh
session fallback is allowed. The launcher itself never retrieves private
provider history. A supported snapshot is a recent window, not an assertion of
complete native history. An older prefix is marked `historyUnavailable` and
has no fabricated provider cursor. A new-session startup without prior state
uses an explicit generated session ID; an existing session ID without a
supported imported checkpoint refuses.

The leader must inventory actual optional-feature use, capture/hash the bounded
supported snapshot and every durable source receipt, prove target stop/exits/no
holders, and preserve exact original startup plus a new stopped backup. Remove
the import flag/snapshot environment after successful verification, without
restarting. Rollback may reuse unchanged Node source only before new Rust work;
after new ingress it requires explicit same-thread state reconciliation. No
automatic migration, restore, restart or original-error clearing is authorized
by this source change.

## Honest capability differences and remaining delivery

The adapter advertises text prompts, queued notifications, transcript observation
and abort. Images, busy steering, background tasks, goals/budgets, model/effort
changes and session transfer are currently unsupported. Older Claude history
cannot be paged through a supported provider API here. These are concrete target
admission checks, not a claim of complete Node parity. File-change input is
rendered as a bounded replacement diff; native structured-patch display parity
is not claimed.

Focused checks use `tests/claude-entry.mjs` with the synthetic producer
`tests/fixtures/claude-stream.mjs`: two queued turns across early idle, terminal
tool retention and ordinary restart, restart between tool result/result with
uncertainty preserved, correlated interrupt, and supported-snapshot import with
same-session new turn and exact Node queue backup. No installed provider or live
Agent is invoked. Protocol unit tests cover terminal/identity conflict, malformed
frames and idle/import admission. Exact native artifact freeze, focused peer
review, current Security target compatibility and operational migration acceptance
remain separate delivery steps; source/fixture results do not imply them.
