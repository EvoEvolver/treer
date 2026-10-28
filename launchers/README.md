# Agent Launchers

Launchers package Agent runtimes as ordinary Treer launch profiles. They
start an Agent-scoped adapter and register an Agent Interface, but they are
not Treer Apps or platform components.

The Agent launcher, `treer-launch`, is part of the Treer installation: it is a
release executable beside `treer`, `treer-agent-host` and `treer-agent-server`,
listed in the same signed release manifest. Every Treer install path puts it
next to `treer` on PATH, and Controller updates replace it with the other
executables. `treer-launch update` updates only the launcher. See
[`agent/README.md`](agent/README.md#installation-and-updates).

A launcher must preserve these boundaries:

- Agent startup must not clone repositories, update packages, or mutate
  Host-wide state.
- Every reusable command, argument, and presentation choice belongs in the
  saved launch profile. Profiles remain plaintext and contain no secrets.
- The Controller sees an ordinary command or shell Agent. It must not gain a
  launcher-specific Agent kind, provider catalog, or lifecycle rule.
- The shared browser interface remains on Agent-private loopback. Ordinary
  terminal-only Agents can still use user-owned command profiles.
- Launcher state is scoped to the Agent. It must not write
  shared runtime state owned by Host, Controller, Proxy, or Core.

There is one launcher:

- [`agent`](agent/README.md) builds `treer-launch`, a native Rust runtime
  (Codex app-server and Claude stream-json adapters) that owns the AIS HTTP
  server and serves the embedded shared React UI, plus the default launch
  profiles. It needs no Node.js. New providers become another adapter in the
  same runtime; Agent interfaces do not belong under `apps/`.

## Instructions For Agents

The launcher is installed with Treer, and every workspace already has the
**Codex + UI** and **Claude + UI** profiles (`treer-launch --harness
codex|claude`). When an operator asks to set up the launcher, check that
`treer-launch --version` runs on the machine (if not, run `treer-agent-server
launcher install`), that the selected provider CLI is installed and
authenticated, and verify the profile with `treer agent admin profile show`.

Creating an Agent from the profile is a separate action. Do it only when the
operator also asks to launch or test an Agent.
