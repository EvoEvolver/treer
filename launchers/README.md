# Agent Launchers

Launchers package optional Agent runtimes as ordinary Treer launch profiles.
They may install dependencies, start an Agent-scoped adapter, and register an
Agent Interface, but they are not Treer Apps or platform components.
They are not included in Treer's binary installation or update flow. An
operator fetches launcher source separately, preferably with a shallow sparse
checkout, only when choosing to install one.

A launcher must preserve these boundaries:

- Installation is explicit. Agent startup must not clone repositories, update
  packages, or mutate Host-wide state.
- Every reusable command, argument, and presentation choice belongs in the
  saved launch profile. Profiles remain plaintext and contain no secrets.
- The Controller sees an ordinary command or shell Agent. It must not gain a
  launcher-specific Agent kind, provider catalog, or lifecycle rule.
- The shared browser interface remains on Agent-private loopback. Ordinary
  terminal-only Agents can still use user-owned command profiles.
- Launcher state is scoped to the Agent or launcher checkout. It must not write
  shared runtime state owned by Host, Controller, Proxy, or Core.

There is one launcher:

- [`agent`](agent/README.md) is a native Rust runtime (Codex app-server and
  Claude stream-json adapters) that owns the AIS HTTP server and serves the
  shared React UI, plus the Node 22+ dispatcher, installer and bundle/update
  scripts, launch profiles and provider recipes. New providers become another
  adapter in the same runtime; Agent interfaces do not belong under `apps/`.

## Instructions For Agents

When an operator asks to set up a launcher, read that launcher's README and
carry the setup through to a saved Treer launch profile. Do not stop after
describing commands or building artifacts. Install only the launcher and
provider the operator selected, preserve its documented default presentation,
and verify the resulting profile with `treer agent admin profile show`.

Creating an Agent from the profile is a separate action. Do it only when the
operator also asks to launch or test an Agent.
