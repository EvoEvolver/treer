# Policy Provider Protocol

Treer can delegate workspace authorization data to a selected Managed App while
keeping authentication, workspace scoping, machine-to-Agent binding, Provider
binding, and recovery paths in the Proxy. The first wire contract is
`treer.policy-provider/v1`.

## Selection And Transport

A workspace owner selects one private Managed App in Workspace settings. During
selection the Proxy fetches and validates both endpoints over the existing
Proxy-to-Controller command channel:

- `GET /v1/manifest` returns `protocol`, `provider_name`, and additive
  `capabilities`. Version 1 requires `policy.bundle.v1`.
  A schema v2 Provider advertises `policy.bundle.v2`.
- `GET /v1/policy/bundle?workspace_id=...` returns `protocol`, `workspace_id`,
  monotonic `revision`, `mode`, `document`, and `generated_at`.

The Controller's `service.http-fetch.v1` command connects only to
`127.0.0.1:<managed-app-port>`, accepts only an origin-relative path, limits the
response to one MiB at the transport layer, and returns parsed JSON. Policy
bundles have the tighter existing 256 KiB document and 300 KiB response limits.
This is not a general Proxy HTTP client and cannot target arbitrary network
hosts.

Outer Provider objects are additive: consumers ignore unknown capabilities and
fields. The embedded `WorkspacePolicyDocument` retains its strict
`schema_version`; incompatible policy language changes require a new schema or
Provider protocol version. Existing subject, action, and resource strings stay
owned by the Proxy.

## Default Policy

Schema v2 adds path/glob/prefix selectors, typed folder bindings, and AND folder
relations without changing defaults. New Core requests
`max_schema_version=2`; old Core rejects schema 2 rather than dropping conditions.
New Core accepts schema 1 unchanged. A v2 Provider also requires the Controller
capability `policy.path-rules.v2`; unsupported Controllers fail before dispatch,
including across Proxy replicas. The envelope stays `treer.policy-provider/v1`.
Limits remain 256 KiB serialized document and 1000 rules, with at most 8 bindings,
32 conditions per rule, and 4096-byte/128-component paths. `*` matches exactly
one component; terminal `**` matches zero or more; prefixes use component
boundaries, not string prefixes. The [fixed v2 contract](research/2026-10-13-policy-schema-v2-contract.md)
has JSON examples, field definitions, and owner diagnostic bridge routes.

Tree context is current database truth rather than bundle contents. Renames and
moves immediately affect the next evaluation even while a previous bundle is
cached. If the Tree changes during evaluation, `policy_tree_changed` asks the
caller to retry; in-flight effects accepted earlier are not rolled back.
Capability/schema incompatibility never falls through to fail-open or cached
v1 rules. Outages continue to follow the configured failure/stale policy below.

This hard rejection applies to the **new Proxy implementation**. Older deployed
Proxy binaries (including d867347) reject parsing schema 2 but route that error
through their existing stale/fail-open behavior; they may therefore continue to
permit requests. Before publishing a v2 bundle, upgrade and drain **every** Proxy
replica and upgrade the Provider's Controller. Keep the Provider on schema 1 and
monitor until that deployment completes. A capability response from one replica
does not prove the whole fleet is compatible. Rolling back Proxy binaries after
v2 publication requires restoring a compatible v1 bundle first and reviewing
its mode/defaults. The App's per-request capability check is not a fleet fence.

The action index shares immutable rules and deduplicates repeated actions, so
large conditions are not cloned once per action. Its storage scales with input
bytes plus action references within the existing document limit. Directory and
recipient discovery through App tokens are filtered by Core discovery/metadata
and human-list Policy, while the verified human's own identity stays available
for Provider authentication; this is not a general Owner action bypass.

A workspace always has an explicit effective policy. If no Provider App is
selected, an existing stored Workspace Policy remains active; otherwise Treer's
built-in `Treer Default` baseline allows operations in monitor mode. This keeps
new and upgraded workspaces backward compatible without presenting an empty or
undefined authorization state.

Workspace owners can install the bundled default Policy App from Workspace
settings after an online machine is available. The Proxy carries the official
App files in its release, uploads them through the bounded machine-file
protocol, creates a private Managed App on an unused port in `8787-8899`, starts
it, validates its v1 manifest and initial bundle, and then selects it. The
installation does not download code from the internet or require a Treer source
checkout on the machine. A partial prior installation can be retried; Treer
updates and restarts the matching official App while preserving its policy data.

## Cache And Failure

The Proxy compiles each immutable bundle and evaluates it locally. It checks
Provider metadata at most every five seconds and uses a per-workspace
singleflight lock so concurrent misses produce one App fetch. A Policy App may
send `POST /api/policy-provider/invalidate` to its local Controller with its
workload headers and a higher revision. The Controller forwards that request;
the Proxy verifies the caller is the configured App's current runtime Agent,
persists the monotonic revision hint, and clears its local cache. This typed
route deliberately bypasses delegated Policy to avoid recursive authorization.

On fetch failure, the last valid bundle remains usable for the configured stale
window. After that window, `fail_closed` rejects governed requests with
`policy_provider_unavailable`; `fail_open` explicitly permits them. Workspaces
without a Provider continue through the legacy database Policy and finally the
current allow default. Provider configuration and removal clear local caches;
other Proxy replicas converge through the five-second metadata refresh.

## Management APIs

Authenticated workspace members may inspect
`GET /api/workspaces/{workspace_id}/policy-provider`. Only a workspace owner may
`PUT` or `DELETE` it. Selection validates the App's private ingress, standard
Managed App service mapping, manifest, and initial bundle before changing the
binding. Treer prevents an active Policy Provider App from being switched to
public ingress.

`POST /api/workspaces/{workspace_id}/policy-provider/default-app` is also
owner-only and accepts the target `server_id`. It is an explicit recovery and
bootstrap path, so it does not depend on delegated Policy being available.

The bundled [Policy App](../apps/policy/README.md) implements the protocol and a
browser UI, but it has no privileged implementation status. Another Managed App
can replace it by serving the same versioned endpoints.
