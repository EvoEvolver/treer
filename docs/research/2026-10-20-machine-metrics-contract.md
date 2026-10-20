# Machine metrics typed contract checkpoint (#220 / #221)

Status: draft models in `treer-protocol/src/machine_metrics.rs`, no advertised
capability, routes, grants, collector or runtime behavior yet. This refines the
[proposal](2026-10-20-machine-monitoring.md) following security 6602/6606 and UI
6604/6605. Implementations must validate these invariants before dispatch,
before serialization and before drawing; serde types alone do not enforce them.
The Rust `MachineMetricsResponse::parse_bounded` now caps the raw bytes before
decoding and validates request/machine binding, finite values, time/age, units,
series/device membership, bucket boundaries/counts/coverage and current memory
pairs. Source callers use `validate` before serialization. Neither is wired into
a live route yet. The browser still needs its own equivalent validator. These
checks establish internal wire consistency, not truthful machine measurements.
Unknown response extension fields may be parsed for compatibility but MUST NOT
be forwarded as raw bytes: Controller inner relays and Proxy return only a
new serialization of the validated typed struct. This drops unknown UUID,
driver and arbitrary nested payload fields. A regression adds such fields at
three nesting levels and requires the forwarded value to equal the original
clean structure. Route wiring must test that it actually uses this path.
Logical CPU count is 1..65,535 when known. GPU names also reject zero-width
U+200B..200F, bidi U+202A..202E / U+2066..2069 and U+FEFF format characters.

## Authorization and local boundary decisions

Retain existing Policy modes. Proposed `machine.metrics.read` uses
RESOURCE_MACHINE: human owner/member seed rules may permit visible machines;
enforce-mode Agent seeds do not add a grant. Monitor or absent Policy permits
authenticated workspace actions under the existing contract: this proposal
does not introduce a new deny-default exception or claim no implicit grant in
those modes. Validate membership then workspace/machine binding then Policy
then dispatch. Non-members/wrong-workspace machines use the same not-found
pattern. No route uses `machine.exec` or `machine.file.read` as a shortcut.
Actual seed changes require security and release review, not this model commit.

Machine metrics deliberately describe aggregate OS-visible load, including
other workloads on shared hosts. The machine owner must see this notice when
enabling enrollment metrics; add a machine-local disable switch which stops
sampling and removes stored history. Disable applies to every same-account
enrollment sharing that collector. Uninstall/last unenrollment removes history;
do not remove it while another enrollment still owns collection. This is an
explicit shared-host property, not per-workspace isolation or per-tenant usage.

One collector per same-account installation uses a machine-private root and OS
lock; stale owner detection is PID plus birth time, not PID alone. Local reads
use only an owner-only Unix socket / SID-restricted Windows named pipe, peer
account verification on every connection, one fixed bounded read command,
bounded frames/rates and no credentials, arbitrary paths or workspace secrets.
Use descriptor-anchored no-follow creation/opening for the store; checksums are
corruption detection, not protection from same-account tampering.

NVML helper uses the Controller's own absolute executable and fixed argv, a
cleared environment, closed unrelated descriptors/handles, bounded framed stdio
and discarded stderr. No PATH/LD_LIBRARY_PATH/DYLD inheritance or arbitrary DLL
search. Only trusted root/SYSTEM-owned non-user-writable system library paths,
with parent/symlink validation; Windows full path/system32 lookup. Apply owned
process-group/kill-on-close-job and resource limits; validate helper output as
untrusted. Slow helper containment must not wait on lifecycle/heartbeat shutdown.
These are implementation gates, not claims proven by the types.

## Request and response rules

Routes remain as proposed in #220. API `from`/`to` either both omitted (resolve
to the last 15 minutes through request acceptance) or both explicit RFC3339;
one omitted fails `machine_metrics_invalid_range`. `max_points` omitted means
360; parse strictly as integer 1..720. Reject duplicate/unknown parameters.
Resolved typed query always has all three fields, `from < to <= accepted_at`,
duration <=24 hours. Controller rechecks range and caps before accessing store.

Fixed query error codes: `machine_metrics_invalid_range`,
`machine_metrics_invalid_query`, `machine_metrics_forbidden`,
`machine_metrics_offline`, `machine_metrics_unsupported`,
`machine_metrics_disabled`, `machine_metrics_busy`,
`machine_metrics_timeout`, `machine_metrics_invalid_response`.
No OS/NVML diagnostic text is returned. Reuse existing not-found errors for
membership/binding failures. No-store applies to failures too. No range or
result in audit/log/trace payloads; logging and transport body paths need tests
and source inspection before activation. OS-generated privileged crash dumps
are outside an application no-logging guarantee; do not configure automatic
Treer metric reply dumps or attach heap dumps to diagnostics.

HTTP mapping for consumers: invalid range/query 400; Policy refusal 403;
offline 503; unsupported 501; disabled 409; busy 429 with Retry-After: 5;
timeout 504; invalid upstream response 502. Non-member/wrong-workspace remains
the existing uniform 404, not a new metrics-specific disclosure. A disabled
collector notice says collection is disabled by the machine owner for this
installation and all same-account enrollments. No browser enable toggle is
authorized without a separate machine-owner setting operation.

Bounds: two queries/source machine, four pending/Proxy machine, one pending
per principal/machine. Coalesce identical in-flight source queries only, no
completed-query history cache on Proxy. Ten-second overall timeout. Source
aggregation must choose at most requested points and at most 7,200 bucket
records TOTAL across all series, coarsening all series identically further if
needed to stay <=1 MiB encoded. No partial response at the byte limit: bounded
failure if even the metadata/current values cannot fit. No response advertises
or activates a capability until the entire source/query path exists.

Each caller independently passes membership, workspace/machine binding and
Policy before joining coalesced work; coalescing is never an authorization
cache. Canceling one subscriber must not cancel another's live subscription;
cancel underlying work when no subscribers remain, subject to transport/source
deadlines. The per-principal cap still applies to coalesced subscribers.

`MachineMetricsResponse.series` contains four system series plus three per GPU
(max 100). `kind` fixes unit: utilization percent 0..100, RAM/VRAM bytes >=0.
CPU is whole-machine percent, not sum of cores. RAM used = total - available
from the same sample/method. RAM total is itself a series: capacity changes
mark the affected bucket partial and break continuity, never divide a historical
mean by today's total. Likewise GPU memory total accompanies memory used.
Valid paired samples require 0 <= available/used <= total; the source refuses
invalid readings, the browser must not silently accept NaN/infinity/range errors.
Values are JSON finite f64, with bytes <= 2^53-1 for exact browser integers;
larger values are unavailable, not rounded into a false claim.

Each series has its own `current`: latest actual field observation independent
of selected history range, without query resampling. `sampled_at` and `age_ms`
refer to that field's acquisition. GPU helper completions must not inherit a
newer CPU/RAM timestamp. `stale_after_ms` is 15,000: older valid observations
have status stale, value null, and retain acquisition time/age for the UI label.
Failure/unsupported readings have value null, with time of that attempt when
known. Absence of any observation has null time/age. New CPU baseline is warming_up.

`age_ms` is exactly floor((generated_at - sampled_at) / 1 millisecond), never
relative to query acceptance or browser wall time. The receiver must account
for transport delay when displaying freshness; Web conservatively adds measured
request elapsed time to accepted source age, then advances with monotonic
elapsed time since acceptance. This may label a borderline field stale slightly
early, but never freezes it at its arrival age. Failed refresh does not reset
the age anchor. Hidden/resumed tabs recompute before showing values; if elapsed
time cannot be trusted across suspension, show stale until a fresh response.
The current value is hidden once effective age exceeds 15 seconds; successful
new data alone resets its anchor. No reliance on source/browser wall-clock
agreement or generated_at as a replacement sample timestamp.

All series share identical ascending half-open bucket boundaries covering
the resolved request range. Empty history: points=[] for every series (no fake
empty-day raster), retained_from=null if nothing retained. Otherwise include
empty intervals as null buckets. `bucket_width_ms` reports actual aggregation,
not the sampling period. First/last actual sample times lie in [from,to).
`valid_count` counts ok values; distinct `status_counts` count observed slots,
including failures, with sum <= expected_count and ok count == valid_count.
Empty/failed-only buckets have null mean/min/max and null first/last VALID
sample times; nonempty values require finite min <= mean <= max.

`partial=true` on any missing/failed slot, collection epoch boundary, capacity
change or sampling discontinuity, even if min/mean/max exist. The browser must
break the line at partial buckets and may draw an isolated marked observation;
it must not infer continuous coverage between first/last timestamps. Status
counts identify observed failure categories; missing_count is expected minus
their sum. An epoch/clock boundary is partial even if slot counts happen to add
up. No interpolation or synthetic zeros. `truncated` indicates requested older
history removed by retention/capacity; `retained_from` identifies the earliest
available record, not a promise of continuous coverage.

`gpus` is the union of current inventory and selected-range history descriptors,
with a stable installation-salted opaque id and name <=128 UTF-8 bytes. Raw
UUID/serial/PCI/driver strings never leave the machine. A removed GPU remains
with present=false and current device_absent; retained historical series keep
its old key. Index reuse never reassigns history. Cap union at 32, prioritizing
current cards then most recently observed historical cards deterministically;
inventory_truncated=true on overflow. All returned GPU series must reference
that descriptor list. Source and browser reject duplicate IDs, orphan series,
duplicate kind/id pairs, >32 descriptors or >100 series. No caller-supplied HTML,
URLs or state text; model names are plain text and fixed enum labels are UI-owned.

## Synthetic presentation examples

These describe wire states for mocks, not live samples or native validation.
Executable JSON examples for all six cases are emitted to stdout by
`cargo run -p treer-protocol --example machine_metrics_fixtures`; the example
reads no machine data and stores nothing. Its tests round-trip all cases through
the bounded parser and reject independently mutated binding/time/value/count/
device/capacity inputs, over-limit frames and truncated JSON.
The removed_gpu case intentionally has gpu_status=no_device, present=false and
retained historical series. Every nonempty example currently has one bucket;
partial_gap has a single partial bucket, whose plotted statistic is the bucket
mean, not a fabricated raw reading at first/last_sample_at. Render it with a
marked point/table value while breaking continuity.
CPU current is deliberately 12.5 while the historical mean is 10 (min 5/max 15)
so a consumer confusing bucket means with current readings fails its fixture.

Rust and browser validators should consume the same emitted fixture documents.
Web can capture the synthetic stdout in its own test assets (no live data) and
apply the same independent invalid mutations as the Rust example tests. Keep
schema/expectation changes synchronized on #220/#221 rather than relaxing one
validator to accept a response rejected by the other.

| Case | Per-series current | Selected-range points | Required presentation |
| --- | --- | --- | --- |
| Healthy | ok, value=12.5, sampled_at=04:59:58Z, age_ms=2000 | expected=12, valid=12, ok=12, partial=false, mean=10/min=5/max=15 | current12.5 separate from bucket mean10 |
| Missing interval | same current as healthy | expected=12, valid=6, ok=6, partial=true, mean=10 | break line; six missing slots, never fill0 |
| GPU utilization failure | collection_failed, value=null, attempt time/age | valid=0, collection_failed=12, partial=true, statistics=null | failure label for utilization; VRAM can remain ok |
| Removed GPU | device_absent, value=null; descriptor present=false | old GPU key has retained earlier buckets | historical card retained, no new index substitution |
| New collector | warming_up CPU / no observation=null timestamps | points=[], retained_from=null | no samples; CPU warming label; no zero line |
| Stale field | stale, value=null, old sampled_at, age_ms=20000 | independent retained points | stale current label; no newer global timestamp substitution |
| Fractional age boundary | generated at a whole second +100us, CPU sampled 200us earlier, age_ms=0 | unchanged historical points | floor the exact nonnegative difference; do not subtract two truncated millisecond timestamps |

Timestamps may carry submillisecond precision. Check ordering before flooring a
duration: even a sample 200 microseconds in the future is invalid with age_ms=0.
The shared generator now emits seven cases, including this fractional boundary.

Web accepts only a fully validated, size-bounded response. Abort/reset on
machine/range change, unmount, logout/session change or offline; pause hidden
tabs, serial refresh with jitter/backoff and bounded failures. No localStorage,
sessionStorage, IndexedDB, service-worker or persistent query cache. GPU names
are text nodes, status/unit strings come from fixed UI mappings. Screenshots
and real-browser acceptance cover these states plus refusal/offline/malformed,
en/zh, light/dark, 390px, keyboard/touch and an accessible bounded values table.

The models do not satisfy helper/IPC, retention, authorization or performance
acceptance. Native three-platform measurements, actual NVIDIA measurement when
hardware exists, Web #221 and UI review remain required for parent completion.

## Local history staging checkpoint (not connected)

The Controller's `machine_metrics::history` module now provides a single-owner,
in-memory ring for validated current-only frames. It bounds serialized payloads
to 17,280 records, 64 MiB total and 64 KiB per record; smaller limits are allowed.
Append, read and explicit maintenance evict observations older than 24 hours by
either wall or monotonic age. The future collector must call maintenance without
viewers and before serving after wake. No missed samples are backfilled.

A backwards wall clock, reset monotonic clock, or divergence exceeding 10 seconds
clears the ring and marks a truncated epoch. A new ring starts empty.
Capacity eviction also sets the instance's sticky truncated
flag; routine 24-hour expiry neither sets nor clears it. A future query adapter
must derive range-specific truncation rather than copy this instance-wide flag
blindly into every response. These in-memory semantics do not prove restart
recovery or periodic cleanup: no timer,
collector startup, journal, IPC or public query invokes this module yet. The
future disk adapter must separately bound metadata and temporary files, validate
corruption and permissions, and reconcile clock epochs before exposing history.
Serialized-byte bounds are not a process RSS claim. Response assembly remains pending.

Successful appends now assign a strictly increasing frame tick. Borrowed
`staged_frames` expose that tick, the current epoch and both acquisition clocks
alongside the bytes, after retention maintenance. Readers do not assign ticks;
rejected appends do not consume one, eviction and epoch clearing do not reset
the sequence, and exhaustion refuses further appends before capacity eviction.
Within an epoch the existing admission rules require increasing wall and
monotonic timestamps. These identities are scoped to one in-memory instance:
startup still needs a separate collector-instance/boot identity for a journal.
A frame tick is not a per-field acquisition identity: cached field values must
retain their original collector-owned identity when projected for aggregation,
not acquire the surrounding frame's new tick. That adapter remains unimplemented.

## Local bucket arithmetic checkpoint (not connected)

`machine_metrics::aggregation` builds uniform buckets locally from at most
17,280 actual field acquisitions, including out-of-range inputs in that budget.
Width rounds up to a 5-second multiple and at most query.max_points buckets are
returned. Windows are half-open; only valid observations contribute to
mean/min/max and first/last sample times. Failures have separate observed counts;
missing slots stay absent, never synthetic zero. Any missing or invalid slot
marks the bucket partial. Unordered input is allowed. True duplicate
collector-owned (epoch, tick) acquisition identities are refused independently
of query origin. Distinct jittered acquisitions within one query-relative
5-second slot coalesce to the latest timestamp (ties by epoch/tick), including
a later failure; they do not fail the query or inflate counts. Statistics and
status counts describe these selected slot observations, not every raw reading.
The adapter must deduplicate repeated cached acquisitions before aggregation
rather than assign new identities and count one acquisition repeatedly.

This primitive currently requires a positive integral-millisecond range span,
including when both endpoints carry the same submillisecond offset. The wire
model accepts more general fractional spans; their expected-count rounding must
be reconciled before exposing this primitive through a route. It does not yet
assemble responses: the caller still owes total-series/serialized response
budgets, per-range retention/truncation, removed-GPU inventory union, fresh
per-field current values, epoch handling and final shared-response validation.
No collection task, ring reader, IPC, routing or persistence calls it yet.

## Field acquisition state checkpoint (not connected)

`machine_metrics::field_cache` keeps at most 100 current fields per collector
instance without implicit eviction. Only `record`, representing an actual
completed acquisition or failed attempt, assigns an (epoch, tick) identity and
timestamp. Reads never refresh either. Each field requires increasing wall and
monotonic acquisition time within an epoch. Epoch changes clear cached fields
without resetting the sequence; invalid updates, capacity refusal and sequence
exhaustion preserve existing fields and do not allocate an identity.

Snapshots compute per-field wire age and hide successful values older than
15 seconds as stale/null while preserving the original acquisition value and
identity for history. A new failed attempt replaces the current success with
an unavailable value; the read projection cannot be recorded as a new stale
acquisition. Clock reversal or disagreement above the ring's 10-second tolerance
refuses the snapshot until the collector handles epoch uncertainty.

This is state arithmetic, not proof of actual hardware sampling. The future
collector must call record only after real acquisition and coordinate epochs;
actual RAM/VRAM collection must use the atomic batch helpers described below. The History
frame format now carries these per-field identities through the local staging
envelope described below. A query adapter must still deduplicate cached
references before aggregation. This cache adds no sampler, timer, journal,
IPC, API or capability.

## Coordinated collector epoch checkpoint (not connected)

`machine_metrics::collector_state` privately owns a History and FieldCache;
History's epoch is authoritative for this state object. Every maintenance,
acquisition and frame admission first synchronizes that epoch. History clock
discontinuity clears the cache; cumulative cache-clock disagreement also clears
History and advances the same epoch. An empty snapshot after uncertainty means
warming_up/no observation, never permission to reuse an earlier field. Only a
new completed capture can populate the new epoch. Acquisition ticks remain
increasing across resets. Epoch exhaustion clears history and permanently
refuses operations rather than wrapping to an old identity.

Frame admission compares every current field against that clock's cache
projection, allowing canonical empty warming_up fields when absent. It refuses
caller-invented or pre-reset values before staging, then runs the existing full
frame validator. This is an inert state boundary, not a sampler or response
assembler. The bounded local staging encoding below preserves original
per-field identities; the outer frame tick cannot replace them. Hardware acquisition, removed-GPU
inventory, per-range epoch/partial flags, journal integration, singleton IPC,
authorization and routes remain unfinished. No runtime capability is enabled.

The future collector must stamp each acquisition completion using its own
wall/monotonic clock pair, never a helper-provided timestamp or earlier dispatch
time. `reset_diagnostics()` exposes the instance's authoritative epoch as its
reset count and one fixed cause (history clock, field clock, acquisition clock).
Repeated diagnostic reads do not advance clocks, epochs or acquisition ticks.
This makes conservative reset loops observable to a future health adapter
without exposing sample content; no logging, endpoint or UI is wired yet.
An assembler must also preserve `Snapshot.reset` when deciding partial buckets,
including when a subsequent frame admission fails.

## Identity-preserving local staging frames (not a durable journal)

History now stores a version-1 envelope containing its epoch, the validated
current-only frame, and at most 100 original field acquisitions. Each carries
kind/GPU key, epoch/tick, original timestamp, status and value. Serialization
copies the collector cache's identity without assigning a new one: repeated
cached references in later frames retain the same identity even after current
becomes stale/null. The original successful value remains available for the
historical acquisition, not for display as a fresh current value.

Envelope encoding and decoding validate the machine, epoch, timestamp/value
links, unique field keys and acquisition ticks, field types and value bounds,
and the existing shared response invariants. A missing acquisition is allowed
only for an empty warming_up series. Impossible acquisition tick exhaustion,
future acquisitions, and acquisitions older than 24 hours are refused. The
envelope and acquisition structs reject unknown fields; the embedded public
frame retains the shared protocol's typed projection behavior. Decoding bounds
raw input at 64 KiB before parsing. Encoding checks model bounds before cloning;
the entire encoded envelope counts against both per-record and ring byte caps.

Maintenance checks every record's earliest acquisition, as well as frame
monotonic age. A new frame cannot extend an old cached acquisition's retention:
if any acquisition expires, the whole frame is dropped without a capacity-loss
truncation flag. This conservative policy may shorten useful coverage. It does
not itself discard a current field from FieldCache. CollectorState maintenance
now retires expired cache fields as described below, before further snapshot
projection or frame admission.

Collector contract: every acquired field, including RAM/VRAM capacities and
other apparently static values, must be freshly re-acquired within each
retention window or retired to canonical warming_up/no observation with a fixed,
content-free diagnostic. Re-reading the same hardware value is a legitimate new
acquisition and identity; copying its old cache entry with a new timestamp is
not. Retire removed-device fields when fresh acquisition is no longer possible;
the future inventory/response adapter still owes the removed-GPU semantics.
The collector must prevent one unrefreshed field from indefinitely refusing
otherwise healthy appends. The expiry fallback is implemented below; timely
hardware re-acquisition and explicit removed-device retirement remain collector
and inventory integration work.

Expiry drops the whole stored frame, including other still-fresh fields; the
query assembler must not assume partial salvage from an expired envelope.
Decode each retained record with its own stored clock and matching epoch, never
the query's current time. Maintenance applies the present-time retention check
before those records become available to a reader.

The format is internal in-memory staging, not a public metrics response,
authenticated integrity proof, restart/boot identity, checksum or file recovery
format. No filesystem journal, query deduplication/assembly, collection task or
route is enabled by this increment.

## Expired current-field retirement checkpoint (not connected)

CollectorState maintenance validates every cached acquisition clock before
retiring anything. Clock uncertainty resets the shared epoch as before; it
cannot be hidden by first deleting an old field. With valid clocks, a field
older than 24 hours by either wall or monotonic time is removed from FieldCache;
exactly 24 hours is retained, and even a submillisecond excess expires. This
runs on the state entry points, including maintenance with no new acquisition,
before current projection and frame admission. The runtime timer that must
invoke it without viewers is still not connected.

A retired key has no observation, value, age or timestamp. The existing
assembler admission rule permits only canonical empty warming_up for that key;
expiry does not manufacture a failed acquisition or refresh the old value.
Fresh fields keep their original identities, epochs do not change for ordinary
expiry, and a later actual acquisition can record the same hardware value with
a new increasing tick. Expired fields cannot indefinitely block healthy frame
appends. Already staged envelopes still expire as whole frames under History's
independent retention checks; this change does not salvage their other fields.

`expired_field_count()` is a pure read of an instance-local saturating count of
expired fields removed by maintenance. Its name is the fixed diagnostic
category; no field identifiers, values or clocks are exposed. Repeated reads
and subsequent maintenance of an already retired key do not increment it.
This is not a durable counter, log or health endpoint. Removed-GPU presentation,
hardware acquisition, query assembly, journal, singleton IPC, Policy and runtime
sampling remain unfinished.

## Atomic memory acquisition state checkpoint (not connected)

FieldCache now supports a bounded transaction of at most 100 unique field keys.
It validates batch shape, stages updates in a bounded cache clone, and commits
only when every update succeeds. Invalid values, duplicate keys, capacity,
clock conflicts or sequence exhaustion preserve all prior fields and ticks.
Each field gets its own increasing acquisition identity with the same collector
completion clock; there is no shared field identity or externally supplied
acquisition tick.

CollectorState provides `record_ram` for one (total, available) byte reading,
deriving used = total - available, and `record_vram` for one GPU's (total, used)
byte reading. Both reject out-of-capacity or inexact wire-integer inputs before
state maintenance. An Ok attempt requires values; a failed attempt requires
none and atomically replaces every field in that memory group with the same
failure status and completion timestamp. GPU utilization remains independent:
unsupported GPU memory must not erase a valid utilization or replace its age.
Stale is still a read projection and cannot represent a new attempt.

The coordinator synchronizes clocks/expiry first. A conflicting batch clock
conservatively invalidates both state components once and retries the entire
batch in the new epoch. That safety maintenance can mutate state independently
of the batch, as before; the acquisition transaction itself never partly
publishes fields or consumes identities on failure. The one-field entry point
uses the same transaction path. Future RAM/NVML adapters must use these grouped
entry points for memory readings and stamp the collector's completion clock;
these helpers do not prove simultaneous native hardware reads, collect memory,
manage GPU inventory or start a timer. No runtime/API is enabled.
