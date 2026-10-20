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
The single-field staging reader below now calls it after identity deduplication.
No collection task, IPC, routing or persistence calls it yet.

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
envelope described below. The single-field reader deduplicates cached
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
format. No filesystem journal, complete response assembly, collection task or
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

## Single-field history reader checkpoint (not connected)

`CollectorState::query_field` connects the in-memory history ring to bucket
aggregation for one typed field key. State maintenance synchronizes epochs,
retires expired current fields and prunes history before any decode. The reader
validates the field key and query, then decodes each retained frame using that
record's own clock and epoch. It never decodes an older frame against query
now and never samples hardware or inserts observations into history.

For the requested key, original (epoch, tick) identities deduplicate repeated
cached references. An identical repeated acquisition contributes once at its
original timestamp; if the same identity has a different timestamp, status or
value, the whole field query fails. Corrupt or invalid staging frames also
refuse the entire query rather than return partial statistics as complete.
Only original observations reach aggregation: stale current projections cannot
replace their historical successes, failed attempts retain their own status,
and cached frames cannot fill gaps or increase status counts. Empty/missing
fields produce empty partial buckets. Repeated reads create no identities.

The reader visits at most the ring's 17,280 records/64 MiB serialized envelope
budget, bounds each decode at 64 KiB and keeps at most 17,280 unique requested
acquisitions. Its result carries the current epoch and whether maintenance
reset it. This is an internal single-field result, not the public response;
per-range truncation/epoch flags, removed-GPU inventory and overall series,
bucket and output-byte budgets remain the full assembler's responsibility.
The eventual multi-series assembler must decode once for all requested fields,
not run this whole-ring scan separately for every series. Cancellation, query
concurrency/deadlines, performance measurements, journal, source service,
authenticated forwarding and public routes are not implemented here. The
existing integral-millisecond-span aggregation restriction still applies.

Before runtime integration, heavy decoding/validation/aggregation must move
outside exclusive collector mutation access. The current synchronous reader
holds that access for its whole scan and is not suitable as a viewer-facing
implementation. The full assembler must obtain bounded immutable retained
bytes or an append-maintained field index under a short access window, then do
the expensive work independently so viewers cannot stall sampling or cleanup.
Snapshot copies and indexes need explicit aggregate memory budgets as well as
the existing ring cap; a 64 MiB allowance cannot be multiplied without a bound
by concurrent viewers.

The runtime query boundary must enforce per-principal and per-machine admission
caps, the 10-second end-to-end deadline and cancellation, including during the
decode/aggregation work, before enabling the route. Identity conflicts map to a
fixed invalid-response error and a content-free diagnostic counter; they must
not trigger retries or partial-success output. These concurrency, isolation,
deadline and diagnostic requirements remain unimplemented by this reader.

## Cooperative query cancellation checkpoint (not connected)

The staging field reader and bucket aggregator now accept a caller-owned
cancellation predicate. Checks run before collector maintenance, before each
retained-frame decode, while building buckets, while visiting observations,
before sorting, while emitting selected observations/status counts and before
returning a result. Once observed, cancellation returns a distinct internal
Cancelled error for the entire query; no partial bucket result is returned.
The existing uncancelled helpers delegate with an always-false predicate.

The regression interrupts a populated query at every reachable checkpoint,
checks the explicit cancellation error and unchanged acquisition identity,
then verifies an uncancelled retry gives the same complete buckets. An already
cancelled request must not perform clock-reset maintenance. Cancellation after
maintenance can still leave normal expiry/reset effects, as any query can.

This is cooperative, not preemptive: one bounded 64 KiB frame decode, existing
bounded maintenance and the at-most-17,280-observation sort are not interrupted
internally. A callback must be cheap and must not block. There is no runtime
deadline, disconnect wiring, admission semaphore or guarantee of wall-clock
response time yet. The reader still holds exclusive collector access; bounded
immutable snapshots/indexes and decode-once multi-series assembly remain
required before a public route. No sampler, route or capability is enabled.

## Detached immutable query view checkpoint (not connected)

History frames now own immutable reference-counted byte slices. A detached view
copies at most 17,280 clock/reference pairs and the bounded machine identifier;
it does not clone or decode serialized frame payloads. CollectorState captures
the maintained epoch/reset and query clock with that view, then returns an
owned object with no collector borrow. The caller can release its collector
lock before decoding and aggregating a field. Old borrowed query helpers stay
available for staging tests; they are not the runtime query path.

Each History instance admits at most one detached view, using an atomic busy
flag shared with its non-Clone reservation. A second view returns SnapshotBusy.
Dropping the first view clears its frame references before releasing admission,
including during worker unwinding. Thus the live 64 MiB serialized ring can
coexist with at most one old view retaining at most another 64 MiB, plus bounded
record/reference metadata. This is a serialized-payload bound, not an RSS cap:
decoded models, aggregation maps and returned buckets have separate allocations.
The view requires mutable access for queries, preventing simultaneous queries
through shared references to that one view in safe Rust.

Appending, eviction and epoch reset continue while the old view exists. The
view intentionally remains a consistent snapshot at its captured clock/epoch,
not a claim of current freshness after reset. The runtime must impose the same
10 s monotonic deadline, drop views promptly on cancellation/disconnect, and
reject an obsolete epoch before publication. It must not retain views as a
history cache or after the retention/deadline boundary. These checks and the
singleton service are not wired yet; per-instance admission is not a global
per-machine/per-principal policy. Capturing references and maintenance remain
bounded work under exclusive collector access and need measurement.

The regression keeps a detached view alive while the collector records and
resets, proves the view still returns its original data/epoch, refuses a second
view even after cancellation, then proves drop and panic release admission and
the next view sees the cleared new epoch. Decode-once multi-series assembly,
response budgets, deadline/Policy/IPC/journal and native collection remain open.

## Decode-once multi-field staging checkpoint (not connected)

The detached view can now answer 1 to 100 unique typed field keys in request
order. It validates the original query before reducing max_points, then caps
each series at min(requested, floor(7200 / requested-series-count)); all fields
therefore share a bucket width and the aggregate output has at most 7,200
buckets. Missing keys retain empty partial buckets. This is a staging result,
not a MachineMetricsResponse: current values, inventory union, per-range
truncation, metadata and the final 1 MiB encoded-response cap are not assembled.

Each retained frame is decoded and validated once for all requested keys.
Original acquisition identity (epoch, tick) indexes both field position and full
observation. Identical cached references deduplicate; either a different field
or changed observation under the same identity refuses the entire query.
Requested acquisitions outside the range also participate in that conflict
check. Invalid frames refuse the whole query even if they have no requested
field. Only original observations, never stale current projections, enter the
existing aggregation/coalescing path. Unrequested fields are frame-validated
but are not cross-frame indexed by this helper.

The temporary index is bounded at 262,144 unique acquisitions across all
requested fields, with at most 17,280 per field. A cap hit returns IndexLimit,
not partial statistics, eviction or an automatic retry. This deliberate staging
limit may refuse dense full-day/many-GPU data even when the serialized ring
fits; it is not a guarantee every maximum-range/maximum-device request succeeds.
Admission remains one detached view per History instance. The index hash table,
per-field observation vectors (at most the same total entries) and aggregation
scratch allocations have bounded element counts, not a measured RSS ceiling;
hash-table growth/transfer temporarily overlaps allocations. Runtime memory
measurements and a defensible process-wide byte budget remain activation work.

Cancellation is checked during decode, indexing, redistribution and each field
aggregation, and no partial multi-field result escapes. Tests compare requested
RAM/CPU results to independent single-field reads, preserve cached gaps and
missing-GPU output, refuse duplicate/empty/oversized key sets, exercise exact
index-cap boundaries and every reachable cancellation checkpoint, reject
conflicting/corrupt frames, and verify 100 series produce 7,200 full-day buckets
while an invalid original max_points cannot become valid through reduction.
No sampler, timer, advertised capability, journal, IPC or public route is added.

## Bounded response encoding checkpoint (not connected)

`response_encoding::encode_bounded` validates an assembled typed response with
the shared wire validator against the original query, machine identity and
source clock before serialization. It then serializes directly into a private
1 MiB-capacity buffer through a capped writer. Every append checks remaining
space before extending; a valid model whose encoding exceeds 1 MiB returns
TooLarge and drops the entire partial buffer. It never serializes an unlimited
temporary Vec and never trims fields, buckets or metadata to make output fit.
Only a successful complete Vec may reach a future transport.

The shared validator previously encoded an unlimited temporary Vec just to
measure its length. Full `validate` now counts serialized writes with an
allocation-free size counter that stops before crossing 1 MiB; its accepted
wire contract is unchanged. New `validate_model` checks only logical invariants
and explicitly omits the encoded-size check. It is for source encoders that
enforce a write cap themselves, not a replacement for `validate` or
`parse_bounded` at a transport trust boundary. The source encoder uses that
model check to avoid serializing twice and report TooLarge distinctly.

The caller-owned cancellation predicate is checked before validation, during
each serializer write and again at completion. Cancellation and invalid-model,
size and serialization failures have fixed content-free internal categories;
no partial bytes are returned. These are not public API status mappings yet.
Validation itself is not preemptible; its model dimensions are bounded by the
existing shared validator. The output buffer allocates 1 MiB after validation,
independently of actual response size. This cap excludes the already assembled
model, index, snapshots and allocator overhead, so the process-wide memory
budget and lifetime/deadline owner remain required before runtime activation.

Tests verify exact byte-cap acceptance and one-byte-over refusal before append,
successful encoding round-trips through the shared bounded parser, invalid
machine/value refusal, cancellation before/during/final serialization returning
no bytes, and a valid status-rich 2,880-bucket model whose JSON exceeds 1 MiB
being rejected as a whole. Its oversized JSON is built only by the test oracle.
The complete response assembler, GPU inventory, per-range metadata, original
fractional-span mismatch, source sampling/journal, Policy/IPC/routes, real
deadline/disconnect and stale-epoch publication checks remain unfinished.

## System-only wire response assembly checkpoint (not connected)

A detached view now also owns the bounded current-field projection captured
by CollectorState at the same maintained clock/epoch, plus the history view's
earliest original acquisition and conservative instance truncation flag. The
system-only assembler derives the four fixed CPU/RAM series internally, queries
their history in one decode pass, and combines those buckets with the captured
per-field current values. Callers provide only static platform metadata
(logical CPU count, memory method and an unavailable/no-device GPU status),
never current measurement values. Missing current fields become canonical
warming_up, not a manufactured zero. Source metadata still needs a real
platform adapter; accepting a method enum does not prove that method ran.

The old-range query does not change current acquisition timestamps or ages:
generated_at/current refer to the view's captured clock, historical buckets to
the explicit range. Later live collection cannot replace the view's captured
current values. Original successful historical observations remain useful even
when their current projection has become stale and hides its value.

This branch refuses if any current field has a GPU identity, or if any retained
frame has GPU descriptors, even outside the requested range. It must not report
an empty inventory while silently dropping an old GPU. Proper current/range
inventory union is still unfinished; this conservative refusal deliberately
limits the branch to system-only snapshots. Allowed GPU metadata statuses are
no_device, driver_unavailable, unsupported_platform and disabled.

Capacity/epoch discontinuity conservatively marks the response truncated and
every bucket partial. The retained_from value is the earliest original field
acquisition still present in this view. These are instance-wide metadata, not
yet the required per-range loss precision; no retained_from is fabricated for
an empty history. A best-effort reset boolean is not used as proof of absence
of loss (it can be consumed by a Busy query); epoch remains authoritative.

Assembly passes through the shared model checks and private capped encoder.
It returns exact-length Box<[u8]> and captured epoch; converting the buffer can
temporarily overlap the at-most-1-MiB original and its compact copy. Cancellation
is checked again after compaction. The receiver still must enforce deadline,
disconnect, latest live epoch and authorization before publication and drop the
view promptly. This helper is not that publication guard or a network endpoint.

Tests round-trip the actual assembled bytes through the shared bounded parser,
keep an old-range mean distinct from a newer captured current (and subsequent
live update), preserve stale-current versus original-history semantics, keep
missing RAM warming_up, mark known capacity loss, reject invalid metadata and
cancellation, and refuse both current GPU fields and retired-but-retained GPU
inventory. GPU union, per-range loss flags, fractional spans, platform sampling,
journal/source singleton/IPC/Policy/routes and measured overhead remain open.

## Historical GPU inventory discovery checkpoint (not connected)

The existing multi-field decode pass now also returns historical descriptors
for GPUs with an original field acquisition in the explicit half-open query
range. Cached frame append times do not decide range membership. Discovery
includes GPU fields even when the requested aggregation keys are system-only;
the existing all-retained `has_gpu_inventory` refusal flag remains separate.
Descriptors come only from already validated StoredFrames, never query input.

The historical map is bounded to 32 opaque ids, sorted by id on output. A new
33rd id refuses the entire query with the fixed internal InventoryLimit category;
there is no eviction, partial response or silent reassignment of identity. Later
retained frames may update display text for an existing id. Every returned
historical descriptor has present=false: historical data cannot establish that
the GPU is currently present. Cancellation is checked while scanning descriptors.
This bounded map is additional query memory, not a process-wide RSS guarantee.

Tests cover repeated cached references, original acquisition versus frame time,
range exclusion, system-only requested keys, latest retained display text,
sorted ids, 32-id acceptance and overflow refusal without altering the map.
Current inventory capture/union, inventory_truncated propagation and full GPU
response assembly remain unfinished. The system-only wire helper still refuses
all retained GPUs, including ones outside the requested range; this change does
not relax that guard or advertise the capability.
