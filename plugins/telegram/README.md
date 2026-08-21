# Treer Telegram plugin

The Telegram plugin bridges the official Bot API to Core Message. It is a
standard-library Python script: its only Treer interface is nested `treer`
commands through the plugin broker. Telegram polling, allowlists, chat/topic
bindings, retry state, and Telegram-to-Core IDs remain plugin-owned; canonical
Message bodies, recipients, ordered DAG contexts, visibility, policy, and
acknowledgements remain Core-owned.

## Bot and Telegram setup

1. Create a bot with BotFather and keep its token outside the repository.
2. Record stable numeric Telegram user and chat IDs. Usernames and display names
   are not authorization keys.
3. For a group, decide whether BotFather privacy mode should stay enabled. With
   privacy mode enabled, Telegram sends commands, replies to the bot, and other
   explicitly addressed messages; disable it only if the bridge must receive
   every group message.
4. Add the bot to configured groups. It needs permission to read the selected
   updates and send messages/replies. It does not need administrator permission
   for ordinary text bridging.
5. Record `message_thread_id` for each forum topic. A chat without a topic uses
   `null` or omits the field.

Version 1 uses `getUpdates` long polling. It needs outbound HTTPS access but no
public webhook, inbound machine port, or Telegram-specific Core route.

## Treer setup

Run the plugin from a dedicated managed bridge Agent. Inbound Telegram text is
authored in Core by that bridge Agent, with sender-asserted external source
metadata containing numeric bot, update, user, conversation, and Telegram
Message IDs. It does not impersonate the Telegram user as a Treer human.

Validate and install the package:

```sh
treer plugin validate plugins/telegram
treer plugin install plugins/telegram
```

Create a JSON configuration using stable target Agent IDs:

```json
{
  "allowed_user_ids": [123456789],
  "bindings": [
    {
      "chat_id": 123456789,
      "target_agent_id": "agent_private_target",
      "wake_agent": false
    },
    {
      "chat_id": -1001234567890,
      "message_thread_id": 42,
      "target_agent_id": "agent_research_target",
      "wake_agent": true
    }
  ],
  "poll_timeout_seconds": 25,
  "receive_wait_milliseconds": 10000,
  "batch_size": 20,
  "retry_initial_seconds": 1,
  "retry_max_seconds": 60,
  "ambiguous_retry_seconds": 5,
  "respond_to_denied": true
}
```

Start it from the bridge Agent with the token in the declared secret variable:

```sh
TELEGRAM_BOT_TOKEN='<BotFather token>' \
  TREER_ENABLE_PLUGIN_EXECUTION=true \
  treer plugin run telegram --config /etc/treer/telegram.json
```

The Proxy must also enable `TREER_ENABLE_CORE_MESSAGES=true`; Telegram does not
use plugin OAuth sessions, but production releases enable the shared
`TREER_ENABLE_PLUGIN_SESSIONS=true` gate for Mail. All rollout gates default
off.

The runner passes the token only to this plugin process. Do not put it in
`plugin.json`, the config file, logs, shell history, or a service definition
visible to other users. Use the process supervisor's protected secret mechanism
for a persistent deployment.

## Message and reply mapping

For an allowed normal text update, the plugin:

1. resolves the exact `(chat_id, message_thread_id)` binding;
2. maps a native `reply_to_message` to a Core parent when that Telegram Message
   has a mapping in the same chat/topic;
3. calls `treer message send` with a bot/update-scoped idempotency key and the
   body on stdin;
4. commits the update offset and Telegram/Core mapping in one SQLite transaction;
5. optionally prompts the target Agent with only the new Core Message ID.

When the bridge receives a Core delivery, it finds the first context with a
Telegram mapping, sends to that chat/topic with Telegram `reply_parameters`,
commits the returned Telegram Message ID, and only then calls
`treer message ack`.

```text
Telegram #100 -> Core M1
Core M2(context=[M1]) -> Telegram #101(reply_to=#100)
Telegram #102(reply_to=#101) -> Core M3(context=[M2])
```

Core may have several parents while Telegram displays one native reply edge.
The first mapped parent chooses the native reply. The plugin states only the
number of additional contexts; it never copies a parent body or cross-chat
context ID into Telegram. Text over Telegram's 4096 UTF-16-unit bound is split,
with later chunks replying to the preceding chunk. A reply to any chunk maps
back to the same Core Message.

`/start` and `/help` report the configured bridge target, `/target` shows its
stable ID, and `/status` reads target metadata when policy allows it. These
control replies are not Core Messages.

## Reliability model

The plugin database under `TREER_PLUGIN_STATE_DIR` stores:

- the last confirmed `update_id`;
- processed update and optional wake state;
- Telegram Message ID to Core Message ID mappings;
- outbound delivery/chunk hashes, attempts, Telegram IDs, and errors.

It does not store canonical Message bodies. Until Core ack succeeds, Core
repeats the delivery and the plugin recomputes each chunk and checks its stored
hash. Inbound retries reuse one Core idempotency key, so a crash after Core send
but before mapping does not duplicate the Core Message. A crash after Telegram
mapping but before Core ack retries only the ack.

Telegram `sendMessage` has no client idempotency key. If Telegram accepts a send
but its response is lost, the plugin records an ambiguous attempt, waits
`ambiguous_retry_seconds`, and retries. That can create a visible duplicate.
This limitation is surfaced in SQLite and is not described as exactly-once
delivery. HTTP 429 honors `retry_after`; temporary failures use bounded
exponential backoff. Definitive Bot API rejection leaves the Core delivery
unacknowledged with an error for operator repair.

Back up the plugin state database with its WAL files or a SQLite online backup
before moving the bridge. Restoring an old mapping database can duplicate
external sends even though Core Messages remain intact.

## Security and current limits

- Numeric Telegram user IDs plus exact chat/topic bindings are the channel
  authorization boundary. Telegram account compromise is outside Treer.
- Policy still authorizes every Message, Agent metadata read, and optional
  prompt. A manifest capability never overrides Policy.
- The plugin supports text, configured private chats/groups/topics, replies,
  `/start`, `/help`, `/target`, and `/status`.
- Attachments, edits, deletes, reactions, polls, voice, payments, Mini Apps,
  webhooks, active-active polling, and Telegram-user human impersonation are
  deferred.
- Core Message retention/export/deletion policy and billing are outside the
  plugin contract.
- The bot token and Message bodies must not appear in logs. The broker withholds
  Treer credentials but is not a hostile same-UID sandbox.
