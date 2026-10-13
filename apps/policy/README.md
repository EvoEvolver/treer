# Policy App

Policy is the bundled implementation of `treer.policy-provider/v1`. It stores a
single workspace Policy bundle, serves it to the Proxy through the Managed App
service, and provides a browser UI for mode, rules, publication, synchronization,
revision history, editable path templates, Core decision simulation and monitor logs.
Schema v1 remains supported; schema v2 adds folder paths and relational conditions.

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
OAuth. Path-policy publications additionally require a verified current workspace
owner through Core validation; OAuth alone never grants App editing. Same-UID processes are
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
mode. On the first history-aware load it is backed up exclusively to
`POLICY_DATA_FILE.pre-history.bak` with mode 0600. Keep a separate private backup
before upgrading. To roll back the storage software, stop only this App through
your operator workflow and restore that backup using the previous App version;
this is an operator procedure, never an automatic deployment action. A recovery
that lowers the persisted revision may require clearing/rebinding the Provider
through the workspace-owner recovery API, so prefer normal in-App rollback.


## Folder-path policies

The Provider envelope stays `treer.policy-provider/v1`. Its manifest advertises
both `policy.bundle.v1` and `policy.bundle.v2`. A schema-2 bundle is served only
when the Controller/Proxy requests `max_schema_version=2`; otherwise the App
returns an explicit unsupported-schema error. It never strips selectors or
conditions to manufacture an unsafe v1 downgrade. Upgrade Core and Controller
with `policy.path-rules.v2` support before publishing path rules.

A rule has an ID, signed 32-bit priority, allow/deny effect, subjects, actions and
resources. Subjects/resources are OR lists; each selector's fields are AND.
Schema 2 adds `tree` selectors: `kind`, `name`, `folder_id`, and one of `path_eq`,
`path_glob`, or `path_prefix:{"path":"/team","include_self":true}`.
Paths are absolute and canonical, case-sensitive, bounded to 4096 bytes and 128
components. `*` matches one whole component; one terminal `**` matches zero or
more. Prefix matches component boundaries. Use exact paths for literal wildcard
names. Moving or renaming a node intentionally changes path-policy matching.

Conditions are AND: `same_folder`, `resource_below_subject_folder` (optional
`strict`), or `resource_below_folder` with a typed folder reference. Up to eight
bindings map names to `{ref:"subject.tree.folder"}`, resource equivalent,
`{id:"folder_id"}`, or `{path:"/team"}`; conditions may reference `{var:"name"}`.
No chained variables or interpolation. Up to 32 conditions, 1000 rules and
256 KiB UTF-8 per document. Missing placement facts never match a path condition.
Core queries current Tree facts; the App never trusts self-reported identity or
Tree placement as real authorization context.

The visual editor edits the first subject/resource selector and preserves
advanced selector fields, additional OR alternatives, bindings and conditions.
Existing prefix `include_self=false` is preserved; edit advanced constructs in
the JSON editor. Unknown fields/types are rejected. Templates generate editable
**monitor drafts**, never publish automatically:

- Team isolation denies the listed collaboration actions by default, grants
  same-folder collaboration and ancestor-folder management; add explicit
  cross-folder grants to the draft.
- Read-only observer grants listed read actions in a target subtree and denies
  the listed write actions for the actor subtree. This is a starter template,
  not an exhaustive guarantee for future actions or processes on the machine.

Review human access and the concrete action lists before enforcement. Owner
Policy Provider/Tree recovery is enforced statically by Core, not generated
rules. Other owner operations may still be denied. Draft validation warns about
rules/defaults that may deny the operator; enforce publication requires an
explicit browser confirmation. The server calls the owner-authorized Core
validator before any v2 publication or rollback; errors leave state unchanged.

## Human identity and diagnostics

Configure `POLICY_PROXY_URL` (Treer Proxy's HTTP(S) base), `POLICY_SERVICE_ID`
and `POLICY_PUBLIC_URL` (canonical App URL, including a tunnel prefix if used).
With a Managed App runtime identity the latter two can be discovered through
`treer app list`. No environment credential or operator token is forwarded as
a human diagnostic credential. Keep the URLs operator-controlled.

Browser authentication accepts `X-Treer-Identity-Token` first and verifies it
on every request through `/.treer/apps/identity/verify` for this service and
workspace, expiry and current directory membership. An invalid injected token
is rejected even if a cookie is valid. Without the header, the standard Treer
OAuth button uses S256 PKCE, a one-use server-side state and a matching private
browser nonce. Sessions and OAuth state are stored in a private adjacent
`.auth.sqlite3`; callback URLs are fixed configuration, never user redirect
parameters. Cookies are HttpOnly, SameSite=Lax and Secure on HTTPS.

Human state changes require `X-Policy-Request: 1`, a permitted Origin when
present, and same-origin/same-site Fetch Metadata. Operator writes additionally
reject cross-origin Fetch Metadata and require JSON and the operator Bearer.
OAuth/identity cannot replace that administration capability. Use workspace-only
ingress; all policy/bundle reads are visible to workspace callers and direct
local-machine access must be separately protected.

The UI delegates simulation and bounded decision logs through Core
`/api/apps/{service_id}/policy/explain`, `/validate`, and `/decisions` using the
verified **human caller** token, never the App's workload token. Core recognizes that App-scoped owner token on its diagnostic routes. Core rejects
non-owners and stale membership. A Core without diagnostics returns an error;
the App never substitutes a local path evaluator with different security rules.
`/v1/policy/test` is retained only as a legacy v1 local dry-run endpoint.
Logs show computed denials (what enforce would deny), effective decisions and
revision context; they are a bounded per-Proxy diagnostic view, not durable,
complete or tamperproof auditing. No prompt/file bodies are logged by this App.

## API and checks

- `GET /v1/session`, `/api/auth/start`, `/api/auth/callback`;
  `POST /api/auth/logout` for human authentication.
- `GET /v1/templates`; `POST /v1/templates/team-isolation` or
  `/read-only-observer` with `{actor_path:"/team",target_path:"/"}` creates a
  draft only.
- `POST /v1/policy/validate` with `{document:...}` validates a draft (operator
  capability required; schema 2 additionally requires owner Core validation).
- `POST /v1/policy/explain` proxies the owner Core explain request (`subject:{kind,id}` / `resource:{kind,id}`,
  or `path` instead of `id`; optional draft `document` and `mode`);
  `GET /v1/policy/decisions` reads the owner Core diagnostic log.
- `POST /v1/policy/publish` with `{expected_revision:N,mode:"monitor",document:...}`;
  `POST /v1/policy/invalidate` with `{}` retries cache notification.
- History/rollback endpoints are described above.

Run `python3 -m unittest discover -s apps/policy/tests -v`. Browser tests use
Playwright supplied externally (no production build/dependencies):
`POLICY_PLAYWRIGHT_MODULE=/path/to/playwright/index.mjs node --test apps/policy/tests/browser.mjs`.
Tests start only loopback fake Proxy/App processes with synthetic credentials,
private temporary state and a closed environment, including ingress and tunnel
prefixes, desktop/light and 390px/dark. No deployed service is contacted.

## Backup and limits

Copy `POLICY_DATA_FILE` and its optional `.pre-history.bak` into an encrypted,
private backup directory (0700, files 0600). SQLite auth sessions contain bearer
tokens: omit them from policy backups where possible, forcing OAuth sign-in
after recovery; if backing them up, use SQLite's backup API and treat the copy
as a credential store. Data is not encrypted by the App; use encrypted storage
and backups. The latest 100 policy revisions are retained, not unlimited history.
New files are 0600 and new directories 0700; existing parent directory access
must be restricted by the operator. One process may write each data file.

Managed Apps run with the machine account's OS permissions. This App is not a
sandbox: use a dedicated user/container and restrict network/volume access for
untrusted workspace code. There is no distributed quota/rate limiter or durable
audit export. Bundle reads intentionally contain rule identities/paths; do not
put secrets in policy strings. State is immutable per revision but integrity
against the machine account/admin is not guaranteed. App-local action checks
(`app.use`, `email.send`, network restrictions) depend on separate Core/App
integration; naming an action in a rule does not invent an enforcement hook.
The simulator resolves existing current Tree paths; it cannot invent nonexistent
nodes or spoof placement. Draft simulations are not persisted decisions.
