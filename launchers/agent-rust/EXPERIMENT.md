# #158 experiment record

## P0: existing Node launchers, read-only

Sampled 2026-10-17 at approximately 20:23 UTC on mac_m2, macOS 15.6.1/arm64,
Node 22.23.3. Only `ps` and `vmmap -summary PID` were used. No inspector,
heap snapshot, signal, process restart, user document or transcript read.
These are existing processes, not a matched workload, so this is a baseline
range and investigation lead, not evidence of a Rust speedup.

| Node PID | RSS from preceding ps (KiB) | Physical footprint (MiB) | Peak footprint (MiB) | VM tag 255 resident / swapped | malloc allocated |
| --- | ---: | ---: | ---: | --- | --- |
| 63787 | 557008 | 135.1 | 867.3 | 74.8 MiB / 7344 KiB | 3869 KiB |
| 63643 | 78544 | 202.7 | 337.4 | 8512 KiB / 139.7 MiB | 3430 KiB |
| 63475 | 83984 | 90.3 | 528.4 | 12.7 MiB / 24.2 MiB | 3768 KiB |

RSS was measured just before vmmap, not atomically with it. Shared library pages,
compression, swap and collection make RSS and footprint different quantities;
do not subtract the columns to estimate a leak. Virtual reservations (~13 GiB)
are not resident memory. VM tag 255 includes V8-managed allocations but is not
an exact `heapUsed` measurement. Existing processes do not expose a safe
memoryUsage endpoint; live heap/native/ArrayBuffer attribution remains unknown.
We will obtain `process.memoryUsage()` only from a disposable Node comparator.

Source observations (not a diagnosis): the Codex adapter retains `fullTurns`
and live item maps, merges/clones history for snapshots, and the server may build
a 10,000-item snapshot for attachment binding before broadcasts. Large histories
and tool output are plausible contributors to peaks and GC pressure. The
sample does not establish an individual leak or explain all of the high-RSS
process. Continue the Rust prototype as directed, rather than claiming a leak
fix replaces the experiment.

## P1 checkpoint

Separate launcher-owned Cargo workspace; production launcher and Core untouched.
The first common HTTP/WS/queue suite passed against both Node and Rust (5 each),
plus Rust restart uncertainty and runtime VCS-placement invalidation tests (2).
Tests use production fake Codex and synthetic Controller credentials. No real
Codex inference or managed Agent was started by these tests.

Core review 3487 identified three blockers in the first checkpoint: uncertain
dispatch could advance the queue, serial notification IO blocked the actor, and
admission receipts changed after dispatch. The follow-up persists a dispatch
lock (no automatic reconciliation/unlock, including restart), uses one bounded
notification worker, and keeps original receipts separate from dynamic queue
positions. Dedicated sealed-provider tests cover accepted-but-lost responses,
missing turn identity/late events, restart, queued→running→terminal receipt
identity, cancellation conflict, and streaming/abort while notification IO stalls.

Follow-up verification: Rust contract/fault suite 16/16; common suite against
source Node 6/6 and the pinned installed official Node bundle 6/6; Rust unit test
1/1. Default and fixture-feature Clippy passed with warnings denied, formatting
and documentation checks passed, and the default release build completed.
Runtime fixture overrides were rejected by the default release binary before
startup. No full repository gate or dependency advisory audit was run. These
results establish the tested prototype boundaries, not production parity.

## P2 comparison

The stopped third run and its limitations are analyzed in [P3](P3-REPORT.md).
The following is the original measurement design, not a claim all its targets
were achieved.

Use two explicitly disposable Agents, one Node and one release-built Rust, with
new native Codex sessions and identical model/reasoning settings, working
directory contents, prompt sequence and UI polling. No production session import.
Record binary SHA/toolchain/source, process tree and worker counts, launcher RSS,
Codex child RSS separately, CPU, serialized history bytes, item/turn/queue counts,
and Node heap/native/buffer figures at startup, idle, during turns, after many
turns and a few hours idle. Report wall-clock and inference durations so the
workload difference is visible. A Rust history-limit stop invalidates comparisons
against an unbounded Node run; do not silently discard output to improve numbers.

Matched real-session RSS and CPU numbers were collected for 68m36s, including
40m07s final idle, before human intervention. Multi-hour samples were not
collected. A disposable browser reproduction confirmed a response-shape crash;
browser interaction acceptance failed. See P3 for the measured launcher-only
benefit and why it is not a production savings estimate.

## P3 decision

The third experiment stopped early after human UI intervention; see the
[P3 report](P3-REPORT.md) for the pinned evidence, matched results and limits.
GO for incremental porting; NO-GO for team replacement. Existing React UI
send/stream/no-crash parity and durable userMessage audit are explicit blockers,
alongside descendant reaping and state placement. Download port coverage must
include #165's corrected **1 GiB** cap, concurrency bound, idle timeout and
pre-stream rejection. No multi-hour stability claim is supported.

Current decision: no team migration. A successful P2 would justify continued
porting, not an automatic production replacement. Port order: Codex contract and
recovery gaps → Claude → remaining ACP adapters, then goals/attachments/session
transfer and the full existing regression suite. Switching must preserve Agent
identity and session, use an approved startup change, retain the Node command for
rollback, and start with a disposable then a non-critical Agent only after review.

Leader #1603590 schedules state-placement implementation after this P3 report,
with no parallel #160 worktree while P2 runs. If Rust is GO, require all launcher
state in the Controller private root, fail-closed migration and no cwd residue
in the Rust port; fix Node separately if it will remain in production long term.
If Rust is NO-GO, implement #160 on Node next. Validate before provider startup
and recover thread plus queue as one unit in either outcome. The experiment's
memory result alone does not waive these production requirements.

Required security work before any team switch (findings from the real P2 run):

- Restore the exact submitted user prompt in the transcript and existing human
  UI, including Controller-attested sender provenance and its trust status.
  The current Rust transcript omits userMessage items; an assistant-only record
  is insufficient to audit requests, especially message-watcher deliveries
  (#158 security3582). Preserve audit data across queue/restart recovery.
- Preserve external message provenance in both model-facing wrappers: visibly
  show channel and account in the From line/marker, plus available conversation,
  message, endpoint and generation metadata from Core external_source. These
  fields are sender-asserted source metadata, not authenticated human identity
  or Policy authority. Keep quoted/external content distinct from trusted Agent
  instructions; do not flatten it into an ordinary trusted prompt (#1593576).
  Validate with wrapper rendering and spoofed/missing metadata cases before
  deployment. This experimental Node/Rust snapshot remains unchanged.
- Contain/reap provider descendants that create their own process groups. The
  second run observed a detached Railway child briefly outliving Rust shutdown;
  add a detached-child teardown regression and review OS containment or exact
  PID/start-time tracking with children-first teardown before production.
- Resolve Node's cwd queue/thread state retention (#160) and role-scoped MCP
  provisioning (#161) through separately reviewed changes, preserving durable
  receipts and avoiding broad legacy-state deletion or silent replay.
