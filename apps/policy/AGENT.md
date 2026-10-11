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
The App's workload credential is used only for the typed invalidation route.

## API

- `GET /health`
- `GET /v1/manifest`
- `GET /v1/state`
- `GET /v1/policy/bundle?workspace_id=...`
- `POST /v1/policy/publish`
- `POST /v1/policy/invalidate`
- `POST /v1/policy/test`
