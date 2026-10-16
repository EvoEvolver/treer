<p align="center">
  <img src="web/public/favicon.svg" width="88" height="88" alt="Treer logo" />
</p>

# Treer

**Run every coding agent, on every machine, from one place.**

Treer is an open, self-hostable control plane for coding agents. Keep Codex,
Claude, Pi, OpenCode, and plain terminals running on the machines that own your
code, then connect their Apps and conversations in the same workspace.

[Try the canary](https://app.canary.treer.ai/) ·
[Product site](https://www.treer.ai/) ·
[Documentation](docs/README.md) ·
[Security model](docs/security.md)

## Configuration over convention

Treer keeps the platform minimal and the workflow configurable. It standardizes
only the contracts needed for Agents, humans, machines, and Apps to work
together: identity, process ownership, routing, and messages. It does not
prescribe one Agent, prompt format, launch command, communication channel, App
stack, or deployment model.

Launch profiles are commands you can edit. Apps are ordinary services you can
replace. Mail, Telegram, and future channels sit outside the core. Start with a
plain terminal, adopt only the pieces you need, and keep the rest of your
environment as it is.

## What Treer brings together

| | Capability | What it means |
| --- | --- | --- |
| **Agents** | Persistent processes and PTYs | Launch any command-based agent, observe it, prompt it, and return after the browser disconnects. |
| **Apps** | Workspace software with its own UI and data | Run HTTP Apps beside Agents. Treer supplies identity, routing, and a stable workspace address. |
| **Communication** | Durable Core Messages | Humans and Agents exchange addressed messages with explicit acknowledgement and ordered context. Mail and Telegram remain replaceable channels. |
| **Network** | Explicit private/public service forwarding | Register a local port, forward it across the workspace, and optionally expose it on a platform-assigned domain or TCP port. |

A saved launch can be restored after machine restart with `treer-agent-server restore --workspace WORKSPACE_ID`. Treer preserves intended-running Agents and Managed Apps; explicitly stopped workloads stay stopped. It does not restore in-memory Agent conversations.

Treer does not ship another Agent runtime. It coordinates the tools and
environments you already use through the browser and CLI. Native iOS and Android
clients are developed in [`mobile`](mobile/).

## How it works

```mermaid
flowchart LR
    Human["Human<br/>Web · CLI · mobile"] <--> Proxy
    BrowserApps["Browser Apps<br/>Mail · custom UI"] <--> Proxy
    Proxy["Treer Proxy<br/>identity · policy · routing · messages"]

    Proxy <-->|outbound connection| Controller

    subgraph Machine["Enrolled machine"]
        Controller["Controller"] <--> Host["Stable Host"]
        Host --> Agents["Codex · Claude · Pi · OpenCode · terminal"]
        Agents --> AIS["Agent Interface Server"]
        Host --> Apps["Workspace Apps"]
    end

    AgentChannels["Agent-run channels<br/>Telegram · custom adapters"] <--> Controller
```

The Host keeps Agent processes and terminal history alive on each machine. The
Proxy connects machines into workspaces and provides shared identity, routing,
policy, and messages. Machines connect outward, so Treer does not require you to
publish SSH or local service ports.

## Get started

The hosted canary is the current place to try Treer. Self-hosting is covered in
the [operator guide](docs/operator-guide.md).

1. Open the [canary console](https://app.canary.treer.ai/) and create or select a
   workspace.
2. Choose **Add machine** and run the installer and enrollment commands the
   console shows. On macOS and Linux the installer is
   `curl -fsSL 'https://proxy.canary.treer.ai/install.sh' | sh`; the console
   also shows the Windows PowerShell form and the single-use enrollment command.
3. Install the [Agent launcher](launchers/agent/README.md#create-an-agent) on that
   machine with `treer-agent-server launcher --node /absolute/node install`
   (Node.js 22+), then choose **+ → Add agent**, pick the machine, and pick a
   profile such as Claude + UI or Codex + UI. A plain terminal needs no
   launcher. Install and sign in to the Agent runtime yourself.

New workspaces include six launcher profiles: Claude, Codex, Cursor, Gemini,
Grok, and OpenCode, each with **+ UI**, plus **Claude (terminal)** and
**Codex (terminal)** for machines without the launcher. Existing workspace
profiles are left unchanged; installing the launcher manages its six UI profiles.

The console opens on a workspace overview, keeps open agent pages in tabs, and
is available in English and Simplified Chinese.

> [!IMPORTANT]
> Agents run with the operating-system authority of the account that starts the
> Treer Host. Treer is intended for machines you control and collaborators you
> trust; it is not a mutually untrusted execution sandbox. Use a dedicated
> account, container, or VM for untrusted code.

Treer currently fits individual developers with several machines, trusted
research groups sharing long-running sessions, and internal teams that want an
inspectable control plane. Read [Product direction](docs/product.md) for its
current scope and non-goals.

## Learn more

- [Operator guide](docs/operator-guide.md): self-hosting, local development,
  machine enrollment, administration, networking, and CLI examples.
- [Product direction](docs/product.md): audience, promise, principles, and
  current boundaries.
- [Architecture](docs/architecture.md): components, protocols, state, and
  information flow.
- [Security model](docs/security.md): trust assumptions, credentials, isolation,
  and known gaps.
- [Workspace Apps](apps/README.md): build services and interfaces for humans and
  Agents.
- Communication Apps: [Mail](apps/mail/README.md) and
  [Telegram](apps/telegram/README.md) are replaceable examples built on Treer's
  public contracts.
- [Documentation index](docs/README.md): every maintained guide and reference.

## License

Copyright 2026 Zijian Zhang.

Treer is licensed under the [Apache License 2.0](LICENSE).
