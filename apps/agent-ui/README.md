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
| `claude` | `src/harness/claude.ts` | native Claude Code `--print` stream-json; native resume and background-task events |
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
# Native Claude Code, saved as the "Claude + UI" profile:
./apps/agent-ui/scripts/apply.sh --harness claude --name claude-ui
# Other agents can still use ACP:
./apps/agent-ui/scripts/apply.sh --harness acp --name acp-ui --acp-command my-acp-agent --display-name Agent
```

The script installs the local Node dependencies, saves the launch profile,
creates the first command Agent, and waits for its verified Interface
descriptor. Additional Agents can be created from that profile. The provider
command (`codex`, `claude`, or the ACP agent) must already be
installed and authenticated.

For local development outside a managed Agent:

```bash
npm --prefix apps/agent-ui install
AGENT_UI_CWD="$PWD" npm --prefix apps/agent-ui start
AGENT_UI_HARNESS=claude AGENT_UI_CWD="$PWD" npm --prefix apps/agent-ui start
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
/absolute/path/to/apps/agent-ui/scripts/treer-agent.sh --harness claude --session-id SESSION_ID
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
# A Claude Code TUI Agent, resuming its own native session:
/absolute/path/to/apps/agent-ui/scripts/switch.sh --harness claude --session-id SESSION_ID
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

## Native Claude Code

Select `AGENT_UI_HARNESS=claude` or `--harness claude` on the launcher, installer,
or switch script. `CLAUDE_BIN` overrides the executable (default `claude`);
`CLAUDE_MODEL` optionally selects the initial model. Claude must already be
authenticated. This adapter talks directly to Claude Code, without ACP or the
legacy `apps/claude-ais` sidecar. It shares the same HTTP server and page.

The process uses `--print --verbose --output-format stream-json --input-format
stream-json --include-partial-messages --dangerously-skip-permissions`, plus
`--resume ID` for an existing session or `--session-id UUID` for a new one.
Initialization, interrupt, and model changes use stream-json `control_request`
messages (`initialize`, `interrupt`, `set_model`). New sessions are saved to
`agent-ui.json` only when Claude emits `system/init` after the first prompt:
before that, the UUID is reserved and no resumable native history exists yet.
An existing saved or explicit ID is always resumed; missing, ambiguous, invalid,
or rejected history fails clearly without creating a replacement session.

Claude does not replay history over stdout on resume. The adapter reads
`$CLAUDE_CONFIG_DIR/projects/<cwd encoding>/<session>.jsonl` (default
`~/.claude/projects`), including user/assistant text, thinking, tool uses and
results. When the working directory changes, an unambiguous session file in
another project directory can be loaded. Thinking is reasoning, Bash is command
execution, Edit/Write/MultiEdit are file changes with unified diffs, TodoWrite is
a checklist, and other tools remain tool calls. Partial text/thinking/tool inputs
update the same history items. Structured patches from tool results refine diffs.

**Steering was tested with Claude Code 2.1.296 in disposable directories.** Sending
an ordinary stream-json user message while a foreground Bash command ran caused
it to be consumed at the next tool boundary, before the original final reply;
the reply incorporated the mid-tool update in the same turn. A top-level
`priority: "now"` experiment split this into two result envelopes and gave no
earlier tool interruption. The adapter therefore sends standard user messages
without undocumented priority fields and advertises `steer: true`. Steering
does not interrupt an executing tool or guarantee immediate token-level changes.
Use explicit interrupt to cancel; queue mode always waits for the turn's `result`.

Background task state comes from `background_tasks_changed`, `task_started`,
`task_updated`, and `task_notification`. All running tasks and the most recent
100 finished tasks stay visible; none are restored after restart. Task outputs
are read only from the working directory or this session's Claude task-output
directory under the local temporary roots (`claude-<uid>/<encoded cwd>/<session>/tasks`).
Model choices come from Claude's initialize response. Reasoning effort is not
exposed by this adapter.

## Browser interface

When supported by the harness, the header's **N running** button opens background
tasks with their status, duration, exit code, and summary. Expand a task to load
its output tail; running tasks refresh every three seconds while open. On narrow
screens, tasks and the file explorer share the drawer. Task content renders as text.

Enter sends a message when idle and steers the current run when busy. Tab queues
a nonempty draft; Shift+Enter adds a newline. While running, separate **Steer**
and **Queue** buttons support touch input. Harnesses without steering automatically
queue messages and display a notice. Queued messages appear above the input and
can be canceled. Failed submissions retain the draft for retry.

Harnesses with Goal support show **Set goal**, or the current objective, status,
token/time usage, and optional token budget progress. Edit, pause/resume (when
supported), and clear controls appear on the goal bar; clearing requires inline
confirmation. `/goal <objective>` sets a goal and `/goal clear` requests clearing
instead of sending a chat message. Completed goals also produce a timeline notice.

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

## Prompt queue and background-task contract

State and WebSocket messages also include `goal` (a Goal or null), with
`{objective, status, tokenBudget, tokensUsed, timeUsedSeconds}`. Status is
`active|paused|blocked|usageLimited|budgetLimited|complete`; numeric metrics can
be null. `harness.capabilities.goal` is null when unsupported, otherwise
`{pause: boolean, budget: boolean}`. `PUT /api/goal` accepts optional
`objective`, `status`, and `tokenBudget` (a positive integer or null); at least
one field is required. `DELETE /api/goal` clears the goal. Both return updated
state. Unsupported adapters return 501 and hide goal controls.

Codex uses native `thread/goal/set|get|clear`, with user provenance on mutations,
and consumes `thread/goal/updated|cleared`. Resume fetches the native goal again.
It supports pause/resume, explicit status, and token budgets. Older app-server
versions without these methods disable the capability without blocking startup.

Claude uses the native `/goal <condition>` and `/goal clear` local commands.
Setting a goal starts work immediately. Plain chat `/goal ...` commands update
the same state. Claude 2.1.296 has no pause/resume or token-budget controls:
`/goal pause` and `/goal resume` set those literal words as new conditions,
so the adapter rejects status/budget updates with 501 rather than simulating
them. Goal commands sent during work may wait for the next native turn boundary.
Acknowledgement comes from native synthetic local-command messages. Completion
comes from a matching successful `stop_hook_summary` in the session JSONL
(no output/errors and no prevented continuation), not from ordinary assistant
prose or a successful result. A stop-hook block-cap override does not count as
completion. Claude removes the native condition on success; the UI retains its
completed record until clear or replacement. A bare `/goal` query does not erase
that record. Resume rebuilds the record from native local-command and stop-hook
history. Live usage sums per-result input/output/cache token counters and
`duration_ms`, not cumulative `modelUsage`; a private usage checkpoint beside
`agent-ui.json` preserves those totals across resume. Without that checkpoint,
historical assistant usage is reconstructed and elapsed time is unknown (null).

`npm run test:goal:live` runs both installed native CLIs in disposable directories,
uses the real browser to set/complete/clear goals, and saves desktop/mobile
screenshots in a temporary directory. It requires authenticated Codex and Claude
installations and does not touch managed Agent processes or sessions.

`GET /api/state` and WebSocket `state` messages include `harness.capabilities`
(`steer`, `backgroundTasks`), `backgroundTasks`, and `queue`. Queue entries are
`{id, text, createdAt}`. `POST /api/prompt` accepts `{prompt, mode?: "steer"|"queue"}`;
`POST /v1/prompts` accepts optional `delivery: "steer"|"queue"` alongside
`operation_id` and `text`; its existing Core `mode: "prompt"` field is ignored.
The default delivery is `steer` for compatibility. Replies include `submission`
with `disposition` (`sent`, `queued`, `steered`), effective `mode`, and `downgraded`.
Unsupported steering becomes queueing with an `explanation`. An idle harness
receives the prompt immediately; queued prompts are sent FIFO after each turn
returns to idle. Errors pause draining. `DELETE /api/queue/:id` removes a waiting
prompt (404 if absent). Queues are memory-only and do not survive restart.

Background tasks are `{id, kind, description, status, startedAt, endedAt,
exitCode, summary, outputFile?}`. Kind is `shell|agent|monitor|other`; status is
`running|completed|failed|stopped`. Timestamps are ISO strings or null; exit code
and summary can be null. `GET /api/tasks/:id/output?tail=65536` returns
`{id, content, size, truncated}` from a registered task's output file. `tail` is a
positive byte count capped at 64 KiB; unregistered task IDs return 404. Reads
must remain in the adapter's trusted output roots, reject external symlinks and
non-regular files, and never accept an output path from a client.

## Checks

```bash
npm --prefix apps/agent-ui ci
npm --prefix apps/agent-ui run check
npm --prefix apps/agent-ui test
# Unit tests use fake Codex, ACP, and native Claude processes.
# Browser regression and desktop/390px screenshots, with an isolated fake Codex.
# No inference, managed Agent mutation, or restart. Chromium is needed once:
npm --prefix apps/agent-ui exec -- playwright install chromium
CODEX_UI_SCREENSHOTS=/tmp/agent-ui-screenshots npm --prefix apps/agent-ui run test:browser
# Opt-in: real Claude credentials/inference, new sessions in a temporary cwd only.
# Covers start/model, tool-boundary steer, queue, background output, interrupt,
# JSONL resume/continuation, and missing-session rejection; no Treer mutations.
npm --prefix apps/agent-ui run test:claude
# Low-level protocol probes (also only new temporary sessions):
node apps/agent-ui/tests/probe-claude.mjs
CLAUDE_PROBE_PRIORITY=now node apps/agent-ui/tests/probe-claude.mjs
CLAUDE_PROBE_BACKGROUND=1 node apps/agent-ui/tests/probe-claude.mjs
# Opt-in: managed Treer environment, real Codex credentials/inference needed.
# Creates two temporary Agents and a temporary Codex + UI recipe profile,
# checks independent ports and restart/context recall, and removes both Agents
# and the profile on completion. Does not restart Host/Controller or self.
npm --prefix apps/agent-ui run test:live
```
