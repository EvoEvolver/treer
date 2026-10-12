# Agent UI

Agent UI is Treer's single-Agent browser interface for coding agents. One
command Agent owns one harness session, one AIS registration, and one embedded
page. There is no thread list or session switcher in the UI.

The page, HTTP server, `treer.agent-interface/v1` endpoints, file browser,
session persistence, and switch script are shared. Each harness is a small
adapter behind `HarnessRuntime` (`src/runtime.ts`) that starts or resumes a
session, sends prompts, interrupts, changes settings, and maps its events to the
shared history model (`src/model.ts`):

| Harness | Adapter | Talks to |
| --- | --- | --- |
| `codex` (default) | `src/harness/codex.ts` | the local `codex app-server` over stdio; native thread resume |
| `acp` | `src/harness/acp.ts` | any [Agent Client Protocol](https://agentclientprotocol.com) agent, such as `claude-agent-acp`, Gemini, Grok, Cursor, or OpenCode |

Add a harness by implementing `HarnessRuntime`; do not copy the UI or server.
ACP agents run their own file and terminal tools. The adapter advertises no
client `fs`/`terminal` capabilities, approves permission requests, switches the
session to the agent's no-prompt mode (for example `bypassPermissions`), and
renders `session/update` messages, thoughts, tool calls, diffs, and plans.
Models and reasoning levels come from the agent's `configOptions`.

The private HTTP listener exposes the browser page plus prompt, transcript,
state, and abort operations. `GET /v1/transcript` pages by turn (`page`/`cursor`,
`limit` default 1). Treer reaches it through the Agent network bridge; no
service record or published port is required.

From a Treer-managed Agent in this checkout:

```bash
./apps/agent-ui/scripts/apply.sh --name codex-ui
# An ACP agent, saved as the "Claude + UI" profile:
./apps/agent-ui/scripts/apply.sh --name claude-ui --acp-command claude-agent-acp --display-name Claude
```

The script installs the local Node dependencies, saves the launch profile,
creates the first command Agent, and waits for its verified Interface
descriptor. Additional Agents can be created from that profile. The provider
command (`codex`, or the ACP agent such as `claude-agent-acp`) must already be
installed and authenticated.

For local development outside a managed Agent:

```bash
npm --prefix apps/agent-ui install
AGENT_UI_CWD="$PWD" npm --prefix apps/agent-ui start
AGENT_UI_HARNESS=acp AGENT_UI_ACP_COMMAND=claude-agent-acp AGENT_UI_CWD="$PWD" npm --prefix apps/agent-ui start
```

The server binds an OS-selected free port on `127.0.0.1` by default. Each Agent
has its own listener; the launcher registers the actual port, not a guessed
port. Set `AGENT_UI_PORT` to pin a private port (a conflict fails startup).
`AGENT_UI_*` settings also accept the older `CODEX_UI_*` names, and
`apps/codex-ui/scripts/` forwards Agents saved with the old path.

## Resume and recovery

Resume an existing native session, including one created by the terminal CLI
(a Codex thread ID, or an ACP session ID such as a Claude Code session), without
importing or summarizing its context:

```bash
/absolute/path/to/apps/agent-ui/scripts/treer-agent.sh --session-id THREAD_ID
/absolute/path/to/apps/agent-ui/scripts/treer-agent.sh --harness acp \
  --acp-command claude-agent-acp --name Claude --session-id SESSION_ID
# Equivalently: AGENT_UI_SESSION_ID=ID .../scripts/treer-agent.sh (--thread-id is an alias)
```

The Codex adapter calls `thread/resume` with `threadId`, the current working directory,
`approvalPolicy: never`, and `sandbox: danger-full-access`. Returned turns appear
in the browser history and turn-based `/v1/transcript` pages. The original
directory need not match a migrated thread's directory. The thread must be
available to this machine's Codex installation/account. Resume failure logs the
thread ID and error, exits without registering an Interface, and never silently
creates a new thread. These permissions grant the Agent the machine account's
authority; Agent UI is not a sandbox. The ACP adapter calls `session/load` (the
agent must advertise `loadSession`), renders the replayed history, and likewise
fails instead of creating a new session.

For a managed Agent, the thread ID is saved atomically with mode 0600 in
`<cwd>/.treer/agents/$TREER_AGENT_ID/agent-ui.json` (Agent directory mode 0700;
an older `codex-ui.json` is renamed on first start). An explicit argument
overrides `AGENT_UI_SESSION_ID`; either overrides the saved
ID. Otherwise the runtime resumes the saved ID, or creates and saves a thread
only if no record exists. Invalid/unreadable records fail rather than losing
context. `agent restart` and Host cold recovery run the saved command and
resume this same thread. Keep the working directory and Codex's own thread
storage (`CODEX_HOME` or `~/.codex`) available. Local runs without a Treer Agent
ID have no automatic Agent-scoped record.

From **inside the Agent you intend to replace**, first inspect the plan:

```bash
/absolute/path/to/apps/agent-ui/scripts/switch.sh --session-id THREAD_ID --dry-run
# Then, only when ready to replace this process:
/absolute/path/to/apps/agent-ui/scripts/switch.sh --session-id THREAD_ID
# A Claude Code TUI Agent, resuming its own session through ACP:
/absolute/path/to/apps/agent-ui/scripts/switch.sh --harness acp \
  --acp-command claude-agent-acp --name Claude --session-id SESSION_ID
```

The script verifies its managed identity, saves a self-only startup command
using the absolute launcher path, and restarts `self` in a separate process
session using `setsid` and a double fork. It does not restart the Host or
Controller or alter other Agents. `startup --cwd .` refers to the machine root;
the launcher additionally receives `--cwd <original absolute directory>` to
preserve the working directory and recovery record. Python 3 is required
for switching. Private plans, restart results/errors, and `restart.log` are
under `<cwd>/.treer/agents/$TREER_AGENT_ID/agent-ui-switches/`. A dry-run only
reads identity and prints the plan; it does not save startup, write files, or
restart. A successful restart request does not prove resume succeeded: inspect
the replacement Agent's Interface and logs. The switching shell exits and the
current Agent process is deliberately replaced.

## Browser interface

Use the **Browse files** button in the header to open the file explorer. Folders
load only when expanded. Selecting a file opens a read-only text preview; on
narrow screens the drawer has Browse/Preview tabs and a close button to return
to the conversation. Refresh reloads expanded directories and the selected
preview. File contents, names, paths, and diffs render as text, never as HTML.

File-change cards expand to show each changed path and its unified diff, with
additions and deletions colored separately (ACP agents report old and new
text; the adapter renders them as unified diffs relative to the working
directory). Plans (Codex plan/todo items and `turn/plan/updated`, ACP `plan`
updates) appear as checklists with pending, active,
and completed steps. Expanded activity cards stay open during state updates.

The page honors `?theme=light|dark` and theme updates sent by its parent window
as `{type: "treer:embed-theme", theme: "light"|"dark"}`. Without a valid explicit
theme it follows the system preference, including subsequent system changes.
All browser requests and assets are relative to the Interface page.

The additional read-only JSON routes are:

| Route | Response |
| --- | --- |
| `GET /v1/files/tree?path=` | `{path, entries: [{name, path, kind, size, has_children}]}` for one directory; empty path is the working directory |
| `GET /v1/files?path=src/main.ts` | `{path, name, content, size, truncated}`; content is at most the first 64 KiB of file bytes, decoded as UTF-8 |

Paths are resolved inside `AGENT_UI_CWD` (the runtime's working directory).
Parent (`..`) components and paths/symlinks outside that root are rejected.
Internal symlinks are usable; external and broken symlinks are omitted from
directory listings. Only regular files can be previewed. `truncated: true`
indicates the file exceeds 64 KiB; `size` is the original byte size. Errors are
JSON with `error.code` and `error.message` (400 invalid type, 403 denied, 404
missing, 405 non-GET method). These routes expose local working-directory data
through the same private AIS listener and Treer access boundary as the UI;
they add no sandbox or separate authentication scheme.

## Checks

```bash
npm --prefix apps/agent-ui ci
npm --prefix apps/agent-ui run check
npm --prefix apps/agent-ui test
# Unit tests cover both adapters (a fake Codex and a fake ACP agent).
# Browser regression and desktop/390px screenshots, with an isolated fake Codex.
# No inference, managed Agent mutation, or restart. Chromium is needed once:
npm --prefix apps/agent-ui exec -- playwright install chromium
CODEX_UI_SCREENSHOTS=/tmp/agent-ui-screenshots npm --prefix apps/agent-ui run test:browser
# Opt-in: managed Treer environment, real Codex credentials/inference needed.
# Creates two temporary Agents and a temporary Codex + UI recipe profile,
# checks independent ports and restart/context recall, and removes both Agents
# and the profile on completion. Does not restart Host/Controller or self.
npm --prefix apps/agent-ui run test:live
```
