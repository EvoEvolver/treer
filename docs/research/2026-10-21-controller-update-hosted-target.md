# Dedicated hosted native update target

Leader owns target allocation. Existing dedicated hosted native CI jobs may
exercise the actual ephemeral OS account without creating an account or VM on
the shared development machine. This is a target candidate, not an accepted
native update environment. Core owns target-contract integration, Core2 owns
the real positive updater fixture, and Release owns the pinned dedicated job.

Release owns the single read-only inventory script in its dedicated job;
Core2 owns production retained-image preflight and the actual native fixture.
The inventory requires hosted-workflow eligibility signals. Those signals alone
do not authorize mutation or prove isolation. It records the effective UID,
canonical passwd HOME and ownership, the first existing fixed Node candidate
and its version, and the actual user-manager connection result. Node hooks are
removed for the version query. Manager stdout/stderr is never published because
it may include environment values. A zero exit code means account/Node version
preconditions were measured; it does **not** mean Node image trust, manager
availability, worker survival or native update acceptance passed. The native
fixture must still exercise production retained-image/ownership checks.

The Linux manager query is `systemctl --user show-environment`; the current
launchd fixture requires `gui/<effective uid>` and queries that exact domain.
A missing user bus/domain or nonzero result is an environment prerequisite
failure. Do not silently substitute another manager, change KillMode, simulate
service-manager output or label process-only execution as manager acceptance.
The inventory must register no unit, invoke no updater or mutate the account.

The dedicated job must pin the complete canonical source and build the actual
Host, Controller and CLI images needed by the Core2 fixture. A real positive
entry must run authenticated bootstrap/SAME-lock transfer, actual Host baseline,
Core image publication, replacement Controller/epoch/health, and fixed launcher
install/status under the real effective-account HOME and trusted Node. Preserve
the source capability gate OFF; use a private test entry, not an enabled public
capability, arbitrary Exec or legacy updater. Synthetic enrollment is isolated
test identity; fabricated CoreHealthy or health is not acceptance.

Use exclusive job-owned roots and ports. Record exact process identities and
manager unit/domain before operations, retain failed evidence, and reconcile
owned children, listeners and manager duties before claiming cleanup. Never
signal a saved unverified PID or operate an enrolled/general Treer service.
Any actual unsupported operation must be returned with the smallest additional
disposable-target requirement and leader as allocation owner. Neither a
prepare-only manager plan nor passing negative/death tests closes this work.

## Executable hosted native fixture (non-default build)

Core2's source adds feature `native-update-acceptance`, never a default or an
official release feature. Only that build recognizes the fixed, no-argument
`--hosted-native-update-acceptance` entry. Release must build the OLD installed
Controller with this feature: the production handoff authenticates its submitting
process against the installed old image, so a separate test binary cannot stand
in for it. The worker still branches through the existing production private
entry before runtime startup; native acceptance/capability remains OFF.

The exact actual fixture is:

```sh
cargo test --locked -p treer-agent-server --features native-update-acceptance \
  --bin treer-agent-server -- --exact \
  self_update::hosted_acceptance::fixture_tests::hosted_complete_native_fixture \
  --ignored
```

It must run ONLY in the Release-owned dedicated hosted job. Before invocation,
Release binds the source/run and artifact manifests, sets
`TREER_DISPOSABLE_RUNNER=1`, and supplies the genuine `GITHUB_ACTIONS=true` and
`RUNNER_ENVIRONMENT=github-hosted` context, with no managed Agent credentials.
These environment prerequisites do not independently attest a hosted runner.
HOME must equal the canonical passwd HOME for the real nonroot effective UID.
No environment HOME substitution, new account, service, VM, PATH-selected Node
or production trust-rule weakening is part of this fixture.

Create one NEW private physical directory directly under that HOME, named
`treer-native-acceptance-<canonical UUID>`, with mode0700. Set
`TREER_ACCEPTANCE_ROOT` to it, `XDG_CONFIG_HOME` to its `xdg` child and
`TREER_STATE_DIR` to its `state` child. Provision only these input files; the
fixture exclusively creates all remaining config/state/workspace files:

- `bin/treer-agent-server`: OLD dedicated feature build, executable and pinned.
- `bin/treer`: matching OLD CLI, executable and pinned.
- `bin/treer-agent-host`: actual compatible Host, executable and independently pinned.
- `artifacts/treer-agent-server`, `artifacts/treer`,
  `artifacts/treer-agent-host`, `artifacts/SHA256SUMS`: NEW actual native
  artifacts and the exact canonical three-entry checksum document.
- `acceptance-input.json`: private0600, exactly `current` (shared Identity),
  `target` (shared Target), `sums_sha256` (NEW document pin), and `host_sha256`
  (the actual unchanged Host pin). All identities/pins come from Release's
  source-bound manifests, never from self-reported hashes of arbitrary files.

The NEW Controller must contain the real verified portable launcher bundle,
prepared through the existing artifact build's `TREER_LAUNCHER_PAYLOAD_DIR` /
`TREER_REQUIRE_LAUNCHER_BUNDLE=1` path. A source-only cargo development binary
has no portable runtime and must fail install; the fixture never supplies
`--allow-network`, an npm path, skipped profiles or a fabricated launcher report.
OLD and NEW source SHAs must match their actual binary version output. Native
image replacement requires different Controller digests. Same-source feature
and normal builds can test physical replacement but are not evidence of a
released version-to-version upgrade.

The fixture starts its own real Host and its old Controller, with synthetic
`ws_native_acceptance` / `srv_native_acceptance` identity and a loopback synthetic
Proxy connection. Its explicit in-memory profile CRUD/readback and machine
snapshot transport support the unmodified launcher's real CLI requests. No
Policy/Proxy database acceptance is inferred from those synthetic handlers;
no arbitrary command or execution handler is supplied. It writes an operator-pinned strictly-forward local catalog,
authenticates all supplied old/new images before executing them, captures the
actual Host/old Controller baseline, and invokes the installed OLD native entry
ONCE. That entry has its own exclusive synced/read-back intent. It calls the
real preparation, SAME-lock handoff, production worker, Core replacement and
fixed launcher install/status. Completion requires the production whole-health
result, Complete receipt and absent matching active claim, followed by a second
read-only operation projection. Any error or uncertain prior intent fails;
there is no POST retry, new UUID, copied ownership, fake health or legacy path.

The dedicated local-artifact preparation seam is compiled only with this
feature. It shares production preparation/admission, checked private file
handles, SUMS/document/image pins and all version/baseline checks. It bypasses
only the network fetch by supplying previously downloaded, Release-bound files;
production HTTPS/redirect/origin validation is unchanged. This is explicitly
NOT distributor transport acceptance, public API/ProxyDB acceptance or manager
lifecycle acceptance. Those results stay separate, as does Windows unsupported.
Actual fixed-candidate Node and account checks run through the production
Invocation in this path. Release owns the single inventory and all dispatches;
no duplicate inventory script or separate preflight-only successor is added.

Cleanup is scoped to the owned fixture Host: verify its retained creation/image
identity, send SIGTERM, reap it through the real Host shutdown path, and require
the private socket gone. The fixture aborts/reaps its synthetic Proxy task and
preserves `fixture-cleanup.json`, native result, durable intents/journal and
launcher files. It does not delete HOME, unknown processes, evidence, or install
any manager unit. Launcher account shim must be absent before the run; afterward
its retained files and the job-private launcher/profile state belong to the
disposable job teardown. On a failed/ambiguous run, Release must inspect the
recorded worker identity/claim and reconcile all owned duties before claiming
cleanup; native failure or timeouts do not prove quiescence. Do not rerun the
same root or silently clear its intent. On success, native-result cleanup is
still separate from process/manager acceptance. Actual hosted execution of this
source is pending Release; local compilation and guard tests are not acceptance.
