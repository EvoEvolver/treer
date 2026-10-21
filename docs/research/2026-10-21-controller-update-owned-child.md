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

The complete worker-death/update-state matrix, Linux/macOS service-manager
survival, independent disposable-account acceptance and the #220 installed-image
metrics-helper guard union remain enable-blocking. The capability stays OFF.
