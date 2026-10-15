# Pending Treer message batching (issue #13)

The watcher packs each receive result before submission and appends subsequent
polls to the last pending message-only queue record. It never changes a running
turn, crosses a normal queued prompt, steers, or changes goal state. Each message
shows its sender and body; a single English warning marks all content as third
party input. IDs appear once in a footer with the retrieval command. Subjects,
timestamps and previews are omitted. Bodies are model input, not instructions
from the launcher, and are now included in the private queue/transcript.
They are also sent to the selected model provider and may contain sensitive
information. Each body line is quoted; display controls are escaped; an ending
marker separates bodies and the footer. These are provenance cues, not a
guarantee against model prompt injection. Empty sender labels are explicit.

All message/delivery checkpoints are committed with the merged text in the same
atomic journal save before delivery-ID acknowledgment. Per-message deduplication,
pending-ack recovery, FIFO and notification-loop suppression remain unchanged.
The journal format stays version 1; new records allow up to 256 checkpoints.
Old ID-only pending notices are preserved rather than reinterpreted. Merging is
bounded at 64 KiB/256 messages; oversized messages include at most 48 KiB of body
on a codepoint boundary and point to the original. Additional batches remain
queued. The configured queue limit counts prompt records, not message count.
Before rolling back to a launcher with the old ten-additional-checkpoint limit,
drain pending merged batches; that version will reject larger acknowledgment
arrays rather than silently drop messages.

Validation: 166/166 launcher tests, typecheck, reproducible UI artifact checks and
the desktop/390px browser suite pass locally on macOS. Regressions cover multiple
polls, restart/dedup, delivery-ID acknowledgments, original operation receipts,
idle dispatch, FIFO barriers, byte/checkpoint bounds, partial commits and disk
failure. Codex/Claude/ACP fake-backend tests remain green; no native Windows run
or real-provider inference was performed for this change. Runtime adapters,
provider process launching and goal controls are unchanged.
