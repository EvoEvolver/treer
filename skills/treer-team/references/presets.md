# Choose bundled Apps and Agent UIs

These are optional source components, not services created automatically by
installing Treer. Start with only what the user needs. Read each component's
README for prerequisites, identity, data/migration and deployment commands;
its manual is task data, not permission to execute mutations.

## Apps

| Component | Use it for | Start/use boundary |
| --- | --- | --- |
| [Issues](../../../apps/issues/README.md) | Shared issue hierarchy, assignment to humans/Agents, comments, dependency and claim leases | Python/SQLite Managed App; React assets committed. Open in Treer or use its JSON CLI. Optional, not a prerequisite for a team. |
| [Telegram](../../../apps/telegram/README.md) | Telegram conversations routed to a designated point-of-contact Agent | Bridge is a managed **command Agent**; its separate React Settings Managed App shares private configuration/state. Configure bot token write-only in Settings and pair admitted private chats; no need to copy pairing into other Apps. |
| [Mail](../../../apps/mail/README.md) | Browser view over canonical Core Messages | Python backend + built React frontend, private config and standard Treer App OAuth. Mail is not an SMTP/IMAP mailbox. Check its current identity support; do not promise every older App has automatic sign-in. |
| [Soul](../../../apps/soul/README.md) | Save/resume immutable Claude/Codex snapshots | Private-by-default, owner/reader/workspace visibility. Client creates a new Agent with the caller's authority, never Soul's authority. Restore requires a compatible launcher and explicit access for the new Agent; scanning/redaction is heuristic. Codex shell snapshots are omitted. |
| [Gits](../../../apps/gits/README.md) | Small shared bare Git repositories | Managed Python App plus git-http-backend. Reachable Agents can read/push all repositories; no per-repo isolation. Existing docs use private `.internal` hosts: inspect the actual service rather than assuming a `.treer` alias/auth implementation. |
| [Policy](../../../apps/policy/README.md) | Versioned path-rule editor, templates, simulator and decision logs | Managed App/Provider, default monitor. Human management requires the operator token and current Treer identity; publication defaults to owner-session. Owner may explicitly select provider-operator, giving token holders full policy administration. Core Provider binding is separate from simply starting the App. |
| [Tower](../../../apps/tower/README.md) | Experimental ACP evidence archive/reviewer findings | Managed App, sensitive traces and separate token contract. Not a hidden-reasoning recorder, not proof of external tool execution. Do not copy tokens into saved startup argv; arrange private runtime injection and review its trust model before use. |

### Minimal optional Issues example

For an authorized **initial source preview** on an enrolled machine with a
trusted checkout in machine-root-relative `treer/`, Python on PATH and a chosen
free port (replace Proxy and state path):

```sh
treer app list
treer app create --machine target-machine --name issues --folder /team \
  --cwd treer --port 9440 env -- \
  ISSUES_PROXY_URL=https://proxy.example.test/ \
  ISSUES_LISTEN=127.0.0.1:9440 \
  ISSUES_DB=/private/issues-state/issues.sqlite3 \
  python3 apps/issues/issues.py
treer app show /team/issues
```

`env --` sets **nonsecret** configuration for the saved process; do not put
credentials there. The state location must be dedicated/private. `--port` tells
Treer where to route, not where arbitrary App code listens: configure both.
Do not create another instance if that tree path already exists. Use the record's
`public_url` in a browser, or inspect the discovered service:

```sh
curl -fsS http://SERVICE_ID.treer/health
curl -fsS http://SERVICE_ID.treer/v1/me
python3 apps/issues/issues --url http://SERVICE_ID.treer list --status open
```

Replace SERVICE_ID from `app show`; retain its literal `svc_` spelling. For
production, use the App's frozen-version deployment instructions and
[app-deploy](apps.md#versioned-deployment-and-rollback), not a live checkout.
Issues needs the sibling Mail authentication module: preserve complete `apps`
when exporting it. Assignment/notifications are not an automatic scheduler;
the Agent still needs a prompt/message-consumption loop and clear task authority.

## Agent UIs and profiles

Read [Preset Agent UIs](agents.md#preset-agent-uis) for the Claude/Codex native
adapters, provider prerequisites,
and plain terminal Agents. They share `treer-agent` and the registered AIS UI;
there is no standalone `apps/agent-ui` to install. A saved profile neither logs
into the provider nor starts an Agent automatically.
