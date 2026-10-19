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

## Proposed transport boundary for review

Keep email absent as disabled and existing `email.outbox_directory` configurations
working unchanged. Add an explicit operator-configured transport enum only in a
separately reviewed increment. Default is outbox; unknown modes/fields fail closed.
Real Proxy mode never reads provider secrets: use only the hub runtime's own App
service token and existing configured Proxy URL. Browser/API management cannot
change mode, origin, credentials, real-recipient restriction or allowlist.

Before dispatch require the fixed real recipient plus an explicit configured
`TREER_EMAIL_ALLOWLIST` match; empty/unmatched allowlist disables real dispatch.
Never infer permission from a stored address or OAuth alone. Existing Proxy Core
admission allows a verified organization member OR an allowlist match, so its
allowlist is not an unconditional second gate. The App must keep its own strict
intersection; this proposal does not widen the Core route or require a Core edit.
All tests use synthetic allowlists and fake/outbox transport.

Freeze one durable notification representation/key across retries. A result
labelled `sent` by the current Proxy means provider acceptance, not mailbox
arrival or human reading. The App must not present it as delivered/read.

### Uncertain send outcomes

Core source `auth/email.rs` stores `sending` before the provider call and `sent`
after success. Failed/stale attempts can be retried; a provider/network failure
or crash between external acceptance and the `sent` update can be ambiguous.
The existing App helper collapses several cases into `Retry`. Therefore the
current code does **not** guarantee exactly-once external email.

Before enabling real sends, classify results explicitly:

- Definite no-send admission failure (wrong recipient/member/pins/allowlist,
  missing sender configuration) never emits mail; fixed status explains recovery.
- Definitive rate/in-progress responses retain the same immutable key and obey
  bounded backoff; an in-progress response is not proof of acceptance or failure.
- Unknown provider outcome, timeout after dispatch, malformed success or lost
  response enters an operator-visible bounded uncertain state. Do not blindly
  call the provider again under a new key or treat it as successful. Review the
  exact retry/reconciliation contract with Core; no receipt body/error leaks.
- Outbox publication success remains `published_outbox`, never provider acceptance.
  Changing mode must not resend completed outbox rows as real notifications.

No request for a new Core API is made by this plan. If the existing route cannot
provide enough outcome classification, first retain uncertain work for manual
reconciliation and disclose the limitation. A provider-specific idempotency
capability would be separate Core-owned work, not guessed by this App.

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
4. Run desktop/390px, light/dark, dedicated/wildcard/embedded browser matrix:
   current member reads/replies, other member/owner denied, invalid identity never
   falls back to cookie, OAuth state single-use/nonce/PKCE, bad Origin/Fetch
   Metadata refused, relative assets/API routes, text-only malicious content.
   Obtain deployed-header acceptance under a separate authorized fixture rollout.
5. Freeze exact SHA with App gate and Core/security review. Only then request
   one separate deployment/activation and real-send authorization. Evidence must
   include recipient/count, mode, acceptance status, retry/reconciliation result,
   member reply received by fixed Agent, negative scope checks and containment.

## Rollback and containment

Pause the exact binding, stop real dispatch through reviewed operator controls,
and preserve private SQLite/outbox snapshots under the usual 0700/0600 and
same-UID limitations. Do not acknowledge/erase ambiguous rows just to make the
queue empty. Rollback to the previous binary/config must not reinterpret completed
outbox receipts as unsent real work, reset keys, lose reply scopes or resend a
possibly accepted provider request. Prior to first real traffic a stopped-state
backup restore can be rehearsed with fakes; after traffic prefer containment and
reviewed reconciliation. No automatic post-send rollback is claimed.
