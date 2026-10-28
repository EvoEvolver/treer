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
| `src/`, `build.rs`, `Cargo.toml` | Native runtime (own Cargo workspace and `Cargo.lock`, separate from Core) |
| `ui/`, `public/` | React page sources and the committed, byte-verified build the runtime serves |
| `scripts/` | Node 22+ dispatcher (`treer-agent.mjs`), installer, bundle/update tooling, Windows/PATH shims |
| `profiles.json` | Bundled launch profiles; also embedded by the Proxy as workspace defaults |
| `provider-recipes.json`, `provider-review.json` | Provider CLI installation recipes embedded by Core ([handoff](provider-recipes.md)) |
| `interface-contract.json` | AIS contract compiled into the runtime and checked by the installer |
| `tests/` | Node integration tests for the runtime and installer, plus fake providers |

Node is still required on each machine: it runs the installer and the small
`treer-agent` dispatcher, which verifies the pinned native image and then execs
it. The Node conversation runtime and its ACP adapter (Gemini, Grok, Cursor,
OpenCode) were removed; `--harness acp` and `--runtime node` now refuse.

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

Official Controller releases embed the same-release launcher payload (UI,
profiles, installer, dispatcher), authenticated by the signed Controller
artifact. The native runtime is bound per machine by a `native-runtime.json`
manifest (schema `treer.native-launcher/v1`) that pins the executable, the
public UI assets and the native provider/Treer tools by path, size and SHA-256:

```json
{
  "schema": "treer.native-launcher/v1",
  "platform": "linux",
  "arch": "x64",
  "source_commit": "<native source full SHA>",
  "executable": {"path": "/physical/runtime", "bytes": 1, "sha256": "<SHA256>"},
  "public_dir": "/physical/runtime-assets/public",
  "assets": [{"path": "/physical/runtime-assets/public/index.html", "bytes": 1, "sha256": "<SHA256>"}],
  "tools": {
    "treer": {"path": "/physical/treer", "bytes": 1, "sha256": "<SHA256>", "version": "<version>"},
    "codex": {"path": "/physical/native-codex", "bytes": 1, "sha256": "<SHA256>", "version": "<version>"}
  }
}
```

`assets` lists every public file (`index.html`, `boot.js`, `ui.js`, `ui.css`,
lazy chunks and the PDF worker). Bind `tools.claude` when that native provider
is installed; a missing provider is reported as `unavailable-provider` and that
harness refuses to start. Installation, status and launch verify physical
paths, platform/architecture, byte counts, SHA-256 and native image headers. A
missing, altered or wrong-platform binding refuses; there is no fallback.
Executable paths must match the native binary's compiled tool bindings (see
[Build and test](#build-and-test)). The manifest is part of the
content-addressed payload and must not be edited after activation.

The default launcher is installed through the local Controller:

```bash
treer-agent-server launcher install --node /absolute/trusted/path/to/node
treer-agent-server launcher update --node /absolute/trusted/path/to/node --require-runtime rust
treer-agent-server launcher status --node /absolute/trusted/path/to/node
treer-agent-server launcher rollback --node /absolute/trusted/path/to/node
```

With multiple local Controller enrollments, discovery fails closed with
`launcher_controller_ambiguous`; select `--workspace WORKSPACE_ID` (and
`--url http://127.0.0.1:PORT` only when that still matches several). Node 22+
is an external prerequisite; its executable is not authenticated by the
Controller signature, so use a trusted absolute path. Provider CLIs and their
authentication remain the operator's responsibility. A development Controller
without a prepared payload refuses installation unless `--allow-network` and a
trusted npm CLI enable lockfile-pinned `npm ci --ignore-scripts`.

The installer checks every file's size/SHA-256, the interface contract and the
selected Controller handshake before activation. The content version is
`sha256-<SHA-256 of bundle-manifest.json>`, installed privately under
`<machine.root>/.treer/launchers/agent/<content-version>/`. Profiles use the
portable `treer-agent` command, whose per-user shim pins Node and the stable
`run.mjs`; an atomic `current.json` pointer selects the default version without
Windows symlink privileges. Running Agents resolve physical assets once and
keep them across updates. Version directories remain for rollback; profile
writes are read back, and failures restore prior profiles and the pointer. No
update restarts running Agents. The contract declares a
`launcher_security_epoch`; rollback refuses versions below the Controller's
floor. Unix state files use 0600 and directories 0700; Windows uses
current-user/SYSTEM-only ACLs.

### Update the default entry

`treer-agent --help` lists launch arguments and the update command:

```sh
treer-agent update                    # local Controller's embedded payload
treer-agent update --latest           # launcher-stable GitHub release channel
treer-agent update --bundle /absolute/prepared --sha256 MANIFEST_SHA256
```

Without `--latest` or a bundle, the command calls the local Controller's
guarded `launcher update --require-runtime rust`; it discovers the Controller
beside the pinned Treer CLI or in its sibling `libexec` (override with
`--controller ABS`). It never retries without the runtime requirement.

`--latest` resolves
`https://github.com/EvoEvolver/treer/releases/download/launcher-stable/launcher-latest.json`
once, selects the current OS/architecture and downloads its versioned package.
Trust comes from the official repository over HTTPS plus the channel's SHA-256
and per-file checksums; it is not an independent signature or rollback
protection. Redirects are limited to GitHub's release asset host. An existing
verified Rust installation is required because native binaries bind their
provider and Treer paths at compile time; the published native's tool paths,
versions and hashes must match it. `--release-url http://127.0.0.1:PORT/latest`
selects a loopback fixture for tests only.

The result is JSON with current and target versions, source commit,
runtime/provider status and whether the version changed. Updating the default
never restarts, migrates or replays running Agents, Host or Controller. A
failed or ambiguous command is not retried automatically; inspect
`treer-agent-server launcher status` first.

### Create an Agent

Install the launcher on each machine (above). Then:

- Web: **+ → Add agent → Claude + UI or Codex + UI**, then choose the machine.
- CLI: `treer agent admin create --machine <m> --kind command --name <n> -- treer-agent --harness claude`.

The installer pins Node and the launcher entry in one per-user command:
`~/.local/bin/treer-agent` on macOS/Linux, or
`%LOCALAPPDATA%\Treer\bin\treer-agent.exe` on Windows. Ensure that directory is
on PATH; the installer does not rewrite shell profiles, registry values or
running Host environments. Profiles use `command=treer-agent`, `cwd=.` and the
same provider arguments on every machine. Accepted arguments are
`--harness codex|claude`, `--session-id ID` (alias `--thread-id`),
`--cwd ABS` and `--name NAME`.

Only the named shim and its integrity receipt are atomically replaced. Links,
unknown existing commands and bin directories writable by another POSIX user
are refused; symlinked `~/.local` or bin directories are unsupported.

To collapse an existing workspace's legacy profiles, install the shim on
**all** machines, run `node launchers/agent/scripts/migrate-profiles.mjs
--dry-run`, review the diff, then rerun with `--apply`. It saves a private 0600
backup of the original inventory first, adopts only recognized launcher
commands with exact provider arguments, and leaves custom or referenced
profiles intact. It does not change any Agent startup or restart anything.

### Local prepared bundles (launcher-only delivery)

An operator can deliver a launcher change without rebuilding the Controller.
From a trusted checkout (after `npm --prefix launchers/agent ci`):

```sh
node launchers/agent/scripts/prepare-bundle.mjs --output /absolute/prepared \
  --source-commit FULL_SOURCE_SHA \
  --native-manifest /absolute/native-runtime.json --native-sha256 SHA256
/absolute/node launchers/agent/scripts/bundle-launcher.mjs install \
  --bundle /absolute/prepared --sha256 MANIFEST_SHA256 --treer /absolute/treer
/absolute/node launchers/agent/scripts/bundle-launcher.mjs status --treer /absolute/treer
/absolute/node launchers/agent/scripts/bundle-launcher.mjs rollback --treer /absolute/treer
```

`prepare-bundle.mjs` rebuilds the UI and refuses stale or extra `public/`
assets, copies scripts/profiles/contract/recipes, and writes
`bundle-manifest.json` (schema `treer.launcher-payload/v1`). The schema's
`runtime.mjs` entry is kept for compatibility with installed launchers and
Controllers; it is a fail-closed stub because every launch goes through
`scripts/treer-agent.mjs`. Record the SHA-256 of `bundle-manifest.json`
independently. Hashes provide integrity, not authorship: this is
**local operator-approved delivery**, not a signed release, and it cannot
deliver Core changes.

`update` takes the same arguments as `install`; `rollback --version
sha256-HASH` selects a retained version; `--agent` narrows and
`--skip-profiles` disables profile writes; `--root ABS` chooses a separate
installation root inside the enrolled machine root.

For a staged switch of running Agents, list explicit Agent IDs/paths in a JSON
array and name one disposable preview target:

```sh
/absolute/node launchers/agent/scripts/bundle-launcher.mjs rollout \
  --bundle /absolute/prepared --sha256 MANIFEST_SHA256 --treer /absolute/treer \
  --targets /absolute/targets.json --preview ag_PREVIEW --rollout-id preview-001
# Repeat exactly the same command with --apply to execute the recorded plan.
```

Only enabled command Agents on this machine with a healthy launcher session are
eligible; custom startups, provider mismatches, the leader and the caller are
rejected. The preview is the sole first batch; later batches hold at most three
Agents. Each target must re-register, serve its manifest and UI, become ready
and keep the same native session ID before the next batch; it then receives one
queued continuation with a stable operation ID. The private
`rollouts/ID.json` journal (0600) records original and replacement startups;
rerunning the same ID reconciles instead of repeating restarts. On failure,
later batches stop and the previous pointer and startups are restored.

### Source installs (development)

`install.sh`, `scripts/apply.sh` and `bootstrap.sh` install from a Git
checkout. They are development entry points: the Git source is not
authenticated as an official release, and Codex/Claude profiles require an
explicit `--native-manifest ABS --native-sha256 HASH`.

```sh
./launchers/agent/install.sh --agent codex --native-manifest /abs/native-runtime.json --native-sha256 SHA256
./launchers/agent/scripts/apply.sh --agent claude   # install the committed local revision
sha=FULL_40_CHARACTER_COMMIT_SHA
curl -fsSL "https://raw.githubusercontent.com/EvoEvolver/treer/$sha/launchers/agent/bootstrap.sh" | sh -s -- --ref "$sha" --agent codex
```

Bootstrap requires Treer, Node.js 22+, npm, Git and tar, sparse-fetches only
`launchers/agent` at the full SHA, verifies HEAD and runs the installer. Omit
`--agent` to save both bundled profiles. Installation never creates or
restarts an Agent; `treer agent admin profile launch "Codex + UI" --name NAME`
is a separate action.

## Resume and recovery

Resume an existing native session, including one created by the terminal CLI,
without importing or summarizing its context:

```sh
treer-agent --session-id THREAD_ID
treer-agent --harness claude --session-id SESSION_ID
```

Codex resumes with `thread/resume` (`approvalPolicy: never`,
`danger-full-access`); Claude resumes its native JSONL session. A requested
resume that fails exits without registering an Interface and never silently
creates a new conversation.

For a managed Agent, the session ID and the durable prompt journal live in the
Controller-provided private state directory (`TREER_AGENT_PRIVATE_STATE_DIR`,
journal `rust-spike.json`, mode 0600 in a 0700 directory). Managed execution
requires an available `agent.private-state.v1` status from the bound loopback
Controller and physical placement outside version control; there is no cwd
fallback. A saved session cannot be overridden by a different resume ID
(`private_state_session_mismatch`), and a second writer is refused
(`private_state_in_use`). `agent restart` and Host cold recovery rerun the saved
command and resume the same session. Keep the working directory and the
provider's own storage (`CODEX_HOME`/`~/.codex`, `CLAUDE_CONFIG_DIR`/`~/.claude`)
available.

Journal publication or placement failure makes the runtime read-only, blocks
dispatch and stops the provider; it does not resume automatically after repair.
Uncertain dispatches are never replayed on restart. The live checkpoint is
bounded at 32 MiB; best-effort plaintext history copies in
`rust-journal-history` keep total journal/log data under 128 MiB per Agent
(at most 64 files, oldest first). History is never used as recovery input.

Codex Agents previously run by the removed Node runtime kept state under
`<cwd>/.treer/agents/<agent-id>/`. Start the native executable directly once with
`--import-node-state` (Agent idle, queue empty) to import it after a byte
backup to private `node-backup-v1`; `--restore-node-backup` restores those
bytes. Legacy state is refused by default.

## Offline session transfer

`treer-agent session ...` exports, inspects, prepares and launches Claude
sessions in a new directory with a fresh session UUID, without touching the
running Agent (macOS/Linux). See [SESSION-TRANSFER.md](SESSION-TRANSFER.md).
The current session's HTTP export is `GET /api/session/export` with loopback
Host and no Origin header; it is a guard, not authentication.

## Native Windows

The Windows build runs the same Codex and Claude adapters in a native PE with
separate platform backends for private state and workspace reads. Existing
private directories must already carry the current user's private DACL; the
runtime checks them without repairing permissions, rejects reparse points and
hardlinks, and holds a share-mode-zero lease against a second writer. Each
provider is assigned a nested kill-on-close Job Object. Directory enumeration
returns HTTP 501 and images are unavailable on Windows; text sessions, queue,
receipts and restart checkpoints are supported. A Mac/Linux image or a Node
payload never establishes Windows support: Windows needs its own accepted
native image and `native-runtime.json`.

The `treer-agent.exe` entry is a small console PE compiled by the OS .NET
Framework 4 `csc.exe`; it forwards argv and exit status without a shell and
places Node in a `KILL_ON_JOB_CLOSE` Job. Installation lives under the enrolled
machine root at `.treer\launchers\agent`; choose a writable user-owned root
(typically beneath `%LOCALAPPDATA%`) when enrolling.

## Add a harness adapter

Implement the provider protocol in `src/` next to the Codex (`runtime.rs`,
`rpc.rs`) and Claude (`claude.rs`) backends and map its events to the shared
history and receipt model. Keep the HTTP API, file browser, prompt journal and
browser UI shared; do not copy the UI or server per provider. Unsupported
capabilities stay false/null, and a requested resume that fails never creates a
new conversation. Bind the provider binary at build time in `build.rs`, accept
the harness in `scripts/treer-agent.mjs` and `scripts/native-runtime.mjs`, add
fake-provider tests under `tests/`, then add a profile to `profiles.json`.
Installation must not start or authenticate the provider.

## Build and test

Default builds bind native tools at **build time**: `build.rs` resolves
`codex` and `treer` (and optionally Claude via `TREER_SPIKE_BUILD_CLAUDE`) to
absolute native executables and refuses script wrappers. If `codex` on PATH is
an npm wrapper, set `TREER_SPIKE_BUILD_CODEX` to the native binary.
`TREER_SPIKE_TARGET_{CODEX,TREER,CLAUDE}` bind a different deployment path.
The `fixture-harness` feature allows fake providers (`CODEX_BIN`, `CLAUDE_BIN`,
`RUST_SPIKE_*_ARGS`) and is for tests only. Use a private `CARGO_TARGET_DIR`.

```sh
# UI, installer and update scripts
npm --prefix launchers/agent ci
npm --prefix launchers/agent run check      # typecheck + byte-for-byte UI build check
npm --prefix launchers/agent test
# Native runtime unit tests
TREER_TEST_NODE="$(command -v node)" \
  cargo test --locked --features fixture-harness --manifest-path launchers/agent/Cargo.toml
cargo clippy --locked --features fixture-harness --manifest-path launchers/agent/Cargo.toml --all-targets -- -D warnings
# Runtime integration tests against a fixture-harness build (run from launchers/agent)
cargo build --locked --features fixture-harness
RUST_SPIKE_BIN=$CARGO_TARGET_DIR/debug/treer-agent-launcher node --test tests/contract.mjs tests/claude-entry.mjs
# Release-feature artifact scan (no fixture overrides may survive)
RUST_SPIKE_RELEASE_BIN=$CARGO_TARGET_DIR/release/treer-agent-launcher node --test tests/default-build.mjs
```

The other `tests/*-entry.mjs` suites (session launch, snapshot, transfer, owner
default, HOME cwd) take the same `RUST_SPIKE_BIN`;
`tests/windows-native-entry.mjs` needs a Windows fixture-feature PE.
UI sources live in `ui/`; after editing them run `npm run build:ui` and commit
`public/`. `check:ui` fails on any byte difference, budget overrun or forbidden
code pattern. `tests/acceptance/` holds the live Console embed/tasks acceptance
scripts (see its README).
