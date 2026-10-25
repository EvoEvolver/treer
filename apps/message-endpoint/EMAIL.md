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
old threads never transfer to a different contact. Verified workspace management Agents may use the existing binding API under
the owner-selected delegation in [AGENT-API.md](AGENT-API.md); arbitrary message
bodies cannot choose a new destination or member. A thread's initiating Agent must be the fixed
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
uses the member-bound recipient and committed single-use dispatch journal.
See [EMAIL-SETUP.md](EMAIL-SETUP.md) for relay preparation and contact binding.
The worker still requires explicit activation and fresh checks; setup is not a send.

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

Tests use private outbox or a closed loopback Proxy; they send no real email.
The Hub uses the owner's normalized, generation-bound contact address rather
than a hardcoded historical test mailbox. The existing Treer email API still
requires the real App/workspace identity and `email.send` Policy, and accepts
only verified organization-member addresses or its operator-managed allowlist.
A member ID/address association asserted in Hub is not an exemption from those
platform checks. Live sends require the intended recipient/content to be
coordinated separately; this source change does not initiate any send. Missing
platform configuration or denied recipients never produce a fake sent receipt.
No full Core body, user-controlled email HTML or external URL goes into
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

## Runtime transport and owner setup

The production assembly opts in through private configuration:

```json
{"email":{"outbox_directory":"/absolute/private/email-outbox","mode":"proxy"}}
```

Omission keeps member routes and email workers absent. `mode` defaults to
`outbox`; it publishes private files without sending email. Explicit `proxy`
constructs the existing Proxy sender and dispatches committed journal attempts
from the real runtime worker. No SMTP/provider key belongs in Hub configuration
or its browser UI: this uses Treer's built-in App email API. The operator's
`activation_enabled` gate and each endpoint's pause/resume state still apply.
Choosing Proxy mode is not evidence that a provider is configured, that an
address is allowed or that any recipient received mail.

The owner setup card uses the existing authenticated, CSRF-protected
`POST /api/configuration` with `{"action":"preview_email_setup"}`. It reports
`email_configured`, nullable `mode`, `activation_enabled`,
`provider_readiness:"unverified"`, and current visible human `members`
(`id`, `name`, `role`). A failed directory lookup is unavailable, never an empty
success. Results are bounded to 1,000 principals / 256 KiB, have no email
addresses, and are not cached for authorization. A directory name is a display
label, not verified ownership of the typed address. Management Agents cannot
read this owner-only setup preview.

Start with the setup card, have the operator select private `email` configuration
and retain pause until the intended contact/relay is bound. Select a current
human member and enter their email, then bind it to an existing paused relay
using `bind_email`. If no relay exists, use the existing endpoint preview/create
flow with the real relay Agent and fixed internal counterpart; this does not
provision a new relay automatically. Binding rechecks membership and revision;
the preview does not grant later access. Resume only after reviewing the existing
endpoint conditions. The browser does not write startup configuration, enable
the global activation gate, provision a provider, or send a test email.

Inspect delivery for each bound endpoint: `outbox` is local publication,
`provider_accepted` is acceptance by the Proxy/provider (not mailbox delivery),
and `uncertain` is held for reconciliation, never automatically resent. A queued
human reply means it entered the existing Core handoff path, not that the Agent
has processed it. Configuration, binding, runtime and delivery status are
separate facts.

Owner `bind_email` goes through the unchanged owner authenticator, confirms a
fresh directory member outside the hub state lock, checks paused state/revision,
and atomically commits target, revision and audit. The target is immutable and
exclusive with Telegram for that endpoint. Failed membership, storage or CAS
cannot partially install a target. The new human setup preview does not expand
the existing delegated binding authority.

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
thread context/idempotency or aborting on the reply foreign key. Owner-triggered, audited payload retirement is now available in the recovery UI.
It closes a fully terminal conversation only after every terminal intent has
been observed terminal for seven days. Each call clears at most 20 source
messages, 20 replies and 20 notification payloads; repeat against the refreshed
revision until complete. Pending replies, unresolved uncertainty and leases
refuse retirement. IDs, digests, receipts and count quota remain as tombstones;
bytes can be reclaimed but tombstone count capacity is not reset. Outbox files
are durably queued for the configured worker to unlink in batches of 20, including
after restart. No automatic TTL, physical secure-erasure, backup cleanup or
indefinite-capacity guarantee is claimed. SQLite WAL/free pages and backups can
retain old bytes. Owner-observed uncertainty resolution remains an explicit
audited action; real-send activation remains separate. Runtime tests use fake loopback Proxy/CLI plus the actual Rust
relay; browser tests use a native binary and synthetic identities, never a live
bot or provider.

The current directory client accepts at most 1,000 principals / 256 KiB per
response. Larger workspaces fail closed for email binding, resume and dispatch rather than trusting partial membership. Directory
pagination is not implemented. The owner settings overview shows at most 20
email targets with an explicit truncation notice; other settings remain usable.

## Activation follow-up

[Email setup](EMAIL-SETUP.md) describes the existing relay and owner binding path.
This mainline integration reuses the deployed 2065 email setup/runtime and the
92fab safe-diagnostics implementation. It does not activate a channel or resend
an old intent. Historical uncertain rows retain their consumed reservation and
null diagnostic. Current configuration and permission must be inspected through
the existing authenticated APIs rather than inferred from this source tree.

### Safe committed-send diagnostics

New committed sends persist a bounded `diagnostic` alongside the dispatch outcome
in the same SQLite transaction. The existing authenticated, owner-only
`inspect_email_delivery` configuration action returns it per item; this action
only reads journal state. No additional authority, endpoint or sending operation
is introduced. Existing Agent binding permissions and owner-only recovery checks
remain unchanged.

The diagnostic contains only fixed enum `stage`, fixed enum `category`, and
`http_status` (known numeric 100–599 or null). Stages cover `token_mint`, `fence`,
`http`, `response`, and `complete`. Categories distinguish unavailability/refusal,
transport, redirect, response read/size/JSON/shape, non-success HTTP status and
accepted response. No upstream message/code, address, token, body, provider error,
URL, or arbitrary string is stored. A response status is retained only after
headers have actually arrived. Payload remains under the existing private
journal boundary; it is not included in this diagnostic.

Every failure still finishes as **uncertain**, including mint or local fence
refusal. These observations do not release the reservation, reset attempts,
rebuild an intent or authorize another send. An accepted response means platform
acceptance, not recipient inbox delivery. If the process dies or the atomic
completion write fails, startup still contains the attempt as uncertain, and a
missing diagnostic remains null. Historical unknown rows also remain null;
this change cannot recover their discarded causes. The additive diagnostic table
has at most one row per existing dispatch and a 256-byte record limit. Keep the
same new binary for containment; an older binary will not write diagnostics.

Example of an observed failed HTTP response (still uncertain, never retry):

```json
{"stage":"response","category":"http_status","http_status":503}
```

Inspect through the existing owner-authenticated API with
`{"action":"inspect_email_delivery","endpoint_id":"ACTUAL_ENDPOINT_ID"}`.
Do not POST to the email transport to obtain status: even the same idempotency
key can cause a new provider attempt after an uncertain outcome.
