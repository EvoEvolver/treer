# ACP Launcher

This optional launcher runs one ACP provider session as one ordinary Treer
Agent. It owns the ACP process, Agent-local journal, cwd-jailed file API, and
private Agent Interface Server. Treer Host, Controller, Proxy, Protocol, and
Web do not know about ACP providers.

The default profiles are headless: they register `prompt.submit`,
`transcript.read`, `state.observe`, and `abort`, with no `ui_path`. They do not
publish a port or create an App, service, virtual host, or ingress.

## Instructions For Agents

When an operator asks you to set up this launcher, completion means that you
have created or updated the requested Treer launch profile. Do not merely quote
this README, clone the source, or build the runtime.

1. Determine the requested provider. Supported values are `grok`, `cursor`,
   `codex`, `claude`, and `opencode`. Do not install every provider.
2. Use the headless profile unless the operator explicitly requests Remote
   Codex UI. UI selection is never implicit.
3. If this launcher is not already present, perform the shallow sparse checkout
   shown below. Do not add launchers to Treer's normal installation.
4. Confirm the provider commands listed in `profiles.json` are installed and
   authenticated. Report a missing command; do not install provider software
   without a separate operator request.
5. Run `apply.sh` with `--no-launch` to save the profile. Add
   `--ui remote-codex` only when explicitly requested.
6. Run `treer agent admin profile show <profile-name>` and report the saved
   command, arguments, cwd, and profile ID. The task is incomplete if this
   verification fails.

For example, an Agent asked to add the headless Codex profile should execute:

```bash
./launchers/acp/scripts/apply.sh --agent codex --no-launch
treer agent admin profile show "Codex ACP"
```

If the operator also asks to create or test an Agent, omit `--no-launch` and
verify the resulting Agent separately with `treer agent show`. Profile creation
must still happen first.

## Install

Treer's normal installer and updater do not place this launcher on a user's
machine. From a managed Agent on the machine that will run the new Agent,
explicitly fetch a shallow sparse checkout:

```bash
TREER_ACP_SOURCE="${XDG_DATA_HOME:-$HOME/.local/share}/treer/acp-launcher"
git clone --depth 1 --filter=blob:none --sparse \
  https://github.com/EvoEvolver/treer.git "$TREER_ACP_SOURCE"
git -C "$TREER_ACP_SOURCE" sparse-checkout set \
  launchers/acp crates/treer-protocol
cd "$TREER_ACP_SOURCE"
```

The root Cargo metadata is included automatically by Git's cone-mode sparse
checkout. Builds are machine-local, so `apply.sh` always launches on that same
machine. First inspect the available provider launchers:

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

## Provenance

The ACP runtime and Remote Codex compatibility work were derived from
[PR #12](https://github.com/EvoEvolver/treer/pull/12), authored by
`fonsh <dufangshi@gmail.com>` through commit
`cd8f1d2b6177743a2ea0aa3f3d5507160a561ed1`. Treer integrated that contribution
as this isolated, profile-driven launcher: the provider runtime remains, while
the PR's Host, Controller, Proxy, Protocol, and Web coupling is intentionally
not part of the resulting tree. The PR branch is retained in `main` ancestry so
its original commits and authorship remain reachable.
