# Email endpoint

A workspace Managed App that lets an Agent cue a human with an email reminder
and a link to a Core Message conversation. Human replies reach the initiating
Agent through `treer message receive`. Email transport/state stays in the App;
Core remains the canonical Message store. The root negotiates an Agent Markdown
manual and a responsive light/dark browser page.

## Deploy

Requires Python 3.10+, installed `treer`, an enabled Core Message rollout,
workspace ingress/App OAuth, and permission for the callers' Message operations.
Keep this directory and sibling `apps/mail/mail.py` together: the App reuses
Mail's maintained OAuth/session/HTTP primitives. No Node build is needed.

Create a private configuration file outside the repository, mode **600**:

```json
{
  "proxy_public_url": "https://proxy.example.test/",
  "listen": "127.0.0.1:9440",
  "mode": "outbox",
  "members": {"usr_MEMBER_ID": "person@example.test"},
  "allowlist": ["person@example.test"],
  "wake_agents": []
}
```

Member IDs come from `treer member list`. Core does not disclose member emails;
the administrator owns this explicit binding. Directory lookup must still find
the bound user as an active workspace human on each notification. Member name,
ID, or bound email can be used with `--to`. An optional nonempty allowlist further
restricts these bindings, using exact addresses or exact `@domain.example`
entries. `EMAIL_ENDPOINT_ALLOWLIST` may override the configuration with a
comma-separated address/domain list; supply it through private environment
setup, not deployment argv. Whitelisted nonmembers are not admitted by this App because no human
Core identity would own their conversation.

Check that there is no existing same-name App, then create workspace access:

```sh
treer app list
treer app create --machine self --name email-endpoint --cwd treer --port 9440 \
  env -- EMAIL_ENDPOINT_CONFIG=/path/to/private/email-endpoint.json \
  EMAIL_ENDPOINT_STATE_DIR=/path/to/private/email-endpoint-state \
  python3 apps/email-endpoint/email_endpoint.py
treer app show email-endpoint
curl http://email-endpoint.treer/health
```

Do not pass `--public`. The App discovers **its own** service ID and allocated
`public_url` through `treer app show` on startup. Alternatively put `service_id`,
`public_url` and `workspace_id` in the private config. Email links always use this
platform-assigned origin. Agent access is `http://email-endpoint.treer/`; read the
root manual or use the executable `apps/email-endpoint/email-endpoint` wrapper.
Only config/state file paths appear in saved deployment argv, never credentials.

The configuration must be available to the supervised process after machine
restart. One active instance owns the SQLite database. Back up
`email-endpoint.sqlite3` and WAL/SHM consistently using SQLite online backup,
together with the `outbox/` directory. State contains OAuth credentials, recipient
bindings, notification summaries and channel metadata; keep directory mode 700.
No complete Core Message body is copied to SQLite or email. Do not put sensitive
content in an explicit `--summary` or subject. Logs never contain bodies,
recipients, access tokens, or provider responses.

## Identity and permissions

Agent notification/status routes require a verified service-audience Treer token,
usually injected by the inherited HTTP proxy. Self-reported sender fields are
rejected. Core rechecks the real initiating Agent's identity and Message Policy.
A thread stays bound to its Agent and one recipient; another Agent cannot reuse
it, and a continuation cannot change the recipient.

Human pages/API first verify `X-Treer-Identity-Token` on **every request**, so a
logged-in Treer user can enter directly once the Proxy identity-forwarding
change is deployed. When there is no forwarded token, App OAuth uses PKCE and a
short-lived HttpOnly SameSite=Lax session cookie; a sign-in button appears only
when necessary. Human replies use that person's token and sender identity.
Every thread read/reply checks the bound recipient, including workspace admins;
admins have a separate read-only outbox preview/status page at `/`. Data APIs
return structured JSON errors. Text uses `textContent`, never HTML rendering.

POST browser routes require `Origin` equal to the App origin. When using a
control-plane browser tunnel, add its exact trusted origin to the config's
`browser_origins` array. Assets and data URLs preserve the tunnel prefix. App
OAuth callbacks still require the dedicated workspace ingress; an installation
without ingress must use the forwarded identity path. No guest magic links.

Set `wake_agents` to exact stable Agent IDs only when the administrator wants
wake behavior. Default is empty. After a durable human Message, the App's **own**
Agent identity calls `treer agent prompt` with a Message ID and a short cue;
`agent.prompt` Policy applies separately. Failure returns a visible
`denied_or_unavailable_message_delivered` or `unavailable_message_delivered`
result; the Message is retained and the human page explains the degradation.
There is no automatic terminal fallback or body copied into prompts.

## Transport, limits and status

`mode` is `outbox` (default) or `proxy`. Outbox writes multipart plain-text/simple
HTML `.eml` files atomically and shows summary/link/delivery previews on the
administrator page. `sent` with `transport=outbox` means **a local file exists,
not that email was sent**. This is the deployment/acceptance mode until the new
Proxy route is deployed.

Proxy mode calls `POST /api/apps/SERVICE_ID/email` using a fresh service token
from its own `treer token create`. It reuses Proxy Cloudflare Email Sending;
From is fixed by Proxy `TREER_EMAIL_FROM` (default `service@treer.ai`). The App
cannot set From or Reply-To and sends only plain-text summary/link; the Proxy
produces escaped HTML. Credentials remain on the Proxy. On
`email_not_configured` (503), the notification is saved to outbox with a visible
explanation. Other failures, including route unavailable, Policy denial and
recipient denial, do not silently turn into successful outbox sends.

No SMTP host, port, TLS settings, username, password or application password is
needed. Operators configure the **Proxy**, not this App: existing Cloudflare
account configuration and `CLOUDFLARE_API_TOKEN`, fixed `TREER_EMAIL_FROM`,
`TREER_EMAIL_ALLOWLIST` exact addresses/domains, and `email.send` Policy for the
Managed App. Do not look for or copy Proxy tokens into an Agent. Only the
explicitly user-authorized real address may be added to local/Canary allowlist
configuration. Automated tests never send real mail. A single real acceptance
email, clearly marked “Treer email-endpoint 测试邮件”, must wait for leader's
instruction after Proxy deployment. No Outlook sender credentials are needed
for receiving this Cloudflare-sent notification.

App defaults: 10 notifications/Agent/hour, 5/recipient/hour; config keys
`agent_limit` and `recipient_limit` adjust them. High urgency does not bypass
limits. Proxy independently enforces 20 attempts/App/hour and
5/recipient/workspace/hour, including retries. Subject and explicit summary
are capped at 150 and 500 bytes; canonical body at 32768 bytes. Inputs reject
unknown fields. Use stable `Idempotency-Key` values for notifications/replies;
identical retries reuse the Message and notification, changed requests conflict.

A persisted intent reserves a fixed parent context before Core dispatch. If the
process stops before recording the Core result, `core_pending=true` tells the
caller to retry the original request/key with a fresh identity token; Core's
idempotency recovers the original Message. The worker deliberately does not
persist caller tokens or body and cannot complete such an intent on its own.
Once Core succeeds, queued emails survive restart. Defaults: 5 attempts,
30-second initial delay with exponential backoff capped at one hour; config
`max_attempts` and `retry_seconds`. Failed entries remain inspectable through
`GET /v1/notifications/ID` and the admin page; no automatic infinite retry.
Manual recovery should inspect the failure before issuing a new notification.

## Verify in a closed environment

```sh
python3 -m unittest discover -s apps/email-endpoint/tests -v
cd apps/email-endpoint
npm install --ignore-scripts
npx playwright install chromium
npm run test:browser
```

The tests remove inherited `TREER_*`, `AGENT_UI_*`, `CODEX_UI_*`,
`EMAIL_ENDPOINT_*` and proxy variables. The Python suite uses a fake App API
and temporary SQLite/outbox; no installed Controller is called. Browser tests
spawn only the local fake, verify header identity, replies, literal XSS text,
denial, direct/tunnel paths, desktop and 390px dark screenshots in
`/tmp/email-endpoint-browser`. The Rust Proxy tests use an isolated PostgreSQL
schema and local fake Cloudflare HTTP sender:

```sh
just test-db-up
TREER_TEST_DATABASE_URL=postgres://treer:treer@127.0.0.1:55432/treer_test \
  cargo test -p treer-proxy app_email -- --nocapture
cargo test -p treer-proxy strict_allowlist
node scripts/check-docs.mjs
```

## Known limits and later options

Email provider acceptance is not inbox delivery confirmation. A provider timeout
or crash between remote acceptance and local state can duplicate a notification;
Core Messages still deduplicate. App state is single-instance SQLite; no
multi-instance scheduler, automatic migrations, mailbox inbound replies, or
email tracking. Thread history is the ordered App-created chain (50 messages
per page); external Core Messages appended outside the App are not discovered
automatically. Outbox previews show the last 100 entries. No automatic data
retention or credential isolation between processes sharing a machine UID.

Guest magic links are a later option only with explicit approval: bearer URLs
can leak through forwarding, browser history and referrers; they would need
expiry, revocation, recipient verification and a clearly defined Core author
identity. They are not implemented or enabled.
