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
