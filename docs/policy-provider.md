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

The bundled [Policy App](../apps/policy/README.md) implements the protocol and a
browser UI, but it has no privileged implementation status. Another Managed App
can replace it by serving the same versioned endpoints.
