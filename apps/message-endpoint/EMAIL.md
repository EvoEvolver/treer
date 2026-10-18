# Email channel contract (implementation in progress)

Email belongs to the existing Rust hub and endpoint relay. There is no SMTP
listener, inbound-mail webhook, third settings App or new human account system.
Mail is a notification containing fixed summary text plus a private thread link.
The bound workspace human reads and replies in the hub; the endpoint Agent
sends that reply to its fixed internal recipient. Human attribution is asserted
by the hub, not a Core-authenticated human author on the endpoint's message.

## Binding and authority

An owner confirms an immutable email target per endpoint generation: exact
normalized address, current workspace human stable ID and fixed internal
counterpart. Address-to-human matching is an owner assertion; Treer OAuth does
not itself prove ownership of the address. Nonmembers are `reply_unavailable`.
No invitations, guest creation, public links or one-time login credentials are
created. Retargeting/reassignment requires a paused, drained new generation;
old threads never transfer to a different contact. Agent JSON cannot choose a
new destination or member. A thread's initiating Agent must be the fixed
counterpart, so replies cannot turn the relay into an arbitrary recipient deputy.

Management remains current-human-owner only. Thread reads/replies use a distinct
current-member verification path: injected identity first, invalid token never
falls back to cookie; otherwise existing opaque OAuth session. Every request
checks verify_app's active human identity, audience, workspace and expiry plus
current directory membership, then exact immutable target/member binding and
thread scope. An owner is not implicitly the recipient of every private email
thread. Do not weaken the existing owner authenticator to admit members.

Member pages use App-owned routes, relative resources and plain text rendering.
Writes require the configured dedicated App Origin, Sec-Fetch-Site checks,
Content-Type, custom CSRF header and bounded strict JSON. Embedded settings remain
read-only. A guessed thread ID is not authority. Logout invalidates only the
App's opaque session and never the Treer login.

## Persistence and sends

New private SQLite tables hold generation-pinned targets, thread/message IDs,
durable email intents/attempts/receipts and member reply idempotency. Thread IDs
are stable opaque digests, not addresses. Pending work is limited to 1,000 messages / 16 MiB per binding and 10,000 /
128 MiB globally. Send attempts are capped at 10 per minute and 100 per day per
binding, and 60 per minute / 1,000 per day globally. Each intent gets at most
eight attempts with a 60-second lease and bounded backoff. Owner pause blocks
new dispatch immediately (an in-flight network request cannot be recalled);
readback/generation fencing applies immediately before dispatch. Network work never holds the SQLite mutex.
Persist the frozen notification intent and idempotency before dispatch; preserve
retry/lease/backoff through restart. Check current binding, membership and
allowlist again before a queued send. Conflict on changed idempotent payloads.
Outbox transport is the default and is explicit in status; it does not claim
that email was delivered. Outbox files are exclusive-created private records.

The production transport may use only existing
`POST /api/apps/{service_id}/email`, authenticated as the actual hub Managed App
with a short-lived token minted from **its own** workload identity. No Agent,
owner or other App credentials, and no SMTP/provider secret in the hub. Proxy
origin and service are configured, never taken from a message; HTTPS (literal
loopback only for tests), no redirects, bounded request/response/deadline and
fixed error codes. An ambiguous provider result can duplicate mail; do not
promise exactly-once external delivery. Core/Proxy owns provider idempotency.

For this task, all tests use outbox or a fake loopback Proxy. Real sending stays
off unless separately authorized, and the only permitted real recipient is
`doomspec@outlook.com`, further restricted by the existing Treer email allowlist.
This cap cannot be broadened by Agent text or an email binding. Missing provider
configuration produces an explicit unavailable/outbox result, never a fake sent
receipt. No full Core body, user-controlled email HTML or external URL goes into
mail: fixed subject, a template with no text derived from Core/external
messages, and the configured dedicated hub App origin plus generated thread ID.
No caller-selected URL, redirect or link credential is permitted.

## Review and limitations

Email extends the hub's browser trust boundary from owner settings to a scoped
member thread view. That change needs Core/security cross-review before release.
Tests must cover two-member IDOR reads/replies, former members, revoked
contacts/membership, forged
or expired tokens/audience mismatch, CSRF, text XSS, idempotency/concurrency,
pause/generation races, quotas, crash replay and summary-only outbox content.
Desktop/390 light/dark browser checks remain hermetic.

State/backups contain plaintext conversation content. 0600 does not isolate
Agents sharing the OS user. Use dedicated OS users/containers and narrow enforce
Policy/mount/network access. No automatic production migration, old email-thread
URL transfer, inbound email or nonmember participation is implemented yet.

## Increment status

The current additive storage module implements fixed-target thread admission,
notification intents, private outbox records, byte/count/attempt limits, lease
reclaim, retry/backoff and pause/revocation fencing. It is **not connected to the
runtime, browser routes, OAuth member sessions or a Proxy email worker yet**.
The storage methods accept already-verified inputs; they do not authenticate a
member or authorize activation by themselves. No existing endpoint is changed.

Outbox publication uses an fsynced temporary file and exclusive hard-link
publication in a private directory, then removes the temporary link and fsyncs
the directory. Existing files must be private single-link regular files with
identical bytes. A crash between publication and temporary-link removal fails
closed on retry; an operator must inspect and reconcile that private record.
Do not remove or overwrite it automatically or interpret an outbox record as
actual mail delivery. Plaintext outbox records need the same backup/retention
and OS isolation as hub state.
