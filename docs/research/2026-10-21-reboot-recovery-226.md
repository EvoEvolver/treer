# Enrolled-machine recovery after reboot (#226)

Investigation starts from installed source `42d00a35a6c7c02baf725c2f62a8ff883a9f0bb5`.
The incident reports missing Host socket on two Controller-only restart attempts,
then Controller/Proxy connectivity followed by `403 policy_provider_unavailable`
on explicit runtime restore. New enrollment later succeeding does not establish
that the previous enrollment credential was invalid.

## Boundaries and correction

A nohup Host does not survive an OS reboot. Controller-only restart requires a
live Host; the CLI now refuses a missing/stale socket with the explicit existing-
identity `restore --workspace` command, before spawning a restart subprocess.
An unreachable socket is not proof that the Host stopped: if it is still running,
the diagnostic directs checking socket access instead of starting another Host.
It does not start a second Host, rotate credentials or enroll again.

Restore uses one loaded installed configuration for both cold Host startup and
the request. Connection readiness is distinct from runtime recovery. The latter
still crosses the authenticated local API and Proxy `runtime.restore` Policy
authorization. A typed 403 `policy_provider_unavailable` now explains that
recovery was not authorized, preserves enrollment, and directs an explicit retry
after the configured provider is restored. It does not retry the POST, override
Policy, treat arbitrary denials as readiness errors, or declare workloads healthy.
Successful restore continues to print the per-workload report, including failures.

The websocket startup-declaration validation path in `proxy.rs` uses exact
machine/workspace authentication and bounded persisted IDs. Proxy
`validate_agent_startup` has no PolicyEngine dependency and retains stopped,
revoked and deleted intent. Policy local startup itself has no synchronous
Proxy authorization call. Existing owner-only default-provider installation is
not an existing-provider cold-boot bypass: it rejects an already configured
binding. No new bootstrap privilege is introduced.

## Evidence and limits

The system incident report establishes an OS reboot. A fixed-label-only scan of
the old machine log in the 18:12 UTC minute found two Proxy connections and 25
`restored Agent startup command` entries, with none of the known startup-validation
or command-restore failure labels. No keys, installed private configuration or
arbitrary diagnostic payload were read. These counts do not identify which
workload was the Policy provider or prove its readiness at the failing request.
The same-machine Policy dependency cycle remains unproven.

Hermetic tests use generated temporary configurations and a loopback HTTP fixture.
They cover missing/stale sockets without subprocess launch, a Policy-unavailable
response followed by a separately invoked successful restore with byte-identical
saved credentials, and other authorization/upstream failures without retry or
misclassification. Existing recovery tests cover stopped/revoked identity and
startup intent. This fixture does not reproduce the historical provider outage,
prove a live recovery, or authorize changes to the active machine. Exact historical
provider failure cause remains unknown; no live restart, deployment, fail-open,
credential inspection or automatic reenrollment is part of this change.

## Private real-Policy cold-restore entry

`scripts/docker-core-e2e.mjs` now has a narrow
`TREER_E2E_SCENARIO=cold-policy-recovery-226` mode. It reuses the ordinary
Docker harness's fresh PostgreSQL/NATS, two Proxy containers, synthetic account
and two new non-root machines. It does not accept an existing machine or enroll
against an external Proxy. An explicit image is required, its revision label
must match the checked-out commit, and Proxy startup uses its immutable image ID.
Use a clean committed checkout when building and running:

```sh
revision=$(git rev-parse HEAD)
docker build --build-arg TREER_BUILD_COMMIT="$revision" -t "treer-recovery226:$revision" .
TREER_E2E_IMAGE="treer-recovery226:$revision" \
  TREER_E2E_SCENARIO=cold-policy-recovery-226 node scripts/docker-core-e2e.mjs
```

The entry saves a real Agent startup declaration on machine A and installs the
bundled default Policy App on machine B. The provider is configured fail-closed
with zero stale allowance. It pauses only B and restarts both new Proxies to
remove compiled Policy cache, then cold-stops and starts A. It requires the
missing-Host `restart-controller` diagnostic, one explicit `restore` refusal
with `policy_provider_unavailable`, and an actual newly started nohup Host and
connected Controller. After unpausing B, actual provider HTTP readiness and a
bounded cache-expiry wait, it invokes `restore` once more. That separate call
must restore/attach the selected workload, load a fresh Policy revision, keep
the machine IDs and saved registration hashes, and reuse the Host started by
the refused call. The receipt counts CLI invocations, not captured HTTP POSTs;
the existing service unit tests cover no automatic POST retry.

The read-only fingerprint helper runs only with explicit synthetic-container
opt-in. It emits hashes of this fixture's generated Controller/Host configs and
machine identity, presence booleans and matched Host PID/birth metadata; it never
emits credentials or config bodies. It refuses ambiguous, aliased, nonregular or
oversized files. It neither adopts nor writes a service PID/birth record. A
failure finalizer unpauses only the new provider container; the outer harness
stops only its exact new containers and retains stopped containers/network/logs.
`recovery226.json` is a success receipt, `results.json` records check outcomes,
and `resources.json` identifies retained resources. `prerequisites226.json` records
fixed install/bootstrap/preflight/connect/status/help substage labels and bounded
error categories; it never copies command arguments, stderr, keys or error text.
The source image must contain the three native binaries plus their generated
`SHA256SUMS`; the runner verifies that exact set before creating services, avoiding
installer fallback to unrelated remote release checksums.
Collect this leaf explicitly when diagnosing a pre-cold failure; an aggregate
prerequisite FAIL alone does not identify the failed substage. Startup workload creation/readiness/declaration and Policy install/identity/configuration
are separate fixed checks in `results.json`. The synthetic Python PTY worker
has no AIS queue: fixture command input explicitly uses `delivery: "steer"`,
without notifications or required receipts; the default queued prompt contract
is not relaxed. Thus preparation failures remain
distinct from cold Host loss and restore. Cold stop/start, absent-Host identity,
restart guidance, first refusal, post-refusal Host identity/cache, provider health,
second restore/report and final identity/revision each have a fixed check label. After restarting Proxy containers, refresh the
harness API base from the actual new Docker port mapping before browser API
queries. `policy226.json` contains at most two fixed projections (HTTP status,
failure mode, revision and error-present boolean), never provider/error bodies. A failed run is not a
successful recovery or proof of historical incident cause.

Focused entry controls (no Docker or managed configuration needed):

```sh
node --test scripts/docker-core-e2e/cold-policy-recovery.test.mjs
python3 -B -m unittest discover -s scripts/docker-core-e2e -p recovery_fingerprint_test.py
```

These controls exercise orchestration refusal, bounded projection and explicit
retry sequencing with synthetic adapters/files. They are not themselves real
Policy or cold-restore acceptance.

### Actual isolated acceptance (2026-10-23)

[Hosted run 39977902738](https://github.com/EvoEvolver/treer/actions/runs/39977902738)
passed with exact source `0fffc102e8e9df837879ae85fe8452f22046ddf8` and
ordinary Docker build features. All 24 fixed checks passed. The real provider
outage returned `policy_provider_unavailable`, with unchanged registration and a
new reachable Host. Provider status was HTTP 200, fail-closed, revision null,
and an error present. After actual provider health returned, the second explicit
restore attached the selected workload, with revision 1 and no cache error;
registration hashes and the first restore's Host PID/creation ticks matched.
The receipt records exactly two explicit restore CLI invocations. All six owned
containers were stopped and unpaused before disposable runner teardown.

Safe `summary.json`, `recovery226.json`, `policy226.json`, `checks.json`,
`prerequisites226.json`, `source-image.json` and `resources.json` support this
result; no credentials, configuration bodies or raw diagnostic errors were
uploaded. Earlier failed jobs remain failures and were not relabelled. The
fixture fixes included same-source installer checksums, explicit steer for its
non-AIS Python PTY worker, and refreshing the external API port mapping after
Proxy container restart.

This establishes Linux/private nohup cold recovery with a remote real Policy
provider. Historical same-machine dependency cycle, physical macOS reboot,
service-manager boot behavior and enrolled-machine recovery remain unproven.
Previously accepted mock-HTTP, private Host and delivered hot-update evidence
remains separate. No live enrolled resource was restarted or re-enrolled.

### Same-machine Policy cold-start entry (2026-10-25)

Issue #226 is high priority and this isolated leaf is in progress. Core2 owns
`scripts/docker-core-e2e/same-machine-policy-recovery.mjs` and harness wiring;
Core owns any production startup/authorization change; Release executes one
fresh isolated job. #111/#222 keep their separate updater/distributor evidence.
The accepted remote-provider run is not repeated by this entry:

```sh
revision=$(git rev-parse HEAD)
docker build --build-arg TREER_BUILD_COMMIT="$revision" -t "treer-recovery226:$revision" .
TREER_E2E_IMAGE="treer-recovery226:$revision" \
  TREER_E2E_SCENARIO=same-machine-policy-recovery-226 node scripts/docker-core-e2e.mjs
```

Both the real default Policy App and declared inert workload run on the new
machine A. The fixture sets fail-closed/zero-stale Policy, cold-stops A, restarts
only owned Proxies to discard cached Policy, then starts A with no Host. Its
first explicit restore uses the normal Host/Controller startup-validation path;
no provider process is started by a fixture command. First success is valid only
with an actual matching workload report. If first restore returns the typed
Policy-unavailable refusal, only a separately invoked second restore is allowed,
after observed real provider HTTP readiness and bounded failure-cache expiry.
Arbitrary errors, an unready provider or altered identity fail closed without
another restore. There is no automatic restore retry loop.

Accept only unchanged enrollment hashes/workload IDs, one retained new Host
identity, actual fresh Policy revision and restored workload. The bounded
`same-machine226.json` records fixed monotonic event stages and first-result
category; it contains no error bodies or credentials. Events establish observation
order, not an independently sampled exact provider process-birth timestamp. All
owned containers must be stopped/unpaused by the existing harness finalizer.
A failure to reach provider health through ordinary startup is evidence of the
remaining ordering problem; do not manufacture success with alternate prestart.
Source controls cover first success, refused-then-success, wrong denial,
registration/Host changes and unavailable health. Actual Docker acceptance is
reported separately after execution, not inferred from those controls.

macOS boot is a distinct later entry requiring a newly allocated disposable
macOS VM/account with its own enrollment, Host and real Policy App. Bind the
actual selected installation mode and boot/start mechanism before running it:
nohup has no boot persistence, so measure missing-Host guidance plus explicit
restore after VM reboot; a service-manager boot claim requires a separately
supported mode and its own evidence. Reboot only that disposable VM, never the
currently enrolled mac_m2 or a team Host. Capture pre/post boot ID, identity
hashes, process creation identity and provider/readiness/restore outcomes, then
remove only the task-owned resources. This VM entry is not currently prepared
or executing and has no active leader allocation request. Linux results do not
establish macOS boot ordering or explain the historical incident.
