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
2. Use the headless profile unless the operator explicitly requests a UI.
   UI selection is never implicit.
3. Use the one-command installer below to install or update a versioned launcher.
   Do not add launchers to Treer's normal installation.
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

From a managed Agent on the target machine:

```sh
curl -fsSL https://raw.githubusercontent.com/EvoEvolver/treer/main/launchers/acp/install.sh \
  | sh -s -- --agent codex
# Explicitly select a presentation:
curl -fsSL https://raw.githubusercontent.com/EvoEvolver/treer/main/launchers/acp/install.sh \
  | sh -s -- --agent claude --ui builtin
```

The script reads `machine.root` from `treer whoami`, fetches an immutable source
commit, and compiles locally with Cargo. It does not install or authenticate
provider commands: Codex still requires `codex` and `codex-acp`; Claude requires
`claude` and `claude-agent-acp` on PATH. Git and Python 3 are also required.
The download/verification hook is reserved for future precompiled artifacts;
none are published by this installer yet, so it always builds missing versions.
A future downloaded binary must pass SHA256 verification before activation;
download or verification failure must fall back to the local build.

Versions live at `<machine.root>/.treer/launchers/acp/<full-commit>/`. The atomic
`current` symlink selects the version for future launches. Repeating the command
fetches the latest `main`, builds a new commit if needed, and updates the saved
profile. Old version directories are retained: a running launcher resolves its
physical directory at startup and keeps using its existing binary and assets.
There is no automatic garbage collection of versions still used by Agents.
Concurrent installs are rejected by `.install-lock`; after a hard interruption,
remove that lock only after confirming no installer is running.

Installation ends with `apply.sh --no-launch` and verifies the saved profile
using `treer agent admin profile show`. Its command is the absolute
`<machine.root>/.treer/launchers/acp/current/launchers/acp/scripts/treer-agent.sh`
path; its default cwd is the machine root (`.`). Override cwd when launching.
A build failure leaves `current` unchanged; profile/verification failure rolls
the pointer back. `--source /path/to/treer` installs the source checkout's
committed HEAD for local testing; uncommitted files are never packaged.

### Source Checkout Alternative

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
`<agent-cwd>/.treer/agents/$TREER_AGENT_ID`. Interface registration happens
once at Agent start. Repeating `treer interface register` rewrites
`registered_at` and remounts any embedded UI. A hot Controller restart restores
the cached descriptor when the Agent process is still the same. `--session-id`
is supported by the launcher script for an explicit custom profile, but no
workspace-wide session-discovery API is added to Core.

Generated binaries, UI checkouts, dependencies, and assets stay under
`launchers/acp/.build/` and are ignored by Git.

### Switch A Running TUI Agent

Have the parent prompt the **target Agent** to run this command inside its own
working directory (do not run it in the parent or copy the target's credential):

```sh
"<machine.root>/.treer/launchers/acp/current/launchers/acp/scripts/switch.sh" \
  --agent codex --ui builtin
# Inspect discovery and argv without changing startup or restarting:
"<machine.root>/.treer/launchers/acp/current/launchers/acp/scripts/switch.sh" \
  --agent claude --dry-run
```

The script matches the native transcript's cwd to the target's current directory
and chooses the most recently modified session: Codex searches
`~/.codex/sessions/**/*.jsonl`; Claude searches top-level session files in
`~/.claude/projects/<encoded-cwd>/`. Canonical cwd comparison also supports
symlinked project paths. Multiple active sessions sharing a cwd are ambiguous;
use `--session-id <native-id>` to select one explicitly. `--summary-file` supplies
your own handoff summary instead of a bounded excerpt of the initial task and
recent user/assistant text. The default excerpt redacts common credential
patterns, but is not a general-purpose secret detector.

The script saves `treer agent startup set` with the selected version's physical
launcher command and `--session-id`. A double-forked helper in a new process
session then runs `treer agent restart self`, surviving termination of the old
PTY process group on Linux/macOS. It does not rely on `nohup` alone. Startup
always stays self-only and cwd remains relative to `machine.root`.

The existing runtime first tries ACP session loading. If loading fails, it
starts a fresh session. The helper waits for the new run generation and AIS
registration, reads the actual session ID from that Agent's local journal, and
only on fallback submits the summary as the first handoff prompt through the
private AIS. This prompt has a stable operation ID for deduplication. It also
updates saved startup to the new session ID so later restarts retain the new
conversation. Avoid sending other prompts during the handoff. A successful load
does not receive an extra prompt.

Local handoff files are private to the machine account under
`<machine.root>/.treer/launchers/acp/switches/<agent-id>/<operation-id>/`:
`plan.json`, `restart.log`, and either `result.json` or `error.txt`. A readiness
timeout leaves the summary there for a manual retry; the helper does not loop
forever or restart the Agent repeatedly. Native Windows is not supported by
these POSIX launcher scripts. No Core session-discovery API, new Agent kind,
credential forwarding, or sandbox is introduced.

### Native Session Load Measurements

On 2026-10-11 (Toronto), macOS arm64, `codex-cli 0.162.1` and Claude Code
`2.1.296`, read-only ACP probes initialized the real adapters and called
`session/load` on existing native TUI transcripts. No model prompt was sent.

| Adapter package | Native history load |
| --- | --- |
| `@zed-industries/codex-acp 0.16.0` | Succeeded; 375 replay updates |
| `@agentclientprotocol/codex-acp 2.2.2` | Rejected the same ID with ACP `-32600`; no replay |
| `@zed-industries/claude-agent-acp 0.23.1` | Succeeded; 193 replay updates |
| `@agentclientprotocol/claude-agent-acp 0.89.1` | Succeeded; 145 replay updates |

The deprecated Zed package names and newer package names both expose the same
CLI executable names. These are local compatibility observations, not a claim
that every native transcript or adapter version can load. Replay establishes
history loading; it does not establish successful model continuation after a
live Treer restart. The fallback and detached restart paths are covered by
isolated script tests with a simulated Treer Controller/AIS.

Run launcher checks with:

```sh
python3 -m unittest discover -s launchers/acp/tests -v
cargo test --locked --manifest-path launchers/acp/runtime/Cargo.toml
sh -n launchers/acp/install.sh launchers/acp/scripts/apply.sh \
  launchers/acp/scripts/treer-agent.sh launchers/acp/scripts/switch.sh
```

## Optional TOWER Capture

Set `TOWER_URL` on an ACP profile to capture its bidirectional JSON-RPC stream
in a TOWER App. `TOWER_TOKEN` supplies the App's optional bearer token.

```sh
TOWER_URL=http://tower.internal TOWER_TOKEN=... \
  ./launchers/acp/scripts/treer-agent.sh \
  --harness codex --base-command codex --server-command codex-acp
```

Capture is disabled when `TOWER_URL` is absent. Enabled launchers append each
new frame to `tower-spool.sqlite` in their existing state directory and upload
ordered batches asynchronously. They do not serialize the complete trace on
each request. Failed uploads remain in the local spool and retry during the
same process or after restart. The provider child does not inherit the TOWER
URL or token.

This first adapter records observable ACP traffic, not hidden chain of thought
or the provider's direct model API requests. The launcher and App currently run
with ordinary machine-account authority; they are not a boundary against a
same-user compromise.

## Provenance

The ACP runtime and Remote Codex compatibility work were derived from
[PR #12](https://github.com/EvoEvolver/treer/pull/12), authored by
`fonsh <dufangshi@gmail.com>` through commit
`cd8f1d2b6177743a2ea0aa3f3d5507160a561ed1`. Treer integrated that contribution
as this isolated, profile-driven launcher: the provider runtime remains, while
the PR's Host, Controller, Proxy, Protocol, and Web coupling is intentionally
not part of the resulting tree. The PR branch is retained in `main` ancestry so
its original commits and authorship remain reachable.
