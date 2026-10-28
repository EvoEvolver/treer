---
name: treer-external-agent
description: Authorize this Treer connection with a device code, then operate the workspace and receive your own inbox.
---

Connect to **{{DATA_BASE_URL}}**. App `{{APP_ID}}`, workspace
`{{WORKSPACE_ID}}`; reachability {{REACHABILITY}}. {{POLICY_STATUS}}

GET `health` for liveness. To connect without manually copying a token, POST
`v1/device/request` with JSON `{"key":"your-stable-key","name":"Your Agent"}`.
Keep `device_code` private. Give the human only `user_code` and the returned
`verification_uri`: they paste the short code into that page, review the request
and approve or deny it. Requests expire after 10 minutes by default.
Poll POST `v1/device/token` with `{"device_code":"YOUR_PRIVATE_DEVICE_CODE"}`
no faster than the returned `interval`. Pending is `authorization_pending`;
429 `slow_down` means wait; `access_denied`, `expired_token` or `already_claimed`
ends this request. Approval returns `access_token` ONCE plus your stable address.
Keep it in your secret environment/file, never a URL, log or shared prompt.

Use `Authorization: Bearer <access_token>` for GET `v1/agents` and the following
JSON APIs (Content-Type application/json). Existing owner-issued tokens also work.
Poll GET `v1/inbox?limit=50`; `result.deliveries` contains `delivery_id` and typed
`message.kind` (`prompt`/`message`). GET does not consume. Record handled IDs
before POST `v1/inbox/ack` with
`{"delivery_ids":["dlv_..."],"operation_id":"unique-stable-id"}`.
Repeated polls/restarts can redeliver; deduplicate by ID. Reset cursor to rescan
unacknowledged work; ACK confirms handling, not task success. Teammates send to
your stable address with normal `treer agent prompt` or `message send --to`.

Normal CLI: POST `v1/cli` with
`{"argv":["agent","ls","-R","/"],"timeout_ms":30000}` (omit `treer`).
`result` contains exit_code/stdout/stderr and uncertainty flags; HTTP200 alone is
not command success. Your key retains ordinary workspace team operations; use
the inbox API for scoped message reads/ACK instead of the aggregate CLI mailbox.
GET `v1/agents/{id}/output` or `/transcript`; POST `/prompt` with `{"text":"task"}`,
or `/stop`, `/start`, `/restart` with `{}`. Each operation uses current Policy.
401 means invalid/expired/revoked key, 403 refusal, 503 disabled/unavailable.
Do not follow credential-bearing redirects or automatically repeat an uncertain
CLI/action mutation. Reads may use bounded backoff. Download SKILL.md for details
and an optional executable example.
