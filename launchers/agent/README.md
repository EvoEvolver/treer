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
All three adapters are unattended, full-authority launch modes: Codex uses
`approvalPolicy: never` and `danger-full-access`, Claude uses
`--dangerously-skip-permissions`, and ACP approves permission requests and
selects a full-access mode where offered. These are deliberate defaults, not
a filesystem or execution sandbox. Use a separate OS account/container/VM for
untrusted tasks; no browser confirmation gate protects the machine here.
ACP agents run their own file and terminal tools. The adapter advertises no
client `fs`/`terminal` capabilities, approves permission requests, switches the
session to the agent's no-prompt mode (for example `bypassPermissions`), and
renders `session/update` messages, thoughts, tool calls, diffs, and plans.
Models and reasoning levels come from the agent's `configOptions`.

The private HTTP listener exposes the browser page plus prompt, transcript,
state, and abort operations. `GET /v1/transcript` pages by turn (`page`/`cursor`,
`limit` default 1). Treer reaches it through the Agent network bridge; no
service record or published port is required.

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
Update Proxy/Controller to the forwarding contract before this strict launcher;
old Controller calls fail with an explicit 403, without browser fallback.

The markers are public browser-defense provenance, not secret credentials or
local-process authentication. Native clients without browser headers remain
trusted on loopback; same-account programs can forge markers. The UI tunnel
authorizes the interface as a whole, not each arbitrary UI command through
semantic Core Policy. Do not expose or port-forward the listener; use external
OS/network isolation for mutually untrusted processes. Direct browser access
is intentionally refused; use the authenticated Treer UI tunnel.

The harness inherits its Agent environment, including its own Treer workload
credential so agent tools can use Treer. This is intentional delegation, not
credential hiding. Transcript, provider stderr, queue previews, task output,
completion notifications and session/memory archives can contain secrets from
commands or conversations; no general content redaction is provided. Session
export excludes known credential/configuration files, not secrets embedded in
allowed conversation or memory files. Never publish an archive as public data.

HTTP JSON bodies are capped at 128 KiB while streaming, WebSocket client
payloads at 16 KiB. Malformed JSON returns a generic 400 without echoing its
contents; ordinary request-error logs retain only the error class. These bounds
do not limit concurrent requests, accumulated transcripts or model tool output.

## Installation and updates

### Local prepared bundles (launcher-only delivery)

An operator can deliver a launcher change without rebuilding/restarting the
Controller. From a trusted checkout, build an offline payload with
`node launchers/agent/scripts/prepare-bundle.mjs --output /absolute/prepared --source-commit FULL_COMMIT`.
Review its source and independently record SHA-256 of `bundle-manifest.json`.
Then use an explicit trusted Node.js 22+ executable and Treer CLI:

```sh
/absolute/node launchers/agent/scripts/bundle-launcher.mjs install \
  --bundle /absolute/prepared --sha256 MANIFEST_SHA256 --treer /absolute/treer
/absolute/node launchers/agent/scripts/bundle-launcher.mjs status --treer /absolute/treer
/absolute/node launchers/agent/scripts/bundle-launcher.mjs rollback --treer /absolute/treer
```

`update` has the same bundle arguments as `install`; `rollback --version
sha256-HASH` can choose a retained version. Installation reuses the official
installer's contract checks, immutable content versions, profile ownership
receipts, save/readback transaction and atomic `current.json` pointer. It neither
creates nor restarts Agents. It saves the normal six profiles unless narrowed
with repeated `--agent` or explicitly disabled with `--skip-profiles` (useful for
isolated previews). `--root ABS` chooses a separate physical installation root
inside the enrolled machine root; paths outside that boundary are rejected;
otherwise the authenticated Controller's enrolled root is used. There are no
Git/npm/network dependencies at installation time, and no shell requirement on
Windows. Prepared bundles must be runtime-ready, regular files only, and match
the supplied manifest hash and every manifest entry. Hashes provide integrity,
not authorship: this is **local operator-approved delivery**, not an officially
signed Controller release. Never run this installer from an unreviewed bundle.
The running Controller's protocol, capabilities/features and security floor
must still be compatible; this channel cannot deliver Core changes.

For a staged switch, put explicit Agent IDs/tree paths in a JSON array and name
one disposable preview target from that list. First run the plan without apply:

```sh
/absolute/node launchers/agent/scripts/bundle-launcher.mjs rollout \
  --bundle /absolute/prepared --sha256 MANIFEST_SHA256 --treer /absolute/treer \
  --targets /absolute/targets.json --preview ag_PREVIEW --rollout-id preview-001
# Repeat exactly the same command with --apply to execute the recorded plan.
```

A healthy baseline launcher must already be installed. Only enabled command
Agents on this machine with an existing healthy launcher/session are eligible;
custom startup commands, provider mismatches, duplicate aliases, the leader and
the caller Agent are rejected. The preview is always the sole first batch;
later batches contain at most three Agents (`--batch-size 1..3`). Existing harness
and ACP provider arguments are retained; the native session and cwd are pinned,
never replaced by a new session. Before advancing, each target must register a
fresh interface instance, serve matching `/v1/manifest` and UI HTTP 200, become
ready and keep the same native session ID within 90 seconds (`--timeout
1000..300000` milliseconds). It then receives one idempotent Treer message to
continue its previous task; the normal message watcher must be enabled to
consume it. There are no automatic workspace-wide targets.

The private `rollouts/ID.json` journal stores original/replacement startups and
phase checkpoints under the installation directory (0700/current-user+SYSTEM
ACL, files 0600); it can contain sensitive saved argv and must not be published.
Keep sensitive ACP parameters in provider configuration/environment, not argv.
After a completed rollout or successful rollback has been checked, the operator
may delete its journal; doing so also removes its replay/recovery history.
Reusing the same ID/arguments reconciles completed restarts from real run
generation/instance state, reuses the continuation idempotency key, and skips
completed steps. Different plans cannot reuse an ID. On a failure, later batches
stop; the previous launcher pointer and changed startups are restored, only this
rollout's restarted targets are restarted again, and their old sessions/UI are
health-checked. Concurrent startup/pointer edits are preserved and reported as
rollback failures; stopped intent is respected. The journal records the failed
stage and rollback result. A rollback does not undo model/tool actions or recall
already delivered continuation messages.
Install and rollout locks reject concurrent operations. After a process crash,
confirm that the recorded operator process is gone before manually removing its
stale `rollouts/.lock` directory and repeating the same command. An interrupted
or incomplete rollback requires inspecting the journal and recovering explicitly,
not blindly starting a new rollout. No command uses broad process killing.

The default launcher is installed through the local Controller (next release):

```bash
treer-agent-server launcher install --node /absolute/trusted/path/to/node
treer-agent-server launcher update --node /absolute/trusted/path/to/node
treer-agent-server launcher status --node /absolute/trusted/path/to/node
treer-agent-server launcher rollback --node /absolute/trusted/path/to/node
```

Official binaries embed the same-release portable runtime, UI, profiles and
installer, authenticated by the signed Controller artifact. No repository,
Git, npm, dependency downloads or Agent creation/restart is needed to install.
Node 22+ is an external prerequisite; its executable is not authenticated by
the Controller signature. Use a trusted absolute path. Provider CLIs and their
authentication remain the operator's responsibility. A development Controller
without the built runtime refuses installation unless explicit `--allow-network`
and a trusted npm CLI enable lockfile-pinned `npm ci --ignore-scripts`.

The installer checks every file's size/SHA256, the interface contract and the
actual selected Controller handshake before activation. The content version is
`sha256-<SHA256 of bundle-manifest.json bytes>`; that manifest covers all payload
files and records the source commit. The private machine-local installation is
`<machine.root>/.treer/launchers/agent/<content-version>/`. New profiles use an
absolute Node executable plus stable `run.mjs`; an atomic `current.json` pointer
works without Windows symlink privileges. Running launchers resolve physical
assets once and retain them across updates. Version directories remain for
rollback; profile writes are read back, and failures restore prior profiles and
the pointer. If profile restoration fails, the candidate stays usable and the
error is explicit. No update restarts running Agents. A private installation
lock serializes mutations; stale locks require manual inspection, not PID-based
automatic breaking. No automatic unlock/prune is provided: preserve retained
versions unless an operator explicitly reviews cleanup. Unix state files use 0600 and managed directories use 0700;
Windows relies on the owning account's private-directory ACL boundary rather
than pretending POSIX mode bits provide ACL isolation.

Status reports current/previous/retained versions, integrity and compatibility;
Core adds visible local Agent startup version references, never their argument
values. Unknown old Controller contracts are reported as unknown and cannot
install/update/rollback. Missing required features or protocol mismatch fail
explicitly. The Node entry repeats the Controller handshake before starting the
runtime. A new Controller may accept an older declared contract only if its
capabilities and required features are supported.
The contract also declares a `launcher_security_epoch`; the Controller reports
its minimum accepted epoch. Rollback refuses older security epochs even when
their protocol capabilities still work. Missing security-floor metadata is
unknown, not compatible. Security fixes must bump the epoch and Controller floor
when earlier retained versions must no longer be activated. Same-name profiles
are adopted only from the known legacy entry or compared against the private
last-verified receipt; owner customizations are preserved by refusing the update,
requiring explicit operator review rather than silent overwrite. The stable
trampoline is hash-compared with verified candidate bytes and atomically repaired
on update; status exposes its integrity separately from immutable version assets.

Profiles live in the workspace, while installations live on individual machines.
The official Controller installer preserves existing local/legacy profile names.
If an unowned default name belongs to another machine, it saves a new profile
such as `Codex + UI (win_native)` instead of replacing the existing one. If that
name is also occupied, the suffix includes the full stable machine ID. Improving
long ID suffix readability is deferred. Machine names must match
`[A-Za-z0-9][A-Za-z0-9._-]{0,63}`; complete profile names are
limited to 128 characters. Names come from Controller metadata, or the authenticated
`treer machine list` record matching its enrolled server ID (operator installation
does not require Agent-only `whoami`). Profile subprocess failures report bounded
exit/spawn status and recognized error codes; arbitrary stderr, paths and secrets
are withheld. Names never come from a guessed hostname or PATH.
The private receipt records provider-to-name mappings atomically with ownership
specifications. Updates and rollbacks reuse these exact names even after a
machine rename; modifications to a receipted profile still require explicit
operator review. Existing profiles on other machines remain untouched.
Commands remain absolute: the trusted Node executable and version-independent
runner are specific to that machine, and Windows/POSIX provider entry points
differ. Machine-relative command rewriting is outside this change. The legacy
Git/source installer below does not gain this production multi-machine policy.

Windows Controller cwd and provider paths can use the `\\?\` verbatim prefix.
Runtime and installer synchronous canonicalization uses `realpathSync.native`;
asynchronous canonicalization uses the native `fs/promises.realpath` API. Keep
both sides of containment comparisons canonical and retain the absolute-relative
escape checks. The Windows smoke starts the runtime with verbatim cwd and npm
shim paths, including file preview, durable queue, prompt and session resume.

`install.sh`, `apply.sh` and the Git bootstrap below are legacy/development
entry points, deprecated for default production installation. They retain the
shared profile save/readback/restoration code, but their Git source is not
authenticated as an official embedded release. `switch.sh` is a separate
explicit Unix session migration/restart operation; installation never invokes
it. Packaged switch uses the portable Node entry and does not require tsx/curl.

Release preparation runs `npm ci` with the repository lockfile, then
`node launchers/agent/scripts/prepare-bundle.mjs --output ABS_DIR --source-commit FULL_SHA`.
The empty/new output contains `runtime.mjs` (including JavaScript dependencies,
without optional native WebSocket addons), assets/scripts/profiles/contract and
`bundle-manifest.json`. The build tool is lockfile-pinned. Official Cargo builds
require this exact-source verified payload; same-account tampering, compromised
build tools and untrusted operator-selected Node remain outside this guarantee.

For a checkout-free install, choose a **full 40-character lowercase commit SHA**
from a reviewed release and use the same value in the download URL and `--ref`:

```bash
sha=FULL_40_CHARACTER_COMMIT_SHA; curl -fsSL "https://raw.githubusercontent.com/EvoEvolver/treer/$sha/launchers/agent/bootstrap.sh" | sh -s -- --ref "$sha" --agent codex
```

Bootstrap requires Treer, Node.js 22+, npm, Git and tar. It shallow/sparse-fetches
only the launcher, checks HEAD against `--ref`, runs the existing installer with
the supplied arguments and removes its temporary checkout. It never creates or
restarts an Agent. `--repository URL` is available for a trusted mirror; `--source`
is intentionally rejected. A missing prerequisite reports installation guidance.
The whole script is a function invoked only on its last line, so a truncated
download before that call does no installation work. The SHA is required as an
argument because a piped script cannot discover which URL curl downloaded.

Or inspect a manually fetched sparse checkout before running anything:

```bash
sha=FULL_40_CHARACTER_COMMIT_SHA
git clone --depth 1 --filter=blob:none --no-checkout https://github.com/EvoEvolver/treer.git treer-launcher
git -C treer-launcher sparse-checkout init --cone
git -C treer-launcher sparse-checkout set launchers/agent
git -C treer-launcher fetch --depth 1 origin "$sha"
git -C treer-launcher checkout --detach FETCH_HEAD
test "$(git -C treer-launcher rev-parse HEAD)" = "$sha" || exit 1
./treer-launcher/launchers/agent/install.sh --source ./treer-launcher --agent codex
```

Pinning a full SHA prevents a branch/tag moving between download and installation;
Git verifies object identity and bootstrap verifies the selected HEAD. It does
not authenticate a release signature or an untrusted mirror/download origin.
Trust the reviewed bootstrap and HTTPS source; for stronger assurance download,
inspect and run the script instead of piping it directly to a shell.

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
has its own listener; the launcher reads `/v1/manifest` before registration and
uses its capabilities, instance ID and UI path directly. It verifies the instance
and CLI-supported protocol before registering, so new AIS capabilities cannot
drift from the registration request. The launcher registers the actual port, not a guessed
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

The Interface page is a React app served from `public/`. It consumes only the routes in
this document; the server contract is unchanged.

**Files and policy.** Four flat files: `index.html`, `boot.js` (runs in `<head>`, no flash
of the wrong look or theme), `ui.js` (React, marked and DOMPurify, bundled) and `ui.css`
(all looks). They are committed because the bootstrap installer checks out git without
esbuild, and they are listed in the payload hash manifest like every other file; nothing
needs `node_modules` at runtime. Budget: at most 5 files and 600 KB. The page is served
with `style-src 'self'` (no inline styles or scripts, no eval), `object-src 'none'`,
`base-uri 'self'` and `form-action 'none'`. HTML reaches the DOM through one component
only (markdown, sanitized with DOMPurify); everything else is rendered as text, and
file names, paths, diffs, task output and errors are never interpreted as markup.
Assets and requests are relative (`<base href="./">`), so the page works behind Treer's
iframe tunnel.

**Looks and themes.** Three selectable looks (A Quiet Editor, B Warm Paper, C Terminal
Contrast; default B) times light/dark. Look: `?look=a|b|c`, else
`localStorage["treer-launcher-look"]`, else B. Theme: a parent
`{type: "treer:embed-theme", theme: "light"|"dark"}` message or `?theme=light|dark`,
else the stored mode (`treer-launcher-theme`: system, light or dark), else the system
preference, including later system changes. The Appearance button (the header on every look) picks look and theme and exports the
conversation as Markdown. On phones there is no bottom bar: one Menu button in the top
bar opens a keyboard-reachable menu with Files, Tasks, Appearance and Theme. Every stored or URL value is checked against a fixed list; storage access is
guarded. `?lang=en|zh` (else the browser language) selects the interface language;
a missing translation falls back to English.

**Conversation.** Messages and plans stay in the conversation. Consecutive machine
activity (reasoning, commands, file changes, tool calls) folds into one summary row
with counts, a failure marker, and elapsed time only when the data supports it;
rows expand to output, and file-change rows show each path's unified diff with signed
lines. Markdown, including tables, code and sanitized links, renders in messages;
images may only be same-origin, `data:` raster or `blob:` and open in a zoomable viewer;
long output opens in a dialog. Earlier history loads through "Load earlier messages"
without losing the reader's place; scrolling up shows "Jump to latest" instead of
pulling the page.

**Composer.** Enter sends when idle and steers the current run when busy; Tab queues
a non-empty draft; Shift+Enter adds a newline. While running, separate **Steer** and
**Queue** buttons support touch. Harnesses without steering queue messages and show a
notice. Failed submissions keep the draft. The status strip above the field shows the
Goal, the latest Plan progress and the Queue count; one popover opens at a time (a
bottom sheet on phones) and Esc closes it. Queued messages can be cancelled in the
Queue popover. The model chip opens model and reasoning-effort pickers; harnesses
without reasoning levels show no effort picker.

**Goals.** Harnesses with Goal support show **Set goal**, or the current objective and
status. The Goal popover shows usage, optional budget progress, delivery and sync
notices, and Edit, Pause/Resume (when supported) and Clear (with inline confirmation).
A goal that cannot make progress (blocked, stale, usage or budget limit, out of sync)
turns the strip warning-coloured. `/goal <objective>` sets a goal and `/goal clear`
asks to clear it instead of sending chat. A completed goal leaves a notice in the
conversation; a cleared goal collapses back to **Set goal**.

**Files and tasks.** The Files panel loads folders only when expanded, supports
arrow-key navigation and a filter over loaded entries, previews a selected file as
read-only text (truncated at 64 KiB, with local errors) and refreshes expanded
folders. When the harness reports background tasks, the Tasks panel shows each
task's status, duration, exit code and summary; expanding a task loads its output tail
and a running task refreshes every three seconds while open. On phones the two panels
share one full-screen slot.

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

## Treer message monitoring

Managed Agents automatically long-poll `treer message receive --wait 30000`
using their own inherited CLI identity. Set `AGENT_UI_MESSAGE_WATCH=0` (also
`false`/`off`) to disable this. Standalone launchers without the injected Agent
ID, Controller URL and workload credential do not monitor messages.

Every received message enters the shared persistent queue with delivery `queue`,
never steer. The launcher commits the message ID and pending acknowledgment in
the queue journal before acknowledging the delivery. Pending acknowledgments
are retried first after restart with a stable operation ID; their receipts stay
pinned until confirmed. Full queues leave messages unacknowledged. CLI/network
errors retry with exponential backoff (1–60 seconds); shutdown cancels polling.
Queued messages survive restart; an already running turn follows the queue's
existing interrupted-on-restart rule rather than executing uncertain work twice.

Pending messages merge into one message-only prompt at the queue tail, including
messages received in later polls while the Agent is busy. Ordinary queued prompts
remain FIFO barriers; running turns are never modified or interrupted. One English
warning marks the content as third-party input, not system instructions. Body
lines are quoted and display control characters are escaped. Each section has
an ordinal followed by sender and body. IDs appear only once in a footer list
with `treer message get <id>` instructions and an explicit ending.
Subject, preview and send time are omitted.
The Agent decides whether to act, reply or ignore; the watcher does not filter by
message type. Sender/body now enter the private queue journal and transcript and
are sent to the selected model provider; message bodies may contain sensitive
information. Quoting identifies provenance but cannot prevent model prompt injection.
Prompts are bounded at 64 KiB/256 messages; larger backlogs split into batches.
An individual body over 48 KiB is shortened on a UTF-8 boundary with an explicit
notice to retrieve the original. Old ID-only pending notices remain readable.
Each ID retains its own durable acknowledgment checkpoint within a batch;
redelivery in a different batch does not enqueue it again while its receipt is
retained (the queue keeps the most recent 1024 terminal receipts).
Older launchers reject journals with more than ten additional checkpoints per
record; drain merged message batches before downgrading to such a version.
Acknowledging removes a delivery from receive polling, not recipient visibility:
Core Message `get` checks sender/recipient membership without filtering on ack
state. The recipient can still retrieve the body after ack, subject to current
`message.read` Policy and message retention. The launcher uses the receive body
and does not issue additional `get` requests.
Watched messages have automatic terminal notifications disabled for every type
to prevent notification loops; Agents can explicitly reply through Treer.

## Prompt queue and background-task contract

`GET /v1/state`, `/api/state` and WebSocket state expose `goal`, including
objective, status, usage/budgets, `reason`, `requiredAction`, `rejectionCount`,
`consecutiveRejections` and `roundsUsed`. Claude also exposes `syncStatus`
(`synced|desynced|exit_unconfirmed`), `nativeObjective` and `syncError`.
Lifecycle outcomes are
`active|blocked|stale|done|cleared`; Codex also supports `paused` and native limit
statuses. Native `complete` maps to `done`. Claude native clear confirmations
(including `No goal set`, also from `/goal` without arguments) remove the goal
bar and persist `goal: null`; Codex retains its native cleared lifecycle record.
Unsupported adapters return 501 and hide goal controls.

The launcher wraps native goals, not a second evaluator. Claude uses `/goal
<condition>` and `/goal clear`; successful matching Stop-hook summaries produce
`done`. Native clear local-command output (including older `Goal cleared: ...`)
produces `null`; explicit impossible goal-status/evaluator results produce
`blocked`. Stream events are consumed immediately and the session JSONL is polled
even after completion, so external clears are observed. A rejection loop need
not end in `result` to be caught.
Ordinary assistant claims of success are not parsed as completion. Codex uses
`thread/goal/set|get|clear` and its update notifications. Claude formal exit
reasons are sent through the durable notification outbox, not kept as a phantom
native goal. Resume rebuilds Claude goal existence/objective/status from native
JSONL; checkpoints supplement usage/budgets only for the same goal generation,
objective and status. A mismatched cached edit is discarded rather than replayed.

Managed Agents have a formal exit command, automatically described to Claude:

```sh
node "$AGENT_UI_GOAL_COMMAND" blocked --reason "Deployment environment is absent" \
  --required-action "Owner: provision the environment and grant deployment access"
node "$AGENT_UI_GOAL_COMMAND" stale --reason "The project was removed"
node "$AGENT_UI_GOAL_COMMAND" done --reason 'The user explicitly asked to close as-is'
```

This submits `POST /v1/goal/exit` with `status`, required `reason`, and optional
`requiredAction`. The request is durably accepted with 202 before the tool returns;
native clearing runs afterward, avoiding a tool/turn-boundary deadlock. State
changes only after native clear acknowledgement. Claude clear interrupts the
running query before sending the local command: waiting for idle would deadlock
when the Stop hook refuses to end the turn. Native acknowledgement has a 30-second
deadline. On timeout, the goal remains active with `syncStatus=exit_unconfirmed`,
the mutation lock and exit journal are released, and the user is notified to
manually enter `/goal clear`. The launcher never claims a hook was released
without acknowledgement. Agent exits (`done|blocked|stale`) and automatic budget
or rejection-loop exits also interrupt the current turn, including an executing
tool, to release the Stop hook. User clear always supersedes a pending Agent exit,
cancels its journal, and sends a fresh native clear; it never returns 409 because
that exit is pending. Replacing the objective invalidates an old pending exit.
The command deliberately delegates user-equivalent native clear authority to
the Agent, bypassing Claude's design that the model cannot itself clear `/goal`.
It is enabled by default; `AGENT_UI_GOAL_AGENT_EXIT=0` disables the supplied
command endpoint and pending exit processing. This is a convenience switch,
not OS isolation from other locally authorized CLI/native inputs.

Three consecutive rejected evaluations without new tool input/result evidence
automatically request clear and record `blocked`. Distinct tool input, file edit
or changed tool output resets the sequence; repeated identical tools/results and
assistant prose do not. Defaults: `AGENT_UI_GOAL_NO_PROGRESS_LIMIT=3`,
`AGENT_UI_GOAL_ROUND_BUDGET=20`, `AGENT_UI_GOAL_TOKEN_BUDGET=100000`.
`PUT /api/goal` can set positive `roundBudget` and `tokenBudget`; null token budget
uses the finite launcher default. Claude rounds count rejected Stop evaluations;
Codex rounds count completed goal turns and use native token budgets. Limits
clear the native condition, record `blocked`, and notify. Token counters include
input/output/cache tokens; streaming assistant and result usage are reconciled
without double counting. Claude SDK result usage is session cumulative; a saved
baseline subtracts usage preceding the goal. Creating or editing an active goal
(including its budget) resets token/time/round/rejection counters. Elapsed time
advances while the goal is active, even without a result event. Completed goals
retain their measured usage when only their budget is edited. Limits apply when usage/events are reported, so a
provider request can overshoot a token budget. If a CLI reports no rejection
events or history, rejection detection cannot infer them from arbitrary prose.
Claude still has no native pause/resume: `pause`/`resume` are literal objectives,
not supported controls. Its advertised budget capability is launcher enforcement.

Explicit user closure prose stays ordinary user input. We do not modify the
native evaluator or regex-close goals based on conversation content. The Agent
may cite the user's instruction in `done --reason`; rejection-loop protection
provides the fallback when the evaluator keeps rejecting.

Formal exits notify workspace owners via `treer member list` + `treer message
send`, using the Agent's own identity. Set `AGENT_UI_GOAL_NOTIFY_TO` to a specific
human ID or tree path instead. Notifications include status, reason and required
action, use stable idempotency keys and a durable retry outbox. Policy must allow
human discovery and message sending (or configure an explicit recipient); failure
is logged and retained. Goals/requests/outbox are private 0600 atomic fsynced files.
The UI displays reason, required action and rejection counts, including on mobile.

UI edits (objective, pause/resume, budgets) have a separate durable context-delivery
receipt `goalDelivery`. The native mutation is followed by an English prompt with
the full new user-authored objective and previous objective. Busy Agents receive
steer; unsupported steer falls back to the shared queue; idle Agents receive a
queued prompt that triggers a turn. Each edit's `operationId` deduplicates native
updates and prompt submissions. UI shows `Sent to Agent`, pending, or failure with
a retry button; retries preserve the edit ID. Failed prompt delivery does not
repeat the native mutation. These notices are user updates, not elevated system
authority. Queued delivery means accepted by the launcher, not that the model
has already acted on it.

Native probes: current Codex in a temporary thread started one native turn after
editing/resuming a paused goal, and the next observed reply knew the new objective.
The fake app-server does not inject `goal/set` into prompts at all; explicit
launcher delivery covers both behaviors. Claude UI objective edits already send
native `/goal ...` user input, and also receive the explicit context receipt.
For a running Claude turn, an objective edit sends the ordinary English steer
immediately, without interrupting. The native local command is deferred until
idle because Claude slash commands otherwise interrupt the query. Until its
acknowledgement, the UI explicitly shows `desynced` and the previous
`nativeObjective`; it does not pretend the Stop hook has changed. **Re-align
native goal** (`POST /api/goal/reconcile`) retries alignment, deferring again if
busy. User clear and safety exits deliberately interrupt; objective edits do not.
Incremental JSONL reads cannot replay an older objective over the desired edit.
If another edit arrives while native alignment is in flight, the old command is
acknowledged without replacing the new objective, then the latest edit gets one
attempt after a short delay at idle. The same failed alignment is not retried in
a loop; its visible error and Re-align button provide explicit recovery.
This change's evaluator/loop tests use fake stream-json only; no real evaluator
was invoked. `tests/probe-goal-edit-codex.ts` is an explicit opt-in native probe.

Old Claude compatibility: static inspection of Anthropic's official
`@anthropic-ai/claude-code-darwin-arm64@2.1.204` package confirms `/goal`,
`Goal set:`, `Goal cleared:`, Stop prompt hooks, `goal_status`, `goal_met`,
`impossible`, and `stop_hook_summary` markers, but no `ProposeGoal` marker. This
does not verify its runtime wire shapes or feature gating. The 2.1.296 command
and evaluator behavior was verified earlier/by the leader. Capability discovery
still hides goal controls if initialize does not advertise `goal`; missing fields
are not treated as proof of completion. Mariana was not accessed or restarted.
Upgrading resets legacy active goal usage counters once, granting a fresh budget
allowance. For checkpoints without a cumulative-usage baseline,
active token/time/round/rejection counters restart (budgets are retained), with a
visible reason explaining that old per-goal usage could not be verified. We do
not relabel old session-wide totals as accurate goal usage. If its saved desired
objective differs from the native goal, native history wins and the incompatible
checkpoint is discarded. Subsequent restarts retain verified counters normally.
Existing Claude sessions acquire this exit only when their saved session is
resumed by the updated launcher: leader should restart that Agent, preserving
its session ID/startup command. This supplies `AGENT_UI_URL`,
`AGENT_UI_GOAL_COMMAND`, system guidance and monitoring; it does not clear an
existing active goal merely on restart. No live Agents were restarted here.

Before native clear, the adapter checkpoints the requested outcome and its
thread/session generation. Resume reconciles a cleared native goal with that
intent, retrying its reason/action notification without resurrecting the UI goal;
a replacement
goal invalidates the old intent. Overlapping ordinary mutations return 409 for
retry; user clear preempts them. Claude wire summaries use `hook_infos`,
`prompt_text`, and `prevented_continuation`, whereas JSONL can use camelCase.
Both shapes are normalized and deduplicated; hook prompts must exactly equal the
nonempty native objective (the native goal registers its condition directly).
Unrelated hooks containing that text do not count. These wire-shape/deadlock regressions use
fake stream-json and a fake checker, not real inference. Install the updated
launcher and resume/restart the affected Agent with its existing session ID to
acquire the fix; this change does not restart any live session itself.

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
export is `GET /api/session/export` with loopback Host and no Origin header.
This rejects external Hosts and supplied Origins, not all browsers: same-origin
browser GETs may omit Origin, and native clients can forge these headers. The
guard is not authentication. Unsupported adapters return 501. Export
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

## Native Windows

Use an official Controller release with its embedded portable launcher payload,
and an explicitly selected, trusted **absolute path to Node.js 22+**. Neither the
installation nor runtime needs Git, sh, curl, npm or a repository checkout. The
source-only development payload still needs the documented explicit npm opt-in.
In PowerShell, for example (replace these paths with your installed binaries):

```powershell
& 'C:\Treer\treer-agent-server.exe' launcher --node 'C:\Program Files\nodejs\node.exe' --treer 'C:\Treer\treer.exe' install
& 'C:\Treer\treer-agent-server.exe' launcher --node 'C:\Program Files\nodejs\node.exe' --treer 'C:\Treer\treer.exe' status
```

The installation lives under the **enrolled machine root** at
`.treer\launchers\agent`, not under the process's accidental current directory.
Choose a writable user-owned root (typically beneath `%LOCALAPPDATA%`) when
enrolling. An existing Host rooted at `C:\Windows\System32` must have its root
explicitly corrected by its operator; installing a launcher does not migrate
machine configuration or restart anything. Versioned assets and the stable
`run.mjs` entry support spaces, Unicode and Windows long paths. Installation
directories and Agent state use protected, inheritable ACLs granting only the
current Windows user and SYSTEM; POSIX uses mode 0700/0600. State files are
flushed before atomic replacement. Node cannot flush Windows directory handles,
so the launcher does not claim Unix directory-fsync power-loss durability there.
Built-in Windows PowerShell is used by absolute system path to apply state ACLs.

Install the desired provider CLI yourself and complete its login yourself.
The launcher neither installs provider CLIs nor reads/exposes their credentials.
Set `CODEX_BIN` / `CLAUDE_BIN` to the provider's trusted absolute `.exe` path,
or a standard npm `.cmd` shim; for ACP set `AGENT_UI_ACP_COMMAND`. Standard npm
shims are resolved to their single `node_modules` JS entry and launched with the
selected Node executable using literal argv, without `cmd.exe` expansion.
Arbitrary `.cmd`/`.bat` scripts are rejected: select their native executable or
JS entry instead. Native binaries are started directly. These paths must exist
on the target machine; a macOS path saved in a profile is not portable to Windows.
Provider subprocesses use protocol pipes underneath the Host's ConPTY launcher.
Normal shutdown kills provider descendants through the absolute system
`taskkill.exe /T /F`; Host termination/job ownership remains a Core concern.

The native CI smoke needs no provider login. It runs a real embedded Controller
installer with a sealed local enrollment/registration fixture and fake ACP,
checks manifest registration, prompt/transcript, save/resume, user/SYSTEM ACLs,
long paths and child-tree cleanup, and captures desktop/390px UI screenshots.
It does not replace a real Host/ConPTY integration test, enroll a machine, or
touch running Agents. Its JSON report records the actual operating system;
running it on macOS is not Windows validation.
Windows installation accepts the Controller's canonical `\\?\` Node/root paths
using native filesystem resolution; symlink roots remain rejected. Profile cwd
checks also reject absolute relative-path results (including prefix mismatches)
and junctions escaping the machine root, while allowing long paths within it.
The legacy source installer applies the same checks and native path resolution
to `whoami` roots (which may also use `\\?\`). Controller requests bypass its
source-install branch and use `controller-install.mjs`; Windows source installs
still require the legacy shell/git/npm prerequisites. Development
`--bundle` smoke also exercises these spellings and mirrors the Controller's
private parent ACL setup, but reports `embedded_controller: false` and cannot
substitute for the matching official Controller smoke.

## Checks

```bash
npm --prefix launchers/agent ci
npm --prefix launchers/agent run check
npm --prefix launchers/agent test
# Unit tests use fake Codex, ACP, and native Claude processes.
# Optional embedded-binary integration: sealed fake enrolled CLI, no credentials
# or Agent creation/restarts. Requires a Controller built with the portable payload.
node launchers/agent/tests/core-installer.mjs /absolute/path/to/treer-agent-server
# Native Windows acceptance (Node 22+, rustc for fixture, Chromium installed):
npm --prefix launchers/agent run test:windows -- --controller C:\Treer\treer-agent-server.exe --screenshots C:\artifacts\launcher
# Browser regression for the React page against the real server with fake Codex and
# Claude processes (and the runtime-controls contract fixture): looks x themes, goals,
# queue, tasks, files, dialogs, CSP violations, phone layout. Desktop/390px screenshots.
# No inference, managed Agent mutation, or restart. Chromium is needed once:
npm --prefix launchers/agent exec -- playwright install chromium
UI_SCREENSHOTS=/tmp/agent-ui-screenshots npm --prefix launchers/agent run test:browser
# UI sources live in launchers/agent/ui. After editing them rebuild the four committed
# files in public/; check:ui (part of `run check`) fails on any byte difference with a
# fresh build, on budget overruns and on forbidden code patterns (HTML sinks outside the
# markdown component, eval, unsafe URLs, absolute network paths).
npm --prefix launchers/agent run build:ui
npm --prefix launchers/agent run check:ui
# Look at the page in a browser with sample data (a stand-in Proxy adds the identity headers):
node launchers/agent/tests/serve-ui-fixture.mjs --port 8099
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
