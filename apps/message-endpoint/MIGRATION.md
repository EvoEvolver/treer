# Telegram bridge migration

This is a cutover plan, not permission to touch an existing bot or process.
The disposable-bot roundtrip passed in #159 (user-confirmed on 2026-10-19).
The test binding was subsequently paused at revision 4. That validates the
sandbox text path, not production migration, native reply continuity, isolation,
or recovery after an ambiguous external send. No production migration or
offline offset/context importer is implemented yet.

## Implementation plan (#203, for Core and security review)

This plan does not authorize a production inventory, credential read, process
change, token rotation, polling probe or send. Tests use synthetic SQLite state,
fake Core/Telegram transports and private temporary directories. Production
snapshot creation and cutover stay with the owner/release operator after a
separate authorization. Keep the old bridge working until that decision.

### Current gaps confirmed in source

| Boundary | Legacy bridge | Current hub/relay | Required before a continuity-preserving cutover |
| --- | --- | --- | --- |
| Poll position | `metadata.last_update_id`, last completed update | `telegram_accounts.offset`, next requested update | Checked monotonic `last + 1` import, no reset |
| Native inbound replies | `reply_to_message.message_id` resolves through `message_mappings` to a Core context | `telegram::Incoming` has no reply ID; `prepare_telegram_forward` constructs empty contexts | Parse and resolve a reply within the same account/chat/topic/endpoint generation |
| Native outbound replies | `first_mapping_for_contexts` selects a mapped native message | Outbound envelope carries contexts, but `prepare_telegram_chunk` ignores them and `Transport::send` has no reply argument | Pin a validated reply target in durable send intent; retry with the same target |
| Actual internal delivery ID | Legacy inbound sender directly receives Core's accepted message ID | Hub's `inbound.core_id` is the fragment handoff to the relay, not its eventual message to the fixed recipient; `relay_forwarded` does not retain that final ID | Durable relay delivery receipt and a separately validated final Core ID; never mistake a fragment ID for it |
| History | `message_mappings` preserves stable Core/native IDs | `telegram_sources` describes a source but is pruned with inbound retention | Explicit historical mapping store/retention, without copying bodies or impersonating old authors |
| Rollback after new traffic | Old offset and mappings alone describe only old traffic | New traffic has new relay authors and new native receipts | Reverse journal plus reconciliation; restoring an old database alone is unsafe |

The planner currently reports these as blockers; a drained local queue is not
proof that the old Core mailbox is drained or an external send did not happen.
An empty new hub is not a lossless replacement for historical mappings.

### Stages and acceptance

1. **Contract and rehearsal fixtures.** Review this plan and define the mapping
   and receipt contract before implementing the trust boundary. Record old/new
   IDs, offset semantics and failure states in synthetic fixtures. The original
   Core message author and ID remain unchanged: importing history never sends a
   replacement message or makes the new relay its author. Disabled legacy
   bindings become paused candidates; runtime registry rows, not config seeds,
   are authoritative.
2. **Native reply continuity.** Implement the inbound/outbound reply mapping and
   durable App-asserted final-recipient receipt, with an explicit capability check between
   the upgraded hub and relay. Older or mismatched runtimes fail closed for
   migration/receipt-dependent activation; basic #159 transport success does
   not establish the capability. Persist receipt intent and result before
   acknowledgement. Duplicate receipts are idempotent; a conflicting ID/digest,
   wrong Core sender, recipient, endpoint generation or workspace is rejected.
   A receipt is App-asserted until independently checked against Core, never a
   user read receipt or an isolation attestation.
3. **Stopped-state forward importer.** Extend the reviewed hub executable with
   an offline operator mode, not a third daemon or a browser SQL endpoint.
   Accept a closed, private immutable legacy snapshot and explicit private
   mapping document into an existing fresh, paused hub state. Reuse the hub's
   actual schema and exclusive state lock. Refuse live/WAL sources, symlinks,
   hardlinks, broad permissions, unsupported schema, foreign identities,
   duplicate/conflicting mappings, unknown delivery states, unfinished work,
   oversized inventories and changed source fingerprints. A missing mapping,
   required operator acceptance/evidence or final offset is a blocker, not a default.
4. **Reverse plan and interruption rehearsal.** Produce a private reversible
   journal with source/destination release and snapshot fingerprints, imported
   key ranges, pre/post offsets, stable IDs, native receipts and transaction
   outcome. Do not export message bodies, tokens, sessions or claimed human
   identities. Rehearse interruption before/during/after commit, repeat import,
   stale/corrupt journals and conflicting native receipts. Automatic rollback
   after new traffic remains disabled until the reverse path is separately
   implemented and reviewed. Containment is a valid result, with outage stated.
5. **Operator cutover.** Only after the preceding tests/reviews, obtain one
   concrete release authorization naming the production bot, exact old/new
   process identities, versions, snapshots, allowed chat scope and rollback
   boundary. Execute the cutover checklist below. The old consumer must be
   proved stopped before the new consumer starts. Reject competing pollers;
   never use `getUpdates` as an inventory tool or rotate a pin to bypass errors.

Each stage is a separately frozen small change. Stage 1 is documentation and
synthetic rehearsal only; stages 2–4 need Core/security review of their concrete
contract and code. Production activation and retirement require their own
decisions, even after a green code review.

### Proposed import and mapping invariants

- Bind every row to the verified bot/account, exact chat/topic, newly created
  relay stable ID and generation, and fixed internal recipient. No alias or
  path is authority. No history is visible across another binding or generation.
- Preserve old Core IDs, native IDs and direction; never manufacture accepted
  receipts, rewrite authors, replay completed deliveries or translate an old
  delivery ID into a new mailbox delivery. Unknown/ambiguous sends require an
  explicit private reconciliation entry before either consumer resumes.
- Import the next offset monotonically. Reject an import that would regress an
  existing account offset or move past unfinished legacy work. Never set offset
  to the largest row merely because a database contains it. Aged updates and
  the hub's ten-minute stale cutoff need an explicit backlog decision.
- Import only terminal historical mappings and bounded metadata, not bodies,
  credential files, OAuth state, executable paths, active leases or activation
  flags. Bindings remain paused; token provisioning remains human-owner-only.
- A transaction writes history, offset and its journal marker together. Repeating
  the same operation against the same fingerprint is a no-op; changed source,
  bindings or output state fails rather than overwriting. An unfinished journal
  cannot be treated as a successful import.
- Historical references resolve only to Core messages that the current fixed
  recipient may access. Do not grant access by trusting a SQLite `core_message_id`
  or a relay claim alone. Missing or unauthorized history must not silently turn
  a requested reply into a new top-level message; return a fixed actionable
  failure and keep it for reviewed recovery.
- All private artifacts remain 0700/0600 outside a checkout. Ticket evidence
  contains statuses, counts, field names, release hashes and masked identifiers
  only. A file permission check does not isolate other same-UID Agents.

### Core contract findings and proposed next boundary

Core review #203 event 5774 accepted the staged plan, not a new receipt/import
contract or production activation. Its source review establishes these blockers:

- `treer message get` returns bodies and requires the caller to be an original
  sender or recipient, in addition to Policy. The hub cannot independently
  verify an otherwise invisible relay-to-recipient message. Broader
  `message.read` Policy alone does not remove this store-level check. A relay
  receipt or digest remains an App assertion, not Core verification.
- A new relay can attach a historical context only if **that relay** can read
  the parent and Policy permits it. The fixed recipient's access does not grant
  the relay access. A context edge does not grant its recipient access to the
  parent either. Importing SQLite IDs cannot transfer historical participation.
- Core receive has no consumer lease: it selects unacknowledged, unexpired
  deliveries. App worker leases are separate. Existing list/receive returns
  bodies; there is no count-only mailbox probe. A zero count is a momentary
  observation, never a fence against later sends or another poller.

### First-cutover contract (security 5775, leader decision 5777)

These gaps do not require new Core APIs for a first, explicitly bounded cutover.
Leader selected the new-context cutoff below as the implementation direction.
The final release authorization must repeat the historical loss; this decision
is not production activation permission.

1. **App-asserted receipts.** Keep the pinned relay's final-message receipt as
   `verification: app_asserted`, with the UI saying “not independently verified”.
   It is neither a read receipt nor Core proof. Use it only for reply mapping in
   the exact account/chat/topic/endpoint generation and fixed-recipient binding;
   never as authorization, as a reason to widen visibility, or across bindings.
   Do not add the hub as a co-recipient. A compromised relay can poison its own
   mapping; record this accepted limitation explicitly in the release decision.
   Future independent receipt checking is separate Core work, scoped to an
   authorized exact relationship/message ID, body-free, rate limited and audited,
   with no out-of-scope existence oracle.
2. **Chosen history boundary.** Leader decision 5777 selected a
   **new-context cutoff** for this migration. This loses historical native
   reply continuity and must name that loss in the owner's release authorization.
   Preserve old IDs/authors and old read-only history; an old native reply must
   return a fixed actionable history-unavailable result, not silently become a
   top-level message. Leader explicitly excluded a continuity grant from this
   task. Any later request for one would be separately designed by Core, outside
   this migration. No App-created grant, impersonation or body copying.
3. **Existing-identity drain report.** No new status route is needed solely for
   this migration. After owner/release disables old admission and fences senders
   (redirect them or restrict their Policy access to the old bridge), run a
   report-only check using the old bridge's own authorized identity and mailbox.
   Observe an empty receive result and `remaining_unacknowledged: 0` twice a few
   seconds apart. Never print/persist bodies, acknowledge, forward, poll Telegram
   or borrow that identity in the new hub. Record counts/times only. Core has no
   consumer leases; reconcile legacy App worker leases/outboxes and ambiguous
   sends separately. Stop the old poller before starting the new one. Repeat the
   old-mailbox report after cutover for stragglers under the authorized operator's
   control. Neither sample is a fence; nonzero/ambiguous evidence blocks progress.

The implementation must distinguish **missing independent Core verification**
from **missing operator acceptance**. The inventory remains report-only and
`ready_for_cutover:false`; it cannot accept App-asserted receipt risk, authorize a
history cutoff, run the old identity's mailbox check, or prove senders fenced.
The planner emits the decided first-cutover contract and unresolved operator
requirements, rather than treating unavailable independent checks as a demand
to implement three new Core APIs. A future reviewed
cutover validator must require the explicit owner decision and operator evidence,
not silently mark those codes resolved because queues are terminal.

Next runtime increment: scoped App-asserted receipt dispatch, native reply
handling, unknown historical replies, report-only drain samples and stragglers,
with the actual native reply/receipt integration separately frozen and reviewed. No automatic
post-traffic rollback is available; keep containment and manual reconciliation
explicit until a reverse journal is implemented and reviewed.

### Stage 2A: scoped receipt wire contract (not runtime-enabled)

`src/telegram_receipts.rs` defines the first local contract slice. There is no
runtime dispatcher, HTTP endpoint, sender, capability negotiation or production
activation wired to it yet. The separate local storage slice below is not invoked
by the hub runtime either. Runtime support must come back for
review before deployment; exporting the Rust module advertises no live capability.

- Capability/protocol string: `treer.telegram.app-asserted-receipts.v1`.
  Mixed/older peers must explicitly negotiate it before receipt-dependent use;
  no fallback to fragment Core IDs or inferred support from version strings.
- Strict JSON, at most 4096 bytes, no duplicate/unknown fields. Verification is
  the single enum value `app_asserted`; `core_verified` is rejected. The future
  UI label is always “not independently verified”. No bodies or credentials.
- Scope includes account ID, chat ID, optional topic ID, endpoint stable ID,
  generation and fixed-recipient stable ID. The handoff also binds job ID,
  payload SHA-256 and original native message ID. The final Core message ID is
  asserted by the relay, not independently read or proven by the hub.
- The caller supplies expected scope/job/digest/native ID from the existing
  binding and durable handoff, not the receipt. The actual sender is read from
  the outer Core envelope; it must equal the pinned endpoint. Parse only after
  that Core envelope has passed existing mailbox sender routing.
- Parsing does not create a mapping or change acknowledgement state. Storage makes exact replay a no-op and changed final ID a conflict, preserving
  binding/generation boundaries. Runtime must atomically commit handoff completion
  with the mapping before ack; capacity is not a reason to hold delivery.
- Resolving a reply never grants context visibility. Core must still reject a
  parent unreadable by the sending relay. For pre-cutover/unknown native replies,
  use an explicit history-unavailable failure; do not silently drop the context.
  Unreadable-context and mixed-peer integration tests remain pending runtime work.

Synthetic conformance tests cover exact-scope success, cross-chat/topic/account/
endpoint/generation/recipient and wrong-sender refusal, job/digest/native-ID
conflicts, invalid Core ID, false verification upgrade, unknown protocol/fields,
duplicate JSON and oversize rejection. These establish only parser boundaries,
not native threading, durable delivery or a completed migration rehearsal.

### Stage 2B: local receipt mapping storage (not runtime-enabled)

`src/telegram_reply_store.rs` adds SQLite mapping helpers using the existing
private `Store`. This is local storage only: no receipt acceptance route, Core
refetch, migration importer, live capability or running worker uses it yet.

- The key is account/chat/canonical-topic/endpoint/generation/recipient/native ID.
  A no-topic transport sentinel of zero normalizes to `None`, stored as SQL zero;
  receipt JSON with `topic_id:0` is invalid. Positive topics remain distinct.
- Record re-parses the strict receipt against the trusted handoff. Exactly equal
  ID/job/digest replay is a no-op; changed final ID/job/digest or reuse of a job
  for another native message is a conflict. The old row stays intact. Only a
  saturating aggregate conflict counter is committed; no body or ID is audited.
  Future dispatch must hold the handoff unacknowledged on conflict.
- Retain a ring of 1000 rows per **stable endpoint across all generations**,
  not 1000 per historical generation. Generation churn cannot grow one endpoint
  beyond its reservation. At most 20 retained endpoint slots bound the aggregate
  to 20000 rows. Eviction removes that endpoint's oldest row only (creation time,
  then insertion order), counts evictions and makes old replies unavailable.
- A new 21st endpoint cannot evict another endpoint's history; it returns the
  explicit `NotRetained` result, never `Full`. Runtime must explicitly retire an
  obsolete endpoint slot through the reviewed retirement procedure before
  replacing it. This bounds endpoint-ID churn as well as generation churn; slot
  retirement now runs inside the existing audited binding-deletion transaction:
  paused/drained checks and the stable-ID tombstone precede removing all of that
  endpoint's generations and incrementing the saturating `retired_rows` count.
  Failure rolls back the entire deletion; no new purge route exists. Hub startup
  migrates history counters before deletion is admitted. A stale retained slot
  may degrade a newly added endpoint's reply history until explicitly deleted,
  but must not block its primary messages or alter another endpoint's mappings.
- The caller supplies durable handoff state. A terminal handoff returns
  `IgnoredTerminal` without recreating history, even if its mapping was evicted
  and the new assertion changes the final ID. If its row is still retained, a
  changed valid assertion increments only the aggregate `terminal_mismatch`
  count; malformed/wrong-sender receipts are rejected before this handling.
  Runtime must derive this state from
  authoritative completion/dedup and atomically commit completion with mapping;
  this local helper alone cannot make the combined crash guarantee. Never derive
  replay admission from absence in the ring.
- Retention exhaustion is non-blocking for primary delivery. Mapping conflicts
  remain a separate bounded, operator-visible recovery hold; actual SQLite or
  private-storage failure follows the hub's storage health boundary. Do not
  collapse capacity degradation, conflicts and storage failure into one indefinite
  retry. Even when primary messages continue, an unresolved native reply remains
  explicit history-unavailable/hold, never silently converted to a top-level send.
- Lookup requires the complete valid scope; unknown/pre-cutover/cross-binding
  references return unavailable. A returned ID is still only an App assertion.
  A compromised relay can assert any syntactically valid ID, including an
  unreadable one: Core's later send checks, not the parser/store, enforce parent
  visibility. Preserve unreadable-context failure/hold; do not omit the context.
- Future dispatch verifies outer Agent principal kind, workspace/mailbox and
  pinned stable ID before invoking the parser/store, with expected values from
  durable handoff/current binding. Neither local helper authenticates its caller.

Local SQLite tests cover replay, conflicting final ID and job reuse, aggregate
counter persistence, reopen under private-store checks, cross-scope/unknown
lookup refusal, no-topic normalization, ring eviction/newest retention, unrelated endpoint preservation, generation
churn, terminal replay after eviction and explicit slot exhaustion without Full. Native send/ack ordering, actual Core negative
contexts, peer negotiation and drain/straggler rehearsals remain runtime work.

### Stage 2C: atomic authoritative queue completion (not dispatched)

`src/telegram_receipt_completion.rs` adds `Hub::complete_telegram_receipt` for
future negotiated receipt mode. Core workers do **not** call it yet. Existing
basic transport still uses its legacy fragment-handoff completion; that path
is now durably exclusive with receipt mode. A bounded per-inbound-job mode row
is pinned before the lease/frame preparation; the mode never changes on retry.
An atomic one-time schema step backfills every existing unpinned inbound row
(including never-leased, leased and terminal jobs) as legacy. Existing explicit
mode pins remain immutable. In that same step a trigger starts recording fresh-job
provenance for future inserts. Only this explicit durable provenance can authorize
initial mode selection; absence of history or provenance always means legacy.
Provenance survives restart until consumed by a mode pin; a previously leased job
still cannot upgrade. Schema initialization never reclassifies fresh jobs on reopen. A future importer
must explicitly pin imported historical jobs as legacy and consume their fresh-row
provenance in the import transaction; insertion time alone does not make historical
transport work eligible for negotiation. Both completion
paths reject the other mode even after terminal completion or store reopening.
The mode row is removed with its inbound job, never retained as unbounded history.
Only internal development preparation can select receipt mode until runtime
negotiation is wired and reviewed. No live capability or activation is
advertised by this increment.

The completion transaction takes SQLite's immediate write reservation and reads
the actual inbound job, current endpoint generation/recipient, non-deleted
binding, current Telegram routing and persisted source metadata. It checks the
private lease against the trusted local `CoreIntent`, reconstructs the expected
job/digest/native scope from durable data (not receipt JSON), validates the outer
Agent sender and recipient against the pinned endpoint/hub, then strictly parses
the receipt. The store's pinned workspace must match this Hub's workspace. The
Core delivery comes from this hub's authenticated workspace mailbox; the current
Core projection does not contain an independent workspace field, so this helper
must never be called with a client-deserialized or foreign-mailbox delivery.

Mapping insertion/eviction or `NotRetained`, terminal queue completion with the
App-asserted final Core ID, and handoff-health reset commit together. Success
returns **after commit**; the eventual Core worker acknowledges only afterwards.
A terminal replay reads durable completion, so removal of its ring history does
not recreate it. Pause after dispatch may not erase an accepted result, while a
changed generation, lease, route or recipient is refused. A conflict commits
only the aggregate counter and a `receipt_conflict` pause/reason; the original
bounded queue row remains pending. It returns an error, leaving Core unacked.
No second body/receipt queue is created. The reason is visible through the
existing binding view; explicit conflict inspection/recovery before resume is
still required before runtime activation.

Development tests cover SQL failure **after eviction but before completion**
(rollback of both history and queue), two concurrent callbacks through the Hub
actor, stale generation/lease/routing and wrong outer-principal/workspace
rejections, capacity degradation without blocking completion, bounded conflict
pause and stable revision, private-store close/reopen after commit without ack,
and terminal replay after history removal plus reopen. The failure injection is
a database abort, not a killed-process/WAL crash or a live Core ack failure.
Those stronger runtime cases, receipt production/capability negotiation,
authoritative restart correlation, unknown/unreadable native replies, explicit
conflict recovery and legacy mailbox drain remain acceptance work. A returned
Core ID is still an App assertion, never independent delivery/read verification.

### Security gates still required before a production decision

- **Token rotation (leader 5796/5798):** required now because the old production
  token appeared in the leader's conversation, and again at cutover. The human
  user regenerates it in BotFather and uses the existing write-only settings page;
  bridge owner confirms polling/health and replacement using status only. As of
  baseline 5800, token replacement/revocation is not confirmed. Do not read the
  transcript or token. Check old config backups by metadata/path only and record
  the rotation date/result on #203. The subsequent cutover installs a second
  fresh token through the hub's human-only path. If rolling back then, the owner
  provisions that new valid token into the old bridge; never reuse a revoked
  token. A further retirement rotation is only needed if another valid credential
  copy may remain exposed.
- **Bound inputs:** the private cutover journal binds SHA-256 of the closed
  snapshot and explicit mapping document, the exact old process verified stopped,
  exclusive destination hub state lock, and source fingerprint recheck at commit.
  Public inventory hashes alone are not a substitute for input file hashes.
- **First-day watcher:** release authorization names a human/operator watcher and
  observation schedule for the first 24 hours. Stop admission and pause the
  affected binding on competing-poller conflict, bot-pin mismatch, identity drift,
  unauthorized routing, unresolved ambiguous send, unexpected stragglers or a
  failed health observation. Do not auto-start the old poller. Until the watcher
  and escalation recipient are named, production activation is blocked.
- **Production management residual risk (accepted, leader 5796):** any verified
  workspace Agent may create/bind/resume any binding and widen its sender list,
  including production bindings. Recipient notices, caps and masked audit are
  the controls; there is no human-owner-only production management class. Repeat
  this accepted risk in the final release authorization, not an unimplemented
  protection claim. Agents retain pause. This acceptance does not authorize
  deployment, token access or production polling.

### Sandbox credential and backup retirement

Per #159 decision 5744, keep its two private rollback backups until the next
planned hub update; record explicit retirement in #203 before deleting
superseded copies. They contain credential/state/mask-key material. Do not copy
them to a ticket, repository or shared location. After the user deletes the
disposable test bot, the human owner removes its token slot through settings and
records the outcome in #203. Deletion/revocation and slot removal are not yet
confirmed. This does not block the completed sandbox ticket or authorize an
Agent to use the human-only token-management path.

## Inventory without credentials

Run the offline operator helper against a private, consistent copy of the old
`apps/telegram` SQLite state. Use canonical paths (no symlink components), a
0700 directory and a 0600 database. Prefer SQLite's online backup API to a raw
copy of an active database: committed WAL is part of the state. Preserve the
complete live state and sidecars separately for recovery. Do not attach the bot
token, config, SQLite, private report or message content to a ticket.

```sh
python3 apps/message-endpoint/tools/telegram-migration-plan.py \
  --legacy-db /private/operator-backup/legacy.sqlite3 \
  --output /private/operator-backup/migration-plan.json
```

This standard-library offline aid is not a third runtime program or Managed
App. It never invokes Treer, opens a network connection, reads a token/config or
changes SQL state. It requires a closed online-backup snapshot with no WAL/SHM/rollback-journal files and opens it
with `mode=ro&immutable=1`, so it cannot create or modify SQLite reader marks or
sidecars. A live WAL source is rejected: its owner/release must make the
consistent online backup first. Apps-dev must not run this against production. The output
is exclusive-created 0600 and includes only bounded public bot/target IDs,
allowlists, status counts, offset conversion and blockers. It omits bodies,
labels, errors, credentials and message mappings themselves. Unknown schema,
statuses, malformed values, unprivate files or excessive inventory fail closed.
Its success exit means **report produced**, never “safe to migrate”. Every
report has `ready_for_cutover:false`; operator and Core evidence are not inferred
from local rows or an apparently empty queue.

The old `last_update_id` is the last completed update; the new account `offset`
is the next requested update. Conversion is `last_update_id + 1`, with old `-1`
meaning new `0`. Never copy the number unchanged or reset it to zero. This
proposal is not imported by the helper. Telegram acceptance after an ambiguous
request cannot be reconstructed from local status alone.

## Cutover checklist

1. Record the actual bot's public ID, old process/controller identity, release
   hashes and authoritative registry (not the configuration seed). Map each
   enabled chat/topic and account-specific sender allowlist to a newly
   owner-created relay stable ID, generation and fixed internal recipient.
   Preserve disabled bindings as disabled. Never reuse an old POC's identity for
   a relay or treat aliases/folder names as authority. Review private profile,
   enforce Policy and OS/container separation; a same-user risk acceptance is
   explicitly unverified isolation, not proof of isolation.
2. Disable old admission using its reviewed owner controls and freeze registry
   changes. Keep only the old poller running while it drains. Drain old Core
   deliveries through **acked**, not just externally **sent**. Resolve pending,
   sending and error chunks; a sending/unknown result needs manual reconciliation
   before replay. Use the old identity’s report-only mailbox check described above; a SQLite
   report cannot prove that mailbox empty. Keep a cutover journal of stable Core IDs,
   Telegram IDs, queue states and administrative actions in private storage.
3. After the drain, take verified private SQLite online backups plus protected
   config/token/outbox backups under the operator's authority. Record checksums,
   integrity checks and the final bot/offset/registry snapshot. Token material
   stays out of reports, argv and logs. Old threads/URLs and author identities
   remain valid; retain the old read-only service if historical URLs require it.
4. Owner rotates the production bot token again at cutover (required by leader
   5796/5798), and installs it through the new hub's write-only token page without putting it in
   argv, links or reports. **Owner/release stops the old poller and verifies its
   exact process identity has stopped.** A database
   row, a stale PID or a config flag does not prove absence of another poller.
   The owner checks webhook configuration with getWebhookInfo only, keeping
   the token out of argv and logs. Do not call getUpdates as an inventory probe:
   that could consume or confirm updates. Process identity must establish that
   other consumers stopped. Only after owner approval of the new poller, check
   that its normal getUpdates is conflict-free; a conflict/second consumer is an immediate stop,
   not a reason to retry against production. Never run the old bridge and new
   hub against the same bot.
5. With BOTH consumers stopped, a separately reviewed offline importer must
   verify snapshot identity/checksums, import the monotonic next offset and
   historical reply/context/dedup mappings into a fresh private hub state, and
   emit a reversible journal. It must never set a binding active, invent old
   authorship, import sessions/credentials or silently discard pending work.
   That importer is **not implemented**; this is a hard blocker for preserving
   existing context in a production migration. An owner-approved new-context
   cutoff is a different, explicitly lossy procedure and must name that loss.
6. Create paused hub bindings and verify current Core paths, machine, generation,
   recipient, exact allowlists and bot getMe readiness. Review the hub's ten-minute
   stale-update cutoff: a longer outage can discard old Telegram updates. Do not
   use live polling as a migration probe, or claim lossless backlog recovery.
   A decision about aged backlog must be made before activating the new poller.
7. Owner/release enables ONLY the new poller, then owner previews and resumes
   individual bindings. Verify text in both directions, reply context, duplicate
   replay, other-user/chat/topic rejection, pause, audit and visible isolation
   declaration. Keep the old process stopped and preserve its recovery material.
   Retirement requires explicit acceptance, not merely healthy processes.

## Rollback

Before new traffic is admitted, owner/release stops the new poller, verifies it
is stopped, restores the unchanged old binding/offset state and resumes ONLY the
old poller. Keep all new bindings paused. Do not change bot identity to bypass a
pin failure.

After new traffic, **do not restore a stale snapshot and start polling**. Pause
both sides, stop the new poller, export the private cutover journal, reconcile
new external receipts and pending/ambiguous sends, drain or explicitly park them,
then reverse-import confirmed mappings and monotonic offset high-water marks
into the old state. Preserve new Core authors and IDs; do not rewrite them as
old POCs. A reviewed reverse importer is not implemented; post-traffic automatic
rollback is therefore unavailable. The safe fallback is containment (paused,
no poller) and a manual reviewed reconciliation, with the outage disclosed.

Recovery backups are held only by the owner/release operator in private storage.
After retirement acceptance, remove retired config/token backups under the
operator retention policy. The mandatory cutover rotation already invalidates
the old bridge credential; rotate again only if a valid copy remains exposed. Retain the credential-free cutover journal as needed.


### Sealed receipt mailbox provenance (local increment)

The currently unused `telegram_receipt_completion::commit_and_ack` supplies the
completion stage for a future negotiated dispatcher. It requires sealed mailbox
authority, verifies that the ack Core client is this Hub's identity, completes
the transaction under the Hub mutex, releases that mutex, then calls Core ack.
Any completion error skips ack. For terminal jobs, ack permission additionally
requires the receipt's final Core ID to match authoritative durable
`inbound.core_id`, even if its mapping was evicted. A changed final ID commits
only a saturating mismatch counter and returns an error without ack, new history,
queue changes or a new hold. Identical scoped terminal receipts remain ackable
without resurrecting evicted history. The lower-level storage helper's
`IgnoredTerminal` result alone does not authorize ack. Regression
checks cover retained/evicted mappings, same-process and reopened stores, unchanged
final IDs, zero ack on mismatch and counter saturation.

An ack error preserves the committed result;
redelivery validates the same receipt and retries its idempotent ack without
another external send. It does not receive generic mailbox values, select FIFO
work or negotiate peers; current workers do not call it.

Synthetic regression cases cover accepted completion visible to a separate fake
CLI before ack, ack failure/redelivery, a wrong ack identity, in-memory COMMIT
refusal and actual private-store pathname drift during the completion transaction.
The drift fixture renames its temporary database from a test-only SQLite update
hook, leaving the real #217 COMMIT hook installed: mapping/queue changes roll
back, no ack CLI call occurs and restoring the name cannot clear the latch.
These are local fault injections, not production rollout evidence. A separate
completion-stage process-death regression now runs the real `commit_and_ack` in
a child test process with a private persistent store and a pinned fake CLI. The
CLI first checks the committed queue, reply mapping and SQLite integrity from a
separate connection, then signals readiness and waits without applying the ack.
The parent SIGKILLs only its own child, observes that the fake CLI stops without
applying that first ack, reopens the same store, and replays the same sealed test
receipt twice. All three ack attempts use identical argv and operation IDs;
replays return `IgnoredTerminal`, leave the single existing mapping unchanged,
and invoke no additional mailbox send.

The same child fixture also pauses from the mapping INSERT update hook, before
COMMIT, while leaving the real private-store commit hook installed. Before the
parent kills it, an independent reader still sees pending work and no mapping;
no ack CLI invocation exists. After SIGKILL and reopening, the same assertions
hold. A single receipt replay then inserts exactly one mapping, commits the
completed job and invokes exactly one ack; no mailbox send occurs. The test-only
hook has a bounded timeout that exits the child without committing if the parent
fails to kill it. It does not alter production hooks or worker behavior.

This covers death after local COMMIT but before acknowledgement takes effect,
including an ack CLI call already in progress. It does not prove actual Core-side
ack idempotence, negotiate a peer, kill an assembled worker, or exercise external
Telegram transport. The sealed receipt is synthetic in this test; production
must still receive through `receive_authenticated`. Mixed-mode FIFO, importer
legacy pins, bounded conflict recovery and an assembled dispatcher crash rehearsal
remain blockers before activating receipt-mode dispatch.

Receipt completion accepts an opaque `core::AuthenticatedDelivery`, not a
`Delivery` deserialized from a request. Only `Core::receive_authenticated` can
construct it in production: fresh self metadata from the integrity-pinned CLI
must match the durable hub workspace before receiving its own mailbox, and the
existing receive projection validates every delivery's recipient. Completion
checks the sealed workspace again, then retains the outer Agent sender/recipient,
current durable identity, mode, lease, generation and scoped receipt checks.
There is no mutable accessor, deserializer or production constructor for the
wrapper; synthetic constructors exist only in unit tests. This is a source-level
provenance boundary, not protection against malicious code running as the same
OS user, a compromised configured CLI, or an independent verification of the
endpoint's claimed final Core ID.

`receive_authenticated` does not acknowledge deliveries or enable negotiated
dispatch.
Production remains on the legacy completion path until the mixed-mode dispatcher,
capability negotiation, bounded conflict recovery and assembled-worker
commit-before-ack crash tests are reviewed. The dispatcher must receive sealed
values through this Core path rather than re-wrap generic `Mailbox::receive` data.

### Proposed terminal mismatch ledger (review required; not implemented)

Issues #203 decisions 6059 and 6062 replace the final no-ack design with a
durably recorded discard. The current `d2eaecb4` no-ack intermediate remains
unactivated. This proposal changes neither workers nor acknowledgement behavior.

After all existing authenticated workspace/principal, generation, mode and
receipt checks, compare the asserted final ID with durable `inbound.core_id`.
Equal terminal receipts remain benign `IgnoredTerminal` replays without mapping
resurrection. A mismatch uses a distinct `TerminalMismatch` disposition; it never
changes the completed job, final ID or reply mappings.

Proposed durable representation and limits:

- Key by SHA-256 of the authenticated opaque Core delivery ID, domain-separated
  and length-framed with workspace and hub identity. Hashing is a storage key,
  not authentication; provenance still comes only from the sealed delivery.
- Store a separate SHA-256 identity digest of the strictly parsed receipt and
  outer Core message ID. Canonical length framing covers every scope, job,
  fragments digest, native ID and asserted final ID, so altered valid assertions
  cannot silently become the same disposition. Store no body or raw receipt.
- Reference the existing declared AUTOINCREMENT inbound job key; obtain the
  binding from that protected parent and its existing declared endpoint text key.
  Never use implicit rowids or add a second mutable copy of binding identity.
  Store first-seen time, an unacked/acked state and bounded conflict flag.
  Fixed-size digest blobs and scalar fields have a checked logical payload
  ceiling of 256 bytes per row. Enforce 32 rows per stable binding across all
  generations and 256 globally (64 KiB logical payload); SQLite page/index/WAL
  overhead is additional and this is not a physical database-size guarantee.
- In one IMMEDIATE transaction insert the unacked disposition and increment the
  saturating terminal-mismatch counter once. Existing key plus equal digest
  returns the stored disposition with no increment. A different digest cannot
  overwrite or acknowledge it: commit a bounded conflict flag and owner recovery
  hold, then return an error. Failed COMMIT always means zero acknowledgement.

Only a committed `TerminalMismatch` permits `commit_and_ack` to call Core with
the original delivery ID and its existing deterministic operation key, outside
the hub mutex. `Core::ack` already requires that ID in the acknowledged or
already-acknowledged response list. Only that successful return permits a second
short transaction to mark the exact key/digest acked. Failure to mark retains an
unacked row; retry repeats the same disposition and ack without another count.
A lost ack response likewise retains the row. Confirmation must never attach to
a replacement row or a conflicting receipt.

Prune only acked rows by a stored AUTOINCREMENT ordinal, not timestamps or implicit
rowids, when capacity is needed, retaining at least
eight acked rows per binding. Never evict an unacked row, including during
endpoint retirement, job cleanup or generation changes. Proposed foreign keys
reject parent deletion while unacked children exist; an explicit guarded cleanup
may delete acked children together with their retired parent. Owner conflict
recovery must confirm acknowledgement or retain the hold, not silently delete an
unacked key. Floors across sufficiently many bindings can exhaust global capacity;
accept that availability cost without bypassing either limit or evicting another
binding's rows. Conflict and full holds are single bounded fields per binding,
cleared only through audited owner recovery. If pruning cannot admit a row, commit an owner-visible full-ledger
hold and pause the binding without incrementing the mismatch counter or acking
the incoming delivery. No receipt caller can evict another binding's rows.

Counter deduplication holds only while a key is retained. After confirmed-acked
records are pruned, an unexpected Core redelivery can count again; the aggregate
counter is therefore at-least-once and saturating, not perpetual exactly-once
accounting. Preserve existing historical attempt counts and document that mixed
provenance rather than relabeling them as distinct deliveries. Retired jobs have
no receipt authority: orphan replays fail closed, never recreate mappings.
Owner-only visibility exposes masked counts, saturation, conflict and capacity
flags; no IDs, digests, bodies or token data enter status responses or logs.

Required local tests cover same receipt after restart without double counting;
changed digest refusal; retained and evicted mappings; failed disposition and
ack-marker COMMITs; ack failure and lost reply; per-binding/global full holds;
acked-only pruning and floor; parent deletion guards and retirement; and process
death on both sides of the disposition COMMIT. Real Core confirmed-ack replay
behavior remains a separate pre-activation rehearsal. The workspace mailbox has
no per-binding receive filter: bounded scheduling/backoff and audited owner
recovery still need implementation and review before a full ledger can be safely
contained without starving other bindings. This proposal alone cannot establish
that containment or authorize receipt-mode activation.

The first inert schema increment implements only the table and SQLite integrity
constraints, with no production initializer or writer caller. It uses the
declared inbound key and restrictive FK, protects referenced parent identity
against updates, and stores two 32-byte digests and five integer scalars (104
logical bytes per row, excluding SQLite overhead). SQL enforces hash types and
widths, scalar states, immutable disposition identity and both capacity limits.
Its declared AUTOINCREMENT ordinal remains monotonic across clock steps and
deleted maxima. Tests cover initialization over existing history, repeated
initialization, VACUUM, restrictive deletion, parent/child identity changes,
invalid hashes/states, multi-generation and global caps, and ordinal ordering.
The floor, acked-only cleanup API, admission digest construction, counter/hold
transaction, confirmed-ack marker and concurrency/rollback behavior are not yet
implemented. Raw SQL in unit fixtures is not a receipt admission interface.

Before this schema is initialized in a runtime store, generic terminal retention
must skip any inbound parent referenced by the ledger (acked or unacked), just
as it skips retained email replies. That guard is now conditional on the ledger
table existing, so pre-ledger stores continue to work and one retained parent
cannot abort cleanup for unrelated bindings/channels. Only a separately reviewed
acked-child cleanup may release those parents. Regression coverage includes both
child states alongside an unrelated prunable job, then explicit synthetic
acked-child removal followed by normal parent retention.

A singleton schema marker records version 1; unknown versions fail initialization
without modifying the ledger. The initial unactivated schema without a marker
is adopted as version 1. Future cap/floor/trigger changes require an explicit
versioned migration, not `CREATE TRIGGER IF NOT EXISTS` alone. A regression checks
the stored capacity trigger against both Rust cap constants. The marker is one
bounded auxiliary record, not a per-delivery log.

Once ledger rows exist, rollback to a pre-ledger binary is unsupported: its old
retention statement can hit the restrictive foreign key and abort cleanup for
all bindings. Contain with the ledger-aware binary and preserve the private store;
do not disable foreign keys or delete unacked children to enable downgrade.

The next inert helper, `telegram_disposition_identity::Identity`, derives the
delivery key from sealed Core mailbox provenance plus expected durable hub and
workspace identity. It reruns strict receipt parsing against caller-supplied
durable expectations; an unchecked deserialized `Receipt` cannot be passed in.
The separate receipt digest length-frames the outer Core message ID, declared
durable job ID and canonical typed receipt bytes, including every scope and
assertion field. Input JSON whitespace/object order does not change identity.
Only two fixed-width hashes and the exact parent tuple are projected for future
storage; the type is not serializable or Debug and no raw bytes are logged.
The helper does not load or validate the durable parent/current generation,
decide terminal mismatch, write a ledger/counter/hold, or authorize ACK. The
future transaction must compare the entire job/endpoint/generation/recipient
tuple before replay or ack marking. Tests cover scoped/domain-framed keys,
per-field assertion changes, outer message/parent changes, canonical formatting
and rejection of wrong workspace, recipient, sender and expected receipt scope.

An independently calculated synthetic golden vector pins both exact v1 hashes.
The canonical encoding includes typed Receipt/Scope field ordering: adding even
an optional field, changing serde serialization or changing a domain tag requires
deliberate encoding versioning and ledger migration, not silent rehashing of old
deliveries. Delivery-key stability across Core versions assumes redelivery keeps
the original opaque delivery ID. A changed delivery ID is a new ledger key and
may count again; the at-least-once accounting limitation also covers that case.
