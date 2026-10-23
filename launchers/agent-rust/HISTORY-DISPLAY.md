# Conversation order and initial view (#239)

The ordered provider turns/items remain the timeline, not a globally sorted list
of user messages. Local durable inputs are joined by exact turn/submission ID.
One native user echo consumes at most one local receipt; equal text from two
accepted inputs does not make those inputs duplicates.

For a new steering input, the journal now records optional `projection_after`,
the last visible item identity **before** the provider RPC. A missing echo is
projected after that boundary, between earlier activity and the subsequent
response. A partial final summary may omit that item; shared identities from the
observed turn retain its relative position. This field is bounded, display-only,
excluded from the admission fingerprint, and never provides recovery authority.
Old journals need no rewrite and remain readable. Native history supplies the
position of old echoed inputs; exact positions of old omitted steering inputs
without any saved boundary cannot be reconstructed and are not invented.

If an entire receipt-associated turn is missing from a complete provider view,
its existing local-input-only projection is inserted before the next known
receipt-associated turn instead of after all history. This does not restore
missing provider output. Older paged/evicted turns are not synthesized back into
the resident window. Turn/item caps and final-tool conflict checks remain.

The browser initially follows the first nonempty thread snapshot. Loading
placeholder scroll events cannot disable that placement. A ResizeObserver
follows later column/viewport growth (for example delayed images) only while the
reader is following. Scrolling up opts out; older-history anchors and jump to
latest keep their existing behavior. This UI renders the backend's bounded turn
window directly; it has no independent virtual-list sorting layer.

User-facing surface/default thread titles and the history-limit error no longer
carry the Rust experiment label. This does not rename binaries, rewrite user
titles, or erase historical failure evidence.

## Focused checks

- `tests/contract.mjs` selection `history order keeps`: producer-shaped history,
  native/omitted user echo, partial final summary, local receipts, same-thread
  restart, ordinary next turn, deduplicated operation retry and no replay.
- `launchers/agent/tests/browser-history-order.mjs`: built UI served by the real
  Rust AIS; desktop/390px Chinese, DOM interleaving, duplicate count, first/refresh
  bottom, reader scroll-up and actual launcher restart. All data is synthetic.
- Web's caller-owned `browser-thread-scroll-239.mjs` covers delayed HTTP/empty
  WS history, dock changes and delayed image growth without replacing turns.
  Generic delayed history also passed the old UI; late image growth is the
  distinguishing old/new control. Neither is a live managed-session acceptance.

## PR18 and delivery attribution

PR18 head `256a4bae` is OPEN at inspection. Its diff changes documentation and
adds `legacyInterrupted` to the real-entry restart test; it changes no runtime
ordering or recovery implementation. The `54bd0d16` runtime baseline already
contains `5820c6ca` / `dc89d3f2`, the ports of exact-ID recovery `0a0de690` /
`ab6f0626`. This change incorporates PR18's missing legacy-interrupted regression
without duplicating that runtime patch or claiming the remote PR was merged.

Release's #236 inventory reports the Mac installed default still native `29da`
while the prepared new runtime is `54bd`; those are distinct from each running
Agent image. No global deployed-recovery claim follows from a default pointer.
The #239 increment is handed to #236 for a new source-bound shipping artifact,
not installed or activated by these tests. Existing Agents are not restarted.
