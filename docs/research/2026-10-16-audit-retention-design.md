# Audit retention and prompt outbox (#103)

Implementation plan; maintained documentation describes shipped behavior.

Accepted prompts currently wait for a complete audit insert. Move only this
high-volume event to a PostgreSQL outbox. Enqueue still waits for a durable
transaction: asynchronous does not mean an in-memory queue or zero database
latency. Measure enqueue against the existing write before claiming a gain.
Capture authenticated actor, event ID and occurrence time in the request; never
include prompt text. One worker per Proxy drains bounded batches using row locks
and `SKIP LOCKED`. Insert and removal share a transaction, with event-ID
deduplication. Restart replays committed entries; database failure after prompt
acceptance remains a documented audit gap, not a retryable prompt failure.

A shared database counter bounds all replicas' pending entries (4096 by default).
Admission and capacity accounting serialize on one settings row; deletion,
including organization cascade, releases capacity. Overflow increments a durable
counter and emits a body-free warning. Database failures emit a separate warning
because commit acknowledgement can be ambiguous. No per-request spawned tasks.
Shutdown stops the worker after HTTP requests drain and attempts a bounded flush;
unfinished committed entries remain for the next Proxy.

The default maximum age is 180 days, uniformly for every organization audit kind,
including authentication, membership and policy. A trusted deployment operator
may explicitly change the shared database setting; omitted configuration retains
the existing setting. Replicas read that setting rather than relying on NATS
cache invalidation. Conflicting explicit replica settings are an operator error.
Deletion uses database time and bounded locked batches, never a whole-table
delete. Cleanup records monotonic, organization/workspace-scoped deletion
watermarks. Increasing retention cannot restore deleted history. A watermark
describes deletion, not a guarantee of a complete audit trail.

Existing authorized audit reads expose retention metadata. Opt-in statistics
report a fixed recent 30-day UTC window of rows/day/action within the authorized
scope. Workspace reads retain their existing sensitive-read Policy gate and
safe projection; they never include another workspace or organization-wide
overflow counters. Organization reads retain their existing role gate.

The migration is additive. Older Proxies continue writing directly and ignore
the new metadata; an old-only deployment will not drain committed outbox rows or
run retention until a new worker returns. No protocol change is required between
Controller and Proxy. Audit completeness, tamper evidence, and database
administrator isolation remain unsupported claims.

Validation: isolated PostgreSQL tests for bounded concurrent admission, overflow,
actor capture, replay/deduplication, multi-worker locking, shutdown, retention
batch bounds, all-kind treatment, tenant-scoped metadata and statistics; compare
enqueue/direct latency without hiding regressions. Run the complete author gate
and authentication-backed Proxy suite before freeze. Security and release review
precede merge; no deployment or real service restart is part of this task.
