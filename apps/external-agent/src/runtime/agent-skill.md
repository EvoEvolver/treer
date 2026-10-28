---
name: treer-external-agent
description: Authorize this Treer connection with a device code, then run Treer CLI commands and read your own inbox.
---

# External Agent connection

Data API: **{{DATA_BASE_URL}}** (not the owner page). App `{{APP_ID}}`,
workspace `{{WORKSPACE_ID}}`. Reachability: {{REACHABILITY}}.
{{POLICY_STATUS}}

Your token acts as this App's Treer Agent. It is not a human account and every
operation is checked against the App's current workspace Policy.

## 1. Get a token (device code)

1. POST `v1/device/request` (no credentials) with
   `{"key":"your-stable-key","name":"Your Agent","expires_in":600}`
   (`expires_in` optional, 60–1800 s). The response has `device_code`,
   `user_code`, `verification_uri`, `expires_in`, `interval` (5 s).
   Keep `device_code` private; show the human only `user_code` and
   `verification_uri`.
2. Poll POST `v1/device/token` with `{"device_code":"..."}` no faster than
   `interval`: 200 `authorization_pending`, 429 `slow_down`, 403
   `access_denied`, 410 `expired_token`, 409 `already_claimed`. Approval
   returns 200 `{status:"approved",access_token,token_type:"Bearer",expires_in,agent}`
   exactly once; a lost response needs a fresh request.

Owners and workspace Agents can also issue keys directly (see the App's root
manual), and a full App key can register agents with POST `v1/external-agents`
`{"key":"assistant1","name":"Assistant","days":30,"authority_confirmed":true}`.

## 2. Run Treer commands

POST `v1/cli` with `Authorization: Bearer <token>` and JSON
`{"argv":["agent","ls","-R","/"],"stdin":"","timeout_ms":30000}` (omit `treer`).
The result is `{exit_code,stdout,stderr,timed_out,truncated,outcome_uncertain}`;
a nonzero `exit_code` is the CLI's own failure (for example a Policy refusal).
Interactive or local-file modes (`agent attach`, `--follow`, `machine put/get`,
`service forward`, `network connect`, `--body-file PATH`) are refused with
`unsupported_cli_mode`; pass message bodies with `--body` or `--body-file -`
plus `stdin`. Limits: 64 KiB request, 256 argv items, 60 s, 2 MiB output.

## 3. Your inbox

Registered agent keys (including device-code tokens) have an inbox at the
stable address `{{APP_ID}}/your-stable-key`; teammates use
`treer message send --to {{APP_ID}}/your-stable-key --body "..."`.

- GET `v1/inbox?limit=50[&cursor=...]` returns `deliveries`,
  `remaining_unacknowledged` and optional `next_cursor`. GET never consumes.
- POST `v1/inbox/ack` `{"delivery_ids":["..."],"operation_id":"unique-stable-id"}`
  after durably recording the result. Repeating the same operation id and
  batch is idempotent.
- Reply with `v1/cli` `["message","send","--to","SENDER_ADDRESS","--body","..."]`.
  Agent keys cannot read the App's aggregate mailbox through the CLI
  (`use_scoped_inbox`).

## Errors

`{"error":{"code":...}}`: 401 `authentication_required`, 429
`capacity_unavailable` (back off), 503 `disabled` (ask the owner to enable the
App), 503 `inbox_unsupported` (Treer must be upgraded), 503 `cli_unavailable`,
502 `outcome_unknown`. Keep the `request_id` for the owner's audit. Never
automatically repeat a mutation with an uncertain outcome, and never send the
token through redirects, URLs or logs.

## Python example

Reads the token from a private 0600 file, checks health, then runs `agent ls`.

```sh
export TREER_EXTERNAL_TOKEN_FILE=/absolute/private/external-agent.token
python3 - <<'PY'
import json, os, stat, urllib.request, urllib.error
BASE = {{PYTHON_BASE_URL}}
fd = os.open(os.environ['TREER_EXTERNAL_TOKEN_FILE'], os.O_RDONLY | os.O_NOFOLLOW)
try:
    m = os.fstat(fd)
    if not stat.S_ISREG(m.st_mode) or stat.S_IMODE(m.st_mode) & 0o077:
        raise SystemExit('Use a private 0600 token file')
    token = os.read(fd, 513).decode('ascii').strip()
finally:
    os.close(fd)
class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *args, **kwargs):
        return None
opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), NoRedirect())
with opener.open(urllib.request.Request(BASE + 'health'), timeout=15) as health:
    if health.status != 200:
        raise SystemExit('Health unavailable')
request = urllib.request.Request(BASE + 'v1/cli', method='POST',
    data=json.dumps({'argv': ['agent', 'ls']}).encode(),
    headers={'Authorization': 'Bearer ' + token, 'Content-Type': 'application/json'})
try:
    with opener.open(request, timeout=60) as response:
        print(json.dumps(json.loads(response.read(4 * 1024 * 1024))['result']))
except urllib.error.HTTPError as error:
    raise SystemExit('HTTP ' + str(error.code) + '; not retried')
PY
```
