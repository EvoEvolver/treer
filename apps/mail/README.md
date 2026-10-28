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
| `email_preview_chars` | no (`80`) | First-line characters in notification emails (0-200; 0 = sender only) |
| `notify_poll_seconds` | no (`30`) | How often the notifier looks at members' inboxes |
| `email_capability` | no (`app.email.v1`) | Proxy `/api/health` capability for the App email route |
| `treer_bin` | no (`treer`) | CLI for `treer token create SERVICE_ID` (the App's runtime identity) |

[`config.schema.json`](config.schema.json) lists every key.

## Email notifications

Optional and off by default. A member turns them on under Notifications (or an
Agent does, see [AGENT.md](AGENT.md)). The worker in [`notify.py`](notify.py)
then looks at that member's unacknowledged Core deliveries every
`notify_poll_seconds`. A Message counts as read once its delivery is
acknowledged: opening Mail acknowledges what it shows, as does `treer message
ack`. Messages already unread when notifications are turned on are never
emailed.

- `digest_minutes` 0 sends at the next poll; N waits until the oldest pending
  Message has been unread for N minutes, then sends ONE email listing every
  pending one. Messages read before then are dropped.
- `scope` `direct` covers only Messages whose sole recipient is the member;
  `all` covers every Message delivered to them. The member's own Messages
  never count.
- The email has the sender, the first `email_preview_chars` characters of the
  first line and a link per Message (`PUBLIC_URL?message=ID`) that needs a
  Treer sign-in. No other body text is sent.

Only [`email_route.py`](email_route.py) talks to Core: `POST
/api/apps/{service_id}/email` with `{to, subject, text, idempotency_key}`
under the App runtime identity, which Core checks against Policy `email.send`.
Without the `email_capability` in the Proxy's `/api/health` the feature reports
`unavailable` and nothing is tracked. Batch content and the idempotency key
(derived from the member and the Message IDs) are frozen before the first
attempt, so every retry is the identical request. Outcomes:

| Result | Core response | Action |
| --- | --- | --- |
| `sent` | 2xx `status: sent` | done |
| `permanent` | 400, 403, 404, 405, 409, 413, 422 | `failed` at once |
| `retryable` | 401, 408, 425, 429, 503, refused connection, no runtime token | backoff 60 s doubling to 1 h; `failed` after 5 attempts |
| `uncertain` | other 5xx, timeout, reset, unexpected 2xx | never resent automatically; Mail asks `GET /api/apps/{service_id}/email/{key}` and resends only if Core says it was not sent |

When Core offers no such check the batch stays `uncertain`; a member or Agent
may retry it explicitly, accepting a possible duplicate.

**Limit: watching needs the member's Mail sign-in grant.** Mail can read only
the Messages of a member whose App OAuth grant it holds (12 hours after the
member signs in to Mail through `/api/auth/start`). Workspace ingress requests
carry 60-second tokens that are not kept. Without a live grant the watch state
is `login_required` and nothing is checked; the Notifications view links to
sign-in. Logging out deletes that browser session's grant.

## Routes

- `GET /api/health`, `GET /api/config`
- `GET /api/auth/start`, `GET /api/auth/callback`, `GET /api/auth/session`,
  `POST /api/auth/logout`
- `GET /api/directory`
- `GET /api/messages` (recent history), `POST /api/messages` (send; optional
  `Idempotency-Key` header), `POST /api/inbox` (unread)
- `GET /api/notifications`, `PATCH /api/notifications/settings`,
  `POST /api/notifications/test`, `POST /api/notifications/batches/ID/retry`,
  `GET /api/notifications/members` (Agents); see [AGENT.md](AGENT.md)

`GET /api/messages` also acknowledges the visible Core deliveries, so it is not a
side-effect-free read.

## Authentication and local security

A supplied `X-Treer-Identity-Token` is verified per request before Bearer or the
OAuth cookie; an invalid supplied identity never falls back to a valid cookie.
Mail accepts human principals with this App's audience and current workspace
membership; the notification routes also accept Agent workload identities. Browser mutations require JSON, reject cross-site Fetch Metadata and
compare Origin with `public_url`, `proxy_public_url` and `browser_origins`;
Agent calls carry no Origin and authenticate with their workload identity.
OAuth state is paired with an expiring HttpOnly SameSite browser nonce.
Authenticated upstream redirects are refused.

The state directory holds pending PKCE requests, browser-cookie-to-token
mappings and notification settings, pending Message IDs and sent email
batches (address, subject, preview text, attempts) in SQLite. Startup tightens the directory to 0700 and the database to
0600. The tokens are plaintext: encrypt disks and backups and restrict backup
permissions. Same-UID processes are not isolated; use a separate unprivileged
user or container/VM on machines running critical services. Mail has no
per-user rate limit.

## Test

```sh
python3 -m unittest discover -s apps/mail/tests -p 'test_*.py' -v
```
