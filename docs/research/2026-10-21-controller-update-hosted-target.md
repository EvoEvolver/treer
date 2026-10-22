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

## Measured Ubuntu target and exact provisioning decision

Inventory-only run `39697055173` measured the hosted Ubuntu22 image
`20261018.327.1`: effective UID1001, matching passwd/environment HOME
`/home/runner` owned by that account with mode0750, and successful actual
systemd user environment/unit-list queries. This establishes query reachability,
not manager survival. The only existing fixed Node candidate was a regular
root-owned `/usr/local/bin/node`, version22.23.3, mode0777, SHA256
`fde6a4bf8d0562f7751d1a2d6cb9b417c4cfe107bbcb0aa3e9a24e125e348f48`.
It fails the unchanged production `trusted_file` writable-component check.

Leader decision #222/7762 authorizes only a **new disposable hosted Ubuntu22.04
job** to change that exact existing pinned file from0777 to0755. Release owns
the provisioning receipt and embeds it in the positive job, without another
inventory-only dispatch. Before mutation require hosted/Linux/nonroot job scope,
fixed canonical pathname, lstat regular/root-owned image, exact realpath,
inode/hash/version and full ancestor owner/mode inventory. After the narrowly
scoped sudo operation, require the same inode/hash/root owner and mode0755;
then run unchanged production trust checks across every component. No other
file or ancestor repair, recursive chmod/chown, link, download, PATH fallback,
account creation, linger or trust-rule relaxation is authorized. A changed pin
or untrusted ancestor is a concrete unsupported prerequisite, not permission
to broaden provisioning. This decision does not apply to an enrolled machine
or the shared development account, and does not enable the product capability.
