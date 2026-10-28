# Treer Mail App

Mail is a small HTTP service and React frontend over Core Message. Humans read
and send Messages through it; Core remains the canonical store for Messages,
contexts, deliveries and acknowledgements. The App root `/` negotiates between
a Markdown manual for Agents ([AGENT.md](AGENT.md)) and the browser UI.

## Build and run

```sh
cd apps/mail/web
pnpm install --frozen-lockfile
pnpm build          # writes web/dist
```

Create a JSON config, then run:

```sh
TREER_APP_CONFIG=/etc/treer/mail.json \
TREER_APP_STATE_DIR=/var/lib/treer/apps/mail \
python3 apps/mail/mail.py
```

Both environment variables are required. Config keys:

| Key | Required | Meaning |
| --- | --- | --- |
| `service_id` | yes | Registered HTTP service (`svc_…`) for this App |
| `public_url` | yes | Enabled workspace ingress for that service; OAuth callback root |
| `proxy_public_url` | yes | Public Proxy URL |
| `listen` | no (`127.0.0.1:8788`) | HTTP listener |
| `web_dir` | no (`web/dist`) | Built frontend |
| `workspace_id` | no | Pins the accepted workspace |
| `browser_origins` | no | Extra trusted browser origins (e.g. a tunnel origin) |

`config.schema.json` documents the first five keys; it does not yet list
`workspace_id` or `browser_origins`, which `mail.py` accepts.

`migrate.py` imports a database from the old standalone Mail into Core Message
with an operator-authenticated `treer` CLI (resumable; the source is not
modified).

## Routes

- `GET /api/health`, `GET /api/config`
- `GET /api/auth/start`, `GET /api/auth/callback`, `GET /api/auth/session`,
  `POST /api/auth/logout`
- `GET /api/directory`
- `GET /api/messages` (recent history), `POST /api/messages` (send; optional
  `Idempotency-Key` header), `POST /api/inbox` (unread)

`GET /api/messages` also acknowledges the visible Core deliveries, so it is not a
side-effect-free read.

## Authentication and local security

A supplied `X-Treer-Identity-Token` is verified per request before Bearer or the
OAuth cookie; an invalid supplied identity never falls back to a valid cookie.
Mail accepts human principals with this App's audience and current workspace
membership. Browser mutations require JSON, reject cross-site Fetch Metadata and
compare Origin with `public_url`, `proxy_public_url` and `browser_origins`.
OAuth state is paired with an expiring HttpOnly SameSite browser nonce.
Authenticated upstream redirects are refused.

The state directory holds pending PKCE requests and browser-cookie-to-token
mappings in SQLite. Startup tightens the directory to 0700 and the database to
0600. The tokens are plaintext: encrypt disks and backups and restrict backup
permissions. Same-UID processes are not isolated; use a separate unprivileged
user or container/VM on machines running critical services. Mail has no
per-user rate limit.

## Test

```sh
python3 -m unittest discover -s apps/mail/tests -p 'test_*.py' -v
```
