# Email endpoint

A folder-scoped Managed App for email reminders and browser Core Message replies.
The tree contains the endpoint App; contacts are configured email addresses, not
nodes or new accounts. Run independent instances in different folders. Email
contains a summary and thread link; replies happen on the web, never via inbound
email. Core remains the canonical body/thread store. No build step is needed.

## Deployment and activation

Python 3.10+, `treer`, enabled Core Messages, registered workspace ingress and
App OAuth are required. Retain sibling `apps/mail/mail.py`, whose HTTP/session
primitives are reused. Each instance needs a separate mode-700 state directory,
mode-600 configuration file, name, service, and listen port. Do not share SQLite
between instances. The database records its workspace/service identity and
rejects reuse by a different endpoint.

Example configuration (outbox preview only):

```json
{
  "proxy_public_url": "https://proxy.example.test/",
  "listen": "127.0.0.1:9440",
  "mode": "outbox",
  "contacts": [
    {"id": "reviewer", "label": "Reviewer", "email": "person@example.test", "user_id": "usr_APPROVED_MEMBER_ID"},
    {"id": "external", "label": "Future contact", "email": "outside@example.test"}
  ],
  "allowlist": ["@example.test"],
  "wake_agents": []
}
```

Contacts have unique stable `id`, unique exact normalized `email`, optional
`label`, `user_id`, and `enabled` (default true). Address/user binding changes
revoke access to old threads rather than transferring them. `members: {USER_ID:
EMAIL}` is accepted for migration and creates `member-USER_ID` contact IDs;
explicit contacts take precedence for that user. An unbound, disabled or no
longer active member contact reports `reply_status=reply_unavailable`. Sending
to it fails before creating a Message or email. No invitations are sent.

Create only after checking names, then move and explicitly activate:

```sh
treer app list
treer app create --machine self --name team-email --cwd treer --port 9440 \
  env -- EMAIL_ENDPOINT_CONFIG=/private/team-email.json \
  EMAIL_ENDPOINT_STATE_DIR=/private/team-email-state \
  python3 apps/email-endpoint/email_endpoint.py
treer agent mv team-email /team/
treer agent ls /team
treer app show team-email
```

Do not use `--public`. `app create` currently has no `--folder`; create then
move. **All new and upgraded endpoints default to sending disabled.** Open the
App as a workspace owner/admin, check the displayed tree path, then click
“Confirm this folder and enable sending”. `POST /v1/admin/activation` accepts
`{"enabled":true,"folder_id":"STABLE_FOLDER_ID"}` (null for root), with human
identity and browser Origin protections. It rejects a stale folder confirmation.
The persisted activation applies only to that folder ID: moving the endpoint
requires confirming its new placement. Same-folder Agents must have current
identity and tree presence; descendants are not implicitly admitted. Renaming a
folder retains its ID and does not revoke activation. No Agent request can
activate the App. Historical recipient replies remain valid after a move.

The process discovers its own service/public URL using `treer app show` and its
runtime ID from `TREER_AGENT_ID`. External supervision must explicitly configure
`service_id`, `public_url`, `workspace_id`, `runtime_agent_id`. Public URL must be
its allocated workspace ingress. Each send and worker attempt reads the current
tree through its own authenticated `treer agent ls -R /`; grant the App narrow
tree/Agent/App read permissions. Lookup failure fails closed, with an explicit
`tree_unavailable` response. Placement check and Core send are separate operations;
a concurrent move can race an already-authorized in-flight Message, but queued
email is rechecked before delivery. There is no Core transaction spanning them.

Only configuration/state paths appear in deployment argv. Config/contact changes
are read at process startup; leader/release-eng schedules any App-only restart.
Do not restart anything during development or migrate the running deployment.

## Deploy a fixed version (P0 production layout)

Do not run production from the repository checkout. An old Python process can
otherwise read new HTML/JavaScript from disk, then reject the new page's API
requests with `Route not found`. The process now snapshots HTML, JavaScript,
CSS and its Agent manual at startup. Existing processes must be replaced by an
authorized App-only deployment to gain this protection; changing a checkout does
not patch already-loaded Python code.

Use `scripts/app-deploy` with a full frozen commit, an absolute stable Python
shim, and a private deployment root. Export component **`apps`**, with entry
`email-endpoint/email_endpoint.py`, so the maintained sibling `mail/mail.py`
comes from the same release. Do not export only `apps/email-endpoint`: it omits
the shared identity/session implementation. Only configuration **paths** belong
in startup arguments, for example `--config /private/endpoint.json`; the existing
`EMAIL_ENDPOINT_CONFIG` is still supported. Never put configuration contents or
credentials in argv. `--state-dir` remains available for ordinary installations.

For app-deploy, initialize `--db-env EMAIL_ENDPOINT_DB`: its injected path
`ROOT/state/app.sqlite3` takes precedence over the ordinary state-directory DB.
Outbox and pre-v2 migration backups live beside that DB, under the same private
state directory. `/deployment-health` (GET/HEAD, no-store) reports the startup
`APP_DEPLOY_VERSION`, stable App runtime ID, database readiness, API version 2,
and SHA-256 hashes of loaded source/shared mail code and the frontend snapshot.
It returns 503 on database failure. No tokens, contacts, paths or message bodies
are returned. A source checkout has `version: null`; it cannot pass a release
health check expecting an exact commit. `/health` remains the compatibility
liveness endpoint, not release acceptance.

First adoption is a coordinated **release operation**, not a development
restart. The old checkout process has no versioned bootstrap/readiness contract,
so do not claim `app-deploy deploy` can automatically roll back that first
adoption. The releaser must record the old startup command, back up the entire
private configuration/state/outbox, stage and test the new release, stop only
this App, copy the stopped SQLite DB to `ROOT/state/app.sqlite3` and its outbox to
`ROOT/state/outbox`, initialize/bootstrap the release pointer, and update the
existing App startup to the `init` result. Keep its App/service ID and workspace
access. Start and verify both local and routed `/deployment-health`, then the
human root overview and thread/reply flow. If adoption fails, stop only this App
and restore the old startup, code and full pre-adoption state/config. Retain both
copies until acceptance. No Controller/Host/Agent restart is needed.

Subsequent upgrades use `stage` then `deploy` with local and routed
`/deployment-health`; the existing version provides app-deploy's rollback
baseline. The tool backs up the managed SQLite file, **not configuration or
outbox**, so snapshot those separately before any change that affects them.
After v1-to-v2 migration, sending defaults to disabled until the owner confirms
the current folder. Never silently enable it during deployment. Tests use
outbox only; they do not send to any real recipient.

## Identity and OAuth limitations

All human access uses Treer identity: per-request forwarded
`X-Treer-Identity-Token` first, otherwise existing Treer App OAuth/PKCE and
HttpOnly SameSite session cookie. Already signed-in Treer users need no extra
App login when identity forwarding is deployed. There are no magic links or
custom email credentials. OAuth state is bound to an independent browser cookie;
a callback without that browser cannot redeem another browser's flow. Ordinary
thread links confer no authority. OAuth callbacks use registered workspace
ingress, including when login starts from an embedded page.

Treer App OAuth requires **current workspace access**, both at authorization
and on subsequent requests. Having a Treer login/account alone does not admit a
nonmember. App claims/directory expose stable user IDs, names and roles, not email
or verified-email claims. An administrator must explicitly bind each address to
the correct stable member ID using authoritative membership records; never infer
this binding from display names or user-entered emails. The App trusts that
administrative binding, not an independently verified mailbox claim. Current
membership and binding are checked on every read/reply, including admins. Wrong
recipient, expired/forged/other-audience tokens, empty injected headers and revoked
members fail even if a valid session cookie exists. Neither another Agent nor an
administrator can impersonate the bound human to reply.

There is no restricted guest role today. Ordinary member onboarding grants
ordinary membership and requires explicit operator approval; it must not be
advertised as reply-only guest access. Nonmember contacts stay configured but
`reply_unavailable`. Future public access/guest onboarding needs a separately
approved authentication/access model; switching an endpoint to public alone
would not make nonmembers eligible for current App OAuth. Contact and thread IDs
remain independent of user IDs so such a later model need not replace their IDs.
This phase adds no Core capability or public listener.

Browser writes enforce exact configured `public_url` Origin or explicit
`browser_origins`, reject cross-site/invalid Fetch Metadata, and use non-GET JSON
mutations. Set the Proxy control-plane origin in `browser_origins` for embedded
writes. Invalid identity headers never fall back to cookies. All displayed text
uses text rendering. Human APIs and assets preserve the embedded prefix
`/api/workspaces/W/apps/ID/ui/`.

## Agent API and delivery

```sh
# Resolve this App through a relative or absolute tree path, not a global name.
apps/email-endpoint/email-endpoint contacts --endpoint /team/team-email
printf '%s\n' 'Private decision details stored only in Core.' | \
  apps/email-endpoint/email-endpoint send --endpoint /team/team-email \
    --to reviewer --subject 'Review needed' --summary 'Please review the decision.' \
    --idempotency-key decision-42
# Continue with --thread th_RETURNED_ID and a new key.
apps/email-endpoint/email-endpoint status --endpoint /team/team-email \
  --notification nt_RETURNED_ID
```

CLI outputs JSON, resolves the path with caller-authenticated tree/App reads,
checks App runtime identity, then calls `http://SERVICE_ID.treer/` through the
inherited identity proxy. Default endpoint is sibling `email-endpoint`; explicit
`--url`/`EMAIL_ENDPOINT_URL` is a compatibility escape hatch and still subject to
server-side folder checks. `notify` remains an alias for `send`. HTTP redirects
are not followed. The full body is read from stdin, never placed in argv.

- `POST /v1/notify`: verified Agent, required `to`, `subject`, `body`, optional
  `summary`, `urgency`, `thread_id`; `Idempotency-Key` required. Contact ID,
  unique label, email or bound member ID is accepted; legacy unique names work.
- `GET /v1/notifications/ID`: originating Agent's delivery status/thread URL.
- `GET /v1/endpoint`: same-folder Agent sees contacts/placement/activation;
  human sees their contacts/recent threads, admin sees all configured contacts.
- `GET /v1/threads/ID`: recipient only, 50 messages with `next_before` pagination.
- `POST /v1/threads/ID`: recipient-authored `body`, `Idempotency-Key`, same-origin.
- `POST /v1/admin/activation`: human owner/admin only, explicit current folder.
- `GET /v1/admin/outbox`: owner/admin, latest 100 summaries/status, no Core bodies.

Agent sends under its own token to the bound human. Human replies use their own
Treer token and target the fixed initiating Agent; contexts preserve the same
Core conversation. Stable idempotency prevents duplicate Message/email creation;
changed content with the same key conflicts. `core_pending=true` requires retrying
the original request/key with fresh caller identity after an ambiguous Core
response; the worker never persists caller tokens/bodies or impersonates them.
Core Messages disabled is reported, not silently replaced with another channel.

`wake_agents` contains exact stable Agent IDs; default empty. An optional
post-delivery prompt uses only Message ID and a cue, subject to separate
`agent.prompt` permission. Failure is visible; the durable Message remains.
Agents read `treer message receive` and acknowledge after handling. The App never
retargets a reply to whichever Agent later occupies a reused name/path.

`mode=outbox` is default and tests always use it or a fake Proxy. Outbox `sent`
means a private `.eml` file exists; **no external email was sent**. Proxy mode
reuses existing `POST /api/apps/SERVICE_ID/email`, fresh App service token, fixed
Proxy From, `email.send` Policy and Proxy `TREER_EMAIL_ALLOWLIST`. App allowlist
can further restrict contacts (`TREER_EMAIL_ALLOWLIST` environment, deprecated
`EMAIL_ENDPOINT_ALLOWLIST` override, or configuration `allowlist`). **All real
recipients, even members, are additionally restricted to doomspec@outlook.com.**
The restriction is checked at admission and at worker delivery, including old
queue entries. No tests contact it. No SMTP/Cloudflare credentials go into Apps.
`email_not_configured` falls back visibly to outbox; other failures do not pretend
that email was sent. Queue delivery rechecks folder, activation, contact binding
and active membership; revocation/placement failures stop that notification.

Default limits: 10 notifications/Agent/hour, 5/contact/hour, subject 150 bytes,
summary 500, body 32768; high urgency does not bypass them. Worker retries
transient failures 5 times, starting at 30 seconds, exponential backoff to 1 hour
(`agent_limit`, `recipient_limit`, `max_attempts`, `retry_seconds`). Failed entries
need inspection. Do not create a new key to blindly resend ambiguous provider
outcomes. Email contains no complete body. Explicit summary/subject are still
sensitive; keep secrets out. Core thread is canonical; no inbound email or promise
of replying in a mail client to the RFC email thread.

## Migration, backup and rollback

Before deployment stop the old App and back up **the whole state directory and
private config**. Keep the old code version. At first v2 startup, an existing
v1 SQLite index is backed up online to a private sibling
`email-endpoint.sqlite3.pre-v2-RANDOM` before adding contact references. Legacy
`members` become stable contacts; matching old threads retain IDs, Messages and
links. Unconfigured/rebound addresses deny old thread access. Old unbound OAuth
flows are discarded; valid session tokens remain subject to current checks.
Sending stays disabled until placement confirmation. Pending old notifications
cannot send automatically before activation; the worker may mark them failed,
so inspect the old queue before upgrade and reconcile intent/idempotency rather
than assuming all will resume.

Automatic backup covers SQLite only (outbox is unchanged by schema migration).
For routine backup, use SQLite online backup plus a consistent private outbox
copy, or stop this App and copy the entire state directory/config. Restoring only
SQLite does not restore old configuration/contact bindings. Rollback: stop v2,
restore the full pre-deployment state/config and old code, then let the operator
start it. A pre-v2 database may be used with its matching pre-upgrade config;
post-upgrade changes are not in that backup. Never print tokens/config/state in
logs. SQLite contains OAuth access/session data; restrict backup access too.

## Checks and known limits

```sh
python3 -m unittest discover -s apps/email-endpoint/tests -v
cd apps/email-endpoint
npm install --ignore-scripts
npx playwright install chromium
npm run test:browser
```

Browser tests also accept `EMAIL_ENDPOINT_PLAYWRIGHT_MODULE` pointing to an
already-installed Playwright module. Fixtures clear inherited Treer/harness/App
identity and proxy settings, use ephemeral signing keys, fake OAuth/PKCE and Core,
local HTTP boundaries and outbox only. They cover startup frontend snapshots,
release readiness and SQLite failure, v1 backup/restoration and disabled activation,
root-overview-to-thread-to-reply navigation through both gateway paths, and desktop/390px, dark/light,
OAuth login/reply, per-request signed identity through both ingress and embedded
prefix, header stripping, XSS, contacts and admin placement activation. Screenshots
default to `/tmp/email-endpoint-browser`; override `EMAIL_ENDPOINT_SCREENSHOTS`.
CLI tests use a fake `treer` executable and local HTTP proxy, no real machines.

One process owns each instance database; no multi-process/HA guarantee. Config is
operator-managed and requires a coordinated App-only restart. There is no guest
access, public gateway, inbound email, attachment handling, per-contact OAuth
provider, automatic invitation/mailbox discovery, subtree sending, transport
credential management or indefinite retries. Current OAuth assumes configured
workspace ingress; embedded signed identity works without a separate App login.
Listing/placement permissions are required and failure is explicit. Real Treer
release acceptance and any deployment/restart are leader/release-eng operations.
