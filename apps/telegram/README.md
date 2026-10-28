# Treer Telegram App

Telegram bridges the official Bot API to Core Message. It has two processes that
share one private state directory:

- `telegram.py`, the **bridge**, runs as a dedicated managed `command` Agent, so its
  nested `treer message` calls use that Agent's identity and Policy. It is the only
  process that polls Telegram.
- `telegram-settings.py`, the **settings App**, is a Managed App with a React page
  (token, pairing, POCs, users, audit) for operators and admin Agents.

Telegram admission, polling offsets, chat/topic bindings, retries and external ID
mappings are App-owned SQLite state (`telegram-state.sqlite3` in
`TREER_APP_STATE_DIR`). Back it up with its WAL files or SQLite's online backup API.
Only one instance may use a given bot token and state database. An ambiguous Bot API
failure can still cause a duplicate on the Telegram side. Core deliveries are
acknowledged only after every outbound mapping is recorded.

## Configuration and running

The configuration matches `config.schema.json`. It **seeds** the registry:
`users` / `allowed_user_ids` (at least one is required) and `bindings`
(chat/topic → target Agent) create rows. Changes made at runtime always win over
the file. Example (replace the placeholder `0` with the real numeric Telegram user
ID, which is also that user's private chat ID):

```json
{
  "bot_token_file": "/Users/me/.config/treer/telegram/bot-token",
  "users": [{"id": 0, "label": "alice"}],
  "bindings": [{"chat_id": 0, "target_agent_id": "ag_POC_AGENT_ID", "wake_agent": false}],
  "settings": {"admin_agent_ids": [], "default_poc": "ag_POC_AGENT_ID"}
}
```

The bridge looks for the bot token in this order: `TELEGRAM_BOT_TOKEN`, the token
saved by the settings App (`STATE_DIR/bot-token`), then `bot_token_file`. The file
must be a regular 0600 file owned by the bridge user. If there is no token, the
bridge waits (`needs_token`) and picks one up within seconds of it being saved.
Tokens never appear in argv, logs or errors. Inbound text and its source metadata
reach Core on stdin (`treer message send --stdin-envelope`). `TREER_CLI` overrides
the `treer` executable.

Deploy from an immutable export of a full commit, not from a mutable checkout:

```sh
SHA=<full commit on origin/main>
ROOT=$HOME/apps-deployed/telegram-bridge
git -C /path/to/repo archive --prefix="releases/$SHA/" "$SHA:apps/telegram" | tar -x -C "$ROOT"
ln -sfn "releases/$SHA" "$ROOT/current"
treer agent admin startup set /infra/telegram-bridge --cwd apps-deployed/telegram-bridge -- \
  env TREER_APP_CONFIG=$HOME/.config/treer/telegram/config.json \
      TREER_APP_STATE_DIR=$HOME/.local/share/treer/apps/telegram \
      /absolute/python3 current/telegram.py
treer app create --machine <server-id> --name telegram-settings --folder /infra \
  --cwd apps-deployed/telegram-bridge --port 9441 env -- \
  TREER_APP_CONFIG=$HOME/.config/treer/telegram/config.json \
  TREER_APP_STATE_DIR=$HOME/.local/share/treer/apps/telegram \
  /absolute/python3 current/telegram-settings.py --proxy-url https://PROXY.example/
```

To upgrade, export the next commit, repoint `current`, then restart both processes.
State lives outside the release, so rolling back means repointing `current`. Do not
create the settings App with `--public`.

The state directory is tightened to 0700, and the databases use a private umask.
Same-UID processes can still read the token and state. For critical deployments,
use a separate OS user or container with minimal mounts.

## Point of contact (POC) routing

A **POC** is the Agent that receives the Messages from one *conversation*: `CHAT`
or `CHAT:THREAD`, where thread `0` is the main chat. Bindings, admitted users, the
audit trail and rate counters are stored in `poc_bindings`, `im_users`, `poc_audit`
and `rate_events`. They are keyed by (channel, account, conversation), so other
adapters could reuse them.

**In the chat** (admitted users only):

| Command | Effect |
| --- | --- |
| `/poc` | Show the POC and whether forwarding is on |
| `/poc AGENT` | Set the POC by `ag_…` ID or absolute tree path. This creates a binding if the chat has none |
| `/poc off` / `/poc on` | Pause or resume forwarding for this chat |
| `/target`, `/status` | Show the POC, or its `treer agent show` status |
| `/pair CODE` | In a private chat, redeem a settings-App pairing code |

**From Agents or people**, send a Core Message to the bridge Agent whose whole body
is a JSON object with an `im` key. These messages are not relayed to the chat. The
reply is a Message to the sender in the same context:

```sh
echo '{"im":"poc.set","conversation":"-10042:9","to":"/team/builder","wake_agent":true}' |
  treer message send --to /infra/telegram-bridge --idempotency-key poc-$(uuidgen) --body-file -
```

| Op | Fields |
| --- | --- |
| `poc.get`, `poc.enable`, `poc.disable` | `conversation` |
| `poc.set` | `conversation`, `to`, optional `wake_agent` |
| `poc.handoff` | `to`, optional `conversation`. The sender must be the current POC; with no `conversation`, everything it serves is handed over |
| `poc.list`, `users.list` | none |
| `audit` | `limit` |
| `users.set` | `user_id`, `label`, optional `enabled` |

Errors come back as `{"ok":false,"error":{"code","message"}}`, with codes such as
`unknown_op`, `invalid_conversation`, `target_unavailable`, `not_poc`,
`no_binding` and `forbidden`. Redelivery is safe: there is one audit row per Core
delivery, so a change applies once. A relayed reply whose whole body is JSON with
an `im` key is treated as control.

`users.set` is closed by default. `control.user_admin_agent_ids` is either a list
of the Agent IDs that may admit users, or `"any"`. Admitting an external person is
a wider trust decision than choosing a POC.

**Authorization boundary.** The bridge Agent's Message Policy *is* the
authorization. Anyone who may message the bridge can redirect any conversation to
themselves and, as POC, write into that chat as the bot. Keep that set minimal.
`poc.list`, `audit` and `users.list` disclose chat IDs and user labels. `/status`
and `/poc AGENT` use `treer agent show`, so the bridge needs Policy to read the
Agents it routes to.

The Message body is exactly what the user typed. The user's label and numeric ID
are in `external_source.metadata` (`user_label`, `user_id`). That metadata is
App-provided, not a verified Treer identity.

## Limits, audit and revocation

Each admitted user has `limits.per_user_per_hour` (default 60). Each conversation
has `per_conversation_per_minute` (default 20) and `max_unreplied` (default 50),
which counts inbound messages since the POC last replied. Only messages that Core
accepted count toward these limits. Chat commands are exempt.

Every change is appended to `poc_audit` (the newest 5000 are kept) and logged to
stderr as `Treer IM poc change: …`. This covers `/poc`, control ops, `users.set`,
pairing and config reconciliation. Rejected requests go to a separate ring of 200
(`poc_rejects`), so bad requests cannot push real changes out. When a POC changes,
the Agents that lost and gained the conversation each receive a fixed notice, at
most once per pair every five minutes.

**Revoking access.** Rows created from the configuration follow the file at every
start. Remove an entry and restart, and the row is disabled and audited. Rows
changed at runtime survive restarts; revoke them with `poc.disable`,
`users.set … enabled:false`, `/poc off` or the settings page. Stopping the bridge
Agent stops everything.

## Settings App

Every route needs a short-lived Treer service token. The page gets it from Treer's
forwarded identity, and the Proxy verifies it for this service and workspace.
There are no cookies, sessions or OAuth. Workspace `owner`/`admin` humans and the
Agents in `settings.admin_agent_ids` are allowed; everyone else gets 403. Writes
also require `X-Telegram-Settings: 1` (humans), a trusted `Origin` and same-site
Fetch Metadata.

- **Bot token (write-only).** Telegram's `getMe` checks the token before it is
  stored. It is then written atomically to `STATE_DIR/bot-token` (mode 0600). Only
  the bot username and the token's last four characters are ever shown. A token for
  a different bot than the bridge was set up with is refused. Attempts are limited
  to 5 per minute per person and 20 per ten minutes overall. Removing the token
  stops the bridge and invalidates open pairing codes.
- **Pairing.** A one-time code (about 49 bits, valid ten minutes, stored hashed) is
  bound to its creator and a POC. Sending `/pair CODE` in a private chat admits that
  Telegram user and binds the chat. A user is blocked after 5 failures in ten
  minutes, and 20 failures overall invalidate all open codes.
- **Also on the page:** conversations (POC, wake flag, pause), allowed users,
  a bridge-wide pause, bridge heartbeat (shown as `offline` after 30 s) and the last
  50 audit entries.

The UI source is in `ui/` and builds to the committed `dist/`, so no Node is needed
at runtime. After changing `ui/`, run `npm --prefix apps/telegram run build:ui`.
`npm --prefix apps/telegram run check` verifies `dist/` byte for byte.

## Tests

```sh
python3 -m unittest discover -s apps/telegram/tests -p 'test_*.py'
npm --prefix apps/telegram run check
```

Slack and other adapters are not implemented.
