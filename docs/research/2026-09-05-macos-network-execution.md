# Native macOS networking execution plan

Approved scope: make native macOS networking match Linux's user-visible
behavior, except for independent Agent network namespaces. This plan is an
execution log, not a statement that the target behavior already ships.

## Delivery gates

1. Own the Network Extension and installer source. Preserve TCP half-close,
   binary payloads, backpressure and cancellation. Build and test locally;
   replace the signed upstream helper only after valid Apple provisioning.
2. Gate Agent execution on acknowledged capture registration. Identify process
   instances, cover descendants and Controller restarts, and reject managed
   traffic when its backend is unavailable. Do not claim ancestry polling covers
   detached/reparented children or extension termination.
3. Connect native capture to Controller networking, keeping transparent egress
   independent from Linux namespace service ingress. Exercise actual Agents,
   ordinary sockets, DNS, virtual services, Policy allow/deny and half-close.
4. Close common Linux/Mac gaps: Direct and relay accounting with Agent
   dimensions, policy invalidation for active streams, bounded queues and
   reconnect/restart behavior. Preserve existing proxy-env behavior.
5. Finish shared DNS integration, UDP/protocol behavior, service port ownership,
   launch/install/update/uninstall, coexistence with Tailscale, sleep/wake and
   isolated multi-Proxy failure tests. Test Apple guest with the same revision.

## Evidence rules

Unit tests and unsigned builds do not establish Network Extension runtime
parity. The existing upstream-helper half-close failure remains an open runtime
gate until the owned, correctly signed extension passes that exact live case.
Never reuse an upstream signature after modifying its binary. Do not disable SIP,
replace the user's Tailscale configuration or restart existing Treer services.

## Deployment prerequisite

This Mac has an Apple Development signing identity, but no provisioning profiles
were found in either standard Xcode profile directory. Installing the owned
extension requires an Apple team/profile permitting Network Extensions, followed
by macOS approval of the new extension. Source and unsigned verification can
proceed independently.

## Maintained documents to update

Architecture, security, operator guide, quality guide and documentation index;
the Apple container skill if guest operator behavior changes. Record completed
checks and remaining gates here without promoting experimental behavior to a
supported security claim.

## Account-free delivery log

Implemented and verified on this branch:

- Owned Swift App/extension, a reusable unsigned check script, and bundle layout
  validation. No upstream signed binary was changed or re-signed.
- Directional half-close/backpressure/cancellation; real 1 MiB localhost TCP
  request followed by a response after EOF. SOCKS identity, hostname framing and
  rejection tests. Installed NEAppProxyTCPFlow acceptance remains separate.
- Kernel audit-token PID-version validation, birth-time matching, registration
  retry and reassignment rejection tests. No detached-child coverage claim.
- Native experimental Controller launch gate and shared-port service ingress.
- Offline Open rejection, transport-epoch filtering on reconnect, and closure of
  Proxy-authorized Direct streams on disconnect; proxy-env bypass is preserved.
- Periodic active-stream Policy reauthorization. Optional Open lifetime tracking
  preserves compatibility with older Controllers; old Direct streams remain
  Open-only. Reauthorization timeout fails closed, with bounded concurrency and
  at most 4096 tracked source streams per connection.
- Live `--revoke-active --local-target`: actual native capture through the
  already-approved upstream helper, two Proxies/NATS and two temporary Mac Hosts.
  Allowed TCP transferred 12/18 binary bytes, then Policy revocation closed the
  established stream; another Agent was denied. Persisted relay totals remained
  12/18. All disposable resources were cleaned up.

Evidence is under ignored `output/macos-network-check/` and
`output/network-research/macos-local-policy-revocation.json`. The Apple guest
variant failed its direct reachability preflight with `No route to host`, despite
an existing bridge100 route. This run does not supersede prior guest evidence or
establish a cause for the current reachability failure.

Subsequent account-free work implemented negotiated Direct write reporting,
per-stream cumulative deduplication, database persistence/failed-flush recovery,
and separately stored Agent detail with an authenticated API and Audit display.
The real two-Proxy lab verifies 12/18 byte relay totals and matching Agent detail,
plus Policy revocation. Evidence: `output/network-research/macos-agent-meter.json`.
Direct counters remain machine-reported and non-billable. Durable receipt-based
replay and acknowledgement have now been added; abrupt pre-checkpoint crash tails
and commit-time bucketing remain limitations.

Still open: complete detached-child lifecycle capture, extension-death protection,
installed capture/DNS/recovery/upgrade validation and Linux private Agent UDP
ingress. These must not be marked complete solely because signing is blocked.

Additional implementation: private identity checkpoints and same-boot live-process
restore; authenticated owned UDP transport, explicit UDP service registration,
source Policy and protocol validation, Direct datagram accounting, cross-Proxy
UDP routing and native flow adapter. Controller network tests pass 22 cases.
`macos-udp-meter.json` records a real two-Proxy/NATS UDP allow (12 bytes out,
18 back), active revocation, denied second Agent and persisted machine/Agent
counts. This is Controller transport evidence, not installed native UDP capture.

The owned DNS path now includes a persistent provider-wide name/address map,
UDP/TCP loopback responder, Controller snapshot sync, TCP/UDP reverse mapping and
exact-domain resolver file ownership/recovery. Eight Swift core and seven platform
tests pass; resolver file tests use temporary directories, not `/etc/resolver`.
The unsigned app builds. Signed system resolver/capture remains untested.

Direct replay uses private Controller checkpoint files and Proxy-issued receipts.
PostgreSQL tests cover cross-recorder restart deduplication, identity binding,
out-of-order reports and transaction rollback when Agent detail fails. The real
`macos-direct-durable.json` lab verifies 12/18 bytes after active revocation,
non-billable machine/Agent rows and an empty acknowledged outbox. The same process
crash can still lose its uncheckpointed tail; delayed usage is bucketed on commit.

Final Controller network coverage is 25 tests, including a stalled handshake that
must not block other Agents. A legacy-schema migration test passes against real
PostgreSQL. The latest guest diagnostic confirms `treer` is running with its
expected eth0/bridge100 addresses; guest-to-host ICMP succeeds while host-to-guest
ICMP does not. This does not identify the cause or validate the new guest path.

Datagram relay windows now bound queued data, including empty datagrams; invalid
credit inflation closes the association. Finished Direct receipts are eligible
for cleanup after the existing 90-day traffic retention window.
