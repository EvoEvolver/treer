# Workspace Apps

Apps can also own a recursive tree of logical Apps and Agents, using their
existing identity and message mailbox. See [the shared tree/message contract](../docs/app-tree.md)
and the runnable [Agent Hub example](agent-hub/README.md).

Embedded App interfaces can open unsandboxed browser tabs from a user gesture.
External links must use `rel="noopener noreferrer"`; external `window.open`
calls must pass the `noopener,noreferrer` features. A full-page button should
open only the App's own URL. Without `noopener`, a popup retains a reference
to the embedded frame and may navigate that frame. The Console iframe does
not grant top-navigation permission.

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

Browser Apps first accept `X-Treer-Identity-Token` and verify it against their
own service/workspace through `/.treer/apps/identity/verify`. Authenticated
Workspace ingress and the embedded Managed App UI tunnel supply fresh 60-second
human tokens, so a logged-in Treer user needs no separate App sign-in. Missing
headers may fall back to standard App OAuth for direct/legacy access; OAuth
human tokens remain 12-hour grants. An invalid or expired supplied token must
be rejected, not silently replaced by a cookie identity. Public ingress does
not issue human identity. Existing Apps must implement this authentication
order; the Proxy does not convert their private cookie/session logic for them.

App code receives the current user's service-scoped authority for 60 seconds.
Treat it as sensitive, verify every request, and check Origin/Sec-Fetch-Site for
browser mutations even when a human identity header is present. Proxy-owned
login/ingress cookies and untrusted Treer headers never reach the App.
Agent-facing Apps may use the local `treer` CLI with the Agent's existing Policy
subject. Core rechecks Policy for every operation; process isolation, secrets,
configuration, state, upgrades, and network access remain deployment concerns.

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
interface, while data pages remain JSON. Agent Interfaces belong to the unified launcher, rather than this App index.
They continue to expose their registered `ui_path`.

- [`issues`](issues/README.md) is a shared human/Agent issue tracker with SQLite,
  hierarchy, dependencies and comments. Assigning or @mentioning an Agent wakes
  it with `treer agent prompt`, so grant the App's runtime identity
  `agent.prompt` in the workspace Policy (otherwise only a Message is sent).
- [`mail`](mail/README.md) is a browser App over App OAuth and Core Message,
  with optional email notifications for unread Messages.
- [`telegram`](telegram/README.md) is a Telegram bridge run by a managed Agent.
- [`soul`](soul/README.md) is an experimental Managed App file server that
  uploads environment-bound state bundles and launches command Agents from them.
- [`gits`](gits/README.md) is a small workspace-local Git Smart HTTP host for
  repositories shared by Agents and humans.
- [`policy`](policy/README.md) is a versioned workspace Policy Provider with a
  browser editor, publication status, cache invalidation, and decision simulator.
- [`external-agent`](external-agent/README.md) lets programs outside Treer
  (Claude Code, Codex, scripts, CI) act as the App's Agent with App-issued
  tokens: device-code onboarding, a native CLI gateway and per-key inboxes.

Each bundled App is source code, configuration schema, documentation, and
tests. Managed Apps currently accept one command and one HTTP UI port; there is
no package manifest, installer, secret broker, migration protocol, or
App-specific session capability.
