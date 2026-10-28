---
name: treer-external-agent
description: Authorize this Treer connection with a device code, then run Treer CLI commands and read your own inbox.
---

Connect to **{{DATA_BASE_URL}}**. App `{{APP_ID}}`, workspace
`{{WORKSPACE_ID}}`; reachability {{REACHABILITY}}. {{POLICY_STATUS}}

1. POST `v1/device/request` with `{"key":"your-stable-key","name":"Your Agent"}`.
   Keep `device_code` private. Give the human only `user_code` and
   `verification_uri`; they approve it on the owner page.
2. Poll POST `v1/device/token` with `{"device_code":"..."}` every `interval`
   seconds until `approved` (returns `access_token` once) or a terminal status
   (`access_denied`, `expired_token`, `already_claimed`). Keep the token secret.
3. With `Authorization: Bearer <access_token>`:
   - POST `v1/cli` `{"argv":["agent","ls","-R","/"]}` runs the real `treer` CLI
     as this App. Check `result.exit_code`; HTTP 200 alone is not success.
   - GET `v1/inbox?limit=50` lists your deliveries (never consumes them).
     POST `v1/inbox/ack` `{"delivery_ids":["..."],"operation_id":"unique-id"}`
     after handling. Deduplicate by `delivery_id`; redelivery is normal.
   Teammates reach you with `treer message send --to {{APP_ID}}/your-stable-key`.

401 = bad/expired/revoked token, 503 `disabled` = ask the owner to enable the App,
503 `inbox_unsupported` = Treer needs an upgrade. Never automatically repeat a
command whose outcome was uncertain. SKILL.md has the full reference.
