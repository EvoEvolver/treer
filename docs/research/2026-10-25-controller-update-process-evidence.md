# #222 bounded process evidence diagnostic

This increment changes only a diagnostic helper and its tests. Release owns
the existing collector/wrapper integration and sole actual execution. Core owns
`scripts/controller-update-process-evidence.py` and its focused test module.
No production updater, capability, permission check or Host path changes.

## Existing evidence and precise gap

Run40169160667/e705 paired native+HTTP/PG entries each passed and exited zero;
native final image/Host checks passed. The additional Release scan failed on
PID907 PermissionError. Stored `final-custody.json` contains only that PID,
empty root holders and claim absent. It does not identify whether exe, cwd or
proc-directory inspection failed, nor retain birth/state/UID. PID907 cannot be
classified retrospectively. The old overall GitHub failure and broader cleanup
UNPROVEN remain unchanged; do not reuse its root, binding or operation ID.

## New evidence contract

The read-only helper scans at most4096 numeric Linux proc entries and emits at
most2MiB JSON. It reads only proc stat/status and exe/cwd symlink metadata; never
cmdline, environment, executable contents or arbitrary descriptors. Output
contains numeric PID/UID/start ticks/PPID/PGID/SID/state and categorical errors,
not comm, raw exe/cwd paths or OS error strings. Status UIDs supplement directory
owner: a non-dumpable same-UID process with a root-owned proc directory is still
examined. A foreign process is excluded only with readable owner and four UIDs
that all differ; unreadable scope evidence remains unknown.

Each candidate gets independent exe AND cwd observations and before/after
identity snapshots. A field failure does not suppress the other field or final
snapshot. PID/start ticks/proc owner/UID changes remain unknown. A stable running
process with any denied, missing or malformed link stays unknown, unless the
other link already proves a root holder (which still fails). Missing exe/cwd
alone never proves exit. An explicitly absent final proc identity is recorded
as disappearance; only stable `Z`/`X`/`x` states independently establish a dead
kernel identity. All errors remain in that record even for an exited process.

Same UID, PPID, PGID or SID does not itself grant task membership. A holder is
established only by a kernel exe/cwd path under the exact physical root, with
path-component prefix boundaries, including deleted images. A successful scan
claims only no exe/cwd root reference at the observation times in the stated
scope. It is not atomic across processes and does not prove no open descriptor,
no escaped descendant or permanent exit. Return0 means that narrow scope is
clear,1 means holder/unknown evidence,2 means unsupported/failed collection.
Do not kill any process based on this scan or turn unknown into unrelated.

For a future fresh collector, Release can call:

```text
python3 -I -B scripts/controller-update-process-evidence.py --root NEW_PHYSICAL_PRIVATE_ROOT --uid ACTUAL_NONROOT_COLLECTOR_UID
```

The root must already be owned0700. The command is read-only and does not create
or delete a root, signal, retry, activate, replace or dispatch anything. Preserve
JSON and exit independently even on failure; do not use an exit-code-only receipt.
No existing wrapper is modified by this Core commit.

## Next actual command: diagnostic only

Release uses a fresh hosted Linux account/window and pinned source. No build,
PostgreSQL, Node/provider, updater binary or enrollment is needed. The following
single test creates its own new0700 temporary HOME child directory and two
bounded Python children. One establishes cwd-root membership; the other sets
its own Linux dumpable flag false to produce actual denied exe/cwd evidence.
Only those `Popen`-owned children are terminated/waited, never a scanned PID or
process group. Both child final identities must be absent, and the protected
live child must remain unknown despite its known fixture ancestry. The children
also have a60s finite lifetime. Only the fresh root is removed after cleanup.

The job must run as its actual non-root account with `GITHUB_ACTIONS=true`,
`RUNNER_ENVIRONMENT=github-hosted`. Clear `TREER_ACCEPTANCE_ROOT`,
`TREER_AGENT_ID`, `TREER_WORKLOAD_CREDENTIAL`, `TREER_OPERATOR_CREDENTIAL` rather
than copying credentials. Select exactly this test, require one run/zero skips,
exit0, and retain its bounded JSON plus stderr/test exit and source hash:

```text
TREER_PROC_EVIDENCE_ACTUAL=1 python3 -I -B scripts/tests/test_controller_update_process_evidence.py ProcessEvidenceTests.test_actual_owned_child_holder_then_exit -v
```

This is new diagnostic evidence only. Even a successful run cannot repair the
missing PID907 observation or relabel40169160667. If the helper reports unknown
on another real process, retain that exact observation; do not broaden authority
or rerun the native/PG update merely to obtain a cleaner collector result.

## Local checks

Focused module:13 synthetic controls PASS, one hosted actual test SKIP on macOS.
They cover denied/missing links, per-field continuation, PID reuse/UID changes,
stable zombie identity, explicit disappearance, known-holder plus denial,
same UID unrelated references, non-dumpable UID scope, parsed comm boundaries,
deleted-image paths and bounded admission. No installed inspection, Host or
Agent lifecycle was run. Hosted diagnostic is NOT_RUN at source freeze.

## Subsequent hosted diagnostic result

Run40172062128 succeeded against exact source
`bfad9d934abb9fe9c2e964d22002e89a41412243`, wrapper
`53053ee4c9a64c776291dac4cad391bcf1e9def1`. The single selected actual
test ran once with zero skips and exit0. The fresh owned child was classified
`task_root_holder`, then `absent_after`; the non-dumpable child retained separate
exe/cwd errno13 observations as `unknown_links`, then `absent_after`. Both
Popen-owned children were waited with exit-15. No updater, PG or Host ran.

Core independently checked the bounded JSON, test stderr and all five artifact
hashes against the consolidated receipt. Receipt:
`.treer/reviews/release222-process-diagnostic-20261025/ACTUAL-RESULT.json`,
SHA256 `0fc1f5d6d93408379f1c32c47a371a84b6190fa2e7b93c2cf5e64f6f2ea77e2f`.
This closes only the new synthetic classification/exit diagnostic leaf. Old
PID907 remains unclassified and run40169160667 remains overall failed with
broader cleanup UNPROVEN. No old root/binding was replayed; production dispatch
activation remains separately unaccepted and capability stays off.
