# Treer Mail App

Mail is a small HTTP service and React frontend over Core Message. It uses
Treer's standard App OAuth flow; the local HttpOnly cookie maps to the returned
short-lived App access token. Core remains the canonical store for Messages,
contexts, deliveries, and acknowledgements.

The App index `/` negotiates between a Markdown manual for Agents and the
browser UI. Its App OAuth callback remains `/api/auth/callback`.

Build the frontend:

```sh
cd apps/mail/web
pnpm install --frozen-lockfile
pnpm build
```

Create a JSON config matching `config.schema.json`, then run:

```sh
TREER_APP_CONFIG=/etc/treer/mail.json \
TREER_APP_STATE_DIR=/var/lib/treer/apps/mail \
python3 apps/mail/mail.py
```

`service_id` must identify the registered HTTP service and `public_url` must be
an enabled workspace ingress for it. `proxy_public_url` is the public Proxy URL.
The state directory stores only pending PKCE requests and browser cookie/token
mappings; protect and back it up like any application credential store.

For a legacy Mail database, stop old writes, back it up, and run `migrate.py`
with an operator-authenticated `treer` CLI. The import is resumable and does not
modify the source database. Existing browser sessions require a new login.

## Authentication and local security

Supplied `X-Treer-Identity-Token` is verified per request before Bearer or OAuth
cookie fallback; invalid supplied identity never falls back to a valid cookie.
Mail accepts human principals with the App audience/current workspace membership.
Optional `workspace_id` pins the workspace explicitly. Browser mutations require
JSON, reject cross-site Fetch Metadata and compare Origin with `public_url`, `proxy_public_url` and
explicit `browser_origins` (for any additional explicitly trusted tunnel origin).
OAuth state is paired with an expiring HttpOnly SameSite browser nonce; older
pending logins must restart. Upstream authenticated redirects are refused.

Use a dedicated state directory: startup tightens it to 0700 and the session
database to 0600, with private SQLite sidecars. The database contains plaintext
OAuth tokens: encrypt disks/backups and restrict backup permissions. Managed
Apps share their OS account's authority; use a separate unprivileged user or
container/VM on machines running critical services. No comprehensive per-user
rate/concurrency limit or encryption-at-rest is provided by Mail itself.
