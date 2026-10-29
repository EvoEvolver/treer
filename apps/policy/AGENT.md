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
- `GET /v1/policy/authority`, `/treer-policy` (single-file client)

Policy administration is locked by default. An operator must configure the
private `POLICY_ADMIN_TOKEN` and supply it as Bearer authentication with JSON
for publish/invalidate, or enter it in the browser password field. Workspace
membership and self-reported role headers do not grant editing. Never include
this token in URLs, source control or logs. Bundle reads remain unchanged.


Schema-2 path policies require a schema-2 capable Core/Controller. The manifest
advertises policy.bundle.v2; bundle callers must request max_schema_version=2.
Never downgrade by removing Tree selectors/conditions. Use monitor initially.
Owner diagnostics and default v2 publication use injected App identity first,
or OAuth fallback, with per-request verification and current owner authorization
in Core. Owner-enabled operator publication uses the runtime-only Core bridge
instead. Publishing still requires the separate private operator capability
(Bearer for CLI/direct access, X-Policy-Admin-Token for browser tunnels). Human mutations
also need X-Policy-Request: 1 and valid browser Origin/Fetch Metadata.
Rollback preserves monotonic revisions and checks concurrent edits.
See README for grammar, editable templates, migration, backups and limits.


Default bundled installations provision `.treer-policy-admin-token` locally
with `--initialize-admin-token-file`; they never return the secret through Treer.
`POLICY_ADMIN_TOKEN` overrides `POLICY_ADMIN_TOKEN_FILE` and the bundled default.
File-backed authorization reads the private 0600 current-user-owned file per
request. Do not use Treer exec/file APIs or Messages to retrieve the secret.
Only a machine administrator may retrieve it in a private local/SSH terminal or
explicitly rotate/recover it with local `--rotate-admin-token-file` (no secret
output, no server/restart). A lost/unsafe file locks editing; persisted
initialization markers prevent silent regeneration on startup. See README.

## Operator-only publication (owner opt-in)

The default Core Provider setting is `publish_authority=owner_session`. Only
a workspace owner may select `provider_operator`; this trusts token holders
with full policy administration, including changing rules that govern themselves.
Owner Provider selection and rescue remain owner-only. `GET /v1/policy/authority`
reports Core's current setting; a missing capability/old Proxy explicitly fails
operator publication instead of falling back to the owner path.
Exception: rules naming administration actions (`org.*`, `workspace.manage`,
`workspace.access.manage`, `audit.read`, `machine.enroll`) cannot be added or
changed with the token alone; Core returns
`policy_admin_owner_session_required` and a signed-in workspace owner must
publish them.

Use the single-file `treer-policy` JSON client for `read`, `submit FILE` and
`preview FILE`. Its private `~/.config/treer-policy/config.json` contains the App
`url` and operator `token`; use `--config FILE` for another private config.
Provision it through an authorized private local terminal/secret mount, never
Treer exec output or Messages. The client requires a caller-owned 0700 directory
and caller-owned 0600 regular file, refuses symlinks and redirects, and never
prints the token. Submit `{expected_revision,mode,document}`; preview uses Core's
explain request. Management tokens are never forwarded to Core authority/diagnostic APIs;
requests to the App carry them through normal private App forwarding.
Use the Agent's own managed HTTP proxy/workload identity. In default mode,
operator-only Agent v2 publication/preview is rejected.

Isolate the administration Agent in its own tree folder and restrict Policy App
access, tree changes, management-Agent control and machine exec/file rights.
Use a dedicated OS user or container with private network/secret mounts. Mode
0600 does not isolate other Agents using the same OS account; monitor mode does
not block access. See README for a reviewed starting rule set and limitations.
