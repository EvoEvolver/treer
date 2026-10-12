# Soul client v2 implementation decisions (pending server contract)

Scope: apps/soul client.py, small launcher adapter module, standalone installer,
client-only hermetic tests, README client sections. No server file edits.

Save: authoritative self session ID from managed runtime/session environment or
current self AIS descriptor/state; explicit ID supported. Never guess another
Agent's UI port or silently choose newest session in a shared cwd. Capture Claude
JSONL, project memory/ and project CLAUDE.md; capture Codex rollout plus matching
shell snapshot. Private temporary snapshots with bounded consistency checks;
redact copies, report only counts/categories, preserve original bytes, clean on
success and failure. Structured metadata + declarative adapter; no recovery shell
scripts in archive. Default redact, explicit no-redact and allow-secrets transport
only after contract. Bounded UTF-8/JSON handling, symlink/path traversal rejection.

Incarnate: resolve name/id@version to immutable metadata/version before remote
read-only preflight. Run target probe through caller's treer exec (stdin=/dev/null).
Probe executable/version, credentials existence only (never bytes), launcher
current/environment path, client/bootstrap availability, disk bytes. Refuse bad
probe by default; force is explicit and records problems. Call caller's treer
agent admin create, omitting --folder to retain caller tree placement unless an
explicit folder is provided. No server incarnation mutation creating Agents.

Restore: immutable bundle/checksum validation, safe extraction. Agent-ID keyed
private runtime/workspace, private harness project/session store. Keep target
login material local; never restore source credentials. Rewrite structured cwd
metadata to isolated working directory as required by harness formats. Mark only
completed restore; repeated startup uses evolved transcript/memory without copy.
Separate launcher module discovers env/current pointer and builds treer-agent.sh
--harness ... --session-id ... --cwd ... argv; no direct harness resume command.

CLI artifact: Python standard library only, one executable command entry without
Soul server venv dependency. Installer and target bootstrap must package the
launcher module consistently with forthcoming server asset contract. curl|sh
must parse complete body before execution and detach subprocess stdin.

Tests: no inherited TREER_*/AGENT_UI_*/CODEX_UI_*/provider secrets or HTTP proxy;
explicit HOME/TMP roots and fake CLI/provider/launcher executables. Local fake
server first, actual local server integration after contract. Session selection,
secret categories/counts, raw originals unchanged, temp cleanup, JSON structure,
preflight default refusal/force, own create/folder, version pinning, declarative
restore, first-run marker, restart evolved state, target home untouched, unsafe
archive failures, installed single-entry CLI, piped installer stdin consumer.

Await exact contract: metadata/schema adapter fields and assets, upload options
redaction/allow-secrets, resolve versions and archive routes, launcher/bootstrap
asset delivery, structured errors and limits. Do not implement against guessed API.

## Handoff (unfinished)

Implementation stopped on leader instruction when client ownership moved to
apps-dev. No client, installer, tests or server code has been changed. Contract
0a099e9 has been read and merged into this branch; API.md is authoritative.
New Agent identities cannot read the creator's private snapshot: refuse before
create and suggest saving a workspace-visible new version, never forward tokens.
Possible Claude adapter fields to agree with server: name/session_id/session_file,
cwd/cli_version, memory_files, project_instructions_file. The existing server
installer is generated inside soul.py (server owner only). A standalone installer
must detach stdin for curl|sh and preserve single-file client asset compatibility.
