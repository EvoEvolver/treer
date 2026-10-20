# Full-resume backward anchors (#158 attempt4)

Supported source inspected on 2026-10-20: installed public package metadata
reports `@openai/codex` 0.162.1. Public tag `rust-v0.162.1` resolves to
`092d3acd6bec3e3a14bdc7e7a2810ab628ab759d`. No installed provider was run,
and no native history, cursor values, authentication or configuration was read.

The [versioned protocol](https://github.com/openai/codex/blob/092d3acd6bec3e3a14bdc7e7a2810ab628ab759d/codex-rs/app-server-protocol/src/protocol/v2/thread.rs)
defines two optional result-level strings on `ThreadResumeResponse`:
`turnsBackwardsCursor` starts backward turn-list hydration;
`itemsBackwardsCursor` starts backward item-list hydration. They are inclusive
anchors for distinct APIs, rather than forward continuation indicators.
`initialTurnsPage` is separate. `ThreadReadResponse` contains only `thread`.

The [versioned producer](https://github.com/openai/codex/blob/092d3acd6bec3e3a14bdc7e7a2810ab628ab759d/codex-rs/app-server/src/request_processors/thread_processor.rs)
implements `paginated_thread_full_turns` for legacy full resume/read: ascending
pages, full item hydration, continuing until `next_cursor` is absent. Both cold
and running full resume use it. Separately `paginated_resume_backwards_cursors`
reads one newest turn and item and returns each page's backward anchor, not its
next cursor. Thus anchors can coexist with full materialization; their presence
does not establish truncation or completeness. These are versioned source
contracts, not verification of private provider data or binary provenance.

Local Node `src/harness/codex.ts` sends resume without paging/excludeTurns and
refreshes with `thread/read(includeTurns:true)`. Rust uses the same full-request
contract. It does not introduce native `thread/turns/list` or `thread/items/list`
pagination, nor fabricate complete item lists from partial ones.

## Narrow compatibility rule

Only a response matched by ID to an outstanding full `thread/resume`, with
provider `initialize.userAgent` reporting the reviewed version `0.162.1`, may treat
nonempty strings of at most 1024 UTF-8 bytes in
`result.turnsBackwardsCursor`/`result.itemsBackwardsCursor` as
nonblocking anchors. Absent/null remain benign. Empty strings and other JSON
types or oversized strings are ambiguous and block candidate interpretation. A result anchor on
`thread/read`, a misplaced `thread` anchor, forward/unknown-direction signals,
`hasMore:true`, or nonnull `initialTurnsPage` still blocks. Request paging,
`initialTurnsPage` (even null), or excludeTurns other than explicit false cannot
grant full-history authority. Unknown/mismatched requests cannot acquire it.

All identity/order/known-status/newest-only checks and live-event conflict fences
remain. No candidate means ordinary bounded projection, never normalization or
completeness authority. No cursor is decoded, logged, followed or copied into
diagnostics. Supplied null/missing timestamps and silent unmarked omissions
retain the previously documented limits of the provider contract.

`paginationMarkers` retains observations, and `paginationBlockers` retains only
blocking fixed schema names (each bounded to 16). `resumeHistoryRequest` records
matched request authority. `backwardCursors.turns/items` report only `absent`,
`null`, `emptyString`, `nonemptyString`, `oversizedString`, or `invalidType`, never length/content.
The initialize user-agent first token is recognized as originator/version;
only exact allowlisted version is recorded as `providerVersion`, otherwise
`unreviewed`. No user-agent text/platform/suffix is retained. Missing/changed
versions keep anchors blocking. This is a reported version, not binary attestation.
All reset per frame. The leader gate requires every prior numeric/status/identity
check, no blocker, and only the two precisely allowed result marker names.
Malformed, misplaced, mixed or missing diagnostics still trigger rollback
before any prompt. See [the leader runbook](ops/LAUNCHER-DEV-RETRY.md).

Synthetic coverage checks metadata before/after turns, zero/one/multiple
candidates, every cursor class and scope, forward mixtures, read/unregistered
request authority, and reset/privacy. Actual entry tests import disposable Node
state, preserve backup bytes, resume a large synthetic thread with both anchors,
then run a new turn with full read observations and verify older AIS pages.
Genuine live concurrency/mismatch/terminal-reopen and partial observer frames
retain dispatch fences. No real migration or acceptance is implied.
