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

Email journal inspection/recovery remains current-human-owner only; binding management uses the existing scoped manager API described in [AGENT-API.md](AGENT-API.md). Thread reads/replies use a distinct
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

The integrated worker defaults to outbox; explicit operator-only `mode:"proxy"`
uses the existing fixed recipient and committed single-use dispatch journal.
See [EMAIL-ACTIVATION.md](EMAIL-ACTIVATION.md#runnable-isolated-assembly) for
assembly commands, owner recovery and the remaining real-provider boundary.

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

## Current outbox increment

The production assembly opts in only when private config has
`"email":{"outbox_directory":"/absolute/private/email-outbox"}`. Omission
keeps all member routes and email workers absent. The config accepts no mail
mode/provider token: runtime is **outbox only**. The Proxy sender is a separately
tested unused library helper; no real mail can be enabled from the browser or
this config. The existing default-off operator activation gate and owner resume
checks apply before either direction can leave a queue.

Owner `bind_email` goes through the unchanged owner authenticator, confirms a
fresh directory member outside the hub state lock, checks paused state/revision,
and atomically commits target, revision and audit. The target is immutable and
exclusive with Telegram for that endpoint. Failed membership, storage or CAS
cannot partially install a target. Agent JSON cannot configure contacts.

The per-endpoint outbox worker verifies current member visibility with a token
minted from the hub's own workload identity, refreshes actual Core metadata,
then checks pause/current generation/target and queue lease. No mutex spans
CLI/directory I/O or outbox publication. A committed outbox receipt permits
completion of the corresponding reassembled Core transfer; publication is never
called email delivery. Human replies carry the original Core message context,
a channel/thread/source label and App-asserted human attribution through the
same actual endpoint relay. Current membership and Core pins are rechecked
before forwarding; revocation/pause blocks subsequent dispatch. An already
issued Core/network call cannot be recalled. Failed work remains leased/retryable
or explicitly parked, never silently dropped. Crashing during attempt eight
becomes terminal after lease expiry instead of leaving an unclaimable intent.

### Member HTTP and browser

The opt-in member service uses separate `__Host-` Secure/HttpOnly/SameSite=Lax
session and login cookies, separate bounded PKCE state and a generated-thread-
only return destination. Session lifetime is capped by the verified capability
expiry. Restart loses in-memory sessions and requires sign-in again. Management
owner cookies cannot authorize a member session; an owner is not implicitly a
recipient of other members' threads. Invalid injected identity never falls back
to a valid member session. Every request verifies active human identity,
audience, workspace, expiry and current directory membership, then exact thread
scope in SQLite.

Routes: `GET /api/email/auth/start?thread=<id>`,
`GET /api/email/auth/callback`, `POST /api/email/auth/logout`, and
`GET|POST /api/email/thread/<id>`. Reply POST accepts only `{key,body}` and returns
queued, not delivered. One-use PKCE/state/browser binding protects the fixed
OAuth callback, which must support normal cross-site navigation. Other GETs
reject present Fetch-Site values except same-origin/none; absent Fetch metadata
is allowed for non-browser clients. Writes require the exact configured Origin,
custom CSRF header and Fetch-Site checks. Authentication precedes the bounded
body read; JSON denies extra fields and has a three-second body deadline.

Confirm the Managed App OAuth client permits the configured dedicated private
App origin and the member callback `/api/email/auth/callback`; the existing
owner callback stays unchanged. No redirect URL comes from request Host or a
thread body. Pending member logins are bounded at 1,000 and use source rate
limits. They remain a bounded member-login denial-of-service surface, separate
from owner sessions. Four blocking HTTP slots and 20 verification requests per
source per ten seconds protect Proxy verification; no positive membership cache
is used. JSON responses have no-store/no-cache, nosniff, no-referrer and the
existing restrictive CSP/frame policy, with no CORS grant. Dedicated-origin
writes only; embedded/foreign App views link to the configured dedicated thread
URL. All body/identity fields render as React text in desktop/390px light/dark.

### Durable state and production blockers

Outbox publication fsyncs an exclusive temporary file, publishes with an
exclusive hard link, removes the temporary link and fsyncs the directory.
Existing records must be private single-link regular files with identical bytes;
verified replay fsyncs them again before accepting a receipt. A crash between
publication and temporary-link removal fails closed: an operator must inspect
and reconcile that private record, never blindly remove or overwrite it.

State, backups and outbox files contain plaintext. Revocation blocks access but
does not erase old records. Retained messages and replies count against the
quota. Referenced accepted Core receipt rows are retained with email replies,
so normal terminal pruning still works for other channels without breaking
thread context/idempotency or aborting on the reply foreign key. There is
**no TTL, owner purge UI or automated private outbox
reconciliation yet**. Cap-full state refuses new work. Do not deploy this
increment for sustained production or delete rows/files to clear capacity while
work is pending. These are explicit blockers for the later real-mail increment,
as are the wired author gate, security/Core approval and separate owner/release
authorization. Runtime tests use fake loopback Proxy/CLI plus the actual Rust
relay; browser tests use a native binary and synthetic identities, never a live
bot or provider.

The current directory client accepts at most 1,000 principals / 256 KiB per
response. Larger workspaces fail closed for email binding, resume and dispatch rather than trusting partial membership. Directory
pagination is not implemented. The owner settings overview shows at most 20
email targets with an explicit truncation notice; other settings remain usable.

## Activation follow-up

[Email activation plan](EMAIL-ACTIVATION.md) records #204 source gaps, the
uncertain-send limitation and the separate review/activation requirements.
Existing runtime remains outbox-only; the plan does not enable real sending.
