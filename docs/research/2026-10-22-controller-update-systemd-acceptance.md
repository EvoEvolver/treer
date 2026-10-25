# Hosted Controller update under a transient systemd user unit

This is the executable successor to the Linux native physical-replacement
fixture accepted at c545d277 (#222, run 39706258946). It adds a real manager
boundary, not the inert heartbeat model in
`scripts/controller-update-manager-fixture.py`. The capability remains OFF.
No enrolled machine, installed service, account or VM is changed by local checks.
The ordinary release build excludes the `native-update-acceptance` feature.

## Dedicated job entry

Release owns the dedicated disposable Ubuntu job and all dispatches. Use a NEW
physical effective passwd-HOME `tn-<16 lowercase hex>` root, the same six private
native copies, source-bound identities and portable bundle as the accepted
fixture. Keep exact approved Node/directory provisioning, owner/mode/no-follow
checks, release-profile images, the 128 MiB limit, private executable copies
0700/nlink1, JSON/SUMS0600 and directories0700. Recompute pins from this job's
actual artifacts. Never reuse a spent operation or root. The manager observer
must remain outside the unit and uses the actual user bus; no HOME substitution,
linger, service enablement or fallback manager is permitted.

Run only this ignored entry (the previous positive entry remains separate):

```sh
cargo test --locked --release -p treer-agent-server \
  --features native-update-acceptance \
  self_update::hosted_acceptance::fixture_tests::hosted_systemd_native_fixture \
  -- --exact --ignored --nocapture
```

All artifact build, source SHA, input binding and environment requirements are
unchanged from the hosted-target contract. The new test reads the same input.
The old installed Controller and test driver require the acceptance feature;
the replacement Controller/CLI and portable launcher are normal release builds.
The helper entry is INTERNAL to ExecStartPost, never a second workflow command:
`self_update::hosted_acceptance::fixture_tests::manager::hosted_systemd_submit_helper`.

## Actual topology and assertions

The observer/synthetic Proxy stay outside the service. A single
`systemd-run --user --no-block` creates a task-owned
`treer-native-<root token>.service`. ExecStart uses `/usr/bin/env -i` followed by
fixed fixture environment and the actual pinned Host `run --config` recipe.
MainPID must exec the pinned Host. Type=simple, Restart=always/RestartSec=2 and
explicit KillMode=control-group describe the tested topology. StartLimitBurst=1
and StartLimitIntervalSec=600 prevent a second service start in the bounded test;
this is not production restart/reboot acceptance. TimeoutStartSec=480 and
TimeoutStopSec=15 bound this unit. Output is null, not raw archived diagnostics.

ExecStartPost runs the fixed ignored helper from the exact test executable with
a clean environment. Before waiting or spawning it creates a synced exclusive
`native-manager-helper-intent.json`; an existing marker refuses, so no manager
restart can resend submit. The observer writes the original dispatch binding
before Host start and releases the helper only after old Controller health,
retained Host identity, unit identity and old Controller identity are checked.
The helper invokes the exact installed OLD Controller's existing private submit
entry once. Host, old/new Controller, submitter, worker and launcher therefore
inherit the unit's cgroup; setsid alone is never used as membership evidence.

After the real peer has authenticated and BEFORE offering SAME-lock ownership,
a feature-only observer reauthenticates the owned worker's actual image/private
session, then reads parent and worker `/proc` cgroup-v2 membership. Both must
match the private expected unit. This is read-only evidence: no fake worker,
new locks, transfer authority, replay or claim release. The observer call and
all its code are absent default builds. Unsupported/ambiguous cgroup-v1 or
unavailable `/proc` refuses; there is no synthetic substitute.

Before ready release the observer accepts only `activating/start-post`, with
matching retained identities. It must not wait for active: ExecStartPost is
waiting for that ready marker. During replacement the only accepted state pairs
are `activating/start-post` and `active/running`; after helper success the fixture
requires `active/running`. Missing or mismatched substates refuse.

Before and after replacement the manager observer checks Id, MainPID,
ControlGroup, InvocationID, NRestarts=0, Transient=yes and
KillMode=control-group. The retained Host image/creation identity must survive;
its direct old/new Controller children are separately authenticated against the
actual pinned digests and their cgroup checked. The old identity must no longer
be live. The original native fixture still requires actual Complete, matching
claim absence, fresh identical operation projection, whole health and final
retained Controller+CLI VerifiedImage checks. No inner result field is rewritten
from `manager_lifecycle_acceptance:not_run`; the separate manager result carries
this test's additional evidence.

## Stop and evidence

Only a successfully acknowledged, still identity-matching task-owned unit is
stopped with `systemctl --user stop <exact unit>`. No unit collision is adopted.
No reset-failed, manager restart, broad process kill or claim clearing is used.
Cleanup requires unit inactive/absent with MainPID0, zero restarts when retained,
empty/removed original cgroup, old retained Host creation identity gone and Host
socket absent. A missing witness remains UNPROVEN for disposable-job teardown;
no failure authorizes retry. Account launcher files remain for job teardown.

Release may add ONLY `native-manager-worker.json` and
`native-manager-result.json` to its existing bounded JSON artifact allowlist.
Both are <=64 KiB,0600,exclusive,synced and contain schema1 plus fixed booleans,
enums and scope labels; no PID, creation token, config, credentials or argv.
The worker receipt proves authenticated image, private session and same cgroup.
The result requires all native checks AND unit cleanup for manager PASS, and
explicitly leaves production service install/reboot NOT_RUN. If unit creation
is uncertain, the result has stage `unit_start`, acceptance `unproven`, and a
fixed cleanup instruction. Preserve task-owned private intent/expected/ready
files in the spent job until teardown; these are INTERNAL control leaves, not
extra upload globs or authorization to operate another unit.

This test can establish Linux transient-user-manager replacement and stop at
its actual tested scope only. It cannot establish system service/reboot
survival, macOS/Windows, actual ProxyDB/Policy, network distribution, a released
version upgrade or enrolled production activation. Do not enable capability
from it. Historical failed-run root causes/cleanup claims remain separate.

## Actual transient-user-manager result

Run [39709601135](https://github.com/EvoEvolver/treer/actions/runs/39709601135)
executed exact source `c56b89203cf062d3f03d30ab144dfcb50401a38a` once.
The ignored manager fixture passed (1PASS/0FAIL,2.91s), with native operation
`1260ddf2-cfc2-4a69-a3df-f85956542801` Complete revision7/healthy and observed
Controller/CLI identity matching the target. The separate manager result reports
retained Host MainPID identity, healthy Controller replacement, authenticated
same-cgroup worker, zero manager restarts, and stopped unit/empty cgroup. The
worker receipt records actual image/private-session/cgroup checks. Host/socket
cleanup passed; account launcher files remain for completed disposable teardown.
The inner native result's manager NOT_RUN field was preserved; the separate
manager receipt supplies this additional tested scope.

Release's `manager-result.json` SHA256 is
`f7f8de947de249f636cb22df87d9e0b81f7af5002bb328764aeeeacdf7648037`.
Core independently checked all36 indexed artifact byte counts/hashes. This is
Linux transient USER-manager, same-source feature-to-normal physical replacement
only, with the scope exclusions above and capability OFF. No automatic retry
is required. Historical run39704103725 UNKNOWN cause/UNPROVEN cleanup is unchanged.

The stricter ActiveState/SubState pairing described above was added afterward
in canonical `faf9a4c436d191f1f3929a9cd6e4cdf1e6c70020`. It has focused local
negative-test evidence; this actual c56 run did not exercise those new assertions.
Do not rebind the completed run or label that follow-up as natively exercised.
