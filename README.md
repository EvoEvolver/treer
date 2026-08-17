# treer

Treer is a distributed runtime and control plane for coding agents.

Each machine runs a stable Treer Host that owns local agent processes, PTYs, and
terminal history. A hot-updatable Controller connects that Host to the central
Proxy, which groups machines into logical workspaces and routes discovery and
control commands between them.

The Proxy is designed to be internet-facing. Browser users authenticate with
sessions, while machines enroll with short-lived one-time links and then use a
workspace-bound machine credential. See [PLAN.md](PLAN.md) for architecture,
protocol, and delivery milestones.

## Run the prototype

Start the proxy and web control plane:

```bash
just stage-artifacts
cargo run -p treer-proxy -- \
  --disable-auth \
  --listen 0.0.0.0:8787 \
  --public-url http://PROXY_HOST:8787
```

`--disable-auth` is intended for local testing. It skips the login screen and
uses a local administrator identity. Omit it and set `ADMIN_PASSWORD` for shared
or deployed servers.

`--public-url` is the URL that other machines can reach. `stage-artifacts`
places the current platform's `treer-agent-host`, `treer-agent-server`, and
`treer` binaries under `dist/<platform>` for the proxy to serve. Release
deployments should stage all required Linux and macOS platform directories or
set `--artifacts-dir` to an equivalent artifact tree. When a platform artifact
is not present locally, the Proxy redirects to the latest Treer GitHub Release;
override its base with `TREER_RELEASE_ARTIFACT_BASE_URL` when mirroring release
assets elsewhere.

The `Build macOS ARM64 artifacts` GitHub workflow builds `darwin-aarch64`
binaries on a native Apple Silicon runner. Manual runs retain the files as a
workflow artifact for 14 days. Pushing a `v*` tag creates or updates that
GitHub Release with the three binaries and a `SHA256SUMS-darwin-aarch64.txt`
file. Ordinary branch pushes do not run the artifact build.

Open the web UI, select a workspace, and choose **Add machine**. The dialog
separates installation from workspace enrollment. The installation command is
public and reusable:

```bash
curl -fsSL 'https://PROXY_HOST/install.sh' | sh
```

The script detects the target platform, installs `treer` to `~/.local/bin` and
the Host and Controller binaries to `~/.local/libexec/treer`. It does not contain
a credential, create workspace configuration, register a service, or start a
process. Override `TREER_INSTALL_DIR` or `TREER_AGENT_SERVER_INSTALL_DIR` when
needed.

The separate connection command contains a 10-minute, single-use enrollment key:

```bash
TREER_ENROLLMENT_KEY='enr_v1_...' \
  "$HOME/.local/libexec/treer/treer-agent-server" connect \
  --proxy 'https://PROXY_HOST/'
```

`connect` decodes the workspace ID from the key, exchanges it for a long-lived
machine credential, uses the current directory as the workspace root, registers
the Host service, and starts it. Linux uses a systemd user service with restart
and linger enabled; macOS uses a per-user LaunchAgent with `KeepAlive`. Override
`TREER_WORKSPACE_ROOT`, `TREER_STATE_DIR`, or `TREER_AGENT_SERVER_LISTEN` when
needed. The first available loopback port starting at `8790` is saved per
workspace.

Installing replacement binaries does not restart a running service.
`service restart-controller` activates an updated Controller without restarting
the Host. The updated Host binary takes effect on the next full service restart.
Existing agents, PTYs, and buffered terminal output stay alive during a
Controller restart while the browser reconnects.

The enrollment key can be used once and contains a versioned encoding of its
workspace ID. The Proxy verifies that embedded workspace against its enrollment
record, then creates a stable server ID and a long-lived credential bound to that
server and workspace.
The credential is stored in the Controller configuration with owner-only file
permissions and is required for both the Controller WebSocket and agent-facing
Proxy API. Production mode requires an HTTPS public URL.

The host administrator manages the service through the agent-server binary, not
the agent-facing `treer` command:

```bash
server="$HOME/.local/libexec/treer/treer-agent-server"
"$server" service status
"$server" service logs --follow
"$server" service restart-controller
"$server" service stop
"$server" service start
"$server" service restart
"$server" service uninstall
```

`restart-controller` is the normal hot-update operation and preserves running
agents. `restart` restarts the long-lived Host itself and therefore terminates
the agents and PTYs owned by that Host.

Add `--workspace WORKSPACE_ID` after `service` when managing a workspace other
than `default`. On Linux, installation prints an actionable warning if systemd
linger cannot be enabled automatically. On macOS, a LaunchAgent starts at user
login; an always-on pre-login LaunchDaemon would require a separate privileged
installation flow.

## Users and invitations

The administrator signs in with username `admin` and the password supplied in
`ADMIN_PASSWORD`. The administrator can create single-use registration links
from **Invite** in the header. Invited users choose their own username and
password; all signed-in users share the same workspaces, machines, agents, and
terminals.

Users, invitations, and sessions are stored in SQLite. Local runs default to
`.treer/proxy.db`; set `TREER_DATABASE_PATH` to put it elsewhere. Changing
`ADMIN_PASSWORD` changes the administrator's next login password without
rewriting existing user accounts.

## Railway

The root `Dockerfile` and `railway.json` make the repository directly
deployable as a Railway service. Railway's injected `PORT` and
`RAILWAY_PUBLIC_DOMAIN` are detected automatically.

1. Create a Railway service from this GitHub repository.
2. Set the required `ADMIN_PASSWORD` service variable.
3. Generate a public domain for the service.
4. Attach a Railway Volume at `/data` so the SQLite users and invitations
   survive deployments.

The image builds and serves Linux agent binaries for its own CPU architecture.
Set `TREER_PROXY_PUBLIC_URL` only when overriding the Railway-generated domain,
and set `TREER_DATABASE_PATH` only when using a volume mount other than
`/data`.

Open `http://PROXY_HOST:8787` to discover servers, create agents, and attach to
their live terminals. The browser terminal supports ANSI colors, alternate
screens, cursor movement, per-keystroke input, paste, and dynamic resize. PTY
input, replay, and live output remain raw bytes from the Host through the
Controller and Proxy to the browser. The Host socket uses length-prefixed binary
frames, and both WebSocket hops use binary frames instead of Base64 JSON payloads.
Agents inherit `TREER_WORKSPACE_ID`,
`TREER_SERVER_ID`, `TREER_AGENT_ID`, and `TREER_AGENT_SERVER_URL`; they can use
the local agent server API to discover or control other agents in the same
workspace.

## Host and Controller

`treer-agent-host` is the stable process boundary. It understands only raw
process commands, PTY input/output, resize, stop, and revisioned output replay.
It does not understand agent kinds, prompts, workspaces, or the Proxy protocol.

`treer-agent-server` is the replaceable Controller. It translates Proxy
commands, detects agent status, exposes the local API, and rebuilds its full
snapshot from the Host after every restart. Mutating Host commands carry stable
operation IDs so a reconnect or retry cannot spawn or stop an agent twice.
Codex and Claude agents start inside the user's interactive shell before their
commands are entered through the PTY. This loads shell configuration such as
`.bashrc` or `.zshrc` before command lookup.

## Workspace network

Every Controller exposes a loopback SOCKS5 proxy and injects its URL into new
agents as `ALL_PROXY`, `all_proxy`, and `TREER_NETWORK_PROXY`. TCP streams are
multiplexed as binary frames over the Controller's existing `/agent/connect`
WebSocket; no machine needs an inbound port. Each stream has an independent
flow-control window, and terminal, transfer, and network frames share the same
authenticated connection.

Network access is denied until a workspace network policy permits a source
machine, destination machine, target host, and port range. Policies can be
managed from the Machines tab. The same panel can create workspace-scoped
virtual hosts. A record maps a stable `.treer` name to a machine, target host,
and optional target port. For example, mapping `api.treer` to `build-machine`,
`127.0.0.1`, port `8080` makes this work without exposing port 8080:

```bash
curl http://api.treer/
curl http://build-machine.treer:8080/
curl http://git.internal.via.build-machine.treer:3000/
```

The network policy must allow the mapped destination, target host, and target
port. Deleting a machine also deletes virtual hosts and policies that point to
it. Virtual host labels are case-insensitive; `via` is reserved for direct
routing.

`build-machine.treer` connects to `127.0.0.1` on that machine.
`host.via.build-machine.treer` connects to `host` from that machine. The first
prototype supplies a standard SOCKS5 layer; programs that ignore proxy
environment variables must be configured to use `TREER_NETWORK_PROXY`.

## Agent collaboration

The `treer` binary talks to the local agent server by default. Managed agents
receive its location in `PATH` and `TREER_BIN`, so they can discover and contact
peers without knowing the proxy address.

```bash
treer whoami
treer agent list
treer agent get reviewer
treer agent rename reviewer code-reviewer
treer machine rename self build-machine
treer machine delete srv_obsolete
treer ssh build-machine
treer ssh build-machine -- cargo test
treer scp results.json build-machine:artifacts/results.json
treer scp -r build-machine:artifacts ./downloaded-artifacts
treer agent attach reviewer
treer agent delete obsolete-helper
treer agent prompt reviewer "Review the parser changes" --wait --timeout 120000
treer agent read reviewer --lines 80
treer agent send-keys reviewer ctrl-c
```

On the machine running an Agent Server, `treer agent attach <target>` opens the
agent's live PTY in the current native terminal. Input, colors, cursor control,
and terminal resize are passed through directly. Press `Ctrl-]` to detach
without stopping the agent. The shorter `treer attach <target>` alias is also
available.

`treer ssh <machine>` opens a new native PTY on another online machine in the
same workspace. It is routed through the local Agent Server and Proxy; the
target does not need an SSH daemon, network address, or SSH keys. The target can
be a server ID, a unique machine name, or `self`/`.`. Use `--cwd <path>` to
select the target working directory, or put a command after `--` for a
non-interactive session:

```bash
treer ssh gpu-worker
treer ssh gpu-worker --cwd project -- cargo test --workspace
```

Remote shells are transient and are not registered as agents. Detaching an
interactive session with `Ctrl-]`, closing the client, or losing the connection
stops that shell and its child process. Non-interactive sessions forward stdin,
stdout, and the remote exit code.

`treer scp` copies regular files or directory trees through the same authenticated
workspace connection. Exactly one operand uses `machine:path`; add `-r` for a
directory:

```bash
treer scp report.json gpu-worker:artifacts/report.json
treer scp gpu-worker:artifacts/report.json ./report.json
treer scp -r results gpu-worker:archive/results
```

Remote paths are relative to the target machine's configured workspace root.
Transfers use binary WebSocket frames, preserve Unix permission bits, verify
declared file sizes and transfer totals, and commit each file with an atomic
rename. Symbolic links and special files are rejected. The current version does
not copy directly between two remote machines.

Targets accept an agent id, a unique agent name, or `self`/`.` from inside a
managed agent. `prompt --wait` waits for observed activity followed by `idle`,
`blocked`, `exited`, or `failed` by default; repeat `--until STATUS` to select
other states. It is state-based coordination, not strict per-prompt turn
correlation.

The original top-level `list`, `prompt`, `read`, and `stop` commands remain as
compatibility aliases. To create a peer:

```bash
treer create --server SERVER_ID --kind command --name shell -- /bin/sh
```

Print the agent skill bundled with the installed binary:

```bash
treer --skill
treer --skills  # accepted alias
```

This does not require a running proxy. The source is
`skills/treer/SKILL.md`, embedded at build time so its instructions match the
CLI version that prints them.

## Checks

```bash
cargo fmt --all -- --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

The same sequence is available as `just check` when `just` is installed.

## License

Copyright 2026 Zijian Zhang.

Treer is licensed under the [Apache License 2.0](LICENSE). Vendored third-party
assets retain their own license terms; see the license files under `web/vendor`.
