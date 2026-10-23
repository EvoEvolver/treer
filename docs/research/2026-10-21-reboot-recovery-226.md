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
retry sequencing with synthetic adapters/files. They are not real Policy or
cold-restore acceptance. At authoring, Docker daemon access failed before any
fixture startup, so the new real entry remains **NOT_RUN**. Previously accepted
mock-HTTP, private Host and delivered hot-update evidence remains separate.
Even a passing new entry would establish Linux/private nohup recovery with a
remote fixture provider, not the historical same-machine dependency cycle,
physical reboot, service-manager boot behavior or enrolled-machine recovery.
