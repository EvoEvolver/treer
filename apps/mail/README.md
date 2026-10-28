# Treer Mail App

Mail lets an Agent email a person a link to a Core Message; the person signs in
to Mail's web inbox and reads and replies in the Core thread. Mail sends only
through Core's App email route. It does not receive email, track whether
anything was read or send reminders. Core Message stays the canonical store;
the App root `/` serves the Agent manual ([AGENT.md](AGENT.md)) or the browser
UI by content negotiation.

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
| `public_url` | yes | Enabled workspace ingress for that service; OAuth callback root and reply-link base |
| `proxy_public_url` | yes | Public Proxy URL |
| `listen` | no (`127.0.0.1:8788`) | HTTP listener |
| `web_dir` | no (`web/dist`) | Built frontend |
| `workspace_id` | no | Pins the accepted workspace |
| `browser_origins` | no | Extra trusted browser origins (e.g. a tunnel origin) |
| `email_capability` | no (`app.email.v1`) | Proxy `/api/health` capability for the App email route |
| `treer_bin` | no (`treer`) | CLI for `treer token create SERVICE_ID` (the App's runtime identity) |

[`config.schema.json`](config.schema.json) lists every key. Workspace Policy
action `email.send` must allow the Mail runtime.

## Sending email

`POST /api/email` accepts a workspace Agent (workload identity through the
App's runtime proxy, `X-Treer-Identity-Token` or Bearer) or a signed-in human,
with `{to | to_member, subject, text, message_id?, idempotency_key?}`; see
[AGENT.md](AGENT.md). Mail appends `Read and reply: PUBLIC_URL/?message=ID`
(or the inbox root) and the caller's name, then calls Core
`POST /api/apps/{service_id}/email` once, under its runtime identity, from
[`email_route.py`](email_route.py). The idempotency key is
`mail-` + a hash of the caller, the recipient and the `message_id` (or the
subject and text, or the caller's own key), so repeats replay in Core. Mail
keeps no send log: `GET /api/email/{key}` wraps Core's status lookup, and an
`uncertain` outcome (Core 5xx, timeout) is returned with its key and never
resent. Mail logs one line per send (caller, recipient, outcome) to stderr.

The reply link opens the web inbox; after App OAuth sign-in `?message=ID`
selects that Message (fetched individually if it is not on the recent page).
Replying needs a Treer account in the workspace.

## Routes

- `GET /api/health`, `GET /api/config`
- `POST /api/email`, `GET /api/email/{key}` (Agents and humans)
- `GET /api/auth/start`, `GET /api/auth/callback`, `GET /api/auth/session`,
  `POST /api/auth/logout`
- `GET /api/directory`
- `GET /api/messages` (recent history), `GET /api/messages/{id}`,
  `POST /api/messages` (send; optional `Idempotency-Key` header),
  `POST /api/inbox` (unread)

`GET /api/messages` also acknowledges the visible Core deliveries, so it is not a
side-effect-free read.

## Authentication and local security

A supplied `X-Treer-Identity-Token` is verified per request before Bearer or the
OAuth cookie; an invalid supplied identity never falls back to a valid cookie.
Browser routes accept human principals with this App's audience and current
workspace membership; the email routes also accept Agent workload identities.
Browser mutations require JSON, reject cross-site Fetch Metadata and compare
Origin with `public_url`, `proxy_public_url` and `browser_origins`; Agent calls
carry no Origin and authenticate with their workload identity. OAuth state is
paired with an expiring HttpOnly SameSite browser nonce. Authenticated upstream
redirects are refused.

The state directory holds pending PKCE requests and browser-cookie-to-token
mappings in SQLite (startup drops the tables of the removed notification
feature). Startup tightens the directory to 0700 and the database to 0600. The
tokens are plaintext: encrypt disks and backups and restrict backup
permissions. Same-UID processes are not isolated; use a separate unprivileged
user or container/VM on machines running critical services. Core's hourly App
and recipient email limits apply; Mail has no per-user rate limit.

## Test

```sh
python3 -m unittest discover -s apps/mail/tests -p 'test_*.py' -v
```
