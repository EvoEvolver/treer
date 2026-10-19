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

Next App-owned increment: synthetic fixtures for scoped App-asserted receipts,
unknown historical replies, report-only drain samples and stragglers, followed by
the native reply/receipt implementation in a separate frozen change. No automatic
post-traffic rollback is available; keep containment and manual reconciliation
explicit until a reverse journal is implemented and reviewed.

### Stage 2A: scoped receipt wire contract (not runtime-enabled)

`src/telegram_receipts.rs` defines the first local contract slice. There is no
runtime dispatcher, HTTP endpoint, sender, mapping table, capability negotiation
or production activation wired to it yet. Runtime support must come back for
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
- Parsing does not create a mapping or change acknowledgement state. Future
  storage must make exact replay a no-op and changed final ID a conflict before
  ack, preserve binding/generation boundaries and enforce retention/quotas.
- Resolving a reply never grants context visibility. Core must still reject a
  parent unreadable by the sending relay. For pre-cutover/unknown native replies,
  use an explicit history-unavailable failure; do not silently drop the context.
  Unreadable-context and mixed-peer integration tests remain pending runtime work.

Synthetic conformance tests cover exact-scope success, cross-chat/topic/account/
endpoint/generation/recipient and wrong-sender refusal, job/digest/native-ID
conflicts, invalid Core ID, false verification upgrade, unknown protocol/fields,
duplicate JSON and oversize rejection. These establish only parser boundaries,
not native threading, durable delivery or a completed migration rehearsal.

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
