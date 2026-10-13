# Policy App

Policy is the bundled implementation of `treer.policy-provider/v1`. It stores a
single workspace Policy bundle, serves it to the Proxy through the Managed App
service, and provides a browser UI for mode, rules, publication, synchronization,
and decision simulation.

Deploy it from a managed Agent on the target workspace:

```sh
treer app create policy --port 8787 -- python3 /path/to/treer/apps/policy/policy.py
```

Then open Workspace settings in Treer, select `policy` under Policy Provider,
choose the failure behavior and stale-cache window, and apply. The App must keep
Workspace access; Treer rejects public access while it is the active Provider.

For the normal setup, use **Install Default Policy App** in Workspace settings.
Treer copies this bundled App to the selected online machine, chooses an unused
port, starts it, validates it, and selects it automatically. No repository
checkout or network download is required on that machine.

State defaults to `apps/policy/data/policy.json`. Override it with
`POLICY_DATA_FILE` when the source checkout is not the desired durable location.
Back up that file. Publication writes it atomically with mode `0600` and uses an
optimistic revision check. The initial revision is monitor mode with no rules,
which preserves the current allow behavior until an operator publishes policy.
Bundled installations store the same state in `.treer-policy-data.json` at the
machine workspace root so App upgrades do not replace it.

The Proxy fetches immutable bundles over the existing Controller connection,
evaluates them locally, and refreshes every five seconds or immediately after a
successful App invalidation. Configured Providers default to fail-closed after
their stale window; fail-open is available as an explicit compatibility choice.

## Administration security

Bundle reads remain available to the Controller. Publishing and invalidation
are **disabled by default**. To enable them set `POLICY_ADMIN_TOKEN` to a
cryptographically random, private operator secret of at least 32 non-whitespace
characters (for example generate with `python3 -c 'import secrets; print(secrets.token_urlsafe(32))'`
in a private terminal). Supply `Authorization: Bearer TOKEN` and
`Content-Type: application/json`; the UI has a password field and retains its
value only in page memory. Never put it in URLs, source control or shell history.
Protect its environment/configuration and use workspace-only ingress. Existing
instances upgrading must configure this secret to resume editing; reads and
Controller bundle polling continue without it. Injected identity/role headers
do not grant policy administration. This capability is separate from Treer
OAuth; human-role authorization is a future integration. Same-UID processes are
not isolated. State writes use exclusive private temporary files and atomic
rename. Do not rely on this App for hostile-tenant isolation.

## Revision history and rollback

Every publication retains an immutable snapshot in the private atomic state
file, including mode and timestamp. `GET /v1/policy/history` lists the latest
100 retained revisions; `GET /v1/policy/revisions/N` reads a snapshot. Both require
operator Bearer authentication. `POST /v1/policy/rollback` accepts
`{"expected_revision":CURRENT,"revision":OLD}` and publishes the old document
and mode as a **new, higher revision**, then invalidates the Proxy. It cannot
rewind the revision counter or silently override a concurrent publication.
The UI confirms permission/mode changes before restoration. Keep external
backups if older history is needed. Use one writer process per data file.

Existing single-document state is imported without changing rules, schema or
mode. Before the first history-aware write it is backed up exclusively to
`POLICY_DATA_FILE.pre-history.bak` with mode 0600. Keep a separate private backup
before upgrading. To roll back the storage software, stop only this App through
your operator workflow and restore that backup using the previous App version;
this is an operator procedure, never an automatic deployment action. A recovery
that lowers the persisted revision may require clearing/rebinding the Provider
through the workspace-owner recovery API, so prefer normal in-App rollback.
