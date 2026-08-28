# Treer Mail App

Mail is a small HTTP service and React frontend over Core Message. It uses
Treer's standard App OAuth flow; the local HttpOnly cookie maps to the returned
short-lived App access token. Core remains the canonical store for Messages,
contexts, deliveries, and acknowledgements.

The App index `/` is a plain-text manual for Agents. The browser UI is served
under `/_human/`; its App OAuth callback remains `/api/auth/callback`.

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
