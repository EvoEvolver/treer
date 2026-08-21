# Treer Mail

Treer Mail is an optional workspace application, not part of the Proxy. It owns
its message database, HTTP API, OAuth session, and React frontend. Removing or
replacing it does not change the Treer runtime.

## Trust and identity

Register the server as an HTTP machine service and enable a `workspace` service
ingress for it. The service ID is its OAuth client ID and workload-token
audience. The Proxy accepts only Authorization Code with S256 PKCE, and the
callback origin must match that enabled ingress.

Humans select **Continue with Treer**. The app receives a workspace-scoped human
token and keeps its own HTTP-only session. Managed Agents obtain a short-lived
token with:

```sh
treer identity token "$TREER_MAIL_SERVICE_ID"
```

Agents send that token as `Authorization: Bearer ...` to the mail app. The app
uses the same token when asking the Proxy for the combined Agent/human directory
or stable recipient resolution. It never reads the Proxy database. Token
verification rechecks that the service exists; human access also rechecks
current organization membership.

## Run locally

```sh
cd apps/mail/web && pnpm install && pnpm build
cd ../../..
TREER_PROXY_PUBLIC_URL=http://127.0.0.1:8787 \
TREER_MAIL_PUBLIC_URL=http://127.0.0.1:8788 \
TREER_MAIL_SERVICE_ID=svc_mail \
TREER_MAIL_DATABASE_URL='sqlite://treer-mail.db?mode=rwc' \
cargo run -p treer-mail
```

For PostgreSQL, use a `postgres://` or `postgresql://` database URL. Schema
creation is automatic on both backends. There is no migration from the removed
Proxy-owned mail tables.

The server also accepts workload tokens directly:

```sh
token="$(treer identity token svc_mail)"
curl -H "Authorization: Bearer $token" http://127.0.0.1:8788/api/directory
curl -X POST -H "Authorization: Bearer $token" -H 'Content-Type: application/json' \
  http://127.0.0.1:8788/api/messages \
  -d '{"recipients":["reviewer"],"context_ids":[],"body":"Ready for review."}'
curl -X POST -H "Authorization: Bearer $token" -H 'Content-Type: application/json' \
  http://127.0.0.1:8788/api/inbox -d '{"limit":50}'
```

Mail remains strictly pull based: sending creates database rows only. It does
not write to a PTY, wake an Agent, create a Proxy event, or change runtime state.
The app process must be supervised independently; a Treer Agent may maintain
the service configuration but is not the service supervisor.

## Configuration

| Variable | Meaning |
| --- | --- |
| `TREER_MAIL_LISTEN` | Listen address, default `127.0.0.1:8788` |
| `TREER_MAIL_DATABASE_URL` | SQLite or PostgreSQL connection URL |
| `TREER_PROXY_PUBLIC_URL` | Browser-reachable Proxy origin |
| `TREER_MAIL_SERVICE_ID` | Registered Treer machine service ID |
| `TREER_MAIL_PUBLIC_URL` | Enabled workspace ingress URL |
| `TREER_MAIL_WEB_DIR` | Built frontend directory |

SQLite uses one connection and is intended for a single app replica.
PostgreSQL uses row locking with `SKIP LOCKED`, allowing multiple replicas to
claim different unread deliveries without duplicate reads.
