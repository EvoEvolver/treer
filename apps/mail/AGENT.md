Treer Mail
==========

This index is for Agents. Browsers and clients sending `Accept: text/html`
receive the human interface at this same URL.

Treer Mail is a human inbox and composer backed by Core Message. Agents should
use the authenticated treer CLI instead of the browser API or browser session.

Read and send Messages
----------------------

    treer message list --limit 50
    treer message get MESSAGE_ID
    treer message send --to RECIPIENT --body "Message text"
    treer message reply MESSAGE_ID --to sender --body "Reply text"
    treer message receive --wait 30000 --limit 50
    treer message ack DELIVERY_ID --operation-id STABLE_OPERATION_ID

Use `treer member list` and `treer agent ls -R /` to discover recipients. Prefer a
durable Message for Agent coordination. A Message does not wake or type into a
peer terminal; use `treer agent prompt` only when immediate attention is needed.

Email notifications
-------------------

Mail can email a member when a Message stays unread. Agents operate it with
their own workload identity; from a managed workload `http://MAIL_NAME.treer/`
injects it (examples use an App named `mail`; or pass `Authorization: Bearer $(treer token create SERVICE_ID)`).
Every route except `members` needs `?user_id=HUMAN_ID` (from
`treer member list`).

Inspect:

    curl -fsS http://mail.treer/api/notifications/members
    curl -fsS 'http://mail.treer/api/notifications?user_id=USER_ID'

The status has `available` (false when the Core email route is absent),
`settings`, `watch.state` (`unavailable`, `disabled`, `no_address`,
`login_required`, `watching`), `pending_messages` and the last 20 `batches`
with every attempt (`sent`, `retryable`, `permanent`, `uncertain` and a code).

Change, test, retry:

    curl -fsS -X PATCH -H 'Content-Type: application/json' \
      -d '{"enabled":true,"digest_minutes":15,"scope":"direct"}' \
      'http://mail.treer/api/notifications/settings?user_id=USER_ID'
    curl -fsS -X POST -H 'Content-Type: application/json' -H 'Idempotency-Key: KEY' \
      -d '{}' 'http://mail.treer/api/notifications/test?user_id=USER_ID'
    curl -fsS -X POST -H 'Content-Type: application/json' -d '{}' \
      'http://mail.treer/api/notifications/batches/BATCH_ID/retry?user_id=USER_ID'

Settings fields: `enabled` (bool), `digest_minutes` (0 = immediately, up to
1440 = one digest after that many unread minutes), `scope` (`direct` = only
Messages sent to the member alone, `all`), `address` (email or null = the
member's account email when the directory exposes one). Agents may only set
`address` to null; members set their own address in the UI. A test email goes
to the effective address; the same `Idempotency-Key` never sends twice.
Retrying an `uncertain` batch may deliver a duplicate: Mail resends on its own
only after Core confirms the first attempt was not sent. `login_required` means
the member must sign in to Mail itself (`/api/auth/start`); Mail watches an
inbox only with that member's 12-hour Mail sign-in grant. Emails contain the
sender, a short first-line preview and links that need a Treer sign-in.

Browser API
-----------

The routes below are for the human UI and require an App OAuth browser session:

    GET  /api/auth/start
    GET  /api/auth/callback
    GET  /api/auth/session
    POST /api/auth/logout
    GET  /api/directory
    GET  /api/messages
    POST /api/messages
    POST /api/inbox
    GET   /api/notifications
    PATCH /api/notifications/settings
    POST  /api/notifications/test
    POST  /api/notifications/batches/BATCH_ID/retry

Do not scrape the human interface or attempt to reuse its cookie. Core applies
the calling Agent's workspace Policy to every treer CLI operation.
