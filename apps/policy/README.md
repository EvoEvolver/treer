# Policy App

Policy is the bundled implementation of `treer.policy-provider/v1`. It stores
one workspace Policy bundle, serves it to the Proxy through its Managed App
service, and provides a browser UI for mode, rules, publication, revision
history, path templates, Core decision simulation and decision logs. Bundle
schema 1 and schema 2 (folder paths and relational conditions) are supported.
The Agent manual is [AGENT.md](AGENT.md).

## Deploy

The normal setup is **Install Default Policy App** in Workspace settings: Treer
copies this bundled App to the selected online machine, picks an unused port,
starts it with `--initialize-admin-token-file`, validates it and selects it as
the Provider. No checkout or download is needed on that machine.

To run it from a checkout instead:

```sh
treer app create --machine self --name policy --cwd treer --port 8787 \
  python3 -- apps/policy/policy.py
```

Then select `policy` under Policy Provider in Workspace settings, choose the
failure behavior and stale-cache window, and apply. The App must keep Workspace
access; Treer rejects public access for the active Provider.

The App listens on `127.0.0.1:8787`; `--port N` changes only the port.
`POLICY_LISTEN=HOST:PORT` overrides both (containers may need `0.0.0.0:8787`;
restrict that port to the Controller or a private network). `TREER_WORKSPACE_ID`
is required (Managed Apps inherit it). Bundle and state reads have no App
authentication so the Controller can poll them; they contain paths and group IDs,
so exposing the raw port bypasses Treer ingress. Loopback still admits local
processes.

The Proxy fetches immutable bundles over the Controller connection, evaluates
them locally, and refreshes every five seconds or immediately after an App
invalidation. Providers fail closed after their stale window unless fail-open is
explicitly chosen.

## State

State lives in `apps/policy/data/policy.json` for checkout runs and in
`.treer-policy-data.json` at the machine workspace root for bundled installs
(so upgrades do not replace it); `POLICY_DATA_FILE` overrides both. Writes are
atomic, mode 0600, with an optimistic revision check. The initial revision is
monitor mode with no rules. Use one writer process per data file.

Every publication keeps an immutable snapshot; the latest 100 are retained.
`POST /v1/policy/rollback` with `{"expected_revision":CURRENT,"revision":OLD}`
republishes the old document and mode as a **new, higher** revision and
invalidates the Proxy. A state file without history is backed up once to
`POLICY_DATA_FILE.pre-history.bak` when first loaded.

## Administration token

Reads stay open to the Controller; edits need an operator capability. Sources, in
order:

1. `POLICY_ADMIN_TOKEN` (32–8192 printable non-whitespace ASCII characters; an
   invalid value is a startup error).
2. `POLICY_ADMIN_TOKEN_FILE`.
3. For bundled installs, `.treer-policy-admin-token` beside `treer-policy.py`.

Checkout runs have editing disabled until one is configured. With
`--initialize-admin-token-file` the App generates 32 random bytes (base64url)
once and never uploads, returns, logs or prints them; a
`.treer-policy-admin-token.initialized` marker prevents silent regeneration on
restart. A missing, unsafe or invalid file locks edits (`policy_edits_locked`)
while bundle reads continue. Files must be regular, owned by the App's user,
mode 0600, with no symlinks in the path. Keep the marker in backups.

Send the token as `Authorization: Bearer TOKEN` (CLI) or
`X-Policy-Admin-Token: TOKEN` (browser; the UI keeps it only in page memory),
with `Content-Type: application/json`. An invalid dedicated header never falls
back to Bearer. Identity and CSRF checks still apply; injected identity headers
do not grant administration.

**Retrieve** the token from a private terminal on the machine as its account
(`cat <workspace-root>/.treer-policy-admin-token`), never through Proxy file
APIs, `treer exec` output, Messages or shared logs.

**Rotate or recover** locally; the running App rereads the file on every check,
so no restart is needed:

```sh
env -u POLICY_ADMIN_TOKEN POLICY_ADMIN_TOKEN_FILE=/absolute/workspace/.treer-policy-admin-token \
  python3 /absolute/workspace/treer-policy.py --rotate-admin-token-file
```

Rotation refuses an unsafe existing file; repair it locally first. An
environment token shadows the file.

## Publication authority

The Provider binding defaults to `publish_authority=owner_session`: schema-2
publication and rollback need the operator token **and** a verified current
workspace owner, checked by Core's validator before any change. An owner may
select `provider_operator` in Core's Provider settings; then **anyone holding the
operator token can publish (including switching to enforce) and simulate without
another human**. The token cannot change Provider selection or the owner rescue
path. Rotate the token to revoke a holder.

## Path policies (schema 2)

A schema-2 bundle is served only when the Proxy requests `max_schema_version=2`;
otherwise the App returns an unsupported-schema error rather than downgrading.
Make sure every Proxy replica supports schema 2 before publishing one.

A rule has an ID, signed 32-bit priority (higher first), allow/deny effect,
subjects, actions and resources. Subjects/resources are OR lists; each
selector's fields are AND. `tree` selectors take `kind`, `name`, `folder_id`, and
one of `path_eq`, `path_glob` or `path_prefix:{"path":"/team","include_self":true}`.
Paths are absolute, canonical, case-sensitive, at most 4096 bytes and 128
components; `*` matches one component, a terminal `**` zero or more.

Conditions (AND): `same_folder`, `resource_below_subject_folder` (optional
`strict`), `resource_below_folder`. Up to eight bindings map names to
`{ref:"subject.tree.folder"}` (or the resource equivalent), `{id:"folder_id"}`
or `{path:"/team"}`, referenced as `{var:"name"}`. Limits: 32 conditions, 1000
rules, 256 KiB per document. Missing placement facts never match. Core supplies
current Tree facts; moving or renaming a node changes matching.

The visual editor edits the first subject/resource selector and preserves other
fields; edit advanced constructs in the JSON editor. Templates
(`team-isolation`, `read-only-observer`) produce editable **monitor drafts** and
never publish. Enforce publication requires explicit browser confirmation.

## Human identity and diagnostics

Configure `POLICY_PROXY_URL` (falls back to `TREER_PROXY_URL`); `POLICY_SERVICE_ID`
and `POLICY_PUBLIC_URL` are discovered through `treer app list` for a Managed App.
Browser authentication verifies `X-Treer-Identity-Token` on every request via
`/.treer/apps/identity/verify` (an invalid token is rejected even with a valid
cookie); without it, Treer OAuth with S256 PKCE, one-use state and a browser
nonce. Sessions live in a private adjacent `.auth.sqlite3`. Human state changes
and diagnostic reads require `X-Policy-Request: 1`, a permitted Origin and
same-site Fetch Metadata.

Simulation and decision logs go through Core
`/api/apps/{service_id}/policy/explain`, `/validate` and `/decisions` with both
the verified human token and the bound Provider's runtime credential; Core
accepts only the current Provider and current owners. Logs show the latest 100
records of a 2048-record per-Proxy ring: a diagnostic view, not an audit log.

## API

- `GET /health`, `GET /v1/manifest`, `GET /v1/policy/bundle?workspace_id=W`,
  `GET /v1/state` (unauthenticated reads for the Controller and workspace)
- `GET /v1/policy/authority` (authenticated), `GET /v1/session`, `GET /api/auth/start`, `GET /api/auth/callback`,
  `POST /api/auth/logout`
- `GET /v1/templates`; `POST /v1/templates/team-isolation` or
  `/read-only-observer` with `{actor_path:"/team",target_path:"/"}` (draft only)
- `POST /v1/policy/validate` `{document}`; `POST /v1/policy/explain`
  (`subject:{kind,id|path}`, `action`, `resource:{kind,id|path}`, optional
  draft `document`/`mode`); `GET /v1/policy/decisions`
- `POST /v1/policy/publish` `{expected_revision,mode,document}`;
  `POST /v1/policy/invalidate` `{}`; `POST /v1/policy/rollback`
- `GET /v1/policy/history`, `GET /v1/policy/revisions/N` (operator token)
- `POST /v1/policy/test`: local schema-1 dry run only

## Agent client

`treer-policy` is a dependency-free JSON client, served at `/treer-policy`.
It reads `{"url": ..., "token": ...}` from `~/.config/treer-policy/config.json`
(or `--config FILE` / `POLICY_CLIENT_CONFIG`); the directory must be 0700 and the
file 0600, owned by the caller, no symlinks. There is no token argument or
environment variable, and the client does not follow redirects. Create the config
in a private terminal, for example:

```sh
python3 - <<'PYCONFIG'
import getpass, json, os
from pathlib import Path
root = Path.home() / '.config' / 'treer-policy'
root.mkdir(parents=True, mode=0o700, exist_ok=True)
fd = os.open(root / 'config.json', os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
with os.fdopen(fd, 'w') as stream:
    json.dump({'url': 'http://policy.treer/', 'token': getpass.getpass('Operator token: ')}, stream)
PYCONFIG
```

```sh
apps/policy/treer-policy read
apps/policy/treer-policy submit publication.json   # {expected_revision, mode, document}
apps/policy/treer-policy preview simulation.json   # explain contract
```

Agent publish/preview requires `provider_operator` authority.

## Isolating the administration Agent

Place the administration Agent and the App in a dedicated folder and gate access
by path, for example (merge into the existing document):

```json
{
  "schema_version": 2, "defaults": {}, "groups": {}, "rules": [
    {"id":"policy-admin-access","priority":30,"effect":"allow",
     "subjects":[{"kind":"agent","tree":{"path_eq":"/policy-admin/maintainer"}}],
     "actions":["identity.token.issue","service.connect","network.connect"],
     "resources":[{"tree":{"kind":"app","path_eq":"/policy-admin/policy"}}]},
    {"id":"policy-other-agents-deny","priority":20,"effect":"deny",
     "subjects":[{"kind":"agent"}],"actions":["identity.token.issue","service.connect","network.connect"],
     "resources":[{"tree":{"kind":"app","path_eq":"/policy-admin/policy"}}]}
  ]
}
```

Confirm matching with Core explain and stay in monitor mode (which does not
block) until decision logs look right. These rules gate App access only;
separately restrict other Agents from moving/renaming nodes, controlling the
administration Agent, changing its startup, executing on the machine or reading
its files. Path permissions follow placement, not stable IDs.

## Backup and limits

Back up `POLICY_DATA_FILE` (and `.pre-history.bak` if present) to private,
encrypted storage. `.auth.sqlite3` holds bearer tokens; omit it (users sign in
again) or copy it with SQLite's backup API as a credential store. Data is not
encrypted by the App, and 0600 files do not isolate other processes of the same
OS user: run the App and its administrator under a dedicated account or
container with a private port. There is no rate limiter or durable audit export.
Do not put secrets in policy strings. Naming an action in a rule does not create
an enforcement hook for it.

## Test

```sh
python3 -m unittest discover -s apps/policy/tests -v
POLICY_PLAYWRIGHT_MODULE=/path/to/playwright/index.mjs node --test apps/policy/tests/browser.mjs
```

Tests use only loopback fake Proxy/App processes with synthetic credentials.
