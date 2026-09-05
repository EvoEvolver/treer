# Agent Launchers

Launchers package optional Agent runtimes as ordinary Treer launch profiles.
They may build dependencies, start an Agent-scoped adapter, and register an
Agent Interface, but they are not Treer Apps or platform components.

A launcher must preserve these boundaries:

- Installation is explicit. Agent startup must not clone repositories, update
  packages, or mutate Host-wide state.
- Every reusable command, argument, and presentation choice belongs in the
  saved launch profile. Profiles remain plaintext and contain no secrets.
- The Controller sees an ordinary command or shell Agent. It must not gain a
  launcher-specific Agent kind, provider catalog, or lifecycle rule.
- Headless interfaces omit `ui_path`. An optional browser interface is a
  separate, explicitly named profile and remains on Agent-private loopback.
- Launcher state is scoped to the Agent or launcher checkout. It must not write
  shared runtime state owned by Host, Controller, Proxy, or Core.

Available launchers:

- [`acp`](acp/README.md) runs one ACP provider session per Treer Agent and
  registers a headless AIS by default. Remote Codex presentation is optional.
