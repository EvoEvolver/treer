# Host partial-frame cancellation investigation

## Confirmed defect, separate from the Windows incident attribution

The Windows 11 exact wrapper at 987f3f2 reported a closed Host connection during
`windows_wait_process`. It recorded neither the process ID being awaited nor
Host liveness/stderr. It therefore does not establish Host exit, a restart
race, or the precise cause of that connection closing. The next native run
retains 30e6f1e's Console/CLR smoke and adds stage IDs plus boolean-only Host
liveness, pipe availability and log error classifications before isolated
cleanup. No deployed Host or user workload is exercised adversarially.

An independent cross-platform defect was confirmed locally: `handle_connection`
constructed `read_frame` inside `tokio::select!`. `read_u32` and `read_exact`
can consume bytes before returning Pending. Selecting output/process delivery
dropped that future, so the next iteration treated remaining payload bytes as
a new length-prefixed request. This can disconnect legitimate clients, or
execute an embedded request from a malformed outer frame. Treat as a P1
candidate within the existing same-account local transport boundary; this is
not evidence of unauthenticated remote access.

## Deterministic isolated reproduction

Tests use an in-memory duplex connection to the real `handle_connection` and
an observed reader that signals exactly when the outer length is consumed.
They withhold payload, generate an isolated short-lived process, and receive
its process/output events before releasing the payload. No sleeps or retries
select the vulnerable window.

- With the old read-per-select behavior, the valid fragmented Sync test fails
  because the connection closes instead of producing its response.
- With that behavior reinstated in the test worktree, a malformed outer payload
  containing a complete framed `RestartController` with a synthetic operation
  ID triggers the fake restart channel. The regression fails with
  `nested request executed from invalid outer frame` in 0.02s. It does not
  restart any real Controller: the test channel is the only recipient.
- Keeping the same pinned read future across event branches makes both tests
  pass. Separate invalid-length/decode/protocol tests require connection failure
  and prove a trailing restart request is not executed.

## Minimal repair and compatibility

Pin one frame future per outer iteration and repeatedly select that same future
against events until it completes. Keep the 16 MiB frame cap, strict bincode
consumption, serialized mutation gate and existing operation idempotency.
Request protocol mismatch now closes the link rather than accepting another
request on it. No wire-format change, new privileges or framing resynchronization
is introduced. Frame sequencing is deferred: it would require explicit protocol
negotiation, and does not replace preserving partial reads.

`host_client.rs` was checked separately: it already reads complete frames in
one dedicated task without select/event cancellation. A length/decode failure
ends that task and fails pending requests, rather than consuming later frames.
Comments document that invariant. Timeout of an individual request does not
cancel its shared reader. No symmetric production rewrite is needed there.

macOS tests can validate the transport regression, but cannot establish the
Windows 11 incident cause or native ConPTY/detached lifecycle success. Those
remain release-eng's matching-binary exact-wrapper acceptance gates, with
security-claude reviewing this fixed candidate before publication.
