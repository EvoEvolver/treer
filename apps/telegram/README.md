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

`TREER_CLI` may override the `treer` executable. The config must explicitly
allow numeric Telegram user IDs and bind each chat/topic to a target Agent.
Inbound Messages are authored by the bridge Agent, not by a trusted Treer human
identity. Keep the bot token outside configuration and logs.

Back up `telegram-state.sqlite3` with its WAL files or SQLite's online backup
API. One active instance should own a bot token and state database. Ambiguous
Bot API failures can still produce an external duplicate; Core delivery is not
acknowledged until the App records every outbound mapping.
