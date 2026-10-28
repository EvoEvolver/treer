Treer Mail
==========

This index is for Agents. Browsers and clients sending `Accept: text/html`
receive the human interface at this same URL.

Treer Mail lets an Agent email a person a link to a Core Message. The person
opens the link, signs in to Mail with Treer and reads and replies in the Core
thread. Mail does not receive email, track reads or send reminders.

Email a person
--------------

First send them a Core Message, so they have something to reply to, then ask
Mail to email them. From a managed workload `http://MAIL_NAME.treer/` injects
your identity (examples use an App named `mail`; elsewhere pass
`Authorization: Bearer $(treer token create SERVICE_ID)`):

    treer member list                                   # find USER_ID
    treer message send --to USER_ID --body 'Please review the draft.'   # note message_id
    curl -fsS -X POST -H 'Content-Type: application/json' \
      -d '{"to_member":"USER_ID","subject":"Review needed","text":"Please review the draft.","message_id":"MESSAGE_ID"}' \
      http://mail.treer/api/email

Fields: exactly one of `to_member` (a workspace member; Core uses their
verified account email) or `to` (any email address), `subject` (one line, at
most 200 bytes), `text` (plain text; with the appended link at most 4096
bytes), optional `message_id` and `idempotency_key`. The email is your text
plus `Read and reply: PUBLIC_URL/?message=MESSAGE_ID` (the inbox when no
`message_id`) and your name. Replying needs a Treer account in this workspace;
someone outside it can read the email but not reply through the link.

Responses:

- `200 {"email_id", "status": "sent", "idempotency_key"}`.
- `202 {"status": "uncertain", "code", "idempotency_key"}`: Core may have sent
  it. Do not send again; ask `GET http://mail.treer/api/email/IDEMPOTENCY_KEY`,
  which returns Core's `sent`, `failed` or `pending`, or `email_not_found`.
- Errors `{"error": {"code", "message", "retryable"}, "idempotency_key"}`, for
  example `email_member_not_found` (404), `email_member_unverified` (409),
  `email_rate_limited` (429), `email_unavailable` (503, this Treer has no App
  email route), `policy_denied` (403: the Mail runtime lacks `email.send`).

The same caller, recipient and `message_id` (or, without one, the same subject
and text) always use the same key, so repeating a request never emails twice.
Pass your own `idempotency_key` to choose differently; reusing it for other
content is `email_idempotency_conflict`.

Messages
--------

Read and send Core Messages with the treer CLI, not the browser API:

    treer message list --limit 50
    treer message get MESSAGE_ID
    treer message reply MESSAGE_ID --to sender --body "Reply text"
    treer message receive --wait 30000 --limit 50

A person's reply in Mail arrives as an ordinary Core Message.

The routes `/api/auth/*`, `/api/directory`, `/api/messages` and `/api/inbox`
serve the human UI and require its browser session. Do not scrape the human
interface or reuse its cookie.
