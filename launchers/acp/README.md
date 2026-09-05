# ACP Launcher

This optional launcher runs one ACP provider session as one ordinary Treer
Agent. It owns the ACP process, Agent-local journal, cwd-jailed file API, and
private Agent Interface Server. Treer Host, Controller, Proxy, Protocol, and
Web do not know about ACP providers.

The default profiles are headless: they register `prompt.submit`,
`transcript.read`, `state.observe`, and `abort`, with no `ui_path`. They do not
publish a port or create an App, service, virtual host, or ingress.

## Install

Run this from a managed Agent in a Treer checkout on the machine that will run
the new Agent. Builds are machine-local, so the installer always launches on
that same machine. First inspect the available provider launchers:

```bash
./launchers/acp/scripts/apply.sh --list
```

Install and launch only the selected providers:

```bash
./launchers/acp/scripts/apply.sh --agent grok
./launchers/acp/scripts/apply.sh --agent cursor --agent opencode
```

Pass `--no-launch` to create or update profiles without creating the first
Agent. The provider command must already be installed and authenticated.
Codex additionally requires `codex-acp`; Claude requires `claude-agent-acp`.
The installer reports missing commands and does not install provider software.

The source of truth is [`profiles.json`](profiles.json). Each saved profile
uses the ordinary command Agent path and runs
`./launchers/acp/scripts/treer-agent.sh` with explicit `--harness`,
`--base-command`, and `--server-command` arguments. Provider executables are
profile data rather than hidden launcher policy, and the runtime refuses to
start a real provider when those arguments are omitted. There is no `kind=acp`
Controller behavior.

## Optional UI

An Agent has no browser UI unless the operator explicitly installs a UI
profile:

```bash
./launchers/acp/scripts/apply.sh --agent grok --ui remote-codex
```

This creates `Grok ACP + Remote Codex UI`, separate from the headless `Grok
ACP` profile. The install step fetches the source and builds it; Agent startup
never performs network or package-manager operations. The upstream repository,
package manager, and immutable commit are recorded in
[`optional-ui/remote-codex.lock.json`](optional-ui/remote-codex.lock.json).

The optional runtime feature owns the Remote Codex `/api/*` and WebSocket
compatibility surface. The resulting private AIS explicitly registers
`ui_path=/`; Treer's existing generic Interface tunnel embeds it without any
ACP or Remote Codex special case.

## State And Recovery

The runtime stores its SQLite journal under
`<agent-cwd>/.treer/agents/$TREER_AGENT_ID`. The launcher refreshes Interface
registration every 20 seconds, so a hot Controller restart does not require an
Agent restart. `--session-id` is supported by the launcher script for an
explicit custom profile, but no workspace-wide session-discovery API is added
to Core.

Generated binaries, UI checkouts, dependencies, and assets stay under
`launchers/acp/.build/` and are ignored by Git.
