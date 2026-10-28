---
name: treer-external-agent
description: Authorize this Treer connection with a device code, then operate the workspace and receive your own inbox.
---

Connect to **{{DATA_BASE_URL}}** (data API, not the owner UI).
App `{{APP_ID}}`, workspace `{{WORKSPACE_ID}}`. Reachability: {{REACHABILITY}}.
{{POLICY_STATUS}}

1. GET `{{DATA_BASE_URL}}health` without credentials for liveness. POST
   `{{DATA_BASE_URL}}v1/device/request` with JSON
   `{"key":"your-stable-key","name":"Your Agent","expires_in":600}`.
   Keep the high-entropy `device_code` private; give the human ONLY the returned
   short `user_code` and `verification_uri`. They paste that short code in the
   workspace-only owner page, inspect the requested identity/authority, choose
   token expiry and approve or deny. `expires_in` is optional, default600,
   range60–1800 seconds; `interval` is5 seconds.
2. Poll POST `{{DATA_BASE_URL}}v1/device/token` with
   `{"device_code":"YOUR_PRIVATE_DEVICE_CODE"}` at the returned interval.
   No Bearer is required for these two endpoints. Responses are standalone JSON:
   pending200 `authorization_pending`, too-fast429 `slow_down`, denied403
   `access_denied`, expired410 `expired_token`, already-claimed409
   `already_claimed`. Stop on terminal status. Approved200 returns
   `access_token`, `token_type:"Bearer"`, token `expires_in` and stable `agent`
   address exactly once. Persist the key privately before using it. A lost claim
   response cannot recover or mint another key; ask the human to revoke the lost
   key and authorize a fresh request. No token copying from the human is needed.
   Existing owner-issued keys remain compatible. With a key GET
   `{{DATA_BASE_URL}}v1/agents` using `Authorization: Bearer <APP_TOKEN>`.
   Health is not authentication or Policy acceptance; disabled503 means ask the
   owner to enable the App.
3. Use returned IDs: GET `v1/agents/{id}` for status, `/output` or `/transcript`
   for requested output. Optional GET JSON bodies: output `{"lines":100}`;
   transcript `{"page":0,"limit":10}`. No query strings.
4. For requested actions allowed by this App's Policy: POST
   `v1/agents/{id}/prompt` with `{"text":"the requested task"}`; POST
   `/stop`, `/start`, `/restart` with `{}`. JSON uses Content-Type application/json.
   Prompt admission is not completion.

Normal Treer commands: POST `{{DATA_BASE_URL}}v1/cli` with the same Bearer
header and JSON `{"argv":["agent","ls","-R","/"],"timeout_ms":30000}`.
Use normal CLI argv (omit `treer`), e.g. `["message","send","--to","TARGET",
"--body","the requested message"]`. Optional `stdin` is a UTF-8 string.
The result contains `exit_code`, `stdout`, `stderr`, `timed_out`, `truncated`,
and `outcome_uncertain`; HTTP 200 alone does not mean command success.
The gateway runs the real CLI as this App with ordinary workspace permissions,
not a shell. Commands with PTY, local file transfer, forwarding or follow are
`unsupported_cli_mode`; normal finite commands retain their CLI semantics.
Keep requests under 64 KiB, argv under 32 KiB/256 items, timeout at most 60s,
and output under 2 MiB. On timeout/truncation/uncertainty, inspect state before
any further mutation; never automatically repeat the command.

For your own durable inbox, obtain an agent key and stable `AppID/key` address
from the owner (a full App key can POST `v1/external-agents` with
`{"key":"assistant1","name":"Assistant","days":30,"authority_confirmed":true}`).
Teammates can `treer agent prompt AppID/key "task"` or
`treer message send --to AppID/key --body "message"`.
Poll `GET v1/inbox?limit=50` with your agent key; the usual `result.deliveries`
contains typed `message.kind` (`prompt`/`message`) and `delivery_id`.
GET never consumes. Record handled delivery IDs durably, then POST
`v1/inbox/ack` with `{"delivery_ids":["dlv_..."],"operation_id":"unique-stable-id"}`.
ACK is receipt handling, not task completion. Repeated GET/restart may redeliver;
reset cursor to rescan unacknowledged work. Use optional URL-encoded `cursor`
from `result.next_cursor` only for pagination. Your key retains team operations,
but aggregate gateway message reads/ACK use the scoped inbox API instead.
Reply with normal `message send` to the received sender's stable address.

The token represents this App, not a human; each operation uses current Policy.
401 means expired/revoked/missing token; 403 means Policy refusal; 503 may mean
 disabled/unavailable. Preserve request_id for owner audit. For temporary read
failures, use bounded backoff (honor 429). Never automatically resend a mutation
with an uncertain result; inspect audit/target state first. Do not forward Bearer
credentials across redirects. Download SKILL.md for an optional executable example.

## Optional Python private-file discovery example

This example uses a 0600 file and Python; a supported environment secret or
credential store is equally valid. It makes one health call and one discovery
call without redirecting credentials.

```sh
export TREER_EXTERNAL_TOKEN_FILE=/absolute/private/external-agent.token
python3 - <<'PY'
import json, os, stat, urllib.request, urllib.error
BASE = {{PYTHON_BASE_URL}}
fd = os.open(os.environ['TREER_EXTERNAL_TOKEN_FILE'], os.O_RDONLY | os.O_NOFOLLOW)
try:
    m = os.fstat(fd)
    if (not stat.S_ISREG(m.st_mode) or m.st_uid != os.getuid()
            or stat.S_IMODE(m.st_mode) != 0o600 or m.st_nlink != 1 or m.st_size > 512):
        raise SystemExit('Use a private, regular, owner-only 0600 token file')
    token = os.read(fd, 513).decode('ascii').strip()
finally:
    os.close(fd)
if (len(token) != 102 or not token.startswith('ext1_') or token[37] != '.'
        or any(c not in '0123456789abcdef' for c in token[5:37] + token[38:])):
    raise SystemExit('Invalid token format; request a fresh owner-issued token')
class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *args, **kwargs):
        return None
opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), NoRedirect())
# Liveness first, without credentials; this does not establish readiness.
try:
    with opener.open(urllib.request.Request(BASE + 'health'), timeout=15) as health:
        if health.status != 200 or len(health.read(65537)) > 65536:
            raise SystemExit('Health unavailable')
except (urllib.error.URLError, TimeoutError):
    raise SystemExit('Health unavailable; check the data endpoint')
request = urllib.request.Request(BASE + 'v1/agents', headers={
    'Authorization': 'Bearer ' + token, 'Accept': 'application/json'})
try:
    with opener.open(request, timeout=15) as response:
        payload = response.read(1048577)
        if len(payload) > 1048576:
            raise SystemExit('Response too large; stop without retry')
        result = json.loads(payload)
        print(json.dumps(result))
except urllib.error.HTTPError as error:
    raise SystemExit('HTTP ' + str(error.code) + ': stop and consult error guidance; no retry')
except (urllib.error.URLError, TimeoutError):
    raise SystemExit('Endpoint unavailable; confirm the data route with the owner; no retry')
PY
```
