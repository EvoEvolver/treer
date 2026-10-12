# Agent Launcher

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

## Installation and updates

Node.js 22+, npm, Git, tar and the Treer CLI must be on PATH. Install provider
CLIs and authenticate them separately; the launcher does not read another
provider's credentials. From a checkout, save one or more profiles:

```bash
./launchers/agent/install.sh --agent codex --agent claude
# Install a committed local revision instead of fetching upstream:
./launchers/agent/scripts/apply.sh --agent codex
# Update to a full commit (or a branch/tag resolved to a full commit):
./launchers/agent/install.sh --ref COMMIT --agent codex
treer agent admin profile show "Codex + UI"
# Creating an Agent is a separate, explicit action:
treer agent admin profile launch "Codex + UI" --name reviewer
```

Omit `--agent` to save all six profiles from `profiles.json`: Codex, Claude,
Gemini, Grok, Cursor and OpenCode. Codex/Claude use native adapters; the others
share ACP. Missing provider binaries are checked at runtime, not installed by
this command. Custom ACP profiles can pass `--harness acp --acp-command BIN`
and repeat `--acp-arg VALUE` to `scripts/treer-agent.sh`.

The installer sparse-fetches only `launchers/agent` into
`<machine.root>/.treer/launchers/agent/<full commit>/`, runs lockfile-pinned
`npm ci --omit=dev`, then atomically changes `current`. Profiles use its absolute
launcher path and the caller's machine-relative cwd (override with `--cwd`).
Every saved command/argv/cwd is read back and verified. Failed installs restore
previous profiles (or remove newly created ones), then restore the previous
`current`; previous version directories remain available. If profile rollback
itself fails, the error is explicit and the candidate entry point stays usable
for remaining profiles. Versions
are immutable; a running launcher resolves its physical directory once, so
updates cannot replace its assets. Installation never creates or restarts an
Agent. `--source CHECKOUT` archives committed files, ignoring dirty changes;
`--repository URL` supports self-hosted/local Git mirrors. Run the script from
its checkout, rather than piping it into a shell.

For local development outside a managed Agent:

```bash
npm --prefix launchers/agent install
AGENT_UI_CWD="$PWD" npm --prefix launchers/agent start
AGENT_UI_HARNESS=claude AGENT_UI_CWD="$PWD" npm --prefix launchers/agent start
AGENT_UI_HARNESS=acp AGENT_UI_ACP_COMMAND=claude-agent-acp AGENT_UI_CWD="$PWD" npm --prefix launchers/agent start
```

The server binds an OS-selected free port on `127.0.0.1` by default. Each Agent
has its own listener; the launcher registers the actual port, not a guessed
port. Set `AGENT_UI_PORT` to pin a private port (a conflict fails startup).
`AGENT_UI_*` settings also accept the older `CODEX_UI_*` names. Saved startup
commands must use `launchers/agent/scripts/treer-agent.sh` or an installed
version's launcher, rather than retired `apps/` paths.

## Resume and recovery

Resume an existing native session, including one created by the terminal CLI
(a Codex thread ID, or an ACP session ID such as a Claude Code session), without
importing or summarizing its context:

```bash
/absolute/path/to/launchers/agent/scripts/treer-agent.sh --session-id THREAD_ID
/absolute/path/to/launchers/agent/scripts/treer-agent.sh --harness claude --session-id SESSION_ID
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
/absolute/path/to/launchers/agent/scripts/switch.sh --session-id THREAD_ID --dry-run
# Then, only when ready to replace this process:
/absolute/path/to/launchers/agent/scripts/switch.sh --session-id THREAD_ID
# A Claude Code TUI Agent, resuming its own native session:
/absolute/path/to/launchers/agent/scripts/switch.sh --harness claude --session-id SESSION_ID
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
authenticated. This adapter talks directly to Claude Code without ACP and
shares the same HTTP server and page.

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
Model choices come from Claude's initialize response, including its
`resolvedModel` mapping from aliases to actual IDs. The picker uses these actual
IDs, deduplicates Default when it resolves to an existing choice, and always
includes the current `system/init.model` even when absent from the catalog.
Before the first init, a configured initial model or Claude's reported Default
resolution is shown. Resume uses the most recent actual model from JSONL until
the next init confirms it. Older CLIs without a catalog fall back to unversioned
`default`, `sonnet`, `opus`, and `haiku` aliases plus the reported current model;
there is no hardcoded model-version list. Hovering the picker shows the actual ID.

**Model and effort changes were tested with Claude Code 2.1.296 in disposable
sessions.** `set_model` works during a foreground Bash command without interrupting
it or restarting Claude. The next inference request, including the continuation
after a tool in the same turn, can use the new model. Settings changes are
serialized; prompts submitted while a change is pending wait for its native
acknowledgement. A rejected change retains the prior setting and pending prompts
still send. Queue mode remains FIFO at result boundaries, without session changes.

The model catalog's `supportsEffort` and `supportedEffortLevels` drive the effort
picker. An empty `apply_flag_settings` overlay probes live protocol support;
unsupported CLIs/models hide the picker on desktop and mobile. Supported models
use `apply_flag_settings` with `settings: {effortLevel: LEVEL}`; null resets to
the native default. These changes are session-only and do not write user settings.
Native `/model` confirmed the changed effort in the real test. `/effort medium`
also works in stream-json and its native acknowledgement updates the page;
`set_effort` is not a supported control subtype. Managed policy may clamp effort.
The default effort is shown as Default until an explicit setting or native
acknowledgement identifies it. Narrow screens put supported controls on a second
header row instead of hiding the effort picker.

`npm run test:claude:settings:fake` checks the actual browser at desktop and 390px
with supported and unsupported effort protocols. `npm run test:claude:settings`
uses authenticated Claude in a new temporary session: set the initial model,
switch during a foreground tool, retain a queued prompt, verify its assistant
model in native JSONL, and confirm effort via `/model`. Both save screenshots to
the printed temporary directory and leave managed Agent processes untouched.

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
`{id, text, createdAt, sender, position}`. Sender paths and one-based FIFO
positions appear in the UI. `POST /api/prompt` accepts `{prompt, mode?: "steer"|"queue"}`;
`POST /v1/prompts` accepts optional `delivery: "steer"|"queue"` alongside
`operation_id` and `text`; its existing Core `mode: "prompt"` field is ignored.
The browser defaults to `steer`; AIS defaults to `queue`. AIS accepts attested
`sender: {agent_id,path,name}` and optional `notify: {on,to?}` only from the
registered Controller's matching `x-treer-agent-id` and
`x-treer-interface-instance` headers, without a browser Origin. Public browser
prompts cannot supply sender identity. Standalone AIS clients cannot supply
sender/notification metadata. Replies include `submission` with stable `id`,
`disposition` (`sent`, `queued`, `steered`) and nullable `position`; browser
replies also report effective `mode` and `downgraded`.
Unsupported steering becomes queueing with an `explanation`. An idle harness
receives the prompt immediately; queued prompts are sent FIFO after each turn
returns to idle. Dispatch failures become terminal `failed` submissions rather
than silently retrying inference. Harness errors pause draining.
`DELETE /api/queue/:id` removes a waiting prompt (404 if absent).

The shared server queue journals to `<cwd>/.treer/agents/$TREER_AGENT_ID/queue.json`
(or `standalone` without an Agent identity), mode 0600. Each bounded journal
commit uses a private temporary file, file fsync, rename and directory fsync
before accepting work. Symlink storage paths and corrupt journals are rejected;
a storage failure closes dispatch rather than running uncommitted work. On
restart pending messages retain FIFO order; uncertain running submissions become
`interrupted`, never resubmitted. `AGENT_UI_QUEUE_LIMIT` (1–4096, default 256)
bounds pending/running work plus undelivered notifications. Full queues return
429. Text is limited to 64 KiB; terminal receipts keep bounded previews, and the
most recent 1,024 delivered terminal receipts remain available for deduplication.
Reusing an operation ID with different text/delivery/source/notification is 409.

Controller-facing `GET /v1/queue` lists pending `submissions`;
`GET /v1/submissions/:id` returns the correlated submission, including terminal
state; `DELETE /v1/queue/:id` cancels only pending work (`interrupted`), with 409
for running work. `/v1/status` adds `busy` and `queue_length` (pending only).
The manifest advertises `prompt.queue`, `prompt.notify`, `prompt.queue.read`.
Submission previews are bounded to 1,024 UTF-8 bytes (input) and 4,096 bytes
(last assistant message). Source paths/times are prepended to model prompts and
shown on user messages, so the native transcript also retains attribution.

By default an attested sender receives `done`, `failed`, and `interrupted`
notifications. Set `notify.on` to select outcomes (empty disables), and `to` to
override the recipient; operators must explicitly supply a recipient. Terminal
notifications use the Agent's own inherited identity via `treer message send`,
including submission/turn IDs, outcome, error and assistant preview. A durable
outbox retries with logged errors and exponential backoff capped at 60 seconds,
using `prompt-submission/<id>/terminal` as its idempotency key. A notification
failure never reruns the task. Pending notifications survive restart. Queue
state is local to the Agent cwd and is not included in portable session exports.

Background tasks are `{id, kind, description, status, startedAt, endedAt,
exitCode, summary, outputFile?}`. Kind is `shell|agent|monitor|other`; status is
`running|completed|failed|stopped`. Timestamps are ISO strings or null; exit code
and summary can be null. `GET /api/tasks/:id/output?tail=65536` returns
`{id, content, size, truncated}` from a registered task's output file. `tail` is a
positive byte count capped at 64 KiB; unregistered task IDs return 404. Reads
must remain in the adapter's trusted output roots, reject external symlinks and
non-regular files, and never accept an output path from a client.

## Add a harness adapter

Prefer an ACP profile when the provider already speaks ACP. Otherwise implement
`HarnessRuntime` from `src/runtime.ts` in `src/harness/<name>.ts`, then select it
in `src/index.ts` and the launcher scripts. The adapter owns only subprocess
protocol, session resume, event/history mapping and capability reporting. Reuse
`src/harness/diff.ts` for file changes and the shared models for plans/tools.
Keep the HTTP API, file browser, prompt queue and browser UI shared. Unsupported
capabilities stay false/null and requested resume failures never create a new
conversation. Add fake-backend tests with `tests/hermetic.mjs` before a profile
in `profiles.json`; installation must not start or authenticate the provider.

The retired Pi/DSH interfaces, Rust ACP runtime, Remote Codex UI and TOWER
capture are no longer bundled. See the
[migration decision](../../docs/research/2026-10-12-single-agent-launcher.md)
for the explicit feature removals. The workspace TOWER App remains separate.

### Session transfer contract

`src/session-transfer.ts` defines `SessionBundle` with schema
`treer.agent-session/v1`: `harness`, confirmed native `sessionId`, original `cwd`,
`exportedAt`, and `files`. Each file has a `session|memory|project` scope, safe
relative POSIX `path`, `encoding: "base64"`, `content` and SHA-256 of its bytes.
Adapters optionally implement `exportSession()` and
`importSession(bundle, {directory, sessionId?})`; `harness.capabilities.sessionTransfer` advertises actual
support (omitted means unsupported). Native Codex and Claude implement both;
ACP does not advertise transfer and returns 501 without provider support.

Only native session files, this project's memory and explicit instruction files
belong in a bundle. Never include provider auth, settings/configuration, Treer
identity, environment, queues or arbitrary working-tree files. This is a file
allowlist, not automatic redaction of secrets a user put into conversations.
Import creates a dedicated new/empty directory, validates every path/hash/size,
and refuses overwrite or symlink escapes. Import allocates a fresh native UUID
(or accepts an explicit fresh UUID), rewriting native session identity/cwd so
resume cannot select the original conversation. It returns `{sessionId, cwd,
environment}`; environment contains only provider storage selectors. It neither
switches nor restarts the current Agent. The caller authenticates the restored
provider separately and launches using the returned cwd/session/storage settings.

The offline CLI is `scripts/session.sh export --harness codex|claude --session-id ID --cwd DIR`,
writing a `SessionBundle` JSON to stdout, and `scripts/session.sh import
--directory NEW_DIR [--session-id NEW_UUID]`, reading that JSON on stdin and
writing `ImportedSession` JSON to stdout. `--input FILE` may replace stdin.
Diagnostics go to stderr; nonzero exit means failure. Current-session HTTP
export is `GET /api/session/export`; unsupported adapters return 501. Export
requires a confirmed native session file; a newly allocated Claude UUID without
any persisted messages is not yet exportable. Export while idle; a file changing
during the snapshot produces a retryable error.

The allowlist includes the native JSONL (Claude subagent JSONL too), Claude's
project auto-memory Markdown, project `.codex/memory/**/*.md` and
`.claude/memory/**/*.md`, and `AGENTS.md`, `CLAUDE.md`, `MEMORY.md` and
`.claude/CLAUDE.md`. Limits are 64 MiB of decoded bytes and 1,024 files. No provider
configuration, auth, shell snapshots, arbitrary project files, queue or running
background processes are transferred. Restore project code separately.

Codex imports use a private `.harness/codex` under the destination and return
`CODEX_HOME`. Only the destination machine's existing `auth.json` and
`config.toml` are referenced by local symlinks; neither enters the archive.
Claude imports use a fresh project entry in the destination machine's native
`CLAUDE_CONFIG_DIR/projects` store, retaining local login and configuration.
The returned `CLAUDE_CONFIG_DIR` refers to that local store. Native IDs and cwd
fields are rewritten, including subagent records; historical message contents
remain unchanged. Imports never launch a process or mutate the current session.

## Checks

```bash
npm --prefix launchers/agent ci
npm --prefix launchers/agent run check
npm --prefix launchers/agent test
# Unit tests use fake Codex, ACP, and native Claude processes.
# Browser regression and desktop/390px screenshots, with an isolated fake Codex.
# No inference, managed Agent mutation, or restart. Chromium is needed once:
npm --prefix launchers/agent exec -- playwright install chromium
CODEX_UI_SCREENSHOTS=/tmp/agent-ui-screenshots npm --prefix launchers/agent run test:browser
# Opt-in: real Claude credentials/inference, new sessions in a temporary cwd only.
# Covers start/model, tool-boundary steer, queue, background output, interrupt,
# JSONL resume/continuation, and missing-session rejection; no Treer mutations.
npm --prefix launchers/agent run test:claude
# Opt-in: new native Codex/Claude sessions, export/import with a fresh UUID,
# provider resume and continuation. Needs both providers logged in locally.
npm --prefix launchers/agent exec -- tsx tests/live-session-transfer.ts
# Low-level protocol probes (also only new temporary sessions):
node launchers/agent/tests/probe-claude.mjs
CLAUDE_PROBE_PRIORITY=now node launchers/agent/tests/probe-claude.mjs
CLAUDE_PROBE_BACKGROUND=1 node launchers/agent/tests/probe-claude.mjs
# Opt-in: managed Treer environment, real Codex credentials/inference needed.
# Creates two temporary Agents and a temporary Codex + UI recipe profile,
# checks independent ports and restart/context recall, and removes both Agents
# and the profile on completion. Does not restart Host/Controller or self.
npm --prefix launchers/agent run test:live
```
