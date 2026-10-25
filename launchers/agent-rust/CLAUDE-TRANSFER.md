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

## Remaining live-transfer contract

`capabilities.sessionTransfer` remains **null**. The offline command is usable,
and managed snapshot is separately usable, but neither is a complete new-runtime
transfer workflow or shared UI action. A new Rust Claude runtime currently
requires a supported private history checkpoint to resume an existing session;
the offline importer intentionally does not fabricate that checkpoint. The next
leaf must implement explicit fresh private checkpoint/bootstrap and new-session
launch wiring, including independent supported authentication of the isolated
config, without importing old receipt/queue/task execution authority. Passing
the returned UUID to resume alone does not implement that contract. Windows transfer is explicitly
unsupported; its native private-file/DACL backend is not replaced by POSIX mode
checks. No real Claude private files or credentials are used by the tests.
