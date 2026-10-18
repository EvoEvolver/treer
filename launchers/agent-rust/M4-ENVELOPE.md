# Launcher handoff envelope v1 — schema review gate

**Migration proposal parked, not approved, per #158/4790.** Scope reset 4767 was
retracted by 4773. Only migration was simplified: one-way Node import after byte
backup, manual restore, and switching only while idle with an empty queue. Quota
and detached reaping remain open pending the user. The offline validator remains
tested but is not the import format or a production activation path. Current
behavior is documented in [README](README.md); this document approves no switch.

Concrete proposal for #158, following [M4](M4-DESIGN.md). This is the separate
Security/Core schema gate requested in 4600/4610/4611, not an implemented API.
All numeric limits below are proposed for review, not Controller defaults.
No exporter, importer, provider-file reader or real-session migration is enabled
by this document. Supported direction is Node/Codex -> Rust/Codex -> Node/Codex;
an older destination without envelope-v1 support is refused before cutover.
Security schema verdict 4682 permits synthetic validators/converters only;
Core authority/cap values remain open. This protects against stale, replayed,
misordered and accidental handoffs. A malicious process under the same OS account
can still read these private directories; same-account isolation is out of scope.

## Representation and bounded parsing

Use a directory capability supplied by Controller, containing one `manifest.json`
and an `objects` directory. Objects are uncompressed regular files named only by
lowercase SHA-256 of their exact bytes. The manifest contains no paths or inline
object bytes. A digest selects a child relative to the pinned objects descriptor;
it is never joined to an input-supplied directory or canonicalized outside it.
Open every component no-follow; require current owner, private permissions,
one hard link and regular files, and compare opened inode metadata before/after
reading. A write racing the read fails the digest check. Do not adopt an input
inode into destination state; write a new exclusive destination file instead.

| Limit | Proposed envelope-v1 value |
| --- | --- |
| Manifest wire bytes | 1 MiB |
| Total object bytes, including retained images | 384 MiB |
| Individual metadata object | 32 MiB |
| Individual image object | 10 MiB |
| Objects | 8,200 |
| Submission records / pending records | 8,192 / 256 |
| Goal edits / pending edits | 1,024 / 256 |
| Images / total image bytes | 8,192 / 256 MiB |
| Source-message checkpoints per submission | 256 |
| Completion members per notification batch | 512 |
| JSON nesting depth / total JSON nodes per metadata object | 32 / 500,000 |
| Maximum string bytes | 100,000, except the <=256 KiB legacy Rust fingerprint; narrower bounds below |

Reject unknown fields, duplicate JSON keys at every depth, invalid UTF-8,
non-finite numbers, non-integer counters, negative counters, trailing JSON and
unknown enum values. Integers must be <= 2^53-1 for Node interoperability. Enforce
wire-byte and nesting limits BEFORE allocating a recursive JSON value; count
nodes during parsing. Never parse image bytes as JSON. Sum checked object sizes
before reading objects; stream hashes/image copies with a <=64 KiB buffer. No
archive extraction, compression, remote URL, symlink, hardlink, FIFO or device.
No arbitrary backend extension map: a new field needs a schema version/review.
Use a custom serde visitor (bounded_json.rs), which detects decoded duplicate
keys and consumes a shared value/key counter before descending. Add an
envelope-specific string admission bound; do not assume serde_json::Value alone
enforces these requirements. Tests exercise each boundary and one over.

Parse, validate and drop ONE metadata object at a time. Retain only bounded
cross-object ID/digest summaries, never all five decoded trees at once. Stream
the 384 MiB payload through the <=64 KiB hash/copy buffer. A 256 MiB live-heap
regression ceiling is proposed for the offline validator, excluding provider and
destination runtime; it must be measured with maximum-shape fixtures before a
full validator freeze. This is a test budget, not an OS-enforced memory quota.
The manifest-only increment reads at most 1 MiB and does not read any object.

## Manifest fields (all required, no additional properties)

| Field | Type / rule |
| --- | --- |
| `format` | Literal `treer.launcher-handoff.v1` |
| `agent_id`, `machine_id` | ASCII `[A-Za-z0-9_-]{1,128}` |
| `handoff_id` | UUID, issued/stored by Controller |
| `source_generation`, `destination_generation` | Positive safe integers; destination strictly greater |
| `source_backend`, `destination_backend` | `node-codex` or `rust-codex`, different values |
| `source_schema`, `destination_schema` | Literal `launcher-envelope-v1`; not raw journal version |
| `thread_id` | Nonempty opaque string, <=256 UTF-8 bytes, no controls |
| `dispatch_fenced` | Boolean; true if any uncertain dispatch/goal/ack remains |
| `objects` | Unique digest/size/kind entries, sorted by digest |
| `submissions`, `goals`, `images`, `notifications`, `message_acks` | SHA-256 references to exactly one JSON object of each corresponding kind |

Each object entry has exactly `sha256` (64 lowercase hex), `bytes` (positive
safe integer) and `kind` (`submissions`, `goals`, `images`, `notifications`,
`message_acks`, `image`). Only one metadata entry per kind, exactly five in all.
Every object must be reachable from the manifest/metadata; reject missing,
unlisted, multiply typed, conflicting or unused objects. Two image IDs may refer
to the same image bytes, but each logical reference retains its independent
retention/accounting identity. Referenced image MIME must agree with structural
validation by the existing image admission code. An object hash alone provides
integrity, not authenticity or authority.

The manifest does NOT contain a token, credential, filesystem path, launch argv,
environment, provider-home location, stderr, transcript cache or raw provider
session files. Provider thread identity is a reference, not permission to inspect
its home; actual provider-file transfer is a separate authorized protocol.

Envelope plaintext contains receipt text, previews and goals. Its directory is
0700, files 0600, and all copies count against private-byte quota. Proposed
retention is at most 24 hours after verified activation, with an operator-approved
cleanup operation scheduled at issuance; deletion must verify exact handoff and
generation ownership. Until deletion is verified, the bytes stay charged. A
failed or uncertain activation retains evidence pending explicit recovery,
fences new handoffs after 24 hours, and alerts the operator to select bounded
retention/cleanup; it never silently deletes the source or releases accounting.
No envelope content is copied to diagnostics. Core must review retention/expiry
with quota accounting before activation can be implemented.

## Accepted work and cross-backend representation

Each metadata object has exactly `version: 1` and `entries: [...]`, except goals,
which additionally has `current` (nullable typed goal below). Empty lists are
required rather than omitted. References within metadata are IDs/digests, never
paths. Array order of submissions is durable queue order; never sort it by time.

Submissions contain these required fields:

- `id`, `operation_id`: nonempty strings <=256 bytes, no controls; unique in
  their respective namespaces. Preserve IDs byte-for-byte, including goal-edit
  operation IDs containing `/`. A destination may not mint replacement IDs.
- `fingerprint`: a string, and `fingerprint_scheme`: `node-v1` or
  `rust-spike-v1`. Node-v1 is 64 lowercase hex SHA-256. Rust-spike-v1 is the
  exact JSON-array string used by runtime.rs, at most 256 KiB, containing
  text/delivery/sender/notify and image metadata when present; it is NOT a hash.
  Parse and validate its duplicate keys/depth/fields under the same limits.
  Preserve the original fingerprint and its exact algorithm.
  Export verifies it against the source's existing implementation. Import must
  verify the same algorithm, not recompute only its native algorithm; operation
  replay is compared using that scheme. Unsupported schemes refuse migration.
- `text`: <=65,536 UTF-8 bytes; `delivery`: `queue` or `steer`;
  `sender`: null or exactly `{agent_id,path,name}`, each <=1,024 bytes/no controls,
  path absolute. It is stored attestation, never new authorization for import.
- `created_at`: RFC3339 UTC timestamp with millisecond precision;
  `text_preview`: <=1,024 bytes; `assistant_preview`: null or <=4,096 bytes;
  `error`: null or <=4,096 bytes. Do not emit these values in diagnostics.
- `state`: `queued`, `running`, `done`, `failed` or `interrupted`;
  `turn_id`, `prior_turn_id`, `turn_scope`: null or opaque <=256 byte IDs.
  `position`: null for nonqueued states, positive safe integer for queued.
- `acceptance`: exactly `{id,disposition,position}`; disposition `queued`,
  `sent` or `steered`, id equals submission id, position null or positive safe
  integer. This immutable receipt is distinct from current queue position.
- `uncertainty`: `none`, `dispatch`, `goal` or `notification_ack`.
  A running record and any source dispatch fence must remain fenced, even if a
  provider snapshot claims idle or an old turn is terminal. Migration never
  settles a receipt, clears a fence, sends a prompt or retries a goal mutation.
  Converters derive the destination fence from actual records/goal/ack state;
  a false manifest declaration cannot clear any running or uncertain record.
- `image_ids`: unique ordered IDs, at most 8, aggregate <=24 MiB per submission;
  `notify_on`: unique sorted subset of terminal outcomes;
  `notify_to`: null or <=1,024 byte recipient. A nonempty on-list needs recipient.

Preserve source fingerprint inputs (including source attachment-ID order and
notify/sender normalization) exactly. Initial exporter rejects a source whose
existing journal is internally inconsistent; no preview truncation, state repair
or automatic ID normalization is part of migration. Reject the whole handoff
when a target cannot preserve a field/semantic, including goal-exit behavior,
batch notification identities or an unsupported goal budget. Preserving an
opaque unvalidated blob is not an acceptable substitute for implementing it.

Notifications are separate entries with exactly `delivery_key`, `recipient`,
`submission_ids`, `turn_id`, `status`, `assistant_preview`, `error`, `attempts`,
`next_attempt_ms`, `delivered`. Preserve the original delivery key (<=256 bytes),
recipient, batch membership and delivered flag; IDs must resolve to terminal
submissions whose recipient/outcome agrees. Attempts are nonnegative integers,
next attempt is a nonnegative safe integer, previews/errors have bounds above.
One active batch per key, no conflicting key/membership. Delivered notifications
are not resent. Lost acknowledgement may still repeat with the SAME idempotency
key under the existing at-least-once semantics; do not claim exactly-once.

Message acknowledgements contain exactly `submission_id`, `message_id`,
`delivery_id`, `ack_pending`, preserving all checkpoints (including Node batches).
Message/delivery IDs use `[A-Za-z0-9_-]{1,256}`. Reject duplicate checkpoint IDs
with different submission or acknowledgement state. Ack only after that accepted
submission is durable in the ACTIVE destination generation. No acknowledgement
is emitted while importing or while a generation is quiesced/fenced.
Reauthorize stored notification recipients, sender-dependent routing and goal
actions under CURRENT Policy at send time in the active generation. Stored
attestation and an old allow decision never grant authority after handoff.

Goals preserve typed `current` plus every edit. A goal includes exactly `id`,
`objective`, `status`, `token_budget`, `round_budget`, `tokens_used`, `rounds_used`,
`time_used_seconds`, `rejection_count`, `consecutive_rejections`, `reason`,
`required_action`, `notification_error`, `sync_status`, `sync_error`,
`native_objective`; absent source values
normalize to null, never invent budget/usage. Objective <=100,000 bytes,
native objective <=100,000, reason/required action/errors <=4,096;
status is `active`, `paused`, `blocked`, `stale`, `done`, `cleared`, `usageLimited`,
`budgetLimited` or `complete`; sync status is null, `synced`, `desynced` or
`exit_unconfirmed`. IDs use the rules above. Budgets are null or positive
safe integers (round budget <=10,000); usage null or nonnegative safe integers.
Time-used seconds is null or a finite nonnegative number <=2^53-1.
An edit has exactly `id`, `input`, `previous`, `goal`, `applying`, `applied`,
`delivery_status` (`pending`,`sent`,`failed`), `prompt_id`, `mode`, `attempt`,
`error`. Input allowlists the existing goal update fields; previous/goal are
typed nullable goals. `sent` implies `applied`, `applied` implies `applying`.
Applying-but-unconfirmed edits retain uncertainty and fence mutation until an
explicit reconciliation; import never performs that reconciliation. A destination
must explicitly support each nonnull field, including round budgets.

Images contain exactly `id`, `sha256`, `mime`, `bytes`, `session_id`, `retained`,
`created_ms`. IDs match `img_[a-f0-9]{32}`, session equals manifest thread identity;
MIME is PNG/JPEG/WebP/GIF, size/digest agrees with the object table, created time
is a nonnegative safe integer. Retention survives migration, including image-only
prompts and drafts. Refuse expired/missing drafts instead of deleting them as a
side effect of export. Preserve every retained image reference; no garbage
collection is authorized by handoff. Raw source file paths/extensions never cross
the envelope; importer chooses a fixed destination filename from validated IDs.

## Controller authenticity, exclusion and crash authority (Core proposal)

Use a Controller-held durable handoff record, not a signed caller JSON blob.
Only an operator-authorized management operation may create it; Agent credentials
cannot mint/activate a handoff. Record fields bind operation UUID, Agent/machine,
source/destination generation/backend, manifest SHA-256 and total object size,
current stage, issued/expiry times and token hash. Generate 32 random token bytes;
deliver only to the one intended handoff worker over authenticated local control,
never via envelope/argv/environment/profile/log. Hash token at rest; compare in
constant time. Proposed lifetime is 15 minutes from issue to activation. Core
must select the actual credential transport and approve this lifetime.
Worker identity must be bound to an authenticated process generation via an
inherited descriptor or peer-credential-checked socket chosen by Core, never a
bearer token in environment, argv or a file. At operator issuance, manifest hash
and size are unbound. Bind them once at `exported`, reported by the authenticated
source worker after durable export; then freeze that binding for destination
validation/activation. A changed digest needs a new operator-authorized handoff.

The worker presents token+handoff id+manifest hash through the versioned control
channel. Controller validates the persisted tuple/current generation, verifies
every object digest, persists destination publication, then atomically consumes
the token AND activates the new generation. Same-operation retries return the
persisted result without creating a new active generation; a different digest
for the same operation is conflict. Expiry never undoes published bytes or
frees quota: it fences activation and requires explicit recovery/new token.
All requests authenticate the *process generation*, not just a shared Agent key.
The capability is absent today; neither backend can activate until Core ships it.

| Persisted stage | Which generation may mutate / restart? |
| --- | --- |
| Before quiescing | Source only |
| quiescing | Neither mutates; source may restart only to finish quiescing/export |
| exported | Neither mutates; source recovery requires explicit abort transition |
| destination_published | Neither mutates; only complete activation or explicit abort with evidence |
| active | Destination only; source permanently fenced |
| aborted (new fenced generation) | Explicitly selected source state under a NEW generation, never old token |

Quiescing closes admission, drains/records in-flight acknowledgements, stops the
owned provider and obtains confirmed ownership-boundary emptiness BEFORE export.
Unconfirmed cleanup means no cutover. Source bytes stay untouched/restartable
under recovery authority, not concurrently writable. Abort after destination
activation is a NEW reverse export containing newly accepted work. Every stale
source mutation, provider action, notification or ack is rejected after fencing.
No startup-file flip, file lock or generation field asserted by the launcher can
substitute for Core enforcement. Controller restart must recover the same stages.

## Required implementation increments and fault evidence

1. Review this schema and concrete Controller authority with Security/Core.
2. Implement bounded validators and synthetic bidirectional conversions with
   field-by-field preservation/refusal; no production migration entry point.
3. Integrate the reviewed Core handoff/quota/ownership capabilities. Until then,
   attempts to activate refuse with fixed `handoff_capability_required`.
4. Run synthetic Node -> Rust -> accepted new work -> Node -> restart, including
   all receipt states, image-only input, goal uncertain ack, notification lost
   ack and source-message batches. Test stale worker/token/generation/digest,
   every stage crash, expiry, simultaneous startup, no duplicate dispatch and
   unchanged source bytes. An unrelated process sentinel must survive cleanup.
5. Malformed tests include duplicate keys, depth/nodes/entry/byte limits,
   dangling/cross-typed digests, MIME mismatch, links/path traversal, unsupported
   fields/versions, contradictory receipts and unrepresentable downgrade state.
   Logs contain only fixed error enums, never data values or hashes of secrets.
   Scan synthetic envelopes and logs for seeded credential canaries; intentional
   receipt text is the only synthetic content allowed. No real Codex home.
   Include source values beyond destination limits (for example Rust text above
   65,536 bytes): refuse the WHOLE handoff with `handoff_unrepresentable`, never
   truncate to make it fit. Node's envelope-v1 importer/exporter and generation
   fencing require a separate reviewed production Node increment with its own
   gates; Rust-only work never satisfies bidirectional migration acceptance.

Migration/rollback is not complete until the real Controller crash matrix and
both runtime consumers pass; converter round trips alone do not establish it.
