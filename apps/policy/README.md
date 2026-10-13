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

The App listens on `127.0.0.1:8787` by default; `--port N` changes only the port
and keeps the loopback bind. Controller forwarding reaches it locally, so direct
machine-network access is unnecessary. `POLICY_LISTEN=HOST:PORT` explicitly
overrides both (including `--port`). Container deployments may need an explicit
bind such as `0.0.0.0:8787`; restrict the published port to the Controller or a
private network. Bundle/state reads intentionally have no App authentication
for Controller access and contain paths/group IDs: exposing the raw port bypasses
private Treer ingress. Loopback still permits access by local machine processes.

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

Bundle reads remain available to the Controller. Normal source-checkout runs
have editing disabled until an operator configures a capability. Default bundled
installations use a private machine-local token file, provisioned by the installer
flag described below. Existing `POLICY_ADMIN_TOKEN` environments remain supported:
the value must be 32–8192 printable, non-whitespace ASCII characters.
Supply `Authorization: Bearer TOKEN` and
`Content-Type: application/json`; the UI has a password field and retains its
value only in page memory. Never put it in URLs, source control or shell history.
Protect its environment/configuration and use workspace-only ingress. Existing
instances upgrading must configure this secret to resume editing; reads and
Controller bundle polling continue without it. Injected identity/role headers
do not grant policy administration. This capability is separate from Treer
OAuth. Path-policy publications default to requiring a verified current workspace
owner through Core validation (the owner may explicitly choose operator-only
publication as described below); OAuth alone never grants App editing. Same-UID processes are
not isolated. State writes use exclusive private temporary files and atomic
rename. Do not rely on this App for hostile-tenant isolation.

## Private administration token file

The default installer passes `--initialize-admin-token-file`. The **App on the
machine**, not the Proxy, generates 32 cryptographically random bytes encoded as
base64url and stores them in `.treer-policy-admin-token` beside the bundled
`treer-policy.py` in the machine workspace root. No secret is uploaded, returned
by HTTP, placed in argv, or written to logs/audit by provisioning. Reinstalling or
upgrading does not overwrite it. Existing bundles/history are unchanged.

Selection order is `POLICY_ADMIN_TOKEN` (compatibility), then
`POLICY_ADMIN_TOKEN_FILE`, then the default adjacent file for a bundled install.
An invalid environment token is an error, not a fallback. A selected file that
is missing/unsafe/invalid locks edits with `policy_edits_locked`; bundle reads
stay available and the App never falls back to an older cached token. Source
checkouts do not create a token unless explicitly initialized; setting a file
path alone never creates it. Relative paths use the process working directory;
prefer a canonical absolute path.

Files must be regular files, owned by the App's effective OS user and mode
**0600**. Symlinks, including directory ancestors, are rejected. Creation uses
exclusive no-follow opens, a private complete temporary file and exclusive
publication; rotation uses an atomic replacement in the pinned parent directory.
This facility requires a POSIX filesystem supporting these operations. Restrict
the parent directory to trusted machine administrators. It does not protect
against another process using the same OS account; use a dedicated user/container.

The private, non-secret `.treer-policy-admin-token.initialized` marker is written
before first provisioning. Because installer argv persists across restarts,
this marker prevents a missing token from silently regenerating on restart.
A crash after writing the marker but before the token leaves editing locked for
explicit local recovery. Keep the marker during backups/upgrades. Losing both
token and marker is indistinguishable from a fresh installation: restore the
marker or disable initialization before starting a recovered installation.

**Retrieve:** a workspace owner with authorized OS access opens a private local
terminal or trusted SSH session as the machine account and reads
`<workspace-root>/.treer-policy-admin-token`, then pastes it into the App's
password field. Do not retrieve it through Proxy file APIs, `treer exec` output,
Core Messages, terminal recording, screenshots or shared logs. A workspace-owner
role does not itself grant OS-file access: without machine access, arrange
controlled secret delivery with the machine administrator. This adds no secret
broker or remote token-recovery endpoint.

**Rotate or recover a lost file:** on that same machine in a private terminal,
run the local command below with canonical absolute paths. It generates a new
secret without printing it and exits without starting or restarting the App:

```sh
env -u POLICY_ADMIN_TOKEN POLICY_ADMIN_TOKEN_FILE=/absolute/workspace/.treer-policy-admin-token \
  python3 /absolute/workspace/treer-policy.py --rotate-admin-token-file
```

The running file-backed App reads the file on every administration check; new
requests reject the old token after replacement, with no restart. In-flight
requests already authenticated may finish. If an existing file has unsafe
permissions/type/contents, rotation refuses it: the machine administrator must
repair or remove it locally before explicit recovery. Never remove the marker to
trigger automatic recovery. An environment token takes precedence and requires
changing that App configuration in a separately authorized operator workflow;
rotating a shadowed file will not invalidate the environment capability.

Back up the token only in encrypted, private credential backups, or omit it and
perform explicit local recovery after restoration. Policy backups need not
contain this secret. No file content appears in App errors or status responses.

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

Before publishing schema 2, upgrade and drain **all** old Proxy replicas. A
capability response or successful validation from one replica cannot establish
fleet-wide support: old replicas may retain stale v1 policy or use their configured
fail-open behavior after rejecting v2. Before rolling a Proxy back to a v1-only
version, restore a v1 policy and confirm every replica has loaded it. App-side
negotiation alone cannot make a mixed-version Proxy fleet safe.

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

Configure `POLICY_PROXY_URL` (Treer Proxy's HTTP(S) base; falls back to trusted
`TREER_PROXY_URL`), `POLICY_SERVICE_ID`
and `POLICY_PUBLIC_URL` (canonical App URL, including a tunnel prefix if used).
With a Managed App runtime identity the latter two can be discovered through
`treer app list`. No operator token is forwarded to Core. The restricted diagnostic bridge sends
the current bound Provider runtime ID/workload credential **and** the verified
human token; neither alone grants access. The runtime never substitutes for the
caller. Keep the URLs operator-controlled.

Browser authentication accepts `X-Treer-Identity-Token` first and verifies it
on every request through `/.treer/apps/identity/verify` for this service and
workspace, expiry and current directory membership. An invalid injected token
is rejected even if a cookie is valid. Without the header, the standard Treer
OAuth button uses S256 PKCE, a one-use server-side state and a matching private
browser nonce. Sessions and OAuth state are stored in a private adjacent
`.auth.sqlite3`; callback URLs are fixed configuration, never user redirect
parameters. Cookies are HttpOnly, SameSite=Lax and Secure on HTTPS.

Human state changes and diagnostic reads require `X-Policy-Request: 1`, a permitted Origin when
present, and same-origin/same-site Fetch Metadata. Operator writes additionally
reject cross-origin Fetch Metadata and require JSON and the operator Bearer.
OAuth/identity cannot replace that administration capability. Use workspace-only
ingress; all policy/bundle reads are visible to workspace callers and direct
local-machine access must be separately protected.

The UI delegates simulation and bounded decision logs through Core
`/api/apps/{service_id}/policy/explain`, `/validate`, and `/decisions` using the
verified **human caller** token alongside the current bound Provider runtime
credential. Core checks both, with fresh runtime credential and owner membership
validation; only the currently configured Provider can use this bridge. Core recognizes that App-scoped owner token on its diagnostic routes. Core rejects
non-owners and stale membership. A Core without diagnostics returns an error;
the App never substitutes a local path evaluator with different security rules.
`/v1/policy/test` is retained only as a legacy v1 local dry-run endpoint.
Logs expose the latest 100 records from a 2048-record per-Proxy ring without
pagination; they show computed denials (what enforce would deny), effective decisions and
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

## Operator publication and Agent client

The Provider binding defaults to `publish_authority=owner_session`: path-policy
publication needs both the operator capability and the current human owner.
An owner may explicitly select `provider_operator` in Core's Provider settings.
In that mode **any caller possessing this App's operator token can publish policy
and simulate requests without another human approval**. This includes replacing
rules or switching to enforce; it is full policy administration, not a bounded
delegation. The token cannot change the Core Provider selection or owner rescue
channel. The App UI has no switch for this setting. Review the displayed authority
before sharing a token, and rotate the token to revoke a holder. Old/mixed Proxy
versions must not silently treat the setting as supported.

`treer-policy` is a dependency-free, single-file JSON client. Source runs serve
it at `/treer-policy`; bundled installs must include `treer-policy-client.py`.
Install a trusted copy locally and invoke it with Python 3 (or mark executable). It reads a machine-local config
from `~/.config/treer-policy/config.json` or `--config FILE` /
`POLICY_CLIENT_CONFIG`. Its directory must be owned by the caller and mode 0700;
the regular file must be owned by the caller and mode 0600. Symlink paths are
rejected. The JSON object contains `url` and `token`. No token argv/environment
option exists, and the client neither prints its token nor follows redirects.
The managed HTTP proxy still supplies the Agent's own workload identity.

Provision the config in an authorized **private local terminal**, using a secret
manager/private mounted file or a non-echoing prompt. Never transfer the token
through Treer exec, Core Messages, recorded terminals or source control. For a
new private config, a local administrator can use:

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

The client refuses existing unsafe directory permissions; correct those locally
before use. A failed provisioning attempt may leave an incomplete private file;
repair it locally. Do not overwrite an existing config without reviewing it.

```sh
apps/policy/treer-policy read
apps/policy/treer-policy submit publication.json
apps/policy/treer-policy preview simulation.json
```

Submission uses the existing publish contract: `{expected_revision, mode,
document}`. Read the current revision first; stale changes fail rather than
silently overwrite another edit. Preview uses the Core explain contract
(`subject`, `action`, `resource`, optional `document` and `mode`). Output is JSON;
API failures exit nonzero. Agent publish/preview requires explicitly enabled
`provider_operator`; default owner-session authorization remains intact.

## Isolating the policy administration Agent

Place the administration Agent and Policy App in a dedicated folder, for example
`/policy-admin/maintainer` and `/policy-admin/policy`. Explicitly allow only that
Agent's path to reach the App, then deny other Agent paths. For example, merge
these schema-2 rules into the existing document (do not replace unrelated rules):

```json
{
  "schema_version": 2, "defaults": {}, "groups": {}, "rules": [
    {"id":"policy-admin-access","priority":10,"effect":"allow",
     "subjects":[{"kind":"agent","tree":{"path_eq":"/policy-admin/maintainer"}}],
     "actions":["identity.token.issue","service.connect","network.connect"],
     "resources":[{"tree":{"kind":"app","path_eq":"/policy-admin/policy"}}]},
    {"id":"policy-other-agents-deny","priority":20,"effect":"deny",
     "subjects":[{"kind":"agent"}],"actions":["identity.token.issue","service.connect","network.connect"],
     "resources":[{"tree":{"kind":"app","path_eq":"/policy-admin/policy"}}]}
  ]
}
```

Confirm actual App/Agent paths and action/resource matching with Core explain.
Keep monitor initially, inspect decision logs, and explicitly enable enforce
only after owner access and recovery checks: **monitor does not block access**.
These two rules cover the App access gate only. Separately restrict other
Agents' ability to move/rename nodes, prompt/control the administration Agent,
change its startup, execute on the machine or read/write machine files. Otherwise
another allowed operation can bypass the intended boundary. Path permissions
follow placement; a same-name replacement is not a stable-ID permission.

Run the Policy App and authorized administration workload under a dedicated
unprivileged OS account or containers/VMs with narrowly shared secret mounts.
Only the App and authorized administrator should read the token/config. Keep the
App port private; a loopback port is reachable by other local processes and
bypasses the Treer path gate. Restrict host/container networking accordingly.
**Mode 0600 does not isolate other Agents running as the same OS user.** Treer
policy cannot prevent that user from reading a file directly or connecting to a
local raw port. Use OS/network isolation in addition to path policy.

The App reads authority from Core without caching and sends no management token
to Core. Operator validate/explain independently check the current Provider
runtime and current database choice on every call. Switching back does not
cancel a write already validated in flight; rotate the token as needed. Upgrade
all Proxy replicas first: one capability response does not prove fleet readiness.
Unknown authority/capability and missing operator endpoints reject publication.
This choice trusts the Provider runtime itself: Core cannot prove that a
compromised App actually checked its token. No grant, scope, expiry or
anti-escalation restriction is promised by operator mode.
