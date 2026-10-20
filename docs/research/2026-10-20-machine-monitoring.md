# Machine-local hardware monitoring (#220, proposal)

Status: implementation proposal for Core/security and Web #221. This document
does not describe shipped behavior. Core owns machine collection, protocol and
query routing; Web owns charts; UI reviews readability. The parent cannot close
until the browser acceptance and native measurements are recorded.

## Collection and platform contract

One collector belongs to the enrolled machine, not to an Agent, workspace or
viewer. Run it under the Controller's machine-local service boundary with an
exclusive collector lock; multiple workspace enrollments on the same machine
must share the collector through a local authenticated, bounded read service,
not each start sampling. Resolve the installation/machine identity and lock
ownership before enabling collection; a per-workspace lock is insufficient.
Do not change Host process ownership or restart deployed Controllers to test.
The implementation must document the existing same-OS-account trust domain;
independent OS accounts/VMs are separate machine installations, not a claim of
cross-account physical-host deduplication.

Sample CPU/RAM every five seconds independently of queries. Use selective
system-only refresh (candidate: sysinfo with CPU/memory refresh only; no
process enumeration, disks, networking, per-core frequency or process lists).
Initialize CPU deltas before reporting utilization; the first interval is
warming_up rather than zero. CPU utilization is 0..100 for the whole machine,
not sum-of-core percentages. Timestamp the measured interval, not its query.

| Platform | CPU / logical processors | RAM contract | NVIDIA |
| --- | --- | --- | --- |
| Linux | aggregate CPU time deltas; online logical count | MemTotal, MemAvailable; used = total - available | NVML when supported driver/library/device are present |
| macOS | aggregate host CPU ticks; logical count | platform available-memory estimate; used = total - available; expose method, not Linux equivalence | unsupported_platform; Apple/AMD excluded |
| Windows | system CPU time deltas; logical count | physical total / available (GlobalMemoryStatusEx semantics); used = total - available | NVML when supported driver/library/device are present |

Confirm the pinned library's exact macOS available-memory formula in source
and report it in `memory.method` before implementation acceptance. Container
values are the OS-visible machine scope; do not silently relabel cgroup quota
as physical-host capacity. Include `scope` metadata and record the test scope.

NVIDIA uses read-only NVML initialization, UUID/name, utilization and framebuffer
memory calls. No nvidia-smi polling, process queries, settings or power controls.
Load only a reviewed driver/system library location, not cwd or a user-provided
library path. Keep GPU UUIDs stable across index changes; preserve independent
per-field status (WDDM/MIG/driver support can differ by metric). Cap inventory
at 32 devices, report inventory_truncated rather than silently losing cards.
Never represent unavailable utilization or memory by 0.

NVML lacks a general interruptible call deadline: a timeout around a Rust
blocking thread does not cancel the driver call. Use one long-lived restricted
internal collector helper for NVML, no inherited Treer credentials, no shell,
fixed framed I/O and bounded stdout; one request at a time. A two-second deadline
marks GPU unavailable, terminates/reaps only that owned helper and retries with
backoff (initial 30 s, maximum five minutes), never spawns overlapping stuck
workers. CPU/RAM, query service, lifecycle and heartbeats remain independent.
An unkillable OS/driver operation is an explicit platform failure: do not wait
for it on the Controller shutdown path or spawn replacements without a bound.

## Local history and aging

Store only under the collector's private machine-local data root: Unix 0700/0600
or current-user/SYSTEM Windows ACLs, no symlinks/reparse traversal or enrollment
credentials in the files. A bounded append journal with independently framed,
versioned records and checksums uses five-minute segments. Proposed hard limits:
17,280 sample records, 64 MiB total segment bytes, 1 MiB per segment, 64 KiB per
record, 32 GPUs per sample; evict oldest records/segments on either limit. Include
headers/index overhead and temporary replacement files in the byte budget.
Capacity pressure can shorten the available range; expose retained_from and
truncated, never promise a full day when the byte cap wins. Prefer periodic
flush, not fsync every five seconds; losing the newest unsynced monitoring
records after power loss is acceptable and must appear as a gap.

Cleanup runs with the collector every minute and before append/query/startup,
even with zero viewers. Filter every returned sample by age; discard a segment
as a unit if it crosses the cutoff and cannot be safely retained within the
maximum age. On resume, prune before serving or appending. While a machine is
asleep/powered off no process can physically erase its files: expired data must
be removed on the first wake/start before it becomes queryable. Do not claim
continuous physical deletion while powered off.

Use monotonic time for scheduling and measured intervals, UTC for chart labels;
store sequence plus collection epoch and detect wall/monotonic divergence.
Backward jumps, large forward jumps and restart with ambiguous age invalidate
the affected history epoch rather than extend retention. Start a fresh CPU
baseline after sleep or clock discontinuity; never backfill missed samples or
draw a zero/interpolated line across gaps. Corrupt/truncated/unknown records are
bounded-read refused/dropped with fixed category diagnostics, never panics or
unbounded recovery; startup must enforce quotas before exposing any history.

## Query, authorization and proposed Web contract

Current machine inventory comes from workspace_snapshot; policy.rs has machine
update/delete/exec/file actions but no machine metrics/read action. Inventory
membership is not automatically authority to read hardware history. Propose the
smallest addition, `machine.metrics.read` on existing RESOURCE_MACHINE, evaluated
by the existing Policy engine for both human and Agent callers. No exec/file
permission, arbitrary command, second policy provider or implicit Agent grant.
Security reviews default member/operator access and explicit Agent grants before
activation. Always check workspace membership, machine binding and Policy before
dispatch. Refusal must reveal no metrics; do not return a capability as a grant.

Proposed capability: `machine.metrics.v1`, advertised only by implementations
with the bounded collector/query path. Public Console route:
`GET /api/workspaces/{workspace_id}/machines/{server_id}/metrics`.
Agent route has the same suffix under `/agent/workspaces/...`; the local CLI
forwards to that same Proxy authorization/query implementation. Both send a
typed `MachineMetrics` Controller command over the existing authenticated link;
there is no metrics field in snapshots, heartbeats or unsolicited events.
Old Controllers return unsupported, not empty or zero metrics.

Query parameters: `from` and `to` as UTC RFC3339, inclusive start/exclusive end,
range <=24 h, `max_points` default 360 / maximum 720. Omission means recent
15 minutes through now; invalid/reversed/future ranges and unknown options fail
with a fixed error. Aggregation is on the source machine, not Proxy or Web.
Each bucket reports actual sample count, first/last timestamp and per-field
count/mean/min/max. No sample means null plus a gap; do not bridge outages.
The current value is separate from bucket averages and includes sample age.

Proposed JSON shape (names to align with Web before coding):

```json
{
  "schema": "machine.metrics.v1",
  "server_id": "machine-id",
  "generated_at": "2026-10-20T05:00:00Z",
  "sample_interval_ms": 5000,
  "retained_from": "2026-10-19T05:00:00Z",
  "truncated": false,
  "scope": "os_visible_machine",
  "memory": { "method": "linux_mem_available" },
  "current": {
    "sampled_at": "2026-10-20T04:59:58Z",
    "age_ms": 2000,
    "cpu": { "status": "ok", "utilization_pct": 12.5, "logical_cpus": 8 },
    "ram": { "status": "ok", "total_bytes": 16000000000, "available_bytes": 10000000000, "used_bytes": 6000000000 },
    "gpus": [],
    "gpu_status": "no_device"
  },
  "bucket_width_ms": 60000,
  "points": []
}
```

Each GPU carries uuid/name and independent utilization/memory status and nullable
values (bytes, 0..100 percent). Closed status vocabulary: ok, warming_up,
no_device, driver_unavailable, unsupported_platform, unsupported_metric,
collection_failed, timed_out, stale. Error responses distinguish forbidden,
offline, unsupported, invalid_range, query_busy and query_timeout. Exact Rust
models and bucket shape must be committed before Web binds to this draft.

Proposed bounds: two concurrent queries per source machine, four in-flight per
Proxy process per machine, ten-second end-to-end timeout, maximum encoded reply
1 MiB. Avoid unbounded waiting queues; fail busy. Cancellation drops pending
reply state and sends cancellation where the existing command transport permits;
otherwise the source work remains bounded by the same deadline and point/byte
limits. Queries only use cached samples; refresh never triggers collection.

Every success/error response uses Cache-Control: no-store. Proxy retains only
bounded in-flight data, never SQL, history cache, disk spool, audit payloads,
trace fields or request/response-body logging. Audit may identify action,
subject and machine without range/results. Check the existing command routing,
broker forwarding and tracing paths for accidental payload persistence. No
retry/outbox persistence for metrics results. Web retains one bounded active
view in memory, cancels on navigation/range change and does not use storage,
service-worker caching or persistent query caches. Offline means unqueryable;
an already displayed view must be explicitly marked stale/offline and cleared
on machine change, never presented as a server-maintained latest sample.

## Delivery and evidence

1. Align this plan with security and Web/UI #221; commit typed protocol and
   permission contract first. Resolve collector sharing across enrollments,
   private local query authentication and secure helper loading in that slice.
2. Implement bounded local store/aging and synthetic sampler tests; then native
   CPU/RAM and isolated NVIDIA collection. No new public/general telemetry bus.
3. Wire Agent/Console routes and source aggregation; prove deny/old/offline,
   limits, cancellation, no logs/body persistence and no heartbeat uploads.
4. Web delivers CPU/RAM/per-GPU charts, current values, units, legends, timestamps,
   15 min / 1 h / 6 h / 24 h switches, discontinuities, refusal/unsupported/offline
   and mobile states; UI review plus real browser evidence is required.
5. Native macOS/Linux/Windows baseline versus enabled under idle and normal load:
   sampler CPU time, RSS including helper, disk bytes/writes and maximum capacity,
   bytes per 15-minute/24-hour query, multiple viewers with one sampling task.
   Measure NVML overhead on actual NVIDIA hardware if available; otherwise record
   unavailable hardware and verify downgrade, never substitute a fake native PASS.

Engineering targets for measurements, not guarantees: CPU/RAM collector p95
below 50 ms per five-second cycle; local collector working set below 32 MiB plus
bounded query buffers; journal <=64 MiB; reply <=1 MiB. Record observed helper
RSS separately and revise budgets based on actual native measurements. Fixtures
must test quotas/expiry without viewers, restart/corruption, sleep/time changes,
multiple cards/removal/partial unsupported, hanging helper and history gaps.
Release remains under the existing graded-gate and leader restart procedures.

Primary references checked for the proposal:
[sysinfo system-only refresh](https://docs.rs/sysinfo/latest/sysinfo/struct.RefreshKind.html),
[NVML API and driver library deployment](https://docs.nvidia.com/deploy/nvml-api/nvml-api-reference.html).
Pinned dependency/API choices and native results remain implementation work.
