# Offline Claude session transfer (#247)

This batch implements an offline export/import command in the native executable
on macOS/Linux. It does not resume, stop, inspect, or modify a running runtime.
The installed dispatcher forwards `treer-agent session ...` to its hash-bound
native executable, without launching a provider or falling back to Node.
An older installed native image must be updated before using this command.
The dispatcher also sets an invalid runtime harness and uses the pinned regular
executable as the runtime cwd: an older image that ignores `session` argv must
refuse its ordinary startup rather than accidentally launch a current session.
The new CLI uses only its explicit transfer arguments, ignoring those runtime
selectors. This guard does not assert that an old image implements transfer.

```sh
treer-agent session export --harness claude \
  --config-dir /absolute/explicit/claude-config \
  --cwd /absolute/source-project --session-id SOURCE_UUID > snapshot.json

treer-agent session import --input /absolute/snapshot.json \
  --directory /absolute/existing-parent/new-project
```

The native binary accepts the same `session` arguments directly. Export writes
one `treer.agent-session/v1` JSON bundle to stdout; import returns
`{sessionId,cwd,environment:{CLAUDE_CONFIG_DIR}}`. Import generates a new UUID;
an explicit `--session-id NEW_UUID` must differ from the source, ignoring case.
Use `session --help` for arguments. Every option is explicit: no ambient HOME
search, stdin input, network fetch, provider command or runtime journal access.

## Bundle and filesystem boundary

The schema, base64/SHA256 entries and allowlist match Node
`src/harness/session-files.ts`: main `session/session.jsonl`, Claude
`session/subagents/**/*.jsonl`, native `memory/**/*.md`, project `AGENTS.md`,
`CLAUDE.md`, `MEMORY.md`, `.claude/CLAUDE.md`, `.claude/memory/**/*.md`, and
`.codex/memory/**/*.md`. Credentials, settings and arbitrary project files are
not enumerated or copied. Allowed memories/instructions can themselves contain
private information; protect the exported bundle accordingly.

Export requires the exact cwd-derived Claude project directory. Unlike Node's
fallback search it refuses a missing preferred session rather than searching
unrelated projects. Project keys use Node-compatible UTF16 character replacement.
Take an export while the source is idle: each file has before/after same-fd
size/mtime/ctime checks, but this is not a multi-file snapshot or a provider
checkpoint. No session-wide lease or active-process claim is made.

Root arguments resolve explicitly to canonical directories and are then held
open; descendants are walked with nofollow/nonblocking descriptors. Source files
must be same-UID regular files with one link and no group/world write permission.
Walked directories must belong to root/current UID and have no group/world write
permission (sticky directories are allowed, matching the native binding trust
boundary). Import also rechecks the held parent's pathname identity before publish.
Symlinks, hardlinks, special files and changing files refuse. Directory scans
have a shared 10,000-entry and depth10 budget; bundles have 1024 files, relative
paths bounded to14 components, 64MiB decoded total, and a 96MiB input-file cap.
Absolute paths, empty/dot/dotdot components, backslashes, NUL and colon are
refused inside bundle paths (colon rejection is stricter than Node on POSIX).
Session JSONL requires a final newline, object records, matching source session
metadata in the main file, valid UTF8/JSON, no duplicate keys, depth32,
500,000 nodes per record and 100,000 records per file. All session files are
validated; all file hashes and canonical base64 encodings are checked.
These are admission bounds, not a 64MiB process-memory bound: raw JSON, decoded
files, JSON trees and rewritten bytes may coexist.

## Import publication and continuation

Import only accepts a **nonexistent destination** under an existing parent.
Existing directories, including empty ones, files and links are preserved and
refused. This is deliberately narrower than Node's empty-existing acceptance.
All entries and rewritten JSONL are validated before creating a stage. Import
rewrites the same existing fields as Node: top-level `sessionId`, `session_id`,
`cwd`, and `payload.cwd`; other native record fields are retained.

Project files and a fresh isolated `.harness/claude` config directory are written
in one private sibling stage (directories0700/files0600). The provider files use
the final destination's project key and new UUID. Files/directories are synced;
one exclusive same-parent rename publishes the whole directory. Linux requires
`renameat2(NOREPLACE)` support; macOS uses `renameatx_np(RENAME_EXCL)`. There is
no weaker check-then-rename fallback. Validation/write failure removes only the
owned stage and leaves no published target. Existing/concurrently created
targets cannot be overwritten. A crash before rename may leave an unpublished
`.session-import-*` stage; this command never scans/deletes other stages.
After rename, a parent-sync failure reports
`import_published_durability_unknown_do_not_retry`: the target is retained,
publication must be inspected, and there is no automatic retry.
If stdout fails after publication, `import_published_output_failed_do_not_retry`
likewise retains the complete directory; inspect it instead of resubmitting.

Unlike Node's shared provider-home restore, this command puts provider files
**inside the new project**. This avoids partial publication across two separate
roots, does not scan or modify existing provider sessions, and creates no
credential symlink/copy. The isolated config needs its own supported authentication
before real provider use. A later, explicit invocation must use the returned cwd,
new session ID and `CLAUDE_CONFIG_DIR`; do not substitute the old global config
directory. Such a launch is separate from transfer and is not executed here.
Same-UID processes remain one trust domain; descriptors and private modes do
not sandbox a malicious process under the same account.

## Managed idle snapshot (separate leaf)

On macOS/Linux, `POST /api/session/export` accepts exactly
`{"threadId":"CURRENT_UUID"}`. It returns the same allowlisted bundle accepted
by the offline importer, with descriptive `runtimeSnapshot` metadata. No path,
config, cwd or destination selector is accepted. Source cwd and explicit absolute
`CLAUDE_CONFIG_DIR` are captured at runtime startup; absent explicit config refuses
without searching HOME. Responses use `Cache-Control: no-store`.

Use the existing authenticated Controller/Proxy UI tunnel. In addition to the
ordinary AIS guard, export requires managed Agent identity and exact current
Agent/interface-instance provenance, with `controller-api` or `proxy-ui` source.
Unmarked direct loopback calls are refused. These public headers are routing
provenance, not cryptographic authentication: a same-account local process can
forge them. The existing Controller/Proxy auth boundary is unchanged; no new
semantic CLI command, credential or general filesystem API is introduced.

The actor serializes the export against prompt/settings commands. It requires
the exact current idle thread and initialized adapter with no active input or
pending interrupt; transport/recovery/storage fences, queued/uncertain receipts,
pending acknowledgements/notifications, any goal and running background tasks
refuse. Provider events continue through the normal event pump during filesystem
work; any observed event invalidates that export, without rolling back the event,
killing the provider or replaying input. A concurrent prompt waits and is handled
normally once the read window ends. One HTTP export is admitted at a time, with
a 4KiB request/2s body bound and 30s read-window deadline. In-progress event
handling retains its existing fatal write deadline. A timed-out blocking file
worker retains its exclusive reader permit until it actually exits, preventing
accumulation of detached reads. All offline path/size/hash checks remain active.

This is **local current-adapter idle custody**, not a provider flush/checkpoint,
global native-session lock, atomic multi-file snapshot, signed handoff or proof
that no other same-UID writer exists. It does not include Launcher private state,
receipts, queue, goals, task authority or credentials. The original session and
files remain in place. No import or new runtime starts through this endpoint.

## Prepare and explicitly launch a new runtime

The smallest native/runtime loop is now separate explicit commands:

```sh
treer-agent session prepare --input /absolute/snapshot.json \
  --directory /absolute/existing-parent/new-project
# Read the returned NEW_UUID and isolated CLAUDE_CONFIG_DIR.
# Supply provider-supported environment auth separately, never as argv/bundle.
treer-agent session launch --directory /absolute/existing-parent/new-project \
  --session-id NEW_UUID --auth environment
```

`prepare` reuses the same all-entry validation, fresh UUID and atomic NOREPLACE
import. It additionally writes `.harness/claude-launch.json` in that same stage,
binding the final cwd, new UUID and rewritten main JSONL hash. All validation,
including admission of at least one user/assistant history envelope, precedes
publication. Ordinary offline `import` remains unchanged and does not produce a
launch plan. `prepare` never starts a provider or initializes private runtime
state. The source files/session are retained unchanged.

`launch` requires exactly directory/session/auth options. It reopens the prepared
directory with the existing nofollow/same-fd boundary, verifies the native hash
and ID, and derives history from the **actual imported native JSONL**, the source
used by Node `loadHistory`. It does not fabricate a supported AIS seed or a
provider result. Each user/assistant envelope is a display-only historical entry,
in native file order, with `projectionOnly:true` and `historySource:native-jsonl`.
`completed` on these entries means a closed *display entry*, never provider
terminal confirmation. No old turn IDs, receipts, input obligations, task/goal
execution or model/effort authority are imported. Non-string native content is
shown as JSON text; this is not full rich historical UI parity. The most recent
50 envelopes are retained, text capped at32KiB with an explicit truncation label,
and older-prefix counts describe envelopes, not reconstructed provider turns.
The full validated native JSONL remains available to the provider for context.

The caller supplies the normal new Agent identity/private-root environment, or
an explicit unmanaged private root for a local foreground invocation. The root
must already exist, pass normal private placement checks and contain no state
other than the ordinary single-writer lease. Existing even idle journals or
auxiliary files refuse. No identity/Agent is created, migrated or restarted by
these commands. A new managed launch retains its normal authenticated quota
checks; the installed dispatcher requires a Claude native binding, selects the
bundled public assets and registers its new AIS through the usual CLI.

Authentication is explicitly **environment** mode: a nonempty
`ANTHROPIC_API_KEY` or `CLAUDE_CODE_OAUTH_TOKEN` must be supplied by the new
invocation. Values are not emitted, copied into the plan, or read from old
config/HOME/keychains by transfer code. `CLAUDE_CONFIG_DIR` is forcibly the
new project's isolated directory. This selects the provider-supported auth
mechanism; initialize ACK does not prove remote account authentication or native
history completeness. The provider still decides auth success on its real
operations. No automatic test prompt or network auth probe is sent.

Before spawning, launch rechecks the main native hash, takes the private lease
and writes an exclusive durable `.harness/launch-intent.json` in the prepared
directory. That target permits **one launch attempt**. Startup uses native
`--resume NEW_UUID` with the isolated config; initialized-current-adapter state
is distinct from historical wire authority. Only successful normal initialization
and complete private checkpoint persistence permits port/interface publication.
Events still pump during initialization; auxiliary task events cannot publish
an early partial bootstrap checkpoint. The checkpoint contains empty imported
receipts/goals/tasks and identifies native history as `providerConfirmed:false`.

Failure never replays inputs, retargets to a fresh session or retries a launch.
The intent and complete imported directory remain for inspection; no checkpoint
or public port is published on initialization refusal. An ordinary complete
checkpoint that was already atomically published before a later port/durability
failure is retained, not rolled back or labelled absent. Port-publication errors
await owned provider shutdown. Existing port-file destinations refuse before
the launch intent/provider. No cross-root atomic transaction is claimed.

After a successful first launch, normal process recovery uses the ordinary
entry with the returned cwd, isolated `CLAUDE_CONFIG_DIR`, `AGENT_UI_SESSION_ID`
and the **same private root**. That path loads the real checkpoint and does not
rerun prepare/launch. The once-only command is not a reusable Host startup
recipe; updating a newly created Agent's persistent startup remains an explicit
owner/operator action. Tests exercise this ordinary restart without resending
the first prompt. These commands do not change any existing startup.

## Remaining live-transfer contract

`capabilities.sessionTransfer` remains **null**. The offline command is usable,
and managed snapshot plus prepare/explicit launch now form an independently
usable POSIX loop. Shared UI/Agent creation/persistent-startup orchestration and
full historical rich rendering are not implemented, so the live capability is
not advertised. Ordinary resume alone still cannot bootstrap an arbitrary old
session without a supported checkpoint; only the explicit validated new-target
launch path admits native-read display history. Windows transfer is explicitly
unsupported; its native private-file/DACL backend is not replaced by POSIX mode
checks. No real Claude private files or credentials are used by the tests.
