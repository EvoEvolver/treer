# Treer Telegram App

Telegram bridges the official Bot API to Core Message. Run it inside a
dedicated managed Agent: its nested `treer message` commands then use that
Agent's normal identity and Policy. Telegram admission, polling offsets,
chat/topic bindings, retries, and external ID mappings remain App-owned state.

Create a BotFather token and a JSON config matching `config.schema.json`, then
run from the managed Agent:

```sh
TREER_APP_CONFIG=/etc/treer/telegram.json \
TREER_APP_STATE_DIR=/var/lib/treer/apps/telegram \
TELEGRAM_BOT_TOKEN='...' \
python3 apps/telegram/telegram.py
```

`TREER_CLI` may override the `treer` executable. The configuration **seeds** the POC
registry (below): `bindings` (chat/topic → target Agent) and `allowed_user_ids` /
`users` (with labels) are inserted only when missing, and every later change made
in the chat or through Core control messages survives restarts and is never
overwritten by the file. At least one of `allowed_user_ids`/`users` is required;
`bindings` may be empty because a chat can be bound with `/poc`. Inbound Messages are authored by the bridge Agent, not by a trusted Treer human
identity. Keep the bot token outside configuration and logs.

Back up `telegram-state.sqlite3` with its WAL files or SQLite's online backup
API. One active instance should own a bot token and state database. Ambiguous
Bot API failures can still produce an external duplicate; Core delivery is not
acknowledged until the App records every outbound mapping.

## Local state isolation

Use a dedicated private state directory; startup tightens its permissions to
0700. Python stores also use a private umask and private database files. Do not
use a shared HOME/project directory as state. Backups remain plaintext and need
restrictive permissions/encryption. This does not isolate same-UID processes.
For critical services use a separate unprivileged user or container/VM with
minimal mounts and resource limits. Workspace ingress is not per-user data ACL
or comprehensive request-rate protection; retain this App's documented trusted
workspace assumptions and do not make it public.

## Point of contact (POC) routing

A **POC** is the Agent that receives the Messages from one chat/topic (the
*conversation*, `CHAT` or `CHAT:THREAD`, thread `0` = the main chat). Bindings,
admitted IM users, an audit trail and rate counters live in the SQLite state
(`poc_bindings`, `im_users`, `poc_audit`, `rate_events`), keyed by
(channel, account, conversation) so another IM adapter can reuse them. The
Telegram-specific delivery state is unchanged.

**From the chat** (admitted users only; commands are free of the rate limits):

| Command | Effect |
| --- | --- |
| `/poc` | Show this chat's POC and whether it is on |
| `/poc AGENT` | Set the POC; `AGENT` is a stable ID, tree path or sibling name, resolved with the bridge Agent's own Policy. In a chat with no binding this creates one |
| `/poc off` / `/poc on` | Pause/resume forwarding for this chat (messages get a notice, nothing is sent) |
| `/target`, `/status` | Show the POC / its `treer agent show` status |

**From Agents or people** through a Core control message to the bridge Agent.
There is deliberately **no human confirmation gate**: whoever can message the
bridge (Message Policy decides) may read and change POCs, including a POC handing
a conversation to another Agent. The verified Core sender is recorded in the
audit. Control messages are JSON objects with an `im` key, are never relayed to
the chat, and are answered with a Message back to the sender in the same context:

```sh
export IM_BRIDGE=/infra/telegram-bridge        # the bridge Agent (ID or tree path)
apps/telegram/im-poc get --conversation=-10042:9
apps/telegram/im-poc set --conversation=-10042:9 /team/builder --wake
apps/telegram/im-poc handoff /team/reviewer                   # I am the POC: pass everything I serve on
apps/telegram/im-poc handoff ag_x --conversation=-10042:9
apps/telegram/im-poc enable|disable --conversation=-10042:9
apps/telegram/im-poc list
apps/telegram/im-poc audit --limit 20
```

Negative chat IDs need the `--conversation=...` form. Raw ops: `poc.get`, `poc.list`,
`poc.set {conversation,to[,wake_agent]}`, `poc.handoff {to[,conversation]}` (the
sender must be the current POC), `poc.enable|disable {conversation}`,
`audit {limit}`, `users.list`, `users.set {user_id,label[,enabled]}`. Errors come
back as `{"ok":false,"error":{"code","message"}}` (`unknown_op`,
`invalid_conversation`, `target_unavailable`, `not_poc`, `no_binding`, `forbidden`…).
Redelivery is safe: the audit row is unique per Core delivery, so a change is
applied once and the result is re-sent under the same idempotency key. A relayed
reply whose whole body happens to be JSON with an `im` key is treated as control.

**Admitting IM users** is a bigger trust decision than choosing a POC, so
`users.set` (and `im-poc user-set`) works only for Agent IDs listed in
`control.user_admin_agent_ids` (default none); otherwise edit the config's
`users` and restart. Unknown or disabled users are refused.

## Limits and audit

Per admitted user `limits.per_user_per_hour` (default 60), per conversation
`per_conversation_per_minute` (20), and `max_unreplied` (50): inbound messages
since the POC last wrote to that conversation, so an absent POC is not flooded.
Only messages Core accepted are counted, so retries after an outage cannot
exhaust a limit; the sender gets a short notice and nothing reaches Core.
Every POC change (chat, control, handoff, failures of control ops) is appended to
`poc_audit` with actor (`telegram:ID(label)` or `agent:ID`/`human:ID`), channel,
old/new target and outcome; the newest 2000 rows are kept (`im-poc audit`).

The Message body is exactly what the user typed. Their label and numeric ID are in
`external_source.metadata` (`user_label`, `user_id`); text claiming another
identity is not checked, and the launcher does not render the source line, so
readers judge such claims themselves. `external_source` is App-provided, so
Treer's quoting of third-party text still applies.

## Deployment notes

Run the bridge inside a dedicated managed Agent (as above) so its nested
`treer message` commands use that Agent's identity; whether an App runtime
identity may `message receive` has not been verified. `/status` and `/poc AGENT`
use `treer agent show`, so the bridge needs Policy to read the Agents it routes to.
Slack and other adapters are not implemented; the registry and control protocol are
channel-agnostic, the Telegram delivery state and Bot API client are not yet.
