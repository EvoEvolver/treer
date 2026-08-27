# AIS kit

Shared `treer.agent-interface/v1` helpers for Treer Agent Interface
adapters. This directory is a library, not a launchable App.

It owns turn paging, prompt `operation_id` deduplication, loopback HTTP
serving, Treer interface registration, and OpenAI-compatible provider
fallback used by live adapter tests.

Adapters in this repository:

- [`../pi-ui`](../pi-ui/README.md) — Pi in-process session
- [`../codex-ais`](../codex-ais/README.md) — Codex app-server
- [`../opencode-ais`](../opencode-ais/README.md) — OpenCode HTTP
- [`../dsh-ais`](../dsh-ais/README.md) — DeepSeek Harness session API
- [`../claude-ais`](../claude-ais/README.md) — Claude Code stream-json session
- [`../grok-ais`](../grok-ais/README.md) — Grok Build ACP (`grok agent stdio`)
- [`../cursor-ais`](../cursor-ais/README.md) — Cursor ACP (`cursor-agent acp`)
