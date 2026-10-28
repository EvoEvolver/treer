# Policy Provider protocol

A workspace owner can delegate authorization rules to a selected private
Managed App. The Proxy keeps authentication, workspace scoping, machine-to-Agent
binding, the Provider binding and owner recovery; the Provider supplies a
versioned rule bundle. The wire contract is `treer.policy-provider/v1`.

## Selection and transport

The owner selects the App in Workspace settings
(`PUT /api/workspaces/{ws}/policy-provider`). The Proxy fetches and validates
two endpoints over the Proxy-to-Controller command channel:

- `GET /v1/manifest` → `protocol`, `provider_name`, additive `capabilities`.
  `policy.bundle.v1` is required; schema 2 Providers also advertise
  `policy.bundle.v2`.
- `GET /v1/policy/bundle?workspace_id=...` → `protocol`, `workspace_id`,
  monotonic `revision`, `mode` (`monitor` or `enforce`), `document`,
  `generated_at`.

The Controller command (`service.http-fetch.v1`) connects only to
`127.0.0.1:<managed app port>` with an origin-relative path and a 1 MiB
transport limit; bundles are further limited to a 256 KiB document and 300 KiB
response. It is not a general HTTP client. Outer Provider objects are additive
(unknown fields and capabilities are ignored); the embedded
`WorkspacePolicyDocument` has a strict `schema_version`.

Selection requires the App's private ingress and standard Managed App service
mapping. An active Provider App cannot be switched to public ingress.

## Policy documents

Rules have `id`, `priority`, `effect` (`allow`/`deny`), `subjects`, `actions`
and `resources`. A rule granting an existing member audit access:

```json
{
  "id": "workspace-audit-reader",
  "priority": 0,
  "effect": "allow",
  "subjects": [{"kind": "human", "id": "usr_READER"}],
  "actions": ["workspace.audit.read"],
  "resources": [{"kind": "workspace", "id": "ws_TARGET"}]
}
```

Schema 2 adds Tree path selectors and folder relations. `*` matches exactly one
path component, a terminal `**` matches zero or more, and prefixes match on
component boundaries. Limits: 256 KiB serialized, 1000 rules, 8 bindings and 32
conditions per rule, paths up to 4096 bytes and 128 components. A schema 2
Provider also needs the Controller capability `policy.path-rules.v2`; Core never
drops conditions it cannot evaluate, and incompatibility fails explicitly even in
fail-open mode.

Tree facts come from PostgreSQL at evaluation time, never from the bundle or
the caller, so renames and moves take effect on the next request while the
bundle stays cached. If the Tree changes during evaluation the request fails
with `policy_tree_changed` and may be retried. A node without a placement
matches no path selector.

`workspace.audit.read` has a fixed baseline: current workspace owners
(including organization owners/admins) may read; other members and machine or
Agent identities need a matched allow rule in **enforce** mode. Monitor mode and
action defaults never grant it; an enforced deny can restrict an owner.

## Default policy and the default App

Without a Provider, a stored workspace Policy document applies if one exists;
otherwise the built-in **Treer Default** baseline allows everything in monitor
mode.

`POST /api/workspaces/{ws}/policy-provider/default-app` (owner only, body
`{"server_id": ...}`) installs the bundled [Policy App](../apps/policy/README.md)
on an online machine: the Proxy uploads the App files carried in its release
through the machine file protocol, creates a private Managed App on a free port
in `8787-8899`, starts it, validates it and selects it. No source checkout or
internet download is needed on the machine. Retrying updates and restarts the
official App while keeping its policy data.

## Publication authority

The binding's `publish_authority` defaults to `owner_session`: rule edits are
validated with a current owner's service-audience token. An owner may switch it
to `provider_operator` with
`PUT /api/workspaces/{ws}/policy-provider/publish-authority`
(`expected_app_id`, choice), which lets the App's own management-token holders
publish using the App's runtime credential against the operator
validate/explain routes. That is full Policy administration authority. The App
must require `policy.publish-authority.v1` and fail without fallback when it is
missing or withdrawn. Changing the selected App resets the choice to
`owner_session`. Core never receives the App's management token.

## Cache and failure

The Proxy compiles each bundle into an action-indexed immutable structure and
evaluates locally. It rechecks Provider metadata at most every five seconds,
with one fetch per workspace for concurrent misses. A Policy App may call
`POST /api/policy-provider/invalidate` on its local Controller with a higher
revision; the Proxy verifies the caller is the configured App's current runtime
and clears its cache.

On fetch failure the last valid bundle is used for `max_stale_seconds`. After
that, `fail_closed` rejects governed requests with
`policy_provider_unavailable`, `fail_open` permits them. Other replicas converge
through the five-second metadata refresh.

## Management and diagnostics

- `GET /api/workspaces/{ws}/policy-provider`: any member; `PUT`/`DELETE`: owner.
- `/api/workspaces/{ws}/policy/explain`, `/policy/validate`,
  `/policy/decisions`: owner diagnostics. Explain resolves paths and can
  simulate a candidate document without installing it; decisions are the
  latest monitor would-deny records held in memory on that Proxy.
- `/api/apps/{service_id}/policy/...`: the same diagnostics for the selected
  Provider, requiring both its runtime credential and a current owner's
  service-audience token; `/policy/operator/*` serves `provider_operator`
  mode.

Provider selection and removal, default-App installation and owner Tree
management are outside delegated Policy, so an owner can always recover. Any
Managed App serving these endpoints can replace the bundled Policy App.
