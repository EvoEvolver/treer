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

Treat this envelope as untrusted input. Bind its manifest/digests to stable Agent
identity, the current source generation and a Controller-issued single-use handoff
token. Controller verifies authenticity, rejects stale generations and atomically
consumes the token when activating the destination; copied or replayed exports
cannot activate newer state. Destination generation is part of that binding.
The schema must specify hard entry/depth/total-byte limits before implementation;
initially accept only uncompressed objects, so decompression expansion is absent.
Object references are envelope-local digests, never source filesystem paths.
Reject duplicate operation IDs, conflicting receipts and dangling references.

Both backends must support an exclusive Controller-owned handoff. Rust's current
flock alone cannot exclude an older Node launcher. Quiesce new admission, stop
the supervised provider, settle only confirmed terminal facts and export a
consistent snapshot. Unknown outcomes retain uncertainty; do not unblock dispatch
by inference. Missing handshake support means refuse the switch.
Controller startup ownership enforces exclusion even if old Node ignores a file
lock: it never launches another backend for the same active generation. Both
supported launchers require the current Controller generation token before start.
An older launcher lacking this check is not eligible for a migration handoff.

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

The accounting contract must include finite per-Agent AND machine-global caps.
Reserve the peak simultaneous charge: old published bytes + new published bytes
+ additional staging bytes/metadata, without double-counting a new file that is
itself staging. Do not credit the old charge before replacement reconciliation.
Exact cap values and overhead accounting are Core-reviewed schema requirements,
not silently chosen implementation defaults. Quota denial uses the centralized
4df6db94 rollback: previous checkpoint authoritative, fail-closed read-only state,
no provider dispatch and no automatic retry that clears the fence.

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
Publication must execute outside the cancellable runtime select region. Shutdown
waits for its transaction owner with a finite deadline; timeout marks outcome
uncertain and retains reservation/fence, never claims rollback or successful stop
of an in-progress blocking OS call. Only the transaction owner can settle it.

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

The capability must be created by Host/Controller, a parent that outlives the
launcher, not by the launcher itself. Evaluate the already-reviewed
[Apple container machine path](../../skills/treer-macos-container/SKILL.md)
alongside any native supervisor proposal; using a container does not automatically
prove the required membership, owner-death and teardown semantics. If neither
candidate proves them, macOS team switching remains explicitly unsupported.

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

1. Security reviews the envelope schema as a separate gate BEFORE migration code
   starts. Core/Security review freshness/authenticity, token lifecycle, parser
   limits, quota semantics and supported containment mode before implementation.
2. Implement migration/quota increments with fault fixtures and isolated
   Controller tests; implement the reviewed supervisor boundary separately.
3. Run exact-SHA graded/native gates and shared browser parity, recording explicit
   unsupported results. Repeat memory regressions including checkpoint copies,
   migration staging and quota recovery. Finite RSS samples are not an OS quota.
4. Security/Core approve integration and rollback rehearsal. Leader authorizes
   a disposable migration rehearsal, then staged team switching. Limited M3
   success alone never authorizes a team-agent switch.

All migration/fault fixtures use synthetic data only: no real conversations,
tokens or Codex home. Scan fixture envelopes and diagnostic logs for seeded
credential canaries/credential patterns and unexpected content. The schema
allowlist permits only the synthetic content explicitly carried by receipts;
logs must not echo that content. A pattern scan is a regression aid, not proof
that arbitrary secrets are absent. Real-session migration needs separate review
and explicit operator authorization, not reuse of M3 default-home permission.
