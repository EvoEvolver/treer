# Telegram bridge migration

This is a cutover plan, not permission to touch an existing bot or process.
The disposable-bot roundtrip is still a prerequisite. No production migration
or offline offset/context importer is implemented by this increment.

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
changes SQL state. It uses a read transaction and `mode=ro`, including committed
WAL. SQLite can still acquire reader locks/shared-memory reader marks; use a
backup when physical immutability of the original files is required. The output
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
   before replay. Read the actual old Core mailbox count as well: a SQLite report
   cannot prove that mailbox empty. Keep a cutover journal of stable Core IDs,
   Telegram IDs, queue states and administrative actions in private storage.
3. After the drain, take verified private SQLite online backups plus protected
   config/token/outbox backups under the operator's authority. Record checksums,
   integrity checks and the final bot/offset/registry snapshot. Token material
   stays out of reports, argv and logs. Old threads/URLs and author identities
   remain valid; retain the old read-only service if historical URLs require it.
4. **Owner/release stops the old poller and verifies it has stopped.** A database
   row, a stale PID or a config flag does not prove absence of another poller.
   Check that Telegram has no competing webhook or other consumer, without
   logging tokens. Never run the old bridge and new hub against the same bot.
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
