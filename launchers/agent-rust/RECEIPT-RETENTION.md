# Receipt retention and pressure (#158)

This separate source increment follows accepted batching `74806250`; it does
not modify that frozen native artifact or activate any runtime.

## Before this increment

Rust retained every terminal receipt until its 4,096-record recovery/admission
ceiling. At that ceiling a new explicit operation or new message record failed;
an existing operation retry could still return its receipt, and message-only
coalescing/checkpoint updates could still fit an existing record. Only queued
records counted toward the 256 pending limit. A healthy long-running agent
could therefore exhaust lifetime capacity without outstanding work.

Node's `prompt-queue.ts` prunes old settled terminal receipts during persistence
and counts running, queued, pending ACK and pending notification obligations.
Its operation/message dedup is scoped to retained records, not permanent
tombstones. Neither implementation promises indefinite receipt lookup.

## New behavior

- Each persistence keeps the newest 1,024 **safe terminal** records plus every
  protected record. Protection includes queued/running work, pending delivery
  ACKs, applicable undelivered terminal notifications, the current active-turn
  identity and fixed runtime errors denoting uncertain dispatch. A global
  dispatch fence disables pruning altogether. Genuine uncertainty still needs
  reconciliation; capacity is not a reason to clear that fence.
- The 256-record pressure limit counts each record with running/queued work,
  uncertain outcome, pending ACK or pending notification once. A full queue can
  still deduplicate an existing delivery or merge into a queued message batch
  without increasing pressure. A new delivery on a settled receipt cannot add
  a 257th pending record. Poll admission remains atomic and submit-before-ACK.
- Pruning is part of the existing atomic journal checkpoint. Failed publication
  restores the prior receipt set and stops dispatch; uncertain publication
  retains the published set and fences as before. Pending ACK cannot be pruned
  until remote ACK succeeds and its local checkpoint commits. No provider
  history, transcript items or image files are deleted by receipt pruning.
- The recovery ceiling remains 4,096 records and existing journal byte limits
  remain unchanged. A normal unfenced queue retains at most 1,024 safe terminal
  plus its protected records, rather than a growing lifetime archive. This is
  deliberately conservative compared with Node's exact pruning arithmetic:
  protected terminal rows do not consume the 1,024 safe slots.
- Retained operations keep their original receipt/conflict behavior across
  restart. Evicted receipt IDs return not-found; ancient operation/message IDs
  outside this finite window have no dedup guarantee. No message is ACKed to
  make room. Already-ACKed ancient deliveries rely on Core's durable ACK, as
  with Node; pending deliveries remain locally protected.
- New ordinary explicit prompts use Node's 65,536 **raw UTF-8 bytes** limit,
  including surrounding whitespace. Exactly 65,536 bytes is accepted. Existing
  journals still decode the old 100,000-byte format, and a matching retained
  oversized operation retry returns its original receipt before applying the
  new-admission bound. This compatibility exception never dispatches a new
  oversized prompt. Existing whitespace/fingerprint behavior is unchanged.

## Checks and remaining scope

Focused runtime tests cover oldest-safe eviction, active/uncertain/queued/
running/ACK/notification protection, obligation settlement, global fences,
pressure accounting and atomic message admission at pressure. Actual HTTP/CLI
entry tests restore a synthetic 4,096-record journal then run a new provider
turn, check retained retry and evicted lookup, protect 256 pending ACKs, inject a
failed completion publication, and exercise multibyte/raw whitespace prompt
boundaries plus a retained legacy oversized retry. The five accepted batching
entry tests are also retained.

`INGRESS-AUTH-RETRY` remains a named follow-up: classify authentication failure
separately from transient failure and implement bounded exponential jitter /
auth stop semantics. The watcher still uses its existing fixed retry interval.
This increment does not claim full Node parity or live acceptance. Rollout
expansion stays paused; only the leader may select/activate a separately pinned
artifact. The accepted batching-only one-target package remains independent.
