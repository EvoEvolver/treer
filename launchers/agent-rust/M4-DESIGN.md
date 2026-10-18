# M4 prerequisites for switching team launchers

Proposal for #158, following leader decision 4578 and Security verdict 4580.
The reviewed implementation remains frozen at
`4df6db947ebfff21c2809d552f728d1454d7888d`. Limited M3 is a fresh Codex pilot;
its success cannot close these prerequisites. This document authorizes no
provider invocation, state access, runtime change or team-agent switch.

## Acceptance boundary and ownership

| Prerequisite | Launcher responsibility | Core responsibility | Required evidence |
| --- | --- | --- | --- |
| Bidirectional migration | Versioned export/import, receipt/goal/image semantics, downgrade fence | Stable identity, exclusive generation, cutover lifecycle | Round trip with continued work and uncertain operations; crash matrix |
| Private-state quotas | Bounded staging, publication ordering, fail-closed recovery | Identity-bound reservation/accounting and reconciliation | Real exhaustion, timeout and crash tests |
| Process ownership | Provider starts inside proven boundary; truthful cleanup status | Host/platform supervisor and generation-bound capability | Native escape, owner-death and PID-reuse tests |

Core owns protocol/platform changes. Agree its contract before implementing
launcher consumers; no ad hoc launcher-owned Host control. Security reviews each
boundary and the final switch plan. Windows/Claude unsupported capabilities stay
explicit; Codex/macOS M4 does not expand the adapter/platform matrix implicitly.

## Migration: preserve accepted work in both directions

The existing [provider bundle](../agent/src/session-transfer.ts) omits launcher
receipts, dispatch fences, notifications, goals and attachment accounting.
Define a separate bounded, versioned launcher envelope. Its manifest records
backend/schema, stable Agent identity/generation, provider thread identity,
object digests/sizes, operation IDs/fingerprints, immutable acceptance receipts,
lifecycle/uncertainty, notification acknowledgements, goal intent/reconciliation
and image references. Never export credentials or arbitrary home/project trees.

Both backends must support an exclusive Controller-owned handoff. Rust's current
flock alone cannot exclude an older Node launcher. Quiesce new admission, stop
the supervised provider, settle only confirmed terminal facts and export a
consistent snapshot. Unknown outcomes retain uncertainty; do not unblock dispatch
by inference. Missing handshake support means refuse the switch.

Validate versions, identities, limits, references and digests before publishing
into a fresh private destination generation. Use exclusive descriptor-based files,
private permissions, no symlink/hardlink adoption and bounded temporary space.
Never merge into or rewrite source state. Publish the destination manifest
atomically and durably before Controller changes startup. Retain the original
until explicit cleanup approval. Failed validation/publication leaves the original
restartable and destination inactive.

Rollback after Rust accepts work needs a NEW Rust-to-Node snapshot containing
that work. Selecting the old Node directory loses receipts and may replay actions.
Define supported downgrade versions; refuse unrepresentable state instead of
dropping fields. Stable notification delivery IDs survive handoff, with existing
at-least-once semantics. Matching a provider thread ID is not a dispatch proof.

Tests: Node -> Rust -> new turns -> Node -> restart; image-only submissions;
goal mutations; queued/running/completed/cancelled receipts; lost dispatch/goal
ack; delivered notification with lost ack; invalid/oversized/traversal bundles;
concurrent old/new startup; crashes before/after destination publication and
Controller cutover. Assert source bytes preserved, one active generation, no lost
acceptance receipts and no duplicate uncertain dispatch. Native provider-file
transfer needs separate explicit access authorization; this design does not
permit inspecting a real Codex home now.

## Quotas: account publication and crash uncertainty

Reuse Controller private-state authority, never cwd fallback. Existing
[image accounting](src/attachments.rs) is a starting point, not evidence that
journal replacement or peak staging bytes are covered. Core must specify charges
for logical retained bytes, physical staging, and atomic replacement of old data.

Proposed transaction: reserve new object and bounded staging overhead under
Agent/generation/object identity; write/fsync temporary bytes; publish by atomic
rename; commit accounting for published digest/size. Retain the old charge until
replacement is reconciled. Rename followed by directory-sync or quota-commit
failure is publication uncertainty: stop admission/provider, preserve published
snapshot and reservation, reconcile by identity/digest on restart. Never release
an ambiguous reservation as though publication could not have occurred. Core
must define idempotent commit/reconcile and expiry without undercounting live data.

Denial/timeout before publication preserves the previous checkpoint. Provider
side effects require a quota-backed durable dispatch intent first. The current
synchronous journal persist cannot just await quota inside mutation handling:
introduce serialized asynchronous store transactions with bounded requests and
cancellation-safe ownership, preserving actor ordering and abort responsiveness.
Shutdown cannot abandon published bytes or release their charge. Keep local caps.

Use a real isolated Controller to test exhaustion, reserve/commit timeout,
Controller restart, duplicate commits, concurrent generations, replacement peak
space, disk full, directory-sync failure and launcher crash at each phase.
Assert no undercounted retained objects, no double charge on retry, reads match
published bytes, and dispatch stays fenced until accounting is unambiguous.
Recovery preserves evidence rather than deleting state to resume.

## Process ownership: establish it before execution

Direct-child wait is not detached-descendant containment. PGID, UID, executable
name/path, cwd, inherited environment, periodic ancestry scans and a launchd label
alone cannot prove membership after setsid/reparenting. Never broaden kill targets
from these hints.

Core must select a supervisor/container boundary with an unforgeable,
generation-bound lifecycle/membership handle. Assess a job, VM, container or
platform supervisor against this requirement: every provider/tool descendant
remains owned from its first instruction through owner death, and the supervisor
can terminate and confirm the entire owned set without affecting unrelated
processes. A launchd proposal must demonstrate those properties; a label alone
is insufficient. If native macOS cannot prove them, mark that mode unsupported
for the team-switch guarantee and propose reviewed containment. No system setup
is authorized by this design.

Start Codex only after verified ownership exists. Controller/Host owns lifecycle;
launcher requests cleanup through that capability and reports confirmed, timed
out or unverified accurately. Crash recovery uses supervisor records and generation
identity, never a machine-wide process sweep. Bound stop time and retain/report
evidence when the OS cannot confirm empty membership.

Native tests: double fork, immediate setsid, reparented MCP, concurrent fork during
stop, launcher SIGKILL, supervisor/Controller restart and PID reuse. An unrelated
sentinel beside every test must survive. Assert all owned descendants exit and
handles never refer to a later generation. Unproven remnants are reported, never
killed. Use closed disposable environments; new machine configuration requires
separate operator approval.

## Delivery and switch gate

1. Core/Security review contracts and supported containment mode; resolve quota
   semantics and ownership feasibility before implementing platform behavior.
2. Implement migration/quota increments with fault fixtures and isolated
   Controller tests; implement the reviewed supervisor boundary separately.
3. Run exact-SHA graded/native gates and shared browser parity, recording explicit
   unsupported results. Repeat memory regressions including checkpoint copies,
   migration staging and quota recovery. Finite RSS samples are not an OS quota.
4. Security/Core approve integration and rollback rehearsal. Leader authorizes
   a disposable migration rehearsal, then staged team switching. Limited M3
   success alone never authorizes a team-agent switch.
