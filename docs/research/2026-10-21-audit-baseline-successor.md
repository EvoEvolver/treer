# Accepted audit baseline successor

Issue #198/7615 and #198/7617 require retaining the remaining accepted Proxy
and Console behavior before proposing a replacement of the deployed baseline.
This Core successor starts at frozen `02e66bed`; it does not amend that source,
its gate results, CI refs or workflows. The earlier minimal embed port's explicit
audit-summary exclusion is now addressed by this separate owning-path change.

## Source mapping

The remaining Core implementation is #191/#192, originally introduced by
`daad71fe` and refined by `79d56bc3`, `bcf09b72` and `246d9fc7`. The cumulative
accepted source is preserved in `e19960bf` (and the deployed `da2b9304` lineage).
No intentional removal of these behaviors was found on the current lineage;
the missing implementation is an ancestry gap.

Nine owning files carry the cumulative accepted implementation:

- `treer-protocol/src/events.rs`: summary wire types and feed filters. The
  category table is already retained by the preceding Core embed reconciliation.
- `treer-proxy/src/api/workspace_audit.rs` and its two test modules: human and
  machine/Agent summary routes, existing audit authorization and scoping,
  no-store on success and errors, input limits and request budgets.
- `treer-proxy/src/audit.rs`, `audit/summary.rs` and `audit/maintenance.rs`:
  shared list filters, aggregation, bounded grouping/statements, and incremental
  newest-first typed-instant backfill without rewriting historical payloads.
- `treer-proxy/src/auth/accounts.rs`: add only `workspace.audit-summary.v1`
  to the existing capability list; current metrics capabilities remain supplied
  by their existing read-model handler.
- `treer-proxy/src/schema.sql`: additive typed-instant columns, total STABLE
  parser, insert trigger, time index and maintenance watermark columns. Existing
  prewarm columns/index and all other current schema remain.

The parser remains STABLE rather than IMMUTABLE. The final accepted tests use
microsecond instants and check that trigger and backfill agree on nanosecond
text rounding. The 4096 action/outcome group bound and fail-closed result above
it remain. These fix-forward details must not be lost by taking only the first
feature commit.

The protocol module-registry difference in Release's remaining-path inventory
is only a blank line and is intentionally not ported. There is no new module
registration dependency: the workspace audit routes and maintenance worker are
already wired in the current source; their restored implementations extend
those existing entry points.

## Retention and verification

No Controller, Host, runtime, metrics, embed/session/bootstrap or Web source is
changed by this Core successor. Web separately restores accepted audit charts,
feed/category presentation and delayed mobile deep-link navigation while
retaining current metrics, prewarm, logout and Add machine behavior. Other
Apps, Issues and launcher source groups remain outside this Proxy/Web scope.

Run protocol tests, the entire isolated PostgreSQL-backed Proxy suite and strict
Clippy on this successor. The suite includes summary authorization and workspace
isolation, exact filters/feed agreement, request budgets, grouping limits,
trigger/backfill rounding and schema readiness tests. Then Release composes the
Web successor and verifies the exact combined source. Predecessor CI and gate
results stay bound to their predecessor; no automatic evidence relabelling.

Deploy-time index building may pause audit writes; the operator guide retains
the accepted startup/index and additive rollback notes. Local isolated tests
authorize no production migration, CI dispatch, live browser probe, machine
update, Proxy/Web rollout or installed Host operation.
