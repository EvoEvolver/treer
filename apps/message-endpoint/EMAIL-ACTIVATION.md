# Email activation increment (#204)

This is a source audit and proposed implementation sequence, not authorization
to deploy, change configuration or send mail. #203 retains priority. All initial
checks use fresh private HOME/state, fake Core/Proxy, synthetic members and local
outbox; never the real provider. A later real-send authorization must name the
recipient `doomspec@outlook.com`, count, exact version and rollback boundary.

## Existing assembly and actual gaps

| Boundary | Present in source | Remaining work |
| --- | --- | --- |
| Notification | `email::Intent::notification`: fixed subject/one-line summary and configured private thread URL; no message body | Keep this exact representation for real transport; no message-derived address, subject or URL |
| Member threads | `email_threads.rs`, `EmailThread.tsx`: injected identity then dedicated Treer OAuth/PKCE, per-request current directory, immutable contact/member/thread checks | Fresh browser acceptance with real Proxy headers before any rollout; owner does not bypass another member's thread |
| Management UI | `main.tsx` binds a paused email target, lists targets and outbox counters; preview/resume shares existing executor | Mode-aware truthful status and recovery, not a second login/settings app |
| Worker | `email_workers::Config` accepts only private `outbox_directory`; worker always calls `write_outbox` | Explicit operator-only real mode, default outbox; no UI or Agent command may turn real sending on |
| Proxy helper | `email_proxy::Sender` uses the hub's own service token, fixed origin/audience, no redirects/proxy environment; strict response limits | Not invoked by the email worker today. Review admission, fresh member/pin readback and post-token fence at actual dispatch |
| Queues | Durable intent digest/key/lease/retry/parking, rate and byte/count limits, pause/readback checks | Distinguish provider acceptance, definite refusal and uncertain result; recovery cannot claim exactly-once external mail |
| Retention | Private SQLite and outbox, bounded retained plaintext | No TTL/purge UI/private outbox reconciliation yet; sustained production remains blocked |

The audit baseline is origin/main `9c57bc44`. #159 Agent binding API successor
`d50167ab` is not an ancestor of that baseline yet. Rebase on the release-integrated
successor before editing management behavior; do not duplicate its authentication,
notices or masked audit implementation. Human-only management wording in older
email docs must be reconciled with the approved Agent API before freezing #204.
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
  the future runtime must add the required binding pause/status containment.
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
API or a persistent worker assembly. There is no owner resolution/delete/retry
operation yet, and no production caller creates journal attempts. There is no
automatic uncertain-row expiry, slot clearing or implicit resume. Fresh Core
pins/member checks, the strict live allowlist, activation fencing, HTTP transport
using frozen bytes, truthful UI, audited owner reconciliation, process-kill and
ack-frame ordering tests still precede real sending. Rolling back to an older
binary that does not understand this journal is not a reviewed recovery method
after any real attempt; keep containment and retained private state instead.
The local tests cover stored-byte/URL conflicts, completed-outbox refusal,
restart containment, sticky ambiguity, local/global capacity and commit failures;
they do not constitute the future end-to-end send/ack crash rehearsal.

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
| `retry_wait` | Only a recognized first-attempt rate admission rejection with a proven no-provider-call contract and no prior ambiguity may back off under bounded attempts using the frozen bytes/key. HTTP 429 alone is insufficient evidence. |
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
   payload conflict, bounded 429 retry, sender disabled, pause during token mint,
   membership withdrawal, changed generation, timeout/ambiguous outcome, crash
   around acceptance and restart with the same key. No actual emails.
2. Implement the reviewed operator transport/outcome model and shared UI/API
   status. Keep contact target and member immutable per generation. Revalidate
   current membership and Core pins outside the hub mutex, then check current
   lease/contact/generation/pause immediately before dispatch. No network under
   the hub mutex. Keep actual actor notices/audit and Agent API behavior from #159.
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
