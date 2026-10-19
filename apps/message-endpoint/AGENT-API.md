# Binding management API

The hub settings UI and Agents use the same `GET /api/settings` and
`POST /api/configuration` executor. This API does not create endpoint Agents,
change their local pins, store their credentials, or turn on the operator's
activation gate. New bindings remain paused. Email remains opt-in and outbox-only.

## Authentication and authority

Use the hub's workspace virtual host, for example `http://message-hub.treer`,
through Treer's authenticated Agent networking. Treer supplies the service identity
header. A separately configured client can supply its own service-audience Bearer
identity. Never put credentials in URLs, shell history, ticket comments or output.

Every request verifies the supplied token against the configured Proxy's App
identity verifier, with the hub service audience and workspace, then requires
one matching Agent entry in the current App directory. A Managed App runtime is
an Agent principal too: its stable Agent ID is the caller, not its service ID.
Invalid injected identity never falls back to Bearer or a browser session.
Directory absence, duplicate IDs, wrong kind, expired/foreign claims or Proxy
failure deny access. Paths are display metadata, not authorization. A null path
is allowed. Directory visibility depends on Core Policy; this snapshot and token
verification do not promise immediate independent Agent revocation.

**Any verified workspace Agent may manage all bindings, including other Agents'
bindings and recipients.** This is the owner's explicit authorization model;
a compromised or prompt-injected workspace Agent can redirect or pause traffic
within the endpoint's existing local recipient pins. Recipient notifications and
audit reveal changes but do not prevent them or constitute human approval.
Keep untrusted Agents out of this workspace or deny their App network access
with Core Policy. The hub itself remains a trusted channel operator.

Browser requests still require the configured exact Origin and Fetch Metadata
checks. A cookie-free Agent identity request without any Origin or Sec-Fetch
headers can omit Origin. All mutations require `X-Endpoint-Request: 1` and
`Content-Type: application/json`, a known Content-Length and at most 16 KiB.
No CORS access is added. Human-owner OAuth remains available; transport token
editing, OAuth flows, recovery and metadata export/purge remain human-owner-only.
Agent identities cannot turn themselves into human owners.

## Requests

Send JSON to `/api/configuration`. Unknown fields are rejected. A minimal
Agent request through the virtual host is:

```sh
curl --fail-with-body http://message-hub.treer/api/configuration \
  -H 'Content-Type: application/json' -H 'X-Endpoint-Request: 1' \
  --data '{"action":"preview_create","endpoint_id":"ag_ENDPOINT","generation":1,"recipient":"ag_RECIPIENT"}'
```

No token argument is needed on the Treer virtual-network route. Use the actual
configured hub name and existing stable IDs, never inferred paths or service IDs.

1. `preview_create`: `endpoint_id`, `generation`, `recipient`. Core metadata
   resolves and validates the endpoint and recipient. Save the returned `preview`.
2. `create`: the same fields, `expected` equal to that preview, `confirm:true`,
   plus a caller-scoped `idempotency_key` (required for Agents, at most128 ID
   characters). Identical concurrent/repeated requests return the original
   `{status:"saved",revision:1,paused:true}` result; key reuse with changed input
   is409. A replay is an original receipt, not a current-state query: fetch
   settings for the present revision. No second audit/notice is created.
3. `bind_telegram`: `endpoint_id`, current `revision`, `confirm:true`,
   `target:{account_id,chat_id,topic_id,allowed_senders}`. Binding must be paused;
   account IDs refer to operator-provisioned slots, never bot tokens. The same
   paused, revision-checked executor accepts `bind_email` with `contact:{address,member_id}` for existing workspace members (see [EMAIL.md](EMAIL.md)).
4. `preview_resume`: `endpoint_id`. Save `preview` exactly as returned. Agent
   responses blank external target IDs and add `confirmation_hash`; echo the
   whole preview rather than reconstructing it. Fresh full state is independently
   compared at commit. `activation_enabled` and `account_ready` must both be true.
5. `resume`: `endpoint_id`, `expected` equal to that fresh preview,
   `confirm:true`, and `evidence:{profile_reviewed:true,isolation,reference,
   held_action,parked_count_ack}`. `isolation` is explicitly `reviewed` or
   `unverified_accepted`; `reference` is required plain nonsecret evidence text.
   `held_action` is `discard` or `release`; acknowledge the preview's parked count.
   An Agent may explicitly accept unverified isolation for any recipient. This
   remains a caller declaration, not App attestation. Operator gate, fresh pins,
   account readiness and existing hold/backlog checks still apply.
6. `pause`: `endpoint_id`, current `revision`, `confirm:true`.
7. `delete`: `endpoint_id`, current `revision`, `confirm:true`. Requires pause
   and no pending, incomplete, ready, parked, held, partially delivered or leased
   work. Otherwise409. This retires only the hub binding and removes its channel
   routing; it never deletes/stops the Core Agent or erases message/audit/dedup
   records. A retained tombstone prevents stable endpoint ID reuse. Provision a
   fresh endpoint Agent for replacement; in-place generation replacement is not
   implemented. Pause cannot recall a send already handed to Core/transport,
   and local drain checks do not prove an empty Core mailbox.

Successful mutations return the new revision; stale revisions and changed
previews return409, validation400, authentication401, authorization/CSRF403,
rate limits429 and unavailable dependencies/storage503. Each mutation response
has a server-generated `X-Endpoint-Request-ID`. Never retry with invented current
revisions: read settings and preview again. A lost success response is ambiguous;
create has its receipt key, while other mutations use compare-and-swap revisions.

## Audit, notifications and limits

Agent mutation audit records contain the verified caller Agent ID, action,
endpoint, prior revision and time. Resume declarations include the explicit
isolation choice/reference and hashed pins. External identifiers in Agent
responses and Agent audit detail are masked, including nested targets; no token
or message body is copied to audit. Do not put secrets in the free-text reference.
A bounded1000-row request ring records request ID, verified caller if available
(or `unverified`), HTTP status and time for rejected/denied calls too. Unfinished
requests retain102. It never trusts a claimed caller ID or records headers/body.
It is a local operational audit, not tamper-proof or a permanent forensic log.

Every Agent-made create/target change/resume/pause/delete queues a Core notice to
the binding's pinned recipient **in the mutation's SQLite transaction**. Audit or
notice storage failure rolls the mutation back. The hub sends using its own
workload identity, with deterministic idempotency keys, outside the state mutex.
The notice identifies the App-asserted caller, action, endpoint and account;
external targets are redacted. It is not an external delivery/read receipt.
Notifications work even when the binding is paused or retired. Core/Policy
failure can delay them; up to8 attempts use60-second leases/retry spacing, then
fail visibly. `/api/settings` includes `settings.management_notices` counters
`pending`, `accepted_by_core`, `failed`. Core acceptance does not prove the
recipient read the notice; exhausted sends require operator investigation.

Limits: five non-retired bindings created per caller Agent, twenty active
bindings total; ten non-pause configuration attempts per caller per60 seconds,
including failed mutations. Pause is containment and exempt from this write
quota. Verification admission precedes Proxy calls: at mostfour concurrent
globally, two per socket source, twenty requests/source/10 seconds, bounded512
source/negative entries. Forwarded clients may share the same source quota.
There is no positive identity cache. The local manager-ID registry is bounded
at1000; it is attribution metadata, never an authentication or authorization
cache. Authoritative audit is bounded at10000 changes, with a bounded emergency
pause record when full. Notices share that bound; rejected calls use the
separate rolling ring. Limits fail closed rather than silently dropping audit.

No deployment, binding activation, real Telegram polling or email delivery is
authorized by installing this code. Use a disposable bot and separate sandbox;
never run two pollers for the production bot.
