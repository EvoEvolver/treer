# Bring an existing Agent into Treer

Use this reference for an existing provider/session, a new ACP provider, or the
built-in Agent UI. Treer manages the process and identity; the optional
`launchers/agent` runtime adapts its conversation protocol. It is not a workspace
App. Prefer the existing launcher over another AIS server or copied UI.

## Choose the integration

| Existing Agent | Integration | What carries over |
| --- | --- | --- |
| Codex | Native `codex` harness (`codex app-server`) | Native thread ID and history |
| Claude Code | Native `claude` harness (stream-json) | Native session ID/history and background-task events |
| CLI with ACP over stdio | Shared `acp` harness | Provider-supported session loading, events and settings |
| CLI without a machine protocol | Plain command Agent | Terminal replay/input; no structured conversation or built-in UI |
| New non-ACP protocol | Implement `HarnessRuntime` | One adapter; reuse AIS, queue, persistence and UI |

This launches/resumes a provider under a Treer Agent identity; it does **not**
attach to an arbitrary already-running process. Confirm the existing session is
not being written by another runtime before resuming it. Do not silently start a
new conversation when the requested session cannot be resumed. Cross-machine
restoration requires the provider's session files as well as its ID; an ID alone
does not transfer history or credentials.

## Install on the destination machine

An enrolled, online Controller/Host, trusted Node.js 22+, and the selected
provider CLI are prerequisites. The user authenticates the provider separately.
Do not inspect/copy login files or put API keys in argv. From a terminal on the
destination machine, with authority to install:

```sh
treer-agent-server launcher --node /absolute/trusted/path/to/node install
treer-agent-server launcher --node /absolute/trusted/path/to/node status
```

On Windows, select the trusted absolute `node.exe` path instead; no `sh` is
needed. The official Controller installs its verified embedded payload and a
machine-local `treer-agent` shim, and saves six shared workspace profiles. It
does not install/login provider CLIs, create Agents, or restart anything. Install
on **each** destination machine before using a shared profile there. Missing
provider executables are runtime errors, not an invitation to use another
provider or conversation.

The Controller resolves bare `treer-agent` only through absolute directories in
the Agent's final PATH, never its working directory. If it reports
`command_not_found`, check launcher install/status; do not bypass discovery with
a relative path such as `./treer-agent`.

The shim pins Node and the local launcher; do not paste another machine's
absolute launcher path into a profile. Native Windows `.exe` and supported
standard npm `.cmd` provider shims are resolved by the **launcher** without
enabling arbitrary shell command execution. This is not Controller command
discovery: on Windows its bare executable lookup accepts `.exe`/`.com`, not an
arbitrary `.cmd` as the Agent/profile command. A nonstandard batch file may require a trusted JS or native
entry point instead. See the [launcher installation guide](../../../launchers/agent/README.md).

## Resume a native session, or start a new one

Replace example machine names, project paths and `SESSION_ID` with discovered
values. `--cwd` is a filesystem directory relative to the destination machine
root; `--folder` is a workspace Tree path. They are independent. Omitting
`--folder` uses the creating Agent's containing folder (operators start at `/`).

```sh
treer agent admin create --kind command --name reviewer --machine build-box \
  --cwd projects/example --folder /team -- \
  treer-agent --harness codex --session-id SESSION_ID
```

For Claude, use `--harness claude` and that provider's session ID. Omit
`--session-id` for a new conversation. The launcher stores the confirmed ID in
`<cwd>/.treer/agents/<agent-id>/agent-ui.json`; retain that state and the same
working directory for later recovery. `--session-id` is not a Treer Agent ID.
Do not reuse a saved Codex session as a Claude session or edit another Agent's
state file. Native Codex/Claude session export/import is available for deliberate
transfers; ACP does not advertise it. Exports can contain sensitive conversation
content even though auth/config files are excluded.
Keep `.treer/` in the project's `.gitignore`: its session state, queues and
transcripts can contain sensitive conversation data. Treat backups accordingly.

For a plain interactive terminal Agent with no AIS/UI:

```sh
treer agent admin create --kind command --name terminal-reviewer \
  --machine build-box --cwd projects/example --folder /team -- codex
```

Use the provider's own flags for resume/permission policy in this mode. Inspect
terminal output with `treer agent read /team/terminal-reviewer --lines 100`.
The default queued semantic prompt requires an AIS advertising `prompt.queue`;
it is not a general way to type into any terminal. Explicit terminal insertion
(`treer agent prompt --steer`) has different, potentially disruptive semantics.

## Worked ACP onboarding

Suppose an existing, trusted executable `/absolute/path/to/acp-provider` speaks
ACP on stdin/stdout. Register its process through the existing launcher:

```sh
treer agent admin create --kind command --name acp-reviewer --machine build-box \
  --cwd projects/example --folder /team -- treer-agent \
  --harness acp --acp-command /absolute/path/to/acp-provider --name "ACP Reviewer"
```

If the provider needs flags/subcommands to select ACP, append one
`--acp-arg VALUE` **per argument**, including values starting with `--`; these are
argv entries, not a shell string. For example, the shipped OpenCode profile uses
`--acp-command opencode --acp-arg acp`. Verify the installed provider's ACP mode
against its own help; generic text output is not ACP. Diagnostic output belongs
on stderr, not the JSON-RPC stdout stream. A `.mjs`/`.cjs`/`.js` ACP entry can be
selected directly and runs with the launcher's Node.

The shared ACP adapter handles initialize/session creation or supported load,
prompt/cancel, session updates, tools/diffs/plans, and provider config options.
It does not advertise client filesystem/terminal tools: the provider owns its
tools. Unsupported steering, goal, background-task or session-transfer features
stay unsupported; do not claim parity with native Claude/Codex. Requested resume
failure must be explicit, not a fallback new session.

## Custom adapter versus custom AIS

For a non-ACP protocol, implement `HarnessRuntime` in
`launchers/agent/src/harness/<name>.ts`: start/resume, prompt, interrupt, settings,
snapshot and state/log/exit events. Map events to shared `src/model.ts` types,
report only implemented capabilities, and reuse `harness/diff.ts`. Add selection
in the entry points and fake-backend tests using `tests/hermetic.mjs`. Reuse the
existing server, UI, durable queue and file guards; do not create an AIS App.
See [Add a harness adapter](../../../launchers/agent/README.md#add-a-harness-adapter).

If an existing product already implements AIS, it may register itself from its
own Treer-managed identity. The minimum registration contract is a live private
loopback listener with `GET /v1/manifest` using `treer.agent-interface/v1` and a
nonempty instance ID. Registration is self-only. Implement the routes for the
capabilities you advertise: `/v1/prompts`, `/v1/transcript`, `/v1/status`,
`/v1/abort`, and queue routes as applicable. Semantic requests also validate target
identity/instance. The browser page and `ui_path` are optional; a UI must use
relative assets/fetch/WS paths. No public port or service record is needed.

Use `treer interface register --help` for `--port`, `--instance-id`, repeated
`--capability`, and optional `--ui-path`. Derive registration values from the
**live manifest**, not a second handwritten list: protocol, instance, capabilities
and UI path must match exactly. The bundled launcher already does this in
`scripts/register-interface.mjs`. Re-register the actual allocated port at
startup; use `treer interface clear` on clean shutdown. Do not repeatedly register
as a health poll. Inspect with `treer interface show` in that Agent's runtime.

Custom AIS implementations must preserve prompt operation-ID deduplication and
honest queue/notify semantics, target-bound browser forwarding guards, and
read-only file containment. UI callers cannot attest queue senders. Do not expose
the loopback AIS directly or treat public provenance headers as secret auth.
The [architecture contract](../../../docs/architecture.md#agent-interface-server)
and existing launcher are authoritative; registration alone does not implement
these protections.

## Preset Agent UIs

All six profiles use **one** embedded React UI and machine-local `treer-agent`:

| Workspace profile | Provider executable/mode |
| --- | --- |
| `Codex + UI` | `codex`, native app-server |
| `Claude + UI` | `claude`, native stream-json |
| `Gemini + UI` | `gemini --experimental-acp --yolo` |
| `Grok + UI` | `grok agent stdio` |
| `Cursor + UI` | `cursor-agent acp` (not a generic `agent` binary) |
| `OpenCode + UI` | `opencode acp` |

These are the shipped `launchers/agent/profiles.json` recipes, not a promise that
every provider version exposes the same options. Inspect the saved profile and
authenticate/install its CLI on the destination machine before launching:

```sh
treer agent admin profile show "Claude + UI"
treer agent admin profile launch "Claude + UI" --machine build-box \
  --name reviewer --cwd projects/example --folder /team
```

In the Treer web console: **Add Agent → select a profile and machine**, then
select the created Agent in Tree to see its embedded UI. Chat/transcript,
queue/steer, abort, read-only files, diffs and themes share one implementation;
model/effort, goals and background tasks appear only where the adapter supports
them. There is no separate Agent UI App to deploy or custom host to configure.

**Authority warning:** built-in harnesses are unattended full-authority modes:
Codex disables approvals with full access; Claude skips permissions; ACP approves
permission requests and selects a no-prompt/full-access mode where available.
Treer identity/folders are not a sandbox. Use an OS account/container/VM when
tasks need isolation; no UI confirmation protects the machine. Do not weaken
browser guards to make direct local browser URLs work; use the Treer UI tunnel.
Do not launch in an untrusted repository with these defaults: providers may load
project hooks, MCP servers and settings from the working directory. Review the
repository first or use an isolated environment.

## Verify the new integration

```sh
treer agent show /team/reviewer
treer agent admin startup show /team/reviewer
treer agent read /team/reviewer --lines 100
treer agent transcript /team/reviewer --page 0
```

Structured transcript applies only with `transcript.read`. Inspect the actual
saved startup and registered interface, then send an authorized small prompt
through the UI or `treer agent prompt /team/reviewer "Reply with ready."`.
Creating an Agent requires the corresponding `agent.create` Policy permission;
investigate authorization failures rather than switching identities to bypass
them.
Check for a real assistant reply: idle or a listening port alone does not prove
provider authentication/model requests work. Failed resume, missing provider or
manifest mismatch should produce a clear error; do not repair them by creating
duplicate Agents, copying credentials, restarting unrelated processes or
silently changing harnesses.
