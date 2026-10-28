# Agent Launcher

The Agent launcher is Treer's single-Agent browser interface for coding
agents. One command Agent owns one provider session, one Agent Interface (AIS)
registration and one embedded page. There is no thread list or session switcher
in the UI.

The runtime is a native Rust executable (`src/`) with two provider adapters:

| Harness | Adapter | Talks to |
| --- | --- | --- |
| `codex` (default) | `src/runtime.rs`, `src/rpc.rs` | the local `codex app-server` over stdio; native thread resume |
| `claude` | `src/claude.rs`, `src/claude_goal.rs` | native Claude Code `--print` stream-json; native resume and background-task events |

Both adapters are unattended, full-authority launch modes: Codex uses
`approvalPolicy: never` and `danger-full-access`, Claude uses
`--dangerously-skip-permissions`. These are deliberate defaults, not a
filesystem or execution sandbox. Use a separate OS account, container or VM for
untrusted tasks; no browser confirmation gate protects the machine.

The directory also holds what surrounds the runtime:

| Path | Role |
| --- | --- |
| `src/`, `build.rs`, `Cargo.toml` | Native runtime and `treer-launch` CLI (own Cargo workspace and `Cargo.lock`, separate from Core) |
| `ui/`, `public/` | React page sources and the committed, byte-verified build, embedded into the binary at compile time |
| `scripts/` | Node UI build and check tooling (development only) |
| `profiles.json` | Launch profiles the Proxy creates as workspace defaults (`treer-launch` plus these arguments) |
| `interface-contract.json` | AIS contract compiled into the runtime |
| `tests/` | Node integration tests for the runtime, plus fake providers |

The installed launcher is one self-contained executable, `treer-launch`; it
needs no Node.js. The Node conversation runtime and its ACP adapter (Gemini,
Grok, Cursor, OpenCode) were removed; `--harness acp` and `--runtime node`
refuse, and `--runtime rust` (still in older saved profiles) is ignored.

The private HTTP listener exposes the browser page plus prompt, transcript,
state and abort operations. `GET /v1/transcript` pages by turn
(`page`/`cursor`, `limit` default 1). Treer reaches it through the Agent
network bridge; no service record or published port is required.

### Local security boundary

The listener binds only `127.0.0.1`. All HTTP routes and WebSocket upgrades
require Host `127.0.0.1:<actual-port>`, `localhost:<actual-port>` or
`[::1]:<actual-port>`; `agent-interface` is accepted only for Controller calls.
Any Origin (including `null`) or Sec-Fetch-* header requires matching protected
`X-Treer-Interface-Source` (`proxy-ui` or `controller-api`), `X-Treer-Agent-Id`
and current `X-Treer-Interface-Instance`. Proxy strips client markers and inserts
`proxy-ui` after authenticating its UI tunnel; Controller semantic calls insert
`controller-api` with no Origin. Only the latter may attest queue senders.
X-Forwarded-* is never trusted. Preflights receive no permissive CORS response.

The markers are public browser-defense provenance, not secret credentials or
local-process authentication. Native clients without browser headers remain
trusted on loopback; same-account programs can forge markers. Do not expose or
port-forward the listener; use external OS/network isolation for mutually
untrusted processes. Direct browser access is intentionally refused; use the
authenticated Treer UI tunnel.

The provider inherits its Agent environment, including its own Treer workload
credential so agent tools can use Treer. This is intentional delegation, not
credential hiding. Transcript, provider stderr, queue previews, task output,
completion notifications and session archives can contain secrets from commands
or conversations; no general content redaction is provided. Never publish them.

## Installation and updates

`treer-launch` is part of every Treer release, next to `treer`,
`treer-agent-host` and `treer-agent-server`, and is covered by the same signed
manifest. Every Treer install path puts it next to `treer`, which is on PATH
and first on every Agent's PATH: `install-official.sh` and the Proxy's machine
install script (`~/.local/bin`), `install.ps1` on Windows, and the container
images (`/app/bin`). Controller updates (`treer-agent-server update` and the
Console's Controller update) replace it together with the other executables;
they install it on machines that predate it. Running Agents keep the image
they started with.

The install also provides `treer-agent` as a deprecated alias (a symlink on
Unix, a copy on Windows) because workspace profiles saved before the rename run
`treer-agent --harness ...`. A pre-Rust `#!/bin/sh` Node shim of that name is
replaced; an unrelated script of that name is left alone with a warning.

Update or repair only the launcher:

```sh
treer-launch update                      # the installed Controller's version
treer-launch update --channel stable     # newest signed release on a channel
treer-launch update --version v0.2.0
treer-agent-server launcher install --from ./target/release/treer-launch  # local build
treer-agent-server launcher status
```

`treer-launch update` runs `treer-agent-server launcher update --dir <its own
directory>`. It fetches the signed release manifest (Ed25519, the same trust as
Controller updates: `--release-base-url`, `--public-key`,
`TREER_RELEASE_BASE_URL`, `TREER_RELEASE_PUBLIC_KEY`), downloads only the
`treer-launch` artifact for this platform and checks its size and SHA-256,
replaces the file atomically (keeping `treer-launch.old`), runs the new
binary's `--version` and restores the previous file if it does not report the
requested version. It is idempotent and never restarts Agents, the Host or the
Controller. `launcher install` and `launcher update` are the same command.

At run time the launcher resolves the Treer CLI from `TREER_BIN` (set by the
Controller for Agents), then PATH, then its own directory; providers from
`CODEX_BIN` / `CLAUDE_BIN`, else `codex` / `claude` on PATH. On Windows PATH
lookup tries `.exe`, `.cmd` and `.bat` in each directory, so npm, pnpm and
yarn installs work as they are: a package-manager `.cmd` shim is run as the
`node <script>` (or bundled `.exe`) it wraps, without `cmd.exe`; any other
batch file is started through `cmd.exe` with Rust's batch argument escaping.
`CODEX_BIN` / `CLAUDE_BIN` may name any of these. The UI is embedded;
`AGENT_UI_PUBLIC_DIR` serves a development `public/` directory instead.

### Upgrading from the Node launcher

Saved profiles and Agent startups that run `treer-agent --harness X
[--runtime rust] ...` keep working through the alias, with the same private
state (`rust-spike.json`, `RUST_SPIKE_*`). Startups that call `node
<machine.root>/.treer/launchers/agent/run.mjs ...` only run while Node and
those old files remain; change them to `treer-launch` with the same arguments.
`<machine.root>/.treer/launchers/agent/` and `~/.local/bin/treer-agent.json`
are no longer used and can be deleted once nothing references them. On
Windows, `install.ps1` replaces the old `%LOCALAPPDATA%\Treer\bin\treer-agent.exe`
shim with the alias.

### Create an Agent

Every workspace has the default profiles **Codex + UI** and **Claude + UI**
(`treer-launch --harness codex|claude`, `cwd=.`):

- Web: **+ → Add agent → Claude + UI or Codex + UI**, then choose the machine.
- CLI: `treer agent admin create --machine <m> --kind command --name <n> -- treer-launch --harness claude`.

Accepted arguments are `--harness codex|claude` (default `codex`),
`--session-id ID` (alias `--thread-id`), `--cwd ABS` and `--name NAME`; see
`treer-launch --help`. With no arguments the runtime is configured only by its
environment (`AGENT_UI_*`) and does not register an interface, which the
integration tests use.

## Resume and recovery

Resume an existing native session, including one created by the terminal CLI,
without importing or summarizing its context:

```sh
treer-launch --session-id THREAD_ID
treer-launch --harness claude --session-id SESSION_ID
```

Codex resumes with `thread/resume` (`approvalPolicy: never`,
`danger-full-access`); Claude resumes its native JSONL session. A requested
resume that fails exits without registering an Interface and never silently
creates a new conversation.

For a managed Agent, the session ID and the durable prompt journal live in the
Controller-provided private state directory (`TREER_AGENT_PRIVATE_STATE_DIR`,
journal `rust-spike.json`, mode 0600 in a 0700 directory). Startup requires an
available `agent.private-state.v1` status from the bound loopback Controller;
the directory's owner and mode are checked once when it is opened, and there
is no cwd fallback. A saved session cannot be overridden by a different resume
ID (`private_state_session_mismatch`), and a second writer is refused
(`private_state_in_use`). `agent restart` and Host cold recovery rerun the saved
command and resume the same session. Keep the working directory and the
provider's own storage (`CODEX_HOME`/`~/.codex`, `CLAUDE_CONFIG_DIR`/`~/.claude`)
available.

The launcher does not wait for a human:

- If the provider process or its stdio transport fails (exit, EOF, write
  error, RPC timeout), the launcher restarts it and resumes the same session
  automatically, with exponential backoff capped at 60 s and no attempt cap.
  `/api/state` reports this under `runtime.recovery` (`state`
  `running`/`restarting`/`storage_failure`, `restarts`,
  `consecutiveFailures`, `nextRestartInMs`, `lastFailure`);
  `POST /api/recovery {"action":"recheck"}` restarts a stopped provider now.
- A prompt in flight when the provider stopped is settled from the resumed
  session's history: a matching turn settles it with that turn's status; a
  Codex prompt the history has no turn for was not delivered and is queued
  again; anything else (an unknown turn, a Claude prompt whose process died)
  is marked failed or interrupted with an explanation. Nothing that history
  shows was delivered is replayed, and the rest of the queue continues.
- A JSON-RPC error reply means the provider did not take the input: that
  prompt is marked failed, and a refused steer (usually its turn just ended)
  runs as the next turn.
- Claude frames the adapter does not understand are logged and skipped;
  permission requests are answered with allow. Only frames for another
  session id stop the provider (which is then restarted).
- Several in-progress turns in provider history: the newest one is live and
  the older ones are shown as interrupted, with a warning in the log.
- The Core Message watcher never gives up, including on authentication
  errors; it retries with capped backoff and reports itself under
  `runtime.messages`.
- Provider startup failure and journal write failure exit the launcher
  non-zero so the Host restarts it.

The live checkpoint is
bounded at 32 MiB; best-effort plaintext history copies in
`rust-journal-history` keep total journal/log data under 128 MiB per Agent
(at most 64 files, oldest first). These copies are never used as recovery input.

### Git commit identity

So that commits are attributable, a managed launcher sets `GIT_AUTHOR_NAME`
and `GIT_COMMITTER_NAME` to the Agent's path (from `treer whoami`, falling
back to its name, then its ID) and `GIT_AUTHOR_EMAIL` and
`GIT_COMMITTER_EMAIL` to `<agent-id>@agents.treer.local` for the harness and
every process it starts. A variable the profile or environment already sets
is left unchanged.

## Offline session transfer

`treer-launch session ...` exports, inspects, prepares and launches Claude
sessions in a new directory with a fresh session UUID, without touching the
running Agent (macOS/Linux). See [SESSION-TRANSFER.md](SESSION-TRANSFER.md).
The current session's HTTP export is `GET /api/session/export` with loopback
Host and no Origin header; it is a guard, not authentication.

## Native Windows

The Windows build is the same launcher: the same journal, history, recovery,
file browser (including directory listing), task output and image attachments.
Only the platform mechanics differ: private files and directories are created
with the current user's protected DACL instead of mode 0600/0700, the
single-writer lease is a `LockFileEx` file lock, journal replacement is a
write-through `MoveFileEx`, and each provider runs in a nested kill-on-close
Job Object instead of a process group. Offline session transfer is
macOS/Linux only. `treer-launch.exe` is the Windows release artifact;
`install.ps1` installs it next to `treer.exe`.

## Browser UI

The page (`ui/`) is a client of the same HTTP/WebSocket API the Console and
Agents use; it adds no endpoint. One design, light and dark, at every width
(standalone, phone, or framed by the Console):

- **Header:** a worded run status (Ready, Working, Last turn failed,
  Restarting, Reconnecting, Blocked), the thread title and short workspace
  path, and Files, Tasks (Claude) and Settings. The tab title carries the
  status while it is not Ready.
- **Conversation:** opens at the latest message and follows new content until
  the reader scrolls up themselves (wheel, touch, keys, scrollbar). Scroll
  resets the browser makes on its own (a hidden Console tab, layout changes)
  never stop following; a hidden tab returns to the latest message, or to the
  reader's place, when shown again. Each turn folds its commands and tool calls into one
  summary row, lists every changed file with +/− counts, its diff and an Open
  action, and shows a failed turn's error with Retry (the same message through
  `api/prompt`) and Copy. A divider marks what is new since this browser last
  saw the conversation. During a provider restart the last conversation stays
  visible under a banner that states the cause, the restart count and how
  many messages were not confirmed (they are never replayed automatically).
- **Run bar and composer:** while a turn runs, the bar above the composer shows
  elapsed time, the current step, goal, plan and queue, and Stop. Enter sends
  (or steers a running turn), Tab queues, Shift+Enter adds a line; while
  running both Steer and Queue are buttons. Attach, Goal and the model/effort
  chip sit under the text field.
- **Drawers:** Files (tree, preview, Download, and Mention, which inserts the
  path into the draft) and Background tasks sit beside the conversation from
  1100px and over it below that.
- **Settings:** model and reasoning effort, theme (System/Light/Dark) and
  Markdown export.

Only per-browser conveniences are stored locally: the theme mode and, per
thread, the last turn seen. The Console's `look` hint is accepted and ignored.

## Add a harness adapter

Implement the provider protocol in `src/` next to the Codex (`runtime.rs`,
`rpc.rs`) and Claude (`claude.rs`) backends and map its events to the shared
history and receipt model. Keep the HTTP API, file browser, prompt journal and
browser UI shared; do not copy the UI or server per provider. Unsupported
capabilities stay false/null, and a requested resume that fails never creates a
new conversation. Resolve the provider binary in `src/tools.rs`, accept the
harness in `src/cli.rs`, add fake-provider tests under `tests/`, then add a
profile to `profiles.json`.
Installation must not start or authenticate the provider.

## Build and test

`build.rs` embeds `public/`; nothing about the build machine is compiled in.
The `fixture-harness` feature allows fake provider arguments
(`RUST_SPIKE_*_ARGS`) and test hooks, and is for tests only. Release builds use
the Core target directory so `treer-launch` lands next to the other release
binaries:
`cargo build --locked --release --manifest-path launchers/agent/Cargo.toml --target-dir target`.

```sh
# UI
npm --prefix launchers/agent ci
npm --prefix launchers/agent run check      # typecheck + byte-for-byte UI build check
npm --prefix launchers/agent test
# Native runtime unit tests
TREER_TEST_NODE="$(command -v node)" \
  cargo test --locked --features fixture-harness --manifest-path launchers/agent/Cargo.toml
cargo clippy --locked --features fixture-harness --manifest-path launchers/agent/Cargo.toml --all-targets -- -D warnings
# Runtime integration tests against a fixture-harness build (run from launchers/agent)
cargo build --locked --features fixture-harness
RUST_SPIKE_BIN=$CARGO_TARGET_DIR/debug/treer-launch node --test tests/contract.mjs tests/claude-entry.mjs
# Real-browser conversation placement (Playwright Chromium): opens at the latest message, standalone and in a
# Console-like frame, slow history, hidden tab, reader scroll-up
RUST_SPIKE_BIN=$CARGO_TARGET_DIR/debug/treer-launch node --test tests/browser-scroll.mjs
# Release-feature artifact scan (no fixture overrides may survive)
RUST_SPIKE_RELEASE_BIN=$CARGO_TARGET_DIR/release/treer-launch node --test tests/default-build.mjs
```

The other `tests/*-entry.mjs` suites (session launch, snapshot, transfer, owner
default, HOME cwd) take the same `RUST_SPIKE_BIN`;
`tests/windows-native-entry.mjs` needs a Windows fixture-feature PE; the
Windows release job runs it with the launcher unit tests.
UI sources live in `ui/`; after editing them run `npm run build:ui` and commit
`public/`. `check:ui` fails on any byte difference, budget overrun or forbidden
code pattern. `tests/acceptance/` holds the live Console embed/tasks acceptance
scripts (see its README).
