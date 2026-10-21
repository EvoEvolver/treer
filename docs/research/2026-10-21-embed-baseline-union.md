# Embed baseline reconciliation onto the current Controller lineage

Issue #198/7597 identified a real ancestry gap. The accepted `42d00a35`
availability implementation and `da2b9304` deployed baseline descend from
`2264f52a`; the proposed `961dd64e` metrics/Host/recovery integration does not
include that accepted embed ancestry. Inspection found no later intentional
deletion or structural move of these missing embed implementations. An absence
in the new tree is not a decision to revert deployed authorization behavior.

## Owning-path dependencies

This reconciliation starts at `961dd64e`, retaining its complete #224 Host,
#220 metrics and #226 recovery source. It applies the cumulative accepted Core
changes only to the following owning boundaries, followed by the Core portion
of `da7cc70c` (the final authored reference is `ed1080bb`):

| Accepted source | Required behavior |
| --- | --- |
| `e0447d51`, `b85f881c`, `f44455f7` (#156) | Select embed versus standalone authority from request context before cookie interpretation; exact Origin on genuine WebSocket upgrades. |
| `78ec30cf`, `e1a77ab0`, `42e90a13` (#194) | Browser-session WebSocket revalidation, bounded concurrent checks, cancellation on every relay exit, shared interface Policy predicate and retained regression tests. |
| `97e63a0d`, `92b84d66`, `ea944d9d` (#198) | Policy-complete same-origin check and nonce-bound v2 bootstrap. The final cumulative source removes the intermediate Server-Timing behavior; that intermediate behavior is not shipped. |
| `42d00a35` (#198) | Exact 204 readiness, identical empty 200 normal absence, initial-only legacy 401 fallback, operational errors retained with no-store, and no mint on unexpected statuses or transport failure. |
| `da7cc70c` (#198) | Purpose-bound speculative codes/sessions, independent prewarm capability, bounded quotas/caps and rotation, hidden bootstrap without Agent HTML, with the same availability partition. |

The Core paths are `api/network/{embed,ingress,http_tunnel}.rs`, their embed
and tunnel-revalidation suites, `auth/{network,handlers/session}.rs`, shared
network wire types, and the additive prewarm storage columns/index. Current
`api.rs` and protocol module registries receive only the new module entries;
current metrics entries remain. The schema receives only the prewarm migration,
not the audit-summary migration. No Controller, Host, runtime, service-recovery
or metrics source is replaced.

The prewarm audit classifications depend on the self-contained protocol
`audit_category.rs` table from #191/#192. This table and its module export are
carried with both embed actions classified as `apps_ingress`; no audit-summary
route, capability advertisement, read model, SQL backfill, chart UI or worker
is introduced. This is an explicit narrow protocol dependency, not an assertion
that the separate audit-summary feature was integrated.

Excluded unrelated accepted-branch changes include launcher task management,
Issues App relations/reminders, audit-summary APIs/charts/backfill and general
Console navigation work. Web owns the accepted caller and availability/browser
suite reconciliation separately; Release owns the final union and verifies that
current metrics/Host/recovery changes and both sets of browser cases survive.
The original five prewarm commits must not be blindly cherry-picked again over
this cumulative Core port: their Core changes are already represented.

## Verification boundary

Original `ed1080bb` author and browser results describe its original source,
not this new union. Run the full isolated PostgreSQL-backed Proxy suite,
protocol tests and strict Clippy for the Core port, then the complete author
gate and retained browser suites on the composed source before a new freeze.
Document exact SHAs and logs. Real deployed cookie behavior and hover latency
remain unverified; this offline reconciliation authorizes no live probe,
deployment, rollback, installed Host operation or machine update.
