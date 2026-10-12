# Codex UI

Codex UI is Treer's single-Agent browser interface for Codex. One command Agent
owns one `codex app-server` process, one Codex thread, one AIS registration, and
one embedded page. There is no thread list or session switcher in the UI.

The private HTTP listener exposes the browser page plus
`treer.agent-interface/v1` prompt, transcript, state, and abort operations.
`GET /v1/transcript` pages by Codex turn (`page`/`cursor`, `limit` default 1).
Treer reaches it through the Agent network bridge; no service record or
published port is required.

From a Treer-managed Agent in this checkout:

```bash
./apps/codex-ui/scripts/apply.sh --name codex-ui
```

The script installs the local Node dependencies, saves the `Codex + UI` launch
profile, creates the first command Agent, and waits for its verified Interface
descriptor. Additional Agents can be created from that profile.

For local development outside a managed Agent:

```bash
npm --prefix apps/codex-ui install
CODEX_UI_CWD="$PWD" npm --prefix apps/codex-ui start
```

The server binds an OS-selected free port on `127.0.0.1` by default. Each Agent
has its own listener; the launcher registers the actual port, not a guessed
port. Set `CODEX_UI_PORT` to pin a private port (a conflict fails startup).

## Resume and recovery

Resume an existing native Codex thread, including one created by the terminal
CLI, without importing or summarizing its context:

```bash
/absolute/path/to/apps/codex-ui/scripts/treer-agent.sh --thread-id THREAD_ID
# Equivalently: CODEX_UI_THREAD_ID=THREAD_ID .../scripts/treer-agent.sh
```

The runtime calls `thread/resume` with `threadId`, the current working directory,
`approvalPolicy: never`, and `sandbox: danger-full-access`. Returned turns appear
in the browser history and turn-based `/v1/transcript` pages. The original
directory need not match a migrated thread's directory. The thread must be
available to this machine's Codex installation/account. Resume failure logs the
thread ID and error, exits without registering an Interface, and never silently
creates a new thread. These permissions grant the Agent the machine account's
authority; Codex UI is not a sandbox.

For a managed Agent, the thread ID is saved atomically with mode 0600 in
`<cwd>/.treer/agents/$TREER_AGENT_ID/codex-ui.json` (Agent directory mode 0700).
An explicit argument overrides `CODEX_UI_THREAD_ID`; either overrides the saved
ID. Otherwise the runtime resumes the saved ID, or creates and saves a thread
only if no record exists. Invalid/unreadable records fail rather than losing
context. `agent restart` and Host cold recovery run the saved command and
resume this same thread. Keep the working directory and Codex's own thread
storage (`CODEX_HOME` or `~/.codex`) available. Local runs without a Treer Agent
ID have no automatic Agent-scoped record.

From **inside the Agent you intend to replace**, first inspect the plan:

```bash
/absolute/path/to/apps/codex-ui/scripts/switch.sh --thread-id THREAD_ID --dry-run
# Then, only when ready to replace this process:
/absolute/path/to/apps/codex-ui/scripts/switch.sh --thread-id THREAD_ID
```

The script verifies its managed identity, saves a self-only startup command
using the absolute launcher path, and restarts `self` in a separate process
session using `setsid` and a double fork. It does not restart the Host or
Controller or alter other Agents. `startup --cwd .` refers to the machine root;
the launcher additionally receives `--cwd <original absolute directory>` to
preserve the Codex working directory and recovery record. Python 3 is required
for switching. Private plans, restart results/errors, and `restart.log` are
under `<cwd>/.treer/agents/$TREER_AGENT_ID/codex-ui-switches/`. A dry-run only
reads identity and prints the plan; it does not save startup, write files, or
restart. A successful restart request does not prove resume succeeded: inspect
the replacement Agent's Interface and logs. The switching shell exits and the
current Agent process is deliberately replaced.

## Checks

```bash
npm --prefix apps/codex-ui ci
npm --prefix apps/codex-ui run check
npm --prefix apps/codex-ui test
# Opt-in: managed Treer environment, real Codex credentials/inference needed.
# Creates two temporary Agents and a temporary Codex + UI recipe profile,
# checks independent ports and restart/context recall, and removes both Agents
# and the profile on completion. Does not restart Host/Controller or self.
npm --prefix apps/codex-ui run test:live
```
