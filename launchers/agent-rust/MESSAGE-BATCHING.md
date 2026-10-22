# Incoming message batching (#158)

The previous watcher requested ten deliveries but synchronously submitted each
one as a separate turn. It also omitted the source-message checkpoints used by
recovery's pending-ACK path. Deduplication was not batching.

The successor follows `agent/src/message-watch.ts`, `message-prompt.ts` and the
message-only tail rule in `prompt-queue.ts`:

- Validate the entire poll (at most ten deliveries), then pack its messages
  before dispatch. A busy provider allows subsequent polls to merge into the
  latest **queued message-only** tail. Running records and explicit prompts
  remain separate; messages never become steering input.
- Retain every sender identity and message/delivery ID in durable checkpoints.
  One message with multiple delivery IDs is presented once and checkpointed for
  every delivery. Conflicting delivery identities refuse the whole admission.
- Persist admission before remote ACK, and persist each ACK locally afterward.
  Recovery retries outstanding ACKs with the same operation key before receiving
  another poll. An uncertain running submission remains fenced, never replayed.
- Pack at most 256 checkpoints and 64 KiB of rendered prompt per record. Split
  when either bound is reached. Quote/escape untrusted bodies and cap the preview
  at 48 KiB, with explicit instructions to read the durable original. No original
  Core message is deleted or silently dropped. Malformed or over-cap input is
  refused without ACK. The CLI response has Node's fixed 16 MiB ceiling.
- A batch keeps its original receipt/operation and first-admission fingerprint
  for retries, even when later polls extend it. Current merged text has its own
  existing journal fingerprint validation. Restart validates framing, identity,
  checkpoint membership and both count/byte bounds.

Old/imported records remain readable and are not retroactively combined. An old
Rust single-message receipt missing checkpoints can acquire a checkpoint only
when its operation and exact old-producer text match; ambiguity refuses instead
of dispatching another turn. Completed tool retention, history normalization,
50-turn resident history and the 128 MiB disk cap are unchanged.

## Concrete ingress parity checklist

| Boundary | Successor status |
| --- | --- |
| One poll before idle dispatch; queued-tail coalescing while busy | Implemented; actor tick dispatches after atomic poll admission |
| Durable source checkpoints, redelivery dedup, restart ACK retry | Implemented, including multiple deliveries for one message |
| Explicit prompt/running/steer/image/notification barriers | Explicit prompts have no message-batch metadata and cannot merge |
| Untrusted framing and 64 KiB message prompt bound | Implemented using Node's framing and preview limits |
| Normal explicit prompt byte limit | Batching-only `74806250` kept 100,000. Separate [receipt successor](RECEIPT-RETENTION.md) aligns new admission to Node's 65,536 raw bytes while preserving retained legacy retries |
| Queue/receipt pressure | Separate receipt successor prunes safe terminals to 1,024 and counts queued/running/ACK/notification obligations toward 256; protects uncertainty and keeps the 4,096 recovery ceiling |
| Retry diagnostics/backoff | Separate [retry successor](MESSAGE-RETRY.md) implements `INGRESS-AUTH-RETRY`: fixed categories, authentication stop, bounded exponential jitter and reset after successful poll/admission/ACK; not yet a deployed parity claim |

## Operational rollout delta

This is an offline source successor to `b40e68d4` / `release-import-v1`.
It does not edit that immutable runtime, any startup, live journal, queue/fence,
provider history or lifecycle. Further target replacements remain paused while
this correction is prepared. Core gen28 finalize is a separate completed task.

After focused source review and a separately pinned native build, the leader may
select one existing Rust target, capture its current startup/provider/thread and
receipt baseline, wait for its natural idle checkpoint, and perform one versioned
runtime update. This is **not** another Node import. Preserve current queued
receipts and ACK checkpoints; do not clear/replay them. Verify the same thread,
message IDs/checkpoint counts, bounded queue and an observed multi-message burst.
No unchanged whole gate or unavailable-browser gate is introduced here.

For rollback, retain the original startup and binary/assets manifest. An older
reader ignores the additive batch metadata and reads the existing combined text,
receipt fingerprint and source checkpoints. Nevertheless, never restore an old
journal over receipts admitted after update: any such ambiguity requires
reconciliation. The author performs no live update or rollback. Frozen native
artifact pins and exact selected-target commands belong to the subsequent
leader-owned operational package, not to this source-only change.
