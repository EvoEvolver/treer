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
