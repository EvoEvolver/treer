# Email activation increment (#204)

This is a source audit and proposed implementation sequence, not authorization
to deploy, change configuration or send mail. Both channel increments share the same assembled hub. All initial
checks use fresh private HOME/state, fake Core/Proxy, synthetic members and local
outbox; never the real provider. A later real-send authorization must name the
recipient `doomspec@outlook.com`, count, exact version and rollback boundary.

## Existing assembly and actual gaps

| Boundary | Present in source | Remaining work |
| --- | --- | --- |
| Notification | `email::Intent::notification`: fixed subject/one-line summary and configured private thread URL; no message body | Keep this exact representation for real transport; no message-derived address, subject or URL |
| Member threads | `email_threads.rs`, `EmailThread.tsx`: injected identity then dedicated Treer OAuth/PKCE, per-request current directory, immutable contact/member/thread checks | Fresh browser acceptance with real Proxy headers before any rollout; owner does not bypass another member's thread |
| Management UI | `main.tsx` binds a paused email target, lists targets and outbox counters; preview/resume shares existing executor | Mode-aware status and bounded owner-only journal inspection/prepared abandonment are implemented; no second login/settings App |
| Worker | `email_workers::Config.mode` defaults to `outbox`; explicit `proxy` uses the committed journal and single-use transport | Production configuration/activation and real mail remain separately authorized; no UI or Agent command changes mode |
| Proxy helper | Worker invokes `send_committed` with frozen bytes, fresh membership/Core readback before reservation and a post-token current binding/lease/stop fence | Real provider and deployed identity/browser acceptance remain unrun |
| Queues | Durable intent digest/key/lease/retry/parking, rate and byte/count limits, pause/readback checks | Distinguish provider acceptance, definite refusal and uncertain result; recovery cannot claim exactly-once external mail |
| Retention | Private SQLite and outbox, bounded retained plaintext | No TTL/purge UI/private outbox reconciliation yet; sustained production remains blocked |

The assembled development boundary merges Telegram `2eb511e1` and email
`e9359a14`. Earlier review notes below describe the staged primitives; the
integration status in this section takes precedence over their historical
“not wired” wording. No installed configuration or channel was changed.

### Runnable isolated assembly

Run from the repository root, using the App's independent Cargo manifest:

```sh
cargo test --manifest-path apps/message-endpoint/Cargo.toml --locked --offline --all-features hub_runtime::positive_fixture -- --nocapture
cargo test --manifest-path apps/message-endpoint/Cargo.toml --locked --offline --all-features owner_configuration_inspects -- --nocapture
```

The native hub, actual Rust endpoint relay, owner HTTP, member thread/reply,
private SQLite and restart paths run together against synthetic loopback
Core/Proxy/Bot/mail services in fresh private state. This is **not** real Treer
Policy/provider or real email acceptance. Accepted and uncertain Proxy responses
both result in exactly one local POST, even across a real hub restart. Duplicate
member reply keys result in one final recipient message. The fake mail route
uses the existing fixed-recipient restriction; it never contacts a provider.

Operator JSON retains `email.outbox_directory` and adds optional
`email.mode: "outbox" | "proxy"` (default `outbox`). `proxy` additionally requires
the existing operator activation gate, active generation-pinned binding, current
workspace member, fresh Core readback, fixed `REAL_RECIPIENT`, normal own-App
service token and upstream allowlist/Policy. Configuring this mode is not
permission to activate it. No UI/Agent action selects transport mode.

Reservation commits before token mint/POST. The post-token fence is local and
samples current time under the write lock; it rechecks stop, pause, generation,
contact, lease and readback validity without another network operation inside
the 45-second reservation budget. Accepted completion persists even if stop or
pause raced the response. Any ambiguous response remains `uncertain`; startup
contains interrupted dispatches and neither path resends them. Retained
uncertainty consumes bounded slots and requires owner reconciliation. Local/global
capacity exhaustion pauses the current binding with `email_capacity_full`;
repeated checks do not increment revision again or release any reservation.

The existing human-owner configuration route now accepts:

- `inspect_email_delivery {endpoint_id}`: masked counters plus at most 20 opaque
  intent IDs/generations/states, with no payload, address, key or reference.
- `abandon_prepared_email {endpoint_id,revision,intent_id,confirm:true}`: current
  paused binding only, using the reviewed atomic audited primitive. Uncertain,
  dispatching and accepted states are refused. Content and tombstones remain.

Both operations are forbidden to Agent callers. Existing owner verification,
expiry, CSRF, request bounds and quotas remain in force. The settings card
requires inspection and explicit exact-item confirmation; no resend/clear UI.
`tests/email-browser.mjs` exercises the native UI through dedicated, wildcard
and embedded local fixtures, including mobile layout and member/owner scope.
Those identities/upstreams are synthetic; deployed-browser acceptance is pending.

OAuth establishes a current Treer member, not mailbox ownership: the address to
member association remains explicitly administrator/manager asserted. No public
access, nonmember reply, mailbox account system, inbound SMTP or magic link.

## Offline journal primitives (not runtime activation)

`email_delivery.rs` now supplies local queue operations for freezing an exact
JSON notification/key, reserving dispatch, persisting an outcome and containing
interrupted attempts. `Queue::new` creates the additive journal schema but does
not change any historical row to real-send mode, recover attempts, start a
worker or call the network. Existing outbox behavior is unchanged for rows that
have never entered this journal.

- `prepare_proxy_delivery` requires a current active leased intent with no
  completed mode. It persists exact bytes (maximum 4096), including the fixed
  recipient, summary, configured URL, key and opaque reference. Re-preparation
  with changed bytes conflicts. Frozen rows count against the existing shared
  email message/reply byte and count limits.
- `begin_proxy_delivery` rechecks the active lease and expected payload under an
  IMMEDIATE transaction. A URL/configuration change conflicts before reservation;
  it never replaces the frozen bytes. It reserves one of 32 per stable binding /
  640 globally before returning an opaque attempt containing the stored bytes.
  Those bytes, rather than `Sender::send`'s existing template regeneration, must
  be used by the future reviewed transport wiring. A failed commit returns no
  attempt. At capacity it returns a fixed error with no state change or POST;
  the assembled runtime pauses the current binding with `email_capacity_full`, increments its revision once, and retains all reservations.
- `finish_proxy_delivery` consumes the attempt. Valid provider acceptance and
  queue completion commit together, releasing its reservation. Uncertainty
  persists without completing the queue or releasing capacity. A failed outcome
  commit grants no acknowledgement permission. Neither ordinary queue completion
  nor the legacy failure path can bypass a recorded ambiguous attempt.
- `recover_interrupted_proxy_deliveries` is for startup with dispatch workers
  stopped: every `dispatching` row becomes `uncertain`, including a crash before
  the POST actually started. It never reissues requests or concludes non-send.
  Ordinary outbox claims/fences exclude every journal row. The separate future
  Proxy claim path may reclaim only `prepared` rows; dispatching/uncertain/accepted
  rows remain excluded even after lease expiry, restart or the Core reservation's
  60-second window.

These are storage primitives, not an authenticated management/reconciliation
API or a persistent worker assembly. There is no owner resolution/delete/retry HTTP
operation yet, and no production caller creates journal attempts. The local
prepared-only abandonment primitive is described below. There is no
automatic uncertain-row expiry, slot clearing or implicit resume. Fresh Core
pins/member checks, the strict live allowlist, activation fencing, HTTP transport
using frozen bytes, truthful UI, audited owner reconciliation, process-kill and
ack-frame ordering tests still precede real sending. Rolling back to an older
binary that does not understand this journal is not a reviewed recovery method
after any real attempt; keep containment and retained private state instead.
The local tests cover stored-byte/URL conflicts, completed-outbox refusal,
restart containment, sticky ambiguity, local/global capacity and commit failures;
they do not constitute the future end-to-end send/ack crash rehearsal.

The unused `Queue::proxy_dispatchable` provides the journal-specific local
positive fence. It repeats live lease/nonce, contact/address/thread, generation,
pause/revocation, dispatching state and exact frozen bytes/URL checks in a short
IMMEDIATE transaction; private-storage identity is rechecked around that read.
Use it after token mint, bound to the correct Hub Store, in addition to fresh
Core/member/allowlist/operator activation checks. It cannot make a later POST
atomic with a subsequent pause. The old outbox `dispatchable` remains negative
for all journal work.
The named `Queue::proxy_dispatch_allowed` callback adapter maps every error to
false without logging it or releasing capacity; future runtime callers must use
this fail-closed conversion rather than assume an unavailable check permits POST.

`Queue::proxy_delivery_status` provides only prepared/dispatching/uncertain/
accepted counts across a stable binding's generations and a local/global
capacity flag. It returns no payload, address, URL, idempotency key or reference,
and fails on private-storage drift. This is an internal storage query, not a new
authenticated HTTP route or rendered settings status; those remain to be wired.
The global-pressure flag reveals one bit about other bindings. Keep this complete
status owner/admin-only; a future member-visible view must omit that flag or use
only binding-local pressure, with separate authentication and masking tests.

Lease expiry during token mint is an explicitly accepted conservative cost:
the fence refuses POST and the reservation stays uncertain. Current claims last
60 seconds, the Core CLI call is capped at 15 seconds and the HTTP request at
20 seconds. `begin_proxy_delivery` now requires at least 45 seconds of remaining
lease inside its reservation transaction (those two budgets plus 10 seconds
margin); 44 seconds is refused with the prepared row and claim unchanged, no
attempt returned and no uncertainty slot reserved. An expired prepared claim can
be reclaimed with its frozen bytes intact. The explicit-time storage seams are
private and test-only; production builds cannot select a caller-supplied clock.
The CLI and HTTP builders use named timeout constants. A regression compares
their actual sum plus the ten-second margin with the reservation minimum, so
raising either timeout requires keeping the admission budget sufficient.

The future worker must use `begin_proxy_delivery_now` for reservation and
`proxy_dispatch_allowed_now` from the callback after token mint. These new local
entry points independently sample the wall clock after acquiring their SQLite
IMMEDIATE write transaction, so time spent waiting for that lock is included.
Clock conversion/storage failure refuses reservation or returns false from the
post-mint fence. The explicit-time seams remain available only for deterministic
unit fixtures. A two-connection
contention regression advances a synthetic clock from the busy handler: budget
lost while waiting refuses reservation without changing prepared state, and
post-mint expiry while waiting refuses dispatch without releasing its reservation.
The wall-clock helper does not claim protection against arbitrary system clock
steps or atomicity with the subsequent external POST.

A synthetic mint-time clock-advance regression demonstrates this callback ordering:
the pre-mint lease passes, the callback observes expiry, no HTTP connection occurs
and the committed reservation becomes uncertain. The separate mint-time pause
regression also remains. The assembled worker and runtime clock wiring are not
implemented. Acceptance after a request has started remains recordable after
expiry, as the journal completion requires.

`Sender::send_committed` is a separate, currently unused assembly primitive. It
consumes the opaque committed attempt, mints this App's service token, rechecks
the caller-supplied current fence after minting and POSTs the stored JSON bytes
without regeneration. Its opaque completion can only be committed to the queue;
it does not return an attempt for another send. Every failure, including a local
token/fence refusal after reservation, conservatively retains uncertainty and
capacity. Dropping a completion or failing its commit leaves the dispatching row
for startup containment. This trades availability for avoiding a retry path; it
does not establish the still-pending runtime authorization checks. Loopback tests
compare the entire received body with the frozen payload, exercise acceptance,
connection loss/truncation/503, and show no second POST or claim. They are not
process-kill tests or evidence of real provider delivery.

The existing outbox worker startup now acquires per-Hub email worker ownership
under the state mutex and commits interrupted-attempt recovery before spawning
its supervisor. A duplicate start is refused before touching journal rows.
Recovery/storage failure returns an error without worker ownership or spawning;
there is no fallback that skips containment. Failed thread spawning releases the
startup guard. Normal shutdown releases it only after the supervisor has joined
its endpoint threads. If joining the supervisor fails, ownership stays latched:
detached endpoint work cannot safely be assumed stopped. Recover by stopping the
process and reopening its private store, not by starting a second supervisor.
This process-local gate complements private-store process exclusion; it is not
distributed ownership or real-send activation. Local tests use an I/O-panicking
fake Core to verify committed recovery, duplicate-start refusal, failed COMMIT
with unchanged journal/ownership, and conservative stop-failure latching. They
also verify that repeated clean restarts preserve prepared, provider-accepted
and uncertain journal states, frozen payloads and intent completion. They do not
yet exercise assembled real-transport worker threads or a process crash.

Shutdown currently uses unbounded `JoinHandle::join`; there is no demonstrated
35-second end-to-end stop deadline. The 15-second CLI and 20-second HTTP limits
bound those individual operations, not scheduling, lock waits or all worker work.
A future real-send worker must allow an already obtained provider-accepted
Completion to commit through `finish_proxy_delivery` despite a stop request. A
stop flag never proves non-send and must not relabel genuine acceptance. If the
outcome/completion is lost, retain dispatching until startup contains it as
uncertain, with no second POST. Before activation, an assembled fake-POST test
must request stop during an in-flight request, exercise both accepted completion
and deliberately lost completion, and verify their different recovery outcomes.
Never release worker ownership on an elapsed timeout while a worker may live.

A hermetic transport/storage composition test now exercises those two result
paths with a persistent private Store and an actual loopback HTTP client. The
fake Proxy consumes the exact frozen POST before the harness requests stop and
pauses the binding; it withholds the response until both actions are complete.
The acquired accepted Completion is committed despite that flag and pause in
one branch. In the other branch the harness deliberately drops Completion.
After closing and reopening SQLite through the actual exclusive startup lease,
accepted stays accepted with a completed
intent, while the lost result becomes uncertain with the intent incomplete.
The frozen bytes persist unchanged, and restoring ordinary pause eligibility
still cannot reserve either journal entry again. A post-mint fresh-clock journal
fence is used. A separate SQLite connection commits the pause while the HTTP
response is withheld, proving the network wait retains neither the Store mutex
nor a SQLite write lock. Both fake threads must finish within a bounded test
wait before joining; this is not a production shutdown deadline.

That stop-flag test is a transport/storage harness, not an assembled Proxy-mode
Workers stop: the stop flag is controlled by the test.

A separate Unix process-death test now kills and reaps its exact child at four
real storage/transport boundaries. A SQLite UPDATE hook holds the reservation
transaction before COMMIT; a second case holds after reservation COMMIT but
before POST. The other two cases consume one exact frozen loopback POST and
return synthetic acceptance, then hold inside the completion transaction before
COMMIT or after successful COMMIT. Hooks retain the real Store commit hook.
Before SIGKILL, the parent independently reads the exact intent and its journal
and reservation state; a ready file alone is not evidence of COMMIT. The child
must remain alive until killed and exit with SIGKILL, with bounded parent waits
and cleanup restricted to that owned child.

Reopening the private Store through the actual startup lease leaves the first
case prepared and eligible for a fresh local reservation with identical bytes
(no POST). Both committed-dispatching cases become uncertain and retain their
reservation, including the case where HTTP never began. Committed acceptance
stays accepted with a completed intent and released reservation. The latter
three refuse a new begin/claim even after unpause and lease expiry. The retained
listener verifies zero POSTs in the first two cases, exactly one in the last
two, and no additional connection through recovery and claim checks.

This proves hermetic process-death containment at these boundaries, not
assembled worker restart ownership or a production shutdown deadline. Real-send
runtime wiring, process supervision and end-to-end worker crash rehearsal remain
activation blockers. The loopback fake does not establish provider delivery.


Before wiring real-send primitives into workers, the remaining storage lifecycle
requirements from review 5999 must also be implemented and tested:

- Keep real-send worker spawning behind this same startup gate; do not introduce
  a second worker factory that bypasses recovery or resets active ownership.
- Give the owner an audited abandonment path for **prepared only** rows after
  configuration changes or revocation. Do not extend it to dispatching,
  uncertain or accepted rows. Coordinate payload removal with the intent foreign
  key and future retention jobs; preserve dedup/history semantics.
- Define and implement bounded retention/purge for accepted payloads (addresses
  and thread URLs), with no removal of unresolved work. Current shared caps
  bound storage size, but do not provide a time-based privacy retention policy.
- Expose masked reserved/uncertain counts and containment reasons so a binding
  stopped by 32 unresolved attempts is diagnosable without bodies or keys.

Any completion error or dropped attempt remains ambiguous and must never cause
a second POST. The loopback test includes a successful fake acceptance followed
by a refused SQLite commit: queue completion rolls back, a second attempt is
refused with the original lease still valid, and startup containment retains
the row and reservation as uncertain. No logging Drop guard is needed: the
already committed dispatching row is the durable fail-closed record.

## Proposed transport boundary for review

Keep email absent as disabled and existing `email.outbox_directory` configurations
working unchanged. Add an explicit operator-configured transport enum only in a
separately reviewed increment. Default is outbox; unknown modes/fields fail closed.
Real Proxy mode never reads provider secrets: use only the hub runtime's own App
service token and existing configured Proxy URL. Browser/API management cannot
change mode, origin, credentials, real-recipient restriction or allowlist.

At **binding admission and dispatch**, require the fixed real recipient plus an
explicit configured `TREER_EMAIL_ALLOWLIST` match; empty/unmatched allowlist
disables real-mode binding and dispatch. The human settings executor and Agent
API share this gate: an unapproved `bind_email` address is refused with a fixed,
audited `email_recipient_not_allowed` code, without storing the attempted address
in audit. Fake/outbox tests use synthetic configured recipients; they do not
weaken the live fixed-recipient restriction.
Never infer permission from a stored address or OAuth alone. Existing Proxy Core
admission allows a verified organization member OR an allowlist match, so its
allowlist is not an unconditional second gate. The App must keep its own strict
intersection; this proposal does not widen the Core route or require a Core edit.
All tests use synthetic allowlists and fake/outbox transport.

Freeze the **exact serialized notification payload and key together** before
any dispatch: recipient, subject, fixed summary, public thread URL and all other
bytes are immutable. A changed URL, contact, template or configuration cannot
silently reuse that key with new bytes, resend completed outbox work, or resolve
an uncertain attempt. Hold a pending configuration mismatch for reconciliation;
do not create a fresh key as an automatic workaround. A result labelled `sent`
by the current Proxy means provider acceptance, not mailbox arrival or human
reading. The App must not present it as delivered/read.

Keep the existing fixed template and **fixed one-line summary**, not a summary
extracted from Agent/external message text. No author-controlled text enters a
mail header or notification summary. Assert bounded length, no CR/LF/control
characters, no markup and no URL except the configured thread link. Tests feed
header-injection strings, markup and attacker URLs through message content and
confirm the outgoing notification bytes stay fixed.

Email resume remains behind the operator activation gate, which neither the
Agent API nor browser management can flip. Any verified workspace Agent may
manage bindings under #159's accepted model, but may not enable real transport,
change its allowlist or bypass that gate. A manager-asserted contact/member
association is not proof of mailbox ownership.

### Uncertain send outcomes

Core source `auth/email.rs` stores `sending` before the provider call and `sent`
after success. Failed/stale attempts can be retried; a provider/network failure
or crash between external acceptance and the `sent` update can be ambiguous.
The App helper now returns `Uncertain` for every non-success or unreadable
result after attempting POST, including redirects, 429, `email_in_progress`,
network failures and malformed success. `Retry` is limited to a local fence
refusal before POST. Token acquisition failure is also pre-POST. These local
results cannot override a previously uncertain attempt. Valid `sent` is provider
acceptance only. This classifier is not wired into the outbox worker and does
not by itself wire the offline outcome journal into runtime dispatch. The current code
does **not** guarantee exactly-once external email.

Before enabling real sends, use this conservative persisted outcome model:

| State/outcome | Meaning and next action |
| --- | --- |
| `prepared` | Frozen payload/key; no dispatch started. Local validation, token mint or fence refusal with no POST is definite no-send. Hold/refuse or retry only after the local condition is resolved. |
| `dispatching` | Reserve uncertainty capacity and persist this state **before** POST. A crash/restart in this state becomes `uncertain`, even if the network operation might not have started. Never recover it by another POST. |
| `uncertain` | First `email_in_progress`, network timeout/error after dispatch, 5xx, `email_delivery_failed`, `email_storage_failed`, malformed/truncated/oversized success, unknown post-dispatch result or recovered `dispatching`. Persist and reconcile; **no automatic POST**, with either the same or a new key. |
| `provider_accepted` | Valid `sent` response/cached prior success. Provider acceptance only; never delivered/read. Persist completion before retiring work. |
| `rejected` | Definite pre-dispatch admission refusal, with no previous ambiguous attempt; fixed reason only. A later refusal cannot override an earlier uncertain result. |
| `published_outbox` | Local outbox publication only. Mode change never converts this terminal state into a real send. |

Core's `email_in_progress` is not a safe-retry signal: it protects a recent
`sending` reservation for only 60 seconds. A stale `sending` or `failed` record
can call the provider again with the **same key** while the original operation
may have accepted mail or still be running. The route supplies no independent
outcome query or provider-idempotency guarantee. Never use POST as a status
probe. Uncertainty is sticky across time (including beyond 60 seconds), restart,
configuration change, subsequent 429/refusal, or a stale/failed Core reservation.
Separate operator reconciliation requires a reviewed outcome decision; it is
not a hidden retry button or permission to discard the row.

Proposed hard uncertainty budget: **32 reserved-or-uncertain rows per binding,
640 globally**, within the existing stricter byte/count admission limits. Reserve
capacity transactionally before each POST so an ambiguous response always has a
slot. Owner settings show pending/uncertain counts and the fixed containment
reason, never response bodies or credentials. At a cap, hold new notifications
and pause that binding before another dispatch; do not drop, acknowledge away,
expire, overwrite or retry uncertain rows to free capacity. Other bindings retain
their independent limits. Successful/definitely rejected attempts release their
reservation; only explicit reviewed reconciliation can retire an uncertain row.

Fake-clock tests must cover ambiguity followed by >60 seconds, `in_progress`,
stale/failed reservation responses, 429, URL/configuration change, and restart:
**zero second external send** in every such case. Also cover a crash immediately
before POST/after acceptance/before local completion, exact frozen payload/key,
reservation saturation and owner-visible counts. No test calls the real provider.

No request for a new Core API is made by this plan. If the existing route cannot
provide enough outcome classification, first retain uncertain work for manual
reconciliation and disclose the limitation. A provider-specific idempotency
capability would be separate Core-owned work, not guessed by this App.

### Human-owner reconciliation aid (proposed, not implemented)

Include a fixed-format opaque reference in the notification's fixed body, such
as `Reference: <16 lowercase hex characters>`. Derive it from a domain-separated
SHA-256 of the frozen idempotency key (`treer-email-marker-v1` domain), never from
message content; expose neither the key nor an underlying provider credential.
Freeze the reference with the exact serialized payload/key before first POST
and retain it in the uncertain row. It is a correlation aid, **not** a Core or
provider receipt, permission token or proof that a particular attempt arrived.
Collisions or multiple attempts sharing a reference cannot establish uniqueness
or authorize a resend. The body still contains no author-controlled summary,
markup or URL beyond its configured thread link.

Reconciliation is a separately reviewed **human-owner-only** operation, outside
the any-Agent binding-management grant. Its decision must distinguish:

| Evidence/outcome | Allowed interpretation |
| --- | --- |
| Owner positively observes the matching reference in the fixed mailbox | Record `owner_observed_receipt`, explicitly owner-asserted, not Core-verified. This is not proof of exactly one delivery, provider identity or which attempt succeeded. Do not resend. |
| Independently verified non-send evidence | Record `externally_verified_non_send` with a reviewed evidence class; mailbox absence and elapsed time do not qualify. No current Core/provider query capability is assumed. A later retry requires its own reviewed authorization path. |
| Reference absent, conflicting, non-unique, or evidence otherwise inconclusive | Remain `uncertain`; retain durable ambiguity and its reserved capacity, and do not POST. Delayed delivery is still possible. |

An accepted `owner_observed_receipt` or `externally_verified_non_send` decision
resolves that exact row and releases its uncertainty reservation in the same
transaction as the outcome and audit record; retained history is not deleted.
An inconclusive decision retains both the unresolved row and its reservation.
Resolution never resumes a paused binding or enables sending automatically,
including after an owner-asserted receipt. Resume is a separate audited action
behind the operator activation gate, and any retry still needs the reviewed
authorization described above.

Audit only verified owner actor, time, opaque reference, decision and bounded
nonsecret evidence-class code. No mail body, screenshots, provider response,
credential or free-text secret is copied to audit. A decision must refer to the
exact frozen row/revision, preserve original uncertainty history and reject a
concurrent/stale decision; no bulk "clear and retry" action. A missing reference
never clears uncertainty or releases capacity. If a future operator action
permits a resend while still unknown, it needs a separately explicit and audited
duplicate-risk decision and code review; it is **not part of this first real-send
increment**. Default containment remains hold/pause, not operator guesswork.

Tests for the eventual implementation cover stable references across restart,
key/payload freezing, missing/colliding/duplicate references, observed receipt
versus absence, stale/concurrent owner decisions, Agent denial, redacted audit,
atomic reservation release only for resolved outcomes, reservation retention for
inconclusive outcomes, no implicit resume, and zero additional external sends
after any inconclusive decision.

## Implementation and acceptance sequence

1. Run the existing hermetic email queue/member/Proxy tests as a baseline. Add
   fake-transport assembly cases before a real worker branch: duplicate key and
   payload conflict, 429 becoming sticky uncertainty with zero further POSTs
   for the frozen intent, sender disabled, pause during token mint,
   membership withdrawal, changed generation, timeout/ambiguous outcome, crash
   around acceptance and restart with the same key. No actual emails.
2. Implement the reviewed operator transport/outcome model and shared UI/API
   status. Keep contact target and member immutable per generation. Revalidate
   current membership and Core pins outside the hub mutex, then check current
   lease/contact/generation/pause immediately before dispatch. No network under
   the hub mutex. Keep actual actor notices/audit and Agent API behavior from #159.
   Implement a journal-specific positive pre-POST fence: the legacy
   `Queue::dispatchable` deliberately rejects every journal row and must not be
   reused for this purpose. Bind the fence to the correct Store/Hub and all the
   current checks above; the opaque attempt alone is not runtime authorization.
   Keep durable lease/contact/generation/pause/revocation checks inside the
   IMMEDIATE `begin_proxy_delivery` transaction, and repeat the journal-specific
   durable fence after token mint immediately before POST. An earlier positive
   check cannot replace either step. Fresh Core/member checks stay outside the
   hub mutex and SQL transaction; no cross-system atomicity is claimed.
3. Finish recovery/retention requirements before sustained use: bounded uncertain
   queue, explicit operator reconciliation, byte/count limits and private outbox
   reconciliation; specify purge/TTL semantics without deleting pending work.
4. Verify thread links are single-purpose and contain no query-borne credentials.
   Thread IDs remain unguessable random identifiers, never bearer authorization.
   An unauthenticated/no-current-member-session read reveals no thread existence
   or content; uniformly require authentication before lookup. Current-member
   verification and immutable member/thread scope still apply after login.
   Run desktop/390px, light/dark, dedicated/wildcard/embedded browser matrix:
   current member reads/replies, other member/owner denied, invalid identity never
   falls back to cookie, OAuth state single-use/nonce/PKCE, bad Origin/Fetch
   Metadata refused, relative assets/API routes, text-only malicious content.
   Obtain deployed-header acceptance under a separate authorized fixture rollout.
5. Freeze exact SHA with App gate and Core/security review. Only then request
   one separate deployment/activation and real-send authorization. The proposed
   first real-send budget is **at most 3 messages**, not an open-ended smoke loop;
   authorization must name the exact binary SHA-256 and configuration SHA-256,
   fixed recipient and allowlist-value hash (never credentials). Any unexpected
   status stops the run. Pause the binding through the API afterwards and record
   the resulting revision. Use an isolated email fixture/state directory: **never
   enable real mode in the sandbox hub carrying the Telegram test state**.
   Evidence must include authorized/attempted/accepted/uncertain counts, mode,
   retry/reconciliation result, member reply received by the fixed Agent,
   negative scope checks and containment. This proposal itself authorizes none.

## Rollback and containment

Pause the exact binding, stop real dispatch through reviewed operator controls,
and preserve private SQLite/outbox snapshots under the usual 0700/0600 and
same-UID limitations. Do not acknowledge/erase ambiguous rows just to make the
queue empty. Rollback to the previous binary/config must not reinterpret completed
outbox receipts as unsent real work, reset keys, lose reply scopes or resend a
possibly accepted provider request. Prior to first real traffic a stopped-state
backup restore can be rehearsed with fakes; after traffic prefer containment and
reviewed reconciliation. No automatic post-send rollback is claimed.


## Prepared-only audited abandonment (primitive and owner HTTP integration)

`Hub::abandon_prepared_email_audited_now` is a local management primitive. The existing human-owner configuration route verifies current owner authority, CSRF and explicit confirmation
of the exact intent and revision before calling. Agent callers are forbidden. It does not authenticate the supplied actor on its own.

Time is sampled after acquiring the IMMEDIATE write lock; explicit-time entry
points exist only in tests. An IMMEDIATE transaction checks the exact current paused binding, its revision
and generation, the journal's `prepared` state, its stored nonce/lease, and an unfinished intent without
a completed mode. It records the verified caller's actor and exact intent ID, previous state and old/new revision in
the existing owner audit, marks the intent parked with `mode=proxy_abandoned`,
clears its nonce/lease, and increments the binding revision atomically. The
journal remains prepared with its original frozen payload; the terminal intent
is the retained tombstone. Masked status separates abandoned items from prepared
work. This is abandonment, not successful delivery or payload deletion.

Repeating the same intent/revision abandonment command is idempotent: no extra
audit or revision bump. A previously claimed Intent cannot reserve dispatch after abandonment, even
after binding resume, lease expiry or reopen. If reservation committed first,
`dispatching`, `uncertain` and `provider_accepted` are all refused; no reservation
is released, no uncertainty is resolved and nothing is retried. Audit failure or
a #217 pathname-drift veto rolls back the intent, audit and revision together.
Full audit capacity fails closed; ordinary pause containment remains available.

Synthetic persistent-store tests cover these states, stale scope/revision,
active binding refusal, old claims after reopen, audit/intent SQL failures and
real commit-hook rollback/sticky refusal on database pathname drift. No Core
command, HTTP request or provider send is executed by these tests. The primitive
does not purge retained payload or conversation data, or supply the still-needed
audited owner reconciliation for uncertain outcomes. Real-send activation and deployed-browser acceptance remain pending; worker assembly and synthetic browser evidence are described above.

The race tests use two actual SQLite connections and barrier-controlled write
transactions. A busy handler observes real lock contention before release; both
reservation-first and abandonment-first orderings are checked. The worker test
connection uses a test-only in-memory Store wrapper for the real second
connection, not a second cooperating App or process ownership claim. The normal
owner Store retains its real #217 commit hook. Separate parent/child tests use
SIGKILL before and after the abandonment COMMIT, inspect committed state from an
independent readonly connection, verify exact child signal/reaping, and reopen
with no partial audit/revision/intent state. This is a local crash test, not
worker assembly or production authorization.

Abandoned prepared items never held a dispatch reservation and release none;
other uncertain reservations remain counted unchanged. Their retained payload
is excluded from status/audit projections and sendable/prepared counts, but must
still consume the shared 4096-byte-per-payload and total email byte/count quotas:
removing that storage accounting would permit unbounded retention. The next
retention slice's first purge target is these abandoned payloads, via explicit
owner-triggered purge while retaining minimal dedupe tombstones; sustained
production remains blocked on that separately reviewed operation. No indefinite
production retention or payload-purge implementation is claimed here.


## Implemented owner recovery successor (#204, 2026-10-22)

This section supersedes the earlier proposed/unimplemented status for the
following bounded development slice; it does not authorize real mail.

The existing freshly verified human-owner configuration API adds
`resolve_email_observed` and `purge_email_payload`, with exact endpoint, revision,
intent and confirmation. The Agent binding-management exception grants neither.
The first also requires the 16-character reference positively observed by the
owner in the intended mailbox. Missing/wrong/colliding reference, stale revision,
unpaused binding, dispatching state and audit/commit failure refuse. Only the
`owner_observed_receipt` evidence class is implemented: provider `uncertain`
history stays unchanged in `email_dispatch`; a separate durable owner resolution
and terminal intent release its uncertainty reservation atomically with audit
and revision. No provider verification, non-send inference, retry or resume is
introduced. Inconclusive observations take no mutation and keep capacity held.

Retirement refuses any unfinished notification, live lease, pending Core reply,
or messages/replies/terminal intents younger than seven days. Terminal age is
conservatively first observed by Queue initialization, including legacy rows;
request JSON cannot select a clock or backdate it. The first successful batch
permanently closes the thread to member reads/replies and new context admission.
Each call clears at most 20 messages, 20 replies (and completed inbound bodies),
and 20 notification payloads; refreshed API/UI metadata retains partially retired
threads so long conversations progress across calls/restarts. It retains source
IDs, digests, receipts, intent states and count-quota tombstones. An exact old
message/reply key remains deduped and conflicting content remains refused. No
pending/uncertain state is deleted. Current contact configuration, Core records,
SQLite historical pages/WAL, external copies and backups are not erased.

Outbox paths never come from requests. A durable bounded queue tells the worker
to remove only terminal intent `<64hex>.json` files beneath its existing private
configured outbox directory, using no-follow file checks and directory fsync.
Missing files complete cleanup idempotently; symlink/hardlink/private-storage
failures retain the queued cleanup. Batches are 20 and restart-safe. UI completion
means logical database payload retirement; file cleanup needs a running worker.
No automatic TTL or unlimited capacity is claimed: tombstones still consume the
existing count budget, and audit-full mutations fail closed.

Worker stop now waits at most 60 seconds for its supervisor. A timeout is exposed
as `stop_timed_out`, keeps the startup ownership latch set and never reclassifies
in-flight work as safely stopped. Detached jobs retain their Hub/Store ownership;
process termination and subsequent exclusive startup recovery remain necessary
for unknown jobs. Accepted HTTP completion can still persist after stop request.

Core review 7926 / test-only `7465ba32` demonstrated a post-token membership gap.
The worker now performs fresh directory membership and Core metadata checks
AFTER token mint, outside the Hub mutex; only afterwards does it acquire the
current local stop/pause/contact/generation/lease fence. The final lease must
still cover the 20-second POST plus 10-second margin. Slow/denied checks leave
the consumed attempt uncertain without POST or resend. These checks cannot make
remote revocation atomic with the later HTTP request; provider authorization
remains independently required. Tests use only new synthetic identities/transport.
