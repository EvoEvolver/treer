# Controller update: directly owned launcher children

This #222 checkpoint implements the reviewed direct-ownership design on top of
the #111 checked image, journal and SAME-lock handoff primitives. It does not
enable the native capability or authorize a real installation. The original
worker owns one fixed install child and then one fixed status child; neither
discovery nor a restarted Controller can recreate that authority.

## Spawn and completion boundary

`Invocation::record_start_intent` creates, syncs and reads back its exclusive
intent through the retained journal directory. Any existing or uncertain intent
refuses another spawn. The worker retains authenticated target Controller/CLI
images and the checked Node recipe, uses fixed arguments and a clean environment,
and gives only the installer its private temporary directory. stdin and stderr
are null; bounded stdout is private parser input, never receipt or log content.

The platform leaf starts a private session. On Linux its pre-exec callback sets
PDEATHSIG to SIGKILL and checks the captured parent PID after setting it, closing
the parent-death setup race. Only async-signal-safe syscalls run in that callback.
The private worker runs on one thread: Linux associates PDEATHSIG with the
spawning thread, so moving this path to a multithread runtime needs a new review.
macOS starts a private session but has no parent-death mechanism wired here.

Immediately after spawn, before accepting output, the observer captures the
owned Child's OS image, authenticates it against the retained Controller image,
and verifies the original parent and private session. It exclusively persists
the receipt binding, operation, intent digest, PID, creation and parent identities
and Controller digest, syncs the file and directory, and reads it back. The
original record handle and bytes remain retained. Completion rechecks its inode,
bytes, intent and retained images after bounded process-group quiescence. It
does not reinterpret a saved PID as current liveness evidence.

The intent-to-record gap remains ambiguous: spawn may have happened even if
recording fails. Observer failure kills/reaps the owned group through the bounded
child guard and yields no quiescent completion witness. Cancellation, worker
death, timeout, record corruption and uncertain quiescence never permit replay,
noexec inference or active-claim release. Linux parent-death handling kills only
the direct child; it does not prove all descendants gone or an unchanged system.
Trusted cooperating installers must keep descendants in the owned group and
signalable. This lifecycle is not a sandbox.

Strict authenticated install/status reports and a fresh whole-health check of
the actual new Controller/CLI, unchanged Host and workload snapshot are still
required before the original worker publishes Complete and matching claim
release. Exit status, stdout or a health response alone cannot do this.

## Recovery and acceptance limits

The supervisor registration, permit arbitration and authenticated-empty-EOF
noexec modules remain unused model primitives for this path. Copied metadata
cannot reconstruct them or authorize a transition. Restart discovery is read
only; unresolved intent keeps the native claim and blocks further updates.
The [operator recovery procedure](../operator-guide.md#controller-updates-from-the-console-integration-in-progress)
requires independent identity/quiescence investigation and an audited local
disposition. There is no automatic recovery/claim-clearing API.

Hermetic controls cover original intent collision, success/failure/cancellation,
empty child records, changed bytes and same-byte inode replacement, with no
second spawn. Platform controls reject observer failure without a completion
witness. A Linux container test SIGKILLs an owned fixture parent without Drop
and observes the separately sessioned direct child die from PDEATHSIG. This is
not a disposable-account native update or service-manager survival acceptance.
Existing handoff, image, report and health negative controls remain mandatory.

The #111/#222 source union combines the directly owned worker checkpoint
`b79ab097` with the accepted `c7cf3b05` baseline. It retains the Host isolation
and Unix writer-reclamation fixes, the #220 installed-image metrics-helper guard,
and the #226 identity-preserving recovery path. The fixed private update-worker
entry runs before the metrics probe and normal Controller startup. Neither
entry can fall through to the other after an invalid invocation. This closes
the missing helper-guard ancestor composition at source level; it does not
attest a live update or service-manager survival.

Core owns this isolated union and its integration checks; Core2 owns the
remaining worker-death/update-state fixtures and native executor follow-up.
The complete worker-death/update-state matrix, Linux/macOS service-manager
survival and independent disposable-account acceptance remain enable-blocking.
The capability stays OFF. No real account, enrolled installation or system
service is modified by source integration or its private refusal tests.

## Source-union development checks

The isolated Core union passed 176 native self-update tests (10 subprocess
helpers ignored as standalone tests), 13 protocol update tests, the real
executable private-entry refusal test, and the installed-image helper test for
replaced, modified and job-local images. Fourteen update API/persistence tests
passed against a fresh private UTF-8 PostgreSQL instance; that instance was
stopped afterward. The Host, runtime and metrics implementation trees remain
byte-identical to `c7cf3b05`.

Web resolved the five overlapping presentation/documentation paths and passed
TypeScript, a production build and 54 focused browser checks across three
engines. These browser checks use local HTTP fixtures. None of these results
is actual enrolled update acceptance, a complete author gate or evidence that
the remaining service-manager/worker-death acceptance matrix is complete.

## Resumed terminal death coverage and manager fixture

The named test
`self_update::web_result::tests::sigkill_publication_windows_are_unconfirmed_after_reopen`
SIGKILLs a real private subprocess at three durable boundaries: Complete receipt
before whole-outcome publication, synced outcome before active-claim removal,
and active-claim removal before the release marker. It opens the journal again
and repeats the real native read-only projection, requiring unconfirmed and
byte-stable receipt/claim state with no release marker or launcher intent.
The last window deliberately has no native active file: its absence is not a
completion witness, and the Proxy's unresolved machine claim must still hold.
These tests construct a valid synthetic terminal receipt; they do not prove a
real Core installation, health observation or authenticated SAME-lock transfer.
The test-only pause points do not exist in production builds.

`scripts/controller-update-manager-fixture.py` supplies a reproducible inert
Host/Controller/worker topology and exact systemd-user/launchd-user command
arrays. Preparation creates only an exclusive private temporary directory,
plan and optional plist; it never installs or invokes a manager. It uses no
Treer executable, enrollment, credential, network, package install or updater.
The actual host entry refuses a managed Agent, root, an omitted disposable
acknowledgment, wrong account, changed script and platform mismatch. Those
checks are guardrails, not proof an account is disposable.

Prepare on each **existing, independently designated disposable account/VM**:

```sh
python3 scripts/controller-update-manager-fixture.py prepare \
  --root /tmp/treer-update-manager-UNIQUE --manager systemd
# On a disposable macOS account instead: --manager launchd
```

Use a fresh nonexistent root under the platform's canonical temporary directory
(on macOS use its actual `TMPDIR` path rather than assuming `/tmp` is canonical).
The output's `manual_start_argv`, optional `manual_kickstart_argv`,
`manual_inspect_argv` and `manual_stop_argv` are exact commands for a separately
explicit manager execution. Preparing them grants no service registration or
account/VM creation permission. Retain the named unit/label and source digest;
never operate a general Treer unit or a previous run's label.

Once that disposable manager fixture is explicitly run, wait for `worker.json`
and `heartbeat`, then exclusively create `replace-controller` inside that root.
The Host kills and reaps only its retained Controller child's private group and
observes whether the worker heartbeat advances. `replacement-result.json` names
this narrow scope. The worker was an actual child of the old Controller and
starts its own session, but is an inert heartbeat, not the updater worker.
Record manager PID/birth/session/cgroup evidence independently. A second case
must stop the **whole fixture unit** and observe its distinct effect: systemd
control-group termination can kill even a detached-session worker. Do not
change KillMode/launchd settings just to make survival appear to pass. Each
case uses a fresh fixture, no replay/adoption, and keeps every failed result.

Normal cleanup requests `stop` and uses only that plan's manager stop argv.
The inert worker has a 90-second absolute lifetime and cooperative done marker;
neither elapsed time, heartbeat cessation nor that marker proves full OS
quiescence/reaping. Independently reconcile exact recorded process identities,
unit/cgroup/label and remaining fixture duties before removing files. Do not
kill a saved PID. Manager teardown may prevent finally/Drop from running;
retain unresolved cleanup rather than inventing success. macOS has no parent-
death mechanism in the real updater path yet, so this fixture cannot close that
gap or enable its capability. Windows stays unsupported.

This gives runnable topology diagnostics before a real native acceptance target
exists. E3/E5 remain open until the actual native worker/image replacement,
launcher/health/claim outcome and relevant manager lifecycle are exercised on
that independently isolated target. E1's earlier/mid-install windows still need
real authenticated worker-kill evidence in addition to the existing cancellation,
intent, image and handoff tests. No claim that one inert probe closes all gates.
