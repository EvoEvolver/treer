# Machine hardware metrics

Controllers with `machine.metrics.v1` collect hardware readings locally every
five seconds, even without viewers. CPU/RAM use selective system refresh with
sysinfo 0.37.2; no process, disk or network inventory is requested. CPU is the
whole-machine percentage, with a warm-up interval after startup or clock reset.
Values describe the OS-visible machine, including its container/VM boundary.

| Platform | RAM available calculation | NVIDIA |
| --- | --- | --- |
| Linux | OS MemAvailable; used = total - available | Fixed system NVML library locations |
| macOS | Saturating free + inactive + purgeable - compressor pages; clamped to total | `unsupported_platform` |
| Windows | Physical available memory; used = total - available | OS-derived System32 NVML DLL only |

Only NVIDIA is supported. Device keys hash a machine-local random salt and the
NVML UUID; raw identifiers do not enter the public response. Missing drivers,
devices, failed collection and unsupported fields remain explicit null/status
values. A current inventory that cannot establish historical-device presence
can refuse the whole query rather than assert that a device disappeared.

## Local ownership and retention

The source is shared by Controllers using the same OS-account state directory,
under `machine-metrics`. Independent accounts or custom state directories are
separate installations; this is not physical-host deduplication across accounts.
An OS writer lock elects one sampler, and an OS query lock admits one assembly
across those Controllers through socket publication. This implementation shares
the private disk store directly; it does not add the local socket/named-pipe
read service proposed in the original research plan. Same-account processes
already share Treer's trust domain.

Unix opens directories/files without following links and checks ownership and
0700/0600 modes. Windows retains directory ancestry handles that prevent rename,
rejects reparse points/hard links, and checks current-user/SYSTEM private ACLs.
Five-minute segments contain bounded JSON records with SHA-256 integrity
checks. Checksums detect accidental corruption, not a malicious owning account.
Queries capture segment lengths once and stream the same snapshot twice;
corrupt, torn or invalid records refuse the whole response.

Retention is at most 24 hours, 17,280 frames, and 64 MiB of logical local bytes
including a fixed metadata allowance. Whole boundary segments expire early,
so the available history can be shorter by up to five minutes. Capacity eviction
sets range-specific truncation; routine expiry produces gaps without claiming
capacity pressure. Cleanup runs with collection and at source open, without a
viewer. Sleeping/stopped machines can only remove expired files on wake/start.
Clock discontinuities reset the collection epoch and CPU baseline. Reset or
capacity eviction invalidates in-flight output before publication.

Local owner controls use the installed Controller binary:

```sh
treer-agent-server metrics status
treer-agent-server metrics disable
treer-agent-server metrics enable
```

Disable immediately refuses queries and removes history on the next collection
tick, or immediately when no collector holds the writer lock. Uninstalling the
last saved service also disables and purges this installation's history. Neither
operation deletes the salt or changes another OS account's installation.

## Query and resource boundary

Both authenticated routes use the same typed command and source response:

```text
GET /api/workspaces/{workspace_id}/machines/{server_id}/metrics
GET /agent/workspaces/{workspace_id}/machines/{server_id}/metrics
```

Workspace membership, machine binding and `machine.metrics.read` Policy apply
before dispatch. Human/Agent identity and Policy are rechecked before output.
The route grants neither exec nor file access. Existing Policy modes remain;
no implicit enforce-mode Agent grant is added. The Policy App's action registry
and any explicit grants must include the action before enforced Agent use.

`from`/`to` are explicit RFC3339 timestamps with a maximum 24-hour span. Omit
both for the last 15 minutes. `max_points` defaults to 360, maximum 720; it is a
ceiling, and the source may downsample further to preserve every series within
the bucket/wire caps. Duplicate/unknown parameters refuse the request. CPU,
RAM and each GPU have independent status, sample counts and gaps.

The source reserves 2 MiB for collection, 14 MiB for one query and its frame,
and 16 MiB while a GPU helper/reader/reaper exists, against the shared 32 MiB
ledger. Reservations precede work and survive deadlines until their owners
actually drop. The source deadline is eight seconds, Proxy ten seconds, and
source socket publication two seconds. Connection loss drops the source future;
HTTP cancellation sends connection-scoped metrics cancellation. Workers check between
records and serialization steps. Other command replay semantics are unchanged.

NVML runs in a credential-free, fixed-argv helper before Tokio initialization.
It owns an inherited salt-file lock, so a surviving helper prevents another
NVML invocation across parent replacement. Output is capped at 16 KiB; a
two-second deadline kills the owned helper, retains credit until reaped, and
backs off 30 seconds through five minutes. A stuck call cannot block CPU/RAM or
Controller shutdown. Linux limits helper data allocations to 12 MiB; Windows
uses a 16 MiB commit-memory job. These controls are not an RSS guarantee or an
untrusted-code sandbox; native driver mappings and overhead require measurement.
The bounded one-shot helper is a change from the proposed long-lived helper.

Proxy admits two concurrent metrics responses per process, with one per actor
and at most four per machine. Admission follows authorization and remains owned
by HTTP bytes through slow-reader transport. Responses are at most 1 MiB and
all outcomes are `no-store`; busy includes `Retry-After: 5`. Metric bodies do not
enter Controller result replay, Proxy history/database/logs or browser storage.
Old Controllers return unsupported, offline machines return unavailable.

## Validation status

The development implementation has exercised actual macOS CPU/RAM sampling,
disk reopen and typed source queries. Isolated PostgreSQL route tests cover
authorization, revoked sessions, invalid source identity, extension filtering
and slow-reader ownership. Synthetic 24-hour disk inputs with 0/1/8/32 GPUs
exercise actual assembly/encoding plus overlapping transport/decode/HTTP stages;
their measured heap peaks are approximately 8.3–8.5 MiB, below the 14 MiB query
reservation. These GPU inputs are fixtures, not native NVML measurements.

Native Linux/Windows collection, real NVIDIA overhead, full author gate and
Web #221 real-source browser acceptance remain required before feature freeze.
The development implementation does not authorize deployment or machine update.
