# Launcher React UI: existing consumer contract and packaging review

Status: design input from launcher-dev, not implementation approval. Source baseline
`5f0dd715199e90edcde685500432426333626fa6` (published launcher); local `main`
was `ac914fe` when this worktree was created. ui-dev owns the companion design
`2026-10-14-launcher-react-ui.md`, prototypes, and frontend. Reconcile baselines
before implementation; do not drop intervening Windows fixes or pending goal fixes.

## Consumer boundary

Authoritative source: `launchers/agent/src/{index,model,runtime}.ts`. All browser
URLs must be relative to `document.baseURI` (`<base href="./">`). Use
`new URL("ws", document.baseURI)` and change http(s) to ws(s), preserving the
Treer tunnel prefix. Do not put credentials, forwarding markers, machine-local
paths, or alternate Proxy URLs into frontend configuration.

### HTTP inventory

Methods matter. Most responses are JSON; state below means the complete state
object described in the next section, not a delta.

| Endpoint | Input | Success / semantics |
| --- | --- | --- |
| `GET api/state`, `GET v1/state` | none | 200 state; recent 50 history items |
| `POST api/history/older` | `{limit:number}` | 200 state with limit + 50, bounded 50–10000 |
| `POST api/prompt` | `{prompt,mode?:"steer"\|"queue",history_limit?}` | 200 state + submission receipt; default steer, unsupported steer downgrades to queue |
| `POST api/interrupt` | `{history_limit?}` | 200 state after interrupt request |
| `POST api/settings` | `{model?,reasoningEffort?:string\|null,history_limit?}` | 200 state; show only model-reported effort choices |
| `DELETE api/queue/:id` | URL-encoded queue id | 200 state, 404 if absent |
| `PUT api/goal` | GoalUpdate + optional stable `operationId` | 200 state; idempotent edit delivery journal |
| `DELETE api/goal` | none | 200 state; explicit user confirmation belongs in UI |
| `POST api/goal/reconcile` | empty JSON | 200 state, or 501 if unsupported |
| `POST api/goal/retry` | `{id}` (goalDelivery.id) | 202 state |
| `GET api/tasks/:id/output?tail=` | registered task id; positive byte count | 200 `{id,content,size,truncated}`; at most last 65536 bytes; 404 absent, 400 unavailable/denied |
| `GET v1/files/tree?path=` | empty path = cwd; encode query value | 200 `{path,entries:[{name,path,kind:"directory"\|"file",size:number\|null,has_children:boolean}]}` |
| `GET v1/files?path=` | relative file path | 200 `{path,name,content,size,truncated}`; first 65536 bytes decoded UTF-8 |
| `GET v1/transcript?page=&limit=` | `cursor` aliases page; page default 0, limit default 1, max 1000 | 200 flattened **turn-based** page (below); not the rich browser-history DTO |
| `GET v1/manifest` | HEAD also accepted | protocol, instance_id, capabilities, ui_path; source is interface-contract.json |
| `GET v1/health` | HEAD also accepted | 200 ok or 503 starting, with instance_id |
| `GET v1/status` | none | agent_id, interface_instance_id, status, busy, queue_length, error |
| `GET api/health` | none | 200/503 `{ok,ready}` |
| `GET .treer/agent` | none | 200/503 surface metadata: protocol/version/ready/title/ui/capabilities |
| `POST v1/prompts` | `{operation_id,text,delivery?:"steer"\|"queue",sender?,notify?,mode?:unknown}` | 202 `{accepted,operation_id,duplicate?,submission}`; default queue; **mode is Core compatibility data and ignored** |
| `GET v1/queue` | none | `{submissions:Submission[]}` containing queued entries only |
| `GET v1/submissions/:id` | submission id | `{submission}`, or 404 |
| `DELETE v1/queue/:id` | submission id | `{submission}`, or 404 |
| `POST v1/abort` | none | 202 `{accepted:true}` |
| `POST v1/goal/exit` | GoalUpdate with done/blocked/stale | 202 durable exit receipt; managed supported harness only, environment may disable |
| `GET api/session/export` | none | SessionBundle; intentionally **not a browser feature**, rejects any Origin and non-loopback Host |

The browser should continue using `api/*` for mutations. It must not populate
attested `sender`/`notify` on `v1/prompts`; Proxy UI forwarding cannot prove sender
identity. Queue cancellation IDs and path components must use encodeURIComponent.
Directory browsing is read-only. Rejecting traversal/external symlinks remains
server-owned; render all returned file data/diffs as text.

Transcript response: `{agent_id,interface_instance_id,page,page_count,next_page,
cursor,next_cursor,entries:[{id,kind,role,content,created_at}]}`. `page_count` is
the number of turns, cursor is a stringified turn offset, and a single turn may
have many entries. `next_page`/`next_cursor` are null when exhausted. This endpoint
omits rich diffs/plan fields; do not rebuild the browser timeline solely from it.

### State DTO and WebSocket

`GET api/state` and WS `{type:"state",...state}` share:

- `ready:boolean`; `runtime:{state:"starting"|"working"|"blocked"|"idle",error:string|null}`.
- `harness:{id,name,capabilities:{steer,backgroundTasks,goal:{pause,budget}|null,
  sessionTransfer?:{export,import}}}`. Hide unsupported controls, never infer
  support from provider name. Session transfer has no new browser write route.
- `thread:null|{id,title,cwd,model,reasoningEffort,status:"idle"|"running"|"error",
  activeTurnId,lastError,createdAt,updatedAt,turns:TurnDto[],hasOlderItems}`.
- `modelOptions:ModelOption[]`: id/model/displayName/description/isDefault/hidden,
  supportedReasoningEfforts `[{reasoningEffort,description}]`, defaultReasoningEffort.
- `backgroundTasks:BackgroundTask[]`: id, kind shell/agent/monitor/other,
  description, status running/completed/failed/stopped, startedAt/endedAt/exitCode/
  summary nullable, outputFile optional. UI never requests an arbitrary outputFile;
  only task id is accepted by the output API.
- `queue:QueuedPrompt[]`: id, text, createdAt, optional sender `{agent_id,path,name}`
  or null and position. Sender data is display data, not browser authority.
- `goal:Goal|null`; `goalDelivery:null|{id,status:"pending"|"sent"|"failed",
  error:string|null,promptId:string|null}`; `history:{limit:number}`.

`TurnDto`: `{id,startedAt:string|null,status:"completed"|"interrupted"|"failed"|
"inProgress",error:string|null,items:HistoryItem[]}`. Item kinds: userMessage,
agentMessage, reasoning, commandExecution, fileChange, plan, toolCall, other.
Fields: id/kind/text plus optional previewText/detailText/status/transcriptOrder/
sourceTurnId; changes `[{path,diff,kind}]`; steps `[{step,status:"pending"|
"inProgress"|"completed"}]`. Preserve stable expansion state with turn+item ids.
Missing optional data is not zero usage or an empty completed plan.

Goal fields: objective; status active/paused/blocked/stale/done/cleared/
usageLimited/budgetLimited/complete; nullable tokenBudget/tokensUsed/timeUsedSeconds;
optional id/reason/requiredAction/roundBudget/roundsUsed/rejectionCount/
consecutiveRejections/notificationError/syncError/nativeObjective;
syncStatus synced/desynced/exit_unconfirmed. GoalUpdate accepts objective/status/
tokenBudget/roundBudget/reason/requiredAction. Keep native and delivery/sync status
distinct; do not optimistically clear a goal following a failed mutation. Pending
goal-authority fix `6c296df` is a separate candidate; coordinate its integration.

`/ws` sends an initial complete state immediately, then complete snapshots on
runtime/queue changes. There are no browser token-delta or tool-event messages.
Client command `{type:"history-limit",limit:number,respond?:boolean}` changes
that connection's window (50–10000); default respond=true sends fresh state.
Re-send the desired history limit after reconnect. For load-older, preserve scroll
anchor and set respond=false after accepting the HTTP state to avoid duplicate
render work. Avoid stale HTTP responses shrinking a larger selected history window.
Reconnect/poll errors must not discard drafts, expansion state, or visible history.

### Errors and browser trust

Submission receipts have `{id,disposition:"queued"|"sent"|"steered",position,
mode,duplicate,downgraded,explanation?}`. A submission lookup has id/sender/
created_at/position/text_preview/state (queued/running/done/failed/interrupted)/
turn_id/assistant_preview/error. Terminal receipts are retained for lookup with a
bounded history; they are not included in GET v1/queue. Prompt text has its own
64 KiB UTF-8 limit, below the 128 KiB JSON-body cap.

No universal typed error-code envelope exists. Most errors are `{error:string}`;
files return `{error:{code,message}}` (`file_access_failed`, `method_not_allowed`).
Normalize both; HTTP status is authoritative. Relevant statuses: 400 malformed
input/mode, 403 guards/denied/disabled exit, 404 missing resource, 405 files not
GET, 409 conflict/not-ready, 413 JSON over 128 KiB, 429 full queues, 500 unexpected,
501 unsupported, 503 goal delivery/startup. Do not retry mutations blindly or
claim that a timed-out prompt was not accepted. api/prompt has no operation-id
contract; goal edits do. WS client payload maximum is 16 KiB.

Listener is 127.0.0.1 only. Browser requests require protected forwarding markers
inserted by authenticated Treer Proxy; direct browser localhost access is refused.
Standalone hermetic browser tests use the existing sealed forwarding fixture,
not weaker production guards. Local native clients are trusted, not authenticated
by a secret header. Do not add tokens to JS, DOM, localStorage or URLs.

Current HTML CSP (unchanged):

```
default-src 'self'; connect-src 'self'; img-src 'self' data: blob:; style-src 'self' 'unsafe-inline'; script-src 'self'; font-src 'self' data:
```

Production React build must work without eval/inline scripts, CDN, remote fonts,
workers or new CSP sources. Markdown remains marked + DOMPurify. Escape all other
content, including goal sync errors, task output, filenames, code/diffs and i18n
interpolation. Existing remote image URLs are blocked by CSP; do not silently add
an image proxy or arbitrary file-fetch capability for the image viewer.

## Packaging and native Windows review

Current public total: 135847 bytes including HTML; JS+CSS = 130800 bytes across
seven files (measurement before React). Proposed final budget: <=600000 bytes
uncompressed JS+CSS, <=5 fixed short JS/CSS files. Report initial-load bytes and
all shipped migration assets separately; old+new coexistence must be explicit.

`prepare-bundle.mjs` currently recursively copies **all public/** unchanged,
copies scripts/profiles/interface contract, bundles the Node runtime with esbuild,
then enumerates every file with path/size/SHA256/executable in bundle-manifest.
It does **not** build browser assets. Launcher-dev will wire an explicit browser
build before enumeration after design approval; no package/install at runtime.
Avoid copying TSX/source maps, fixture data, npm trees or obsolete build files.
Use production React and bundle all browser dependencies locally. Keep React
build tooling in dev dependencies if no server import requires it.

Fixed filenames (e.g. ui.js/ui.css) are preferable: every response is no-store and
the entire version directory is already content-addressed by manifest SHA256.
Per-chunk hashed filenames give no current cache benefit. If introduced, esbuild
must produce imports/HTML consistently and clean stale files before enumeration;
never enumerate before build. Missing/extra/changed assets fail verifyBundle.

`bundle-contract.mjs` permits a 2 MiB manifest and 32 MiB regular-file reads,
rejects links/special files and unsafe/duplicate path entries, and verifies every
listed size/digest and the exact found set. React assets need no protocol/security
epoch bump. Do not weaken these limits for a <=600 KB UI. Rust embedded payload
also validates exact file count/entries. The Controller embeds the **prepared**
payload, so changing tracked public files alone is not official artifact proof.

Windows versions live beneath `<root>/.treer/launchers/agent/sha256-<64hex>/`;
files are copied and ACL-protected individually. More nested/hashed chunk names
increase paths, manifest entries and ACL operations. Keep a shallow public tree,
short ASCII filenames and no per-component output directories. Keep existing
native realpath/verbatim cwd handling, protected ACLs and atomic current.json
switch unchanged. Running processes retain immutable version assets across update.
The UI requires no node_modules in an installed portable payload.

## Fixtures, verification and stage boundary

Use tests/hermetic.mjs, fake-codex/fake-acp/fake-claude and the existing sealed
browser tunnel. Snapshot fixtures should cover unsupported capabilities, null
thread/goal, errors, large history, live partial text, plans, diffs, background
output truncation, queue downgrade/sender/cancel, goal sync/delivery failure and
retry, XSS strings, both themes and Chinese fallback/interpolation.

No new usage/duration fields ship at design stage. If ui-dev needs them, first
agree a separate optional DTO patch. Current TurnDto has no elapsed duration,
token usage or nested task hierarchy: do not fabricate "worked N seconds" from
missing timestamps or infer a task tree from text. Show a generic activity summary
when reliable measurements are unavailable. Optional fields can be omitted by ACP.

After user prototype approval: npm test/typecheck/check/browser parity; execute
browser-controls and relevant fake Claude settings tests; prepare portable payload,
verify/tamper test its manifest, measure bytes/file count, then windows-smoke with
that prepared payload and official Controller. Preserve existing assertions for
registration/7 capabilities, verbatim cwd/npm shim, session resume, ACL/long paths,
desktop+390px screenshots, no JS errors/overflow and process-tree cleanup. Native
proof belongs to a frozen official Controller build, not only JS --bundle mode.

Migration: ship React opt-in alongside legacy first; keep the legacy fallback
loadable without inline query dispatch; separate default-switch commit only after
parity and security reviews. ui-dev owns DOM/assertion migration; launcher-dev
owns build/manifest/Windows smoke integration. Security-claude reviews exact lockfile,
dependency reasons/licenses/advisory snapshot, emitted bundle and unchanged CSP.
No new dependency or build integration is approved by this inventory alone.
