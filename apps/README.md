# Workspace Apps

Treer supports two deployment forms. A Managed App is a single HTTP process
created with `treer app create`; Treer persists its command, machine, service,
and virtual host, and restores the process after exit or Controller reconnect.
With wildcard ingress configured, Treer also owns a stable,
workspace-authenticated `public_url` for the App by default. An Agent may pass
`treer app create --public` to make only that Managed App's declared HTTP port
an anonymously accessible public origin; the App is then responsible for any
application-level authentication.
The human UI manages existing Apps but does not create them. Open an App's
settings to switch its dedicated origin between Workspace authentication and
anonymous public access. Agents create Apps with `treer app create`.
An externally managed App is started by an operator or another supervisor; a
logged-in workspace user then registers its ordinary service through the
control plane. Managed Agents cannot register service, virtual-host, or ingress
records directly. Neither form grants special trust to App code.

Browser-facing Apps use the standard App OAuth endpoints and a short-lived,
service-audience bearer token. Agent-facing Apps may use the local `treer` CLI
with the Agent's existing Policy subject. Core rechecks Policy for every
operation; process isolation, secrets, configuration, state, upgrades, and
network access remain deployment concerns.

Every standalone App follows the [App guidelines](GUIDELINES.md): `/` negotiates
between an Agent-readable GitHub Flavored Markdown manual and the human HTML
interface, while data pages remain JSON. Agent Interfaces such as Codex UI and
Pi UI are embedded AIS surfaces rather than standalone App indexes and continue
to expose their registered `ui_path`. The generic ACP thread UI is not an
in-tree App; install it once on the Host with `treer ui install`.

- [`mail`](mail/README.md) is a browser App over App OAuth and Core Message.
- [`telegram`](telegram/README.md) is a Telegram bridge run by a managed Agent.
- [`pi-ui`](pi-ui/README.md) is an Agent-scoped browser interface loaded inside
  a Pi Agent.
- [`codex-ui`](codex-ui/README.md) is Treer's single-Agent browser interface for
  Codex.
- [`soul`](soul/README.md) is an experimental Managed App file server that
  uploads environment-bound state bundles and launches command Agents from them.
- [`gits`](gits/README.md) is a small workspace-local Git Smart HTTP host for
  repositories shared by Agents and humans.
- [`paper`](paper/README.md) is a small filesystem-backed collaborative LaTeX
  editor with Yjs, inline review macros, and server-side PDF compilation.
- [`ais-kit`](ais-kit/README.md) is the shared Agent Interface helper library.
- [`codex-ais`](codex-ais/README.md), [`opencode-ais`](opencode-ais/README.md),
  [`dsh-ais`](dsh-ais/README.md), [`claude-ais`](claude-ais/README.md),
  [`grok-ais`](grok-ais/README.md), and [`cursor-ais`](cursor-ais/README.md) are
  per-Agent AIS sidecars over Codex app-server, OpenCode HTTP, DeepSeek Harness
  session APIs, Claude Code stream-json, and Grok Build / Cursor ACP. They
  register semantic capabilities without a bundled browser page.

Each bundled App is source code, configuration schema, documentation, and
tests. Managed Apps currently accept one command and one HTTP UI port; there is
no package manifest, installer, secret broker, migration protocol, or
App-specific session capability.
