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

`TREER_CLI` may override the `treer` executable. The configuration **seeds and
reconciles** the POC registry (below): `bindings` (chat/topic → target Agent) and
`allowed_user_ids` / `users` (with labels) create the rows they describe, but every
change made at runtime (in the chat, by a control message, `users.set`) is
runtime-owned and is never overwritten by the file. See *Revoking access*. At least one of `allowed_user_ids`/`users` is required;
`bindings` may be empty because a chat can be bound with `/poc`. Inbound Messages are authored by the bridge Agent, not by a trusted Treer human
identity. Keep the bot token outside configuration and logs: put it in a private file and
name it with `"bot_token_file": "/absolute/path"` in the configuration (used when
`TELEGRAM_BOT_TOKEN` is not set). The file must be a regular file (not a symlink) owned by
the bridge's user with mode 0600, hold one ASCII token of at most 512 bytes, and its content
is never echoed in an error; the token then appears in neither argv nor the environment.

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
| `/poc AGENT` | Set the POC; `AGENT` is a stable ID (`ag_…`) or an absolute tree path (`/team/agent`), resolved with `treer agent show` under the bridge Agent's own Policy. In a chat with no binding this creates one |
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

**Admitting IM users** (`users.set`, `im-poc user-set`) is open to any sender the
bridge accepts by default, like POC changes, and is audited the same way. Admitting an
external person to talk to the POC is a wider trust decision than choosing a POC, so
`control.user_admin_agent_ids` can restrict it to a list of Agent IDs (an empty list:
nobody; then edit the config's `users` and restart). Unknown or disabled users are
refused either way.

## Limits, audit and revoking access

Per admitted user `limits.per_user_per_hour` (default 60), per conversation
`per_conversation_per_minute` (20), and `max_unreplied` (50): inbound messages
since the POC last wrote to that conversation, so an absent POC is not flooded.
Only messages Core accepted are counted, so retries after an outage cannot
exhaust a limit; the sender gets a short notice and nothing reaches Core.

**Audit.** Every *change* (`/poc`, `poc.set|handoff|enable|disable`, `users.set`,
and startup reconciliation) is appended to `poc_audit` with actor
(`telegram:ID(label)`, `agent:ID`, `human:ID` or `config`), via, old/new target and
outcome; the newest 5000 are kept (`im-poc audit`), and each change is also logged
as one line on stderr (`Treer IM poc change: …`), which is append-only in the App/Agent
log. Rejected or invalid control requests are **not** written there: they go to a
separate ring of 200 (`poc_rejects`), so cheap bad requests cannot push change
records out. Read-only ops are not audited.

**Change notices.** A POC change is not gated, but it is visible: the Agent that
lost the conversation and the one that gained it (never the actor) each get a
fixed short Message, `IM point of contact changed for conversation C: OLD -> NEW,
by ACTOR.`, so a hijack does not go unnoticed (a chat actor is named by its numeric
ID only, never its label). Creating a new binding or re-sending the same target
sends nothing, and each conversation/recipient pair is told at most once per five
minutes, so flipping a POC back and forth cannot spam either side.

**Revoking access.** Rows created from the configuration follow the file at every
start: delete a user from `allowed_user_ids`/`users`, or a binding from `bindings`,
and restart the bridge — the row is **disabled** and audited (`config.revoke_user`,
`config.revoke_binding`); putting it back re-enables it. Rows an operator or Agent
changed at runtime are runtime-owned and survive restarts, so revoke those with
`im-poc disable --conversation=…`, `im-poc user-set ID LABEL --disabled`, or
`/poc off` in the chat. Stopping the bridge Agent stops everything.

**Authorization boundary.** The bridge Agent's Message Policy *is* the
authorization for IM conversations: whoever may send it a Message can redirect any
conversation's inbound IM text to themselves and, as POC, write into that chat as
the bot. Keep that set minimal. `poc.list`, `audit` and `users.list` disclose
Telegram chat IDs and user labels and are as sensitive as changing a POC. Error
codes such as `target_unavailable` also reveal what the bridge can see.

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

## Running the bridge from a release directory

The bridge is a long-running process, not an App: run it as a managed Agent (kind `command`)
whose saved startup command points at an immutable export of `apps/telegram` at a full commit,
so the Agent never executes a mutable checkout:

```sh
SHA=<full 40-character commit on origin/main>
ROOT=/Users/dooms/apps-deployed/telegram-bridge
git -C /path/to/repo archive --prefix="releases/$SHA/" "$SHA:apps/telegram" | tar -x -C "$ROOT"
python3 -m unittest discover -s "$ROOT/releases/$SHA/tests" -p 'test_*.py'   # optional gate
ln -sfn "releases/$SHA" "$ROOT/current"
treer agent admin startup set /infra/telegram-bridge --cwd apps-deployed/telegram-bridge -- \
  env TREER_APP_CONFIG=/Users/dooms/.config/treer/telegram/config.json \
      TREER_APP_STATE_DIR=/Users/dooms/.local/share/treer/apps/telegram \
      /absolute/stable/python3 current/telegram.py
```

Upgrade by exporting the next commit, repointing `current` and `treer agent restart`; keep the
previous release for rollback (the SQLite state, including bindings and audit, lives in
`TREER_APP_STATE_DIR` and is independent of the release). Config example (the placeholder
user/binding `0` never matches a real Telegram ID; replace it with the real numeric user ID,
which is also the chat ID of that user's private chat, and restart. Removing an entry disables
the registry row it created):

```json
{
  "bot_token_file": "/Users/dooms/.config/treer/telegram/bot-token",
  "users": [{"id": 0, "label": "REPLACE-with-the-telegram-user-id"}],
  "bindings": [{"chat_id": 0, "target_agent_id": "ag_POC_AGENT_ID", "wake_agent": false}]
}
```

## Settings App (token, pairing and POCs in the browser)

`telegram-settings.py` is a small Managed App with a React page (`ui/` builds to the committed `dist/`, `npm --prefix
apps/telegram run check` verifies it byte for byte, exactly like Issues) for everything an operator would otherwise do in a
terminal. It runs next to the bridge Agent and shares its private state directory: the bridge keeps the Agent identity it needs
to send and receive Core Messages, the App provides Treer login. Nothing talks to Telegram's update stream except the bridge.

- **Who may open it.** Every route, including the read-only status, needs a short-lived Treer service token verified by the Proxy for
  this App's service and workspace (the page gets it from Treer's forwarded identity; there are no sessions, cookies or OAuth
  here). Allowed: human workspace `owner`/`admin`, and Agents listed in `settings.admin_agent_ids` of the configuration.
  Hiding a button is not authorization: members and other Agents get 403 from the API itself. Writes also require the
  `X-Telegram-Settings: 1` header (humans), a trusted `Origin` and same-site Fetch Metadata.
- **Bot token (write-only).** Paste the BotFather token once: it is checked with Telegram's `getMe` (fixed host, no redirects,
  short timeout) *before* anything is stored, written to `STATE_DIR/bot-token` (mode 0600, atomic, symlinks refused) and the
  input is cleared at once. The API and page only ever show the bot's username and the token's last four characters; the token
  is never returned, logged, put in an error (only exception types are logged) or sent to a log/argv/env. Replace and Remove
  work (Remove overwrites the file, stops the bridge and invalidates open pairing codes). Saving a token that belongs to a
  *different* bot than the one this bridge was set up with is refused. Attempts are limited to 5/minute per person and 20 per ten
  minutes overall, so the endpoint cannot be used to probe tokens. `TELEGRAM_BOT_TOKEN` and `bot_token_file` remain as fallbacks
  (order: environment, settings file, `bot_token_file`); with none of them the bridge waits (`needs_token`) and loads a token
  saved later within seconds, rebuilding itself when it changes.
- **Pairing.** *Generate pairing code* shows a one-time code once (10 symbols from a 30-letter alphabet, ~49 bits, ten minutes,
  salted hash stored, bound to its creator and a POC). In Telegram, in a **private chat** with the bot, send `/pair CODE`: that
  Telegram user is admitted and the chat is bound to the POC. The check and the spending happen in one transaction (no replay),
  a Telegram user is blocked after 5 failures in ten minutes (even with the right code) and 20 failures overall invalidate every
  open code. Group chats never consume a code. Success is audited (`pair.user`, `poc.pair`) and the POC and the person who
  created the code receive a fixed short Message.
- **Also on the page:** the conversations table (change a POC, the wake flag, pause/resume one conversation), the allowed users
  (add, relabel, disable), a bridge-wide pause (stops forwarding both ways; messages wait), bridge state with a heartbeat
  (`offline` after 30 s of silence) and the last 50 audited changes with their actors. Everything user-controlled is rendered as
  text. There is deliberately no "export configuration".

Configuration needs one extra block, read only by the settings App (the bridge ignores it); the Treer Proxy URL is a start-up
argument because an App's saved command cannot carry environment variables:

```json
{ "settings": { "admin_agent_ids": [], "default_poc": "ag_POC_AGENT_ID" } }
```

```sh
treer app create --machine <server-id> --name telegram-settings --folder /infra --cwd apps-deployed/telegram-bridge \
  --port 9441 env -- TREER_APP_CONFIG=/Users/dooms/.config/treer/telegram/config.json \
  TREER_APP_STATE_DIR=/Users/dooms/.local/share/treer/apps/telegram \
  /absolute/stable/python3 current/telegram-settings.py --proxy-url https://PROXY.example/
```

Do not use `--public`. The page is the only place a token is entered; give the App and the bridge the same OS user and
state directory (mode 0700). Whoever can administer that user can read the token file - the usual limit of a same-user App.
