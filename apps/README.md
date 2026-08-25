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
- [`soul`](soul/README.md) is an experimental Agent-scoped file server that
  uploads environment-bound state bundles and launches command Agents from them.

Each App is source code, configuration schema, documentation, and tests. There
is deliberately no App manifest, package installer, local command broker, or
App-specific session capability.
