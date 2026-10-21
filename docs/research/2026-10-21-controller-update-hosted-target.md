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
