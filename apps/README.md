# Workspace Apps

Treer supports two deployment forms. A Managed App is a single HTTP process
created with `treer app create`; Treer persists its command, machine, service,
and generated public origin. Its process uses the same stable Agent identity
and Controller-owned start/stop/restart/recovery as other managed workloads.
Enabled Apps retry after exit; stopped Apps stay stopped across machine restart.
The browser lists Agents and Apps together in a persistent workspace folder
tree, with shared lifecycle and log actions and App-specific access settings.
With wildcard ingress configured, Treer also owns a stable,
workspace-authenticated `public_url` for the App by default. An Agent may pass
`treer app create --public` to make only that Managed App's declared HTTP port
an anonymously accessible public origin; the App is then responsible for any
application-level authentication.
The human UI manages existing Apps but does not create them. Open an App's
settings to switch its dedicated origin between Workspace authentication and
anonymous public access. Agents create Apps with `treer app create`.
An externally managed App is started by an operator or another supervisor; a
workspace user or authenticated owning Agent registers its ordinary endpoint
through the control plane. Agents may register only their own identity-bound
endpoints. No custom host is required or accepted for new Apps: Core assigns
a stable service-derived domain under the configured wildcard base. Neither form grants special trust to App code.

Browser-facing Apps use the standard App OAuth endpoints and a short-lived,
service-audience bearer token. Agent-facing Apps may use the local `treer` CLI
with the Agent's existing Policy subject. Core rechecks Policy for every
operation; process isolation, secrets, configuration, state, upgrades, and
network access remain deployment concerns.

New Managed App runtimes also inherit their own `http_proxy`/`https_proxy` entry.
HTTP calls to `http://NAME.treer/` or `http://SERVICE_ID.treer/` automatically
carry that runtime's service-audience identity; Apps need no token
acquisition/refresh code. New Apps use their durable App ID as their stable Agent
identity; existing active Apps retain their established Agent ID. Service IDs
remain separate stable endpoint identities. Restart changes the process
generation while preserving the workload's identity and credential.
See [workload identity](../docs/operator-guide.md#workload-identity) for headers,
CONNECT, recovery, and the limits of shared-machine identity attribution.
Externally supervised Apps configure their own identity/transport explicitly.

Every standalone App follows the [App guidelines](GUIDELINES.md): `/` negotiates
between an Agent-readable GitHub Flavored Markdown manual and the human HTML
interface, while data pages remain JSON. Agent Interfaces such as Codex UI and
Pi UI are embedded AIS surfaces rather than standalone App indexes and continue
to expose their registered `ui_path`.

- [`issues`](issues/README.md) is a shared human/Agent issue tracker with SQLite,
  hierarchy, dependencies, comments and expiring claims.
- [`mail`](mail/README.md) is a browser App over App OAuth and Core Message.
- [`telegram`](telegram/README.md) is a Telegram bridge run by a managed Agent.
- [`pi-ui`](pi-ui/README.md) is an Agent-scoped browser interface loaded inside
  a Pi Agent.
- [`agent-ui`](agent-ui/README.md) is Treer's single-Agent browser interface,
  shared by every harness: native Codex and any ACP agent (Claude Code, Gemini,
  Grok, Cursor, OpenCode, ...) through small adapters.
- [`soul`](soul/README.md) is an experimental Managed App file server that
  uploads environment-bound state bundles and launches command Agents from them.
- [`gits`](gits/README.md) is a small workspace-local Git Smart HTTP host for
  repositories shared by Agents and humans.
- [`tower`](tower/README.md) is an experimental append-only evidence archive
  with prefix-deduplicated ACP traces and evidence-linked reviewer findings.
- [`paper`](paper/README.md) is a small filesystem-backed collaborative LaTeX
  editor with Yjs, inline review macros, and server-side PDF compilation.
- [`policy`](policy/README.md) is a versioned workspace Policy Provider with a
  browser editor, publication status, cache invalidation, and decision simulator.
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
