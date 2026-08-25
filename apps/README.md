# Workspace Apps

Apps are ordinary services started and supervised by an operator or a managed
Agent. Treer does not install, sandbox, or grant special trust to their code.

Browser-facing Apps use the standard App OAuth endpoints and a short-lived,
service-audience bearer token. Agent-facing Apps may use the local `treer` CLI
with the Agent's existing Policy subject. Core rechecks Policy for every
operation; process isolation, secrets, configuration, state, upgrades, and
network access remain deployment concerns.

- [`mail`](mail/README.md) is a browser App over App OAuth and Core Message.
- [`telegram`](telegram/README.md) is a Telegram bridge run by a managed Agent.
- [`pi-ui`](pi-ui/README.md) is an Agent-scoped browser interface loaded inside
  a Pi Agent.
- [`codex-ui`](codex-ui/README.md) is Treer's single-Agent browser interface for
  Codex.
- [`soul`](soul/README.md) is an experimental Agent-scoped file server that
  uploads environment-bound state bundles and launches command Agents from them.
- [`ais-kit`](ais-kit/README.md) is the shared Agent Interface helper library.
- [`codex-ais`](codex-ais/README.md), [`opencode-ais`](opencode-ais/README.md),
  [`dsh-ais`](dsh-ais/README.md), [`claude-ais`](claude-ais/README.md),
  [`grok-ais`](grok-ais/README.md), and [`cursor-ais`](cursor-ais/README.md) are
  per-Agent AIS sidecars over Codex app-server, OpenCode HTTP, DeepSeek Harness
  session APIs, Claude Code stream-json, and Grok Build / Cursor ACP. They
  register semantic capabilities without a bundled browser page.

Each App is source code, configuration schema, documentation, and tests. There
is deliberately no App manifest, package installer, local command broker, or
App-specific session capability.
