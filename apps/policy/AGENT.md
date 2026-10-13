# Treer Policy

Treer Policy owns a workspace's versioned Policy bundle. Browsers and callers
that send `Accept: text/html` receive the control interface.

## Inspect

```sh
curl -H 'Accept: application/json' "$APP_URL/v1/manifest"
curl -H 'Accept: application/json' "$APP_URL/v1/policy/bundle?workspace_id=$TREER_WORKSPACE_ID"
```

## Mutate

Use the browser interface to edit, validate, publish, and simulate Policy. A
publish increments the revision atomically and asks the local Treer Controller
to invalidate the Proxy cache. `POST /v1/policy/invalidate` with `{}` retries
that notification without changing the bundle.

Keep this Managed App's ingress set to Workspace access. Its mutation routes
rely on Treer ingress authentication and must never be exposed anonymously.
The App's workload credential is used for typed cache invalidation and the
restricted diagnostic bridge: Core additionally requires the verified current
human owner's App-audience token and the active Provider binding. Neither
credential alone authorizes diagnostics; the App cannot act as an owner deputy.

## API

- `GET /health`
- `GET /v1/manifest`
- `GET /v1/state`
- `GET /v1/policy/bundle?workspace_id=...`
- `POST /v1/policy/publish`
- `POST /v1/policy/invalidate`
- `GET /v1/policy/history`, `/v1/policy/revisions/N`
- `POST /v1/policy/rollback`, `/v1/policy/validate`
- `GET /v1/templates`, `POST /v1/templates/team-isolation`
- `POST /v1/policy/explain`, `GET /v1/policy/decisions`
- `POST /v1/policy/test` (legacy v1 only)

Policy administration is locked by default. An operator must configure the
private `POLICY_ADMIN_TOKEN` and supply it as Bearer authentication with JSON
for publish/invalidate, or enter it in the browser password field. Workspace
membership and self-reported role headers do not grant editing. Never include
this token in URLs, source control or logs. Bundle reads remain unchanged.


Schema-2 path policies require a schema-2 capable Core/Controller. The manifest
advertises policy.bundle.v2; bundle callers must request max_schema_version=2.
Never downgrade by removing Tree selectors/conditions. Use monitor initially.
Human diagnostics and v2 publication use injected App identity first, or OAuth
fallback, with per-request verification and current owner authorization in Core.
Publishing still requires the separate private operator Bearer. Human mutations
also need X-Policy-Request: 1 and valid browser Origin/Fetch Metadata.
Rollback preserves monotonic revisions and checks concurrent edits.
See README for grammar, editable templates, migration, backups and limits.
