# External HTTP message channel

A separate channel beside Telegram, using its POC/admission/audit helpers with
independent SQLite files. It has exactly two durable message queues. No Telegram
bot, webhook, email ingress, enrollment or remote administration is involved.

The public listener has **only four routes**; `/`, `/health`, assets and settings
return 404. A separate **workspace-only** settings App is for current human
owners/admins. It accepts only the Proxy-injected `X-Treer-Identity-Token`, verified
with Core against its own service/workspace on every request, then checks current
directory membership/role. No cookie, Bearer, OAuth or invalid-token fallback.

## Processes and deployment

Run three processes with the same private state directory, under a dedicated
unprivileged machine account. The public process imports only the queue store;
it never opens `admin.sqlite` or invokes Treer. A bounded, same-UID Unix socket
supports only pairing redemption. This is a code boundary, **not OS isolation**:
same-UID processes can read each other's files. For critical services use an
independent OS user/container/VM, minimal mounts and network, resource limits,
and no Host/Docker sockets or unrelated credentials. Pairing IPC requires Unix
peer credentials (Linux/macOS/BSD); Windows is not supported in v1.

Export an immutable `apps/telegram` release from a full reviewed SHA, and point
these saved commands at that release, never at a mutable development checkout.
Use absolute Python, release and state paths in real installation commands.
The following placeholders are operator choices; commands are **not** executed
by the App. First check `treer app list` for existing names and port conflicts.

```sh
# Dedicated command Agent: its own Treer identity/Policy performs Core delivery.
treer agent admin create --machine MACHINE --name http-bridge --folder /infra \
  --kind command --cwd RELEASE -- \
  /absolute/python3 http-bridge.py --state-dir /private/http-state

# Deliberately public DATA App; TLS/hostname supplied by configured Treer ingress.
treer app create --machine MACHINE --name http-endpoint --folder /infra \
  --cwd RELEASE --port 9442 --public -- \
  /absolute/python3 http-endpoint.py --state-dir /private/http-state --port 9442

# PRIVATE settings: never add --public.
treer app create --machine MACHINE --name http-settings --folder /infra \
  --cwd RELEASE --port 9443 -- \
  /absolute/python3 http-settings.py --state-dir /private/http-state --port 9443 \
  --proxy-url https://PROXY.example/ --endpoint-url https://DATA-APP.example/
```

Listeners default to `127.0.0.1`; `--listen` explicitly overrides. The state path
can also come from `TREER_APP_STATE_DIR`; the bridge CLI from `TREER_CLI`.
Settings discovers its own Managed App using its runtime Agent ID, never an
inbound header; deployments without discovery can supply `--service-id`,
`--workspace-id`, `--public-url` explicitly. Both listeners are Python standard
library only; no Node/build chain at runtime. The settings UI shares Telegram's
local CSS tokens. Do not copy only `http_app/` without the sibling shared modules.

Give the bridge only message send/receive/ack and directory/metadata permissions
needed for its intended POCs. It refuses old Controllers without
`message send --stdin-envelope`; message text and external source never enter
argv. Only the settings App needs its normal App identity verification/directory
API authority. Do not give the public process workspace management authority.
Managed App runtime credentials still exist in its environment; code-level
separation cannot protect those from a compromised same-account process.

## Pairing and external client

In the private settings page select a visible POC (stable Agent ID; canonical
path is display only), label, optional token expiry (at most one year), and
**Generate pairing code**. The code is shown once with a copy button and expires
in ten minutes. It has a random 128-bit lookup handle plus a ten-character
secret; both parts are stored hashed, with salt for the secret. Tokens contain
256 random bits; only SHA-256 hashes persist. **Rotation has an offline window:** the old token is revoked when the owner
generates a code, not when the client redeems it. Revoke/rotate cancels pending
pairings and immediately invalidates the previous token. If an exchange response
is lost or a process dies between code consumption and token installation,
re-pair; the consumed code cannot be recovered or replayed.

Keep codes, tokens, request files and responses private. Never place them in
URLs, shell history, argv, environment, screenshots or logs. Example: create a
0600 `pair-request.json` **outside shell history**, containing the code in the
`code` field, and redeem it using a private file; response contains the token.

```sh
umask 077
curl --fail-with-body --silent --show-error \
  -H 'Content-Type: application/json' --data-binary @pair-request.json \
  https://DATA-APP.example/v1/pair > pair-response.json
```

Do not use curl redirect following. Extract the returned `token` into a private
0600 file and remove the pairing request/response when no longer needed. For
curl data requests, put the `Authorization: Bearer …` header in a private file
and use `--header @headers.txt`, never a literal argument. `client.py` is a small
HTTPS-only alternative that refuses redirects and reads a private owned regular
no-symlink token file (not the token itself) from argv:

```sh
python3 client.py https://DATA-APP.example/ /private/token inbox < request.json
python3 client.py https://DATA-APP.example/ /private/token outbox
python3 client.py https://DATA-APP.example/ /private/token ack < ack.json
```

`request.json`: `{"body":"Hello","idempotency_key":"unique-for-this-client"}`.
`ack.json`: `{"up_to":1}` after processing that returned sequence. Save/process
before acknowledging; retries can return the same item. The tiny client prints
message JSON, so protect its stdout files too. It deliberately never prints or
stores the token.

## Public JSON contract

| Method and route | Request / response |
| --- | --- |
| `POST /v1/pair` | `{code}` → `{client_id, token, token_type:"Bearer", expires_at}`; one-time code, no Bearer required |
| `POST /v1/inbox` | Bearer; `{body, idempotency_key, context?}` → 202 `{message_id}` after SQLite commit |
| `GET /v1/outbox?after=0&timeout=30` | Bearer; cursor/timeout optional integers, timeout 0–30 seconds → `{items:[{message_id,seq,body,created_at}],failures:[{message_id,code,created_at,failed_at}],next_after}` |
| `POST /v1/outbox/ack` | Bearer; `{up_to}` → `{ok:true}`; monotonic and cannot acknowledge unseen sequence numbers |

All JSON timestamps are UTC RFC 3339, nullable expiry means no expiry. Unknown
fields/duplicate JSON keys/non-finite numbers, transfer encodings, duplicate
Content-Length, compressed requests and unknown routes/methods are rejected.
Bodies are bounded to 64 KiB (message text 60,000 UTF-8 bytes). No CORS headers.
Errors use `{error:{code,message}}`, static credential-free descriptions,
`Retry-After` on limits. A previously accepted item that later becomes
unroutable is terminally failed: its pending capacity is released, its body is
cleared, `failures` reports its ID/reason (latest 100, retained one day from
failure), and replaying its original idempotency key returns 409 with that
reason. Send a new key without the expired context to start again. Forged/expired/revoked tokens have the same 401 response.
Pause returns 503; capacity/rate returns 429. Retry the **same** idempotency key
and same content after temporary errors; a different payload with that key is
409. Idempotency is retained while the corresponding inbox row exists.

External `context` is an opaque public message ID already mapped to **this**
client. Arbitrary Core IDs, recipients, POC, source or management fields are
not accepted. Core IDs and internal POC IDs never enter public output. New
inbound messages receive a fixed untrusted-content/outbound-visibility prefix,
server-assigned `external_source` channel/account/client and label. JSON that
looks like Telegram `im` commands remains plain message text; it cannot change
POCs. External content does not authorize tool execution. **POC replies are
visible outside the workspace; never send credentials or internal information.**

The POC replies to the bridge with `--context` pointing at the received Core
message. Every reply context must map to this same client and the verified Core
sender must be the currently bound POC. No single-client fallback. The bridge
persists output and a metadata-only audit before Core acknowledgment. Retried
Core deliveries cannot enqueue duplicate replies. Incoming sends have a stable
Core idempotency key and a recoverable 60-second lease.

## Private settings API

Open through workspace ingress or `/api/workspaces/W/apps/ID/ui/`; relative
assets and API work under either prefix. HTML/Markdown root negotiation follows
App guidelines; all assets and JSON require the same current human identity.
Pairing cooldown is visible as `pair_backoff` in state and on the page; the
public/bridge processes write at most one small private non-secret diagnostic
file per cooldown window, never anonymous-failure rows in SQLite. Sustained
anonymous pairing traffic can keep legitimate redemption cooling down; no
issued codes are destroyed because of it.
State changes also require a trusted configured App/Proxy Origin,
`X-Http-Settings: 1`, and non-cross-site Fetch Metadata. No public management API.

- `GET /v1/state`: current user, client labels/POCs/status, queue counts, visible
  Agent paths, latest 50 metadata audit entries. No tokens, codes, salts or bodies.
- `POST /v1/pairings`: `{label,poc,expires_at?}`; `poc` is a visible stable Agent
  ID and expiry is RFC 3339 with timezone; returns one-time code/client/expiry.
- `POST /v1/clients/ID/rotate`: same fields; revokes first, returns a new code.
- `POST /v1/clients/ID/revoke`: `{}`; cancels current token and pending codes.
- `POST /v1/clients/ID/pause` or `/v1/pause`: `{paused:true|false}`.

The human UI supports English/Chinese, light/dark, narrow screens, native
keyboard controls and text-only rendering. It never stores credentials in
browser storage; dismiss/reload clears the one-time code. Queue writes check the
current token generation inside their transaction; a rotation between header
authentication and body parsing cannot commit an old request. Pausing does not
cancel an already accepted in-flight Core send.

## Limits, retention and backup

Defaults: 100 pending items / 4 MiB each direction per client; 10,000 queue rows
and 256 MiB bodies globally; 100,000 bounded mappings; 1,000 configured clients.
Oldest undelivered/unacknowledged items stay; new items get 429. Authenticated
requests: 240/min/client; new inbox items: 60/hour/client and 1,000/min globally;
bridge delivery additionally 20/min/conversation and 100 unreplied. Management
writes 20/hour/human. Pair exchanges have global 10/min admission and bounded
cooldown, with bounded in-memory known-code failure counts; unknown failures
never write SQLite or invalidate issued codes. Anonymous failures have a separate
60/min response budget; no client budget is spent. Restart resets RAM counters.

Long polls: two/client, 16 globally, authenticated before admission, 30 seconds
maximum, no open SQLite transaction while waiting. Pause/revoke rechecked every
200 ms and before response; the listener caps all connections at 40. Body read
has a five-second deadline; header reads use a five-second **idle**, not total,
timeout. Slow headers can occupy all 40 slots until the Core ingress protection
is deployed; keep this listener loopback-only.
These are **not trustworthy per-IP limits or flood protection**. App ignores
forwarding/IP headers. Until Core #146 is deployed, protect the public listener
with an operator-controlled edge rate/body/header/time/concurrency policy; no
claim that Proxy currently supplies it. Reverse-proxy response timeout must
exceed 30 seconds. #146 is separate and not a prerequisite to this App version.

Core deliveries with invalid bodies, sender or context use a bounded 128-record
metadata diagnostic; after three attempts or one day they are audited and
acknowledged without exporting/storing their bodies. One invalid delivery never
blocks the remainder of a received batch. For full/mapping-capacity/paused
clients, replies not yet persisted have three attempts **and a 60-second grace**
(or one day, or diagnostic capacity exhaustion), then metadata audit + Core ack.
Cumulative drop counts/reasons appear in private settings state and UI. The
bridge sends the bound POC a metadata-only Core notice, at most one/client/minute
and 20/minute globally, using stdin and an idempotency key; a failed notice is
audited and is not retried indefinitely. This diagnostic is best effort (a crash
after reserving its rate slot can lose it); counters/audit remain authoritative.
This can discard a legitimate reply while a client is offline/full/paused; it is
not lossless delivery. Already persisted outbox items are never evicted. This
bounds Core receive-window starvation from a single unavailable client. Inbound
items whose context vanished, whose client was revoked, or that remained
unavailable for 24 hours get the terminal failure described above. This is bounded retry, not a guaranteed dead-letter inbox.

The bridge prunes acked output after seven days, delivered input after one day,
and mappings after 30 days. Terminal failures expire one day after failure.
Unacked outbox bodies never expire automatically; an operator must restore
capacity. Pending inbound items time out when the bridge next leases them after
24 hours; paused clients retain their accepted items until resumed/revoked. Old idempotency keys can be reused after
cleanup. Pending codes expire and old pairing rows are pruned. Audit is bounded.
All queues, mappings, files and backups are **plaintext**, no at-rest encryption.
The state directory is 0700, databases/SQLite WAL/SHM and socket 0600. Token/code
plaintext exists transiently in process memory for exchange; buffers are cleared
best effort, but Python cannot guarantee every immutable memory copy is wiped.
Request bodies, raw errors, tokens and codes are never logged.

Back up `data.sqlite` and `admin.sqlite` with SQLite's online backup API into a
private 0700 directory, files 0600, or stop **only this bridge/App's processes**
and copy both databases with any live WAL files. The two files are not a single
transaction: for a mutually consistent recovery point quiesce these three
processes before backup/restore. Protect/encrypt backup media. Do not copy a live
SQLite file alone or change the running service without operator authorization.
Restore both files together; keep the prior immutable release. There is no
migration of Telegram data and no changes to its existing DB or admission rules.

## Verification

```sh
env -i PATH="$PATH" python3 -I -m unittest discover -s apps/telegram/tests -v
# Focused HTTP browser fixture (requires an installed local Playwright):
env -i PATH="$PATH" TELEGRAM_PLAYWRIGHT_MODULE=/path/to/playwright \
  node --test apps/telegram/tests/http_channel/browser.mjs
```

The fixture sanitizes forwarded headers/cookies and injects per-request human
identity only on the private path, with synthetic Core identity/CLI and actual
local HTTP/SQLite/socket processes. It covers ingress-shaped and embedded-prefix
flows at desktop/390px, both themes/languages, pairing → message → reply → ack →
revoke, plus a real 30-second forwarded poll. This is **not** evidence that a
particular deployed Proxy accepts a 30-second poll; that requires Core's actual
public-ingress handler test and deployment acceptance. No live machine testing.
