# Soul v2 HTTP contract

This contract is agreed before the independent server/client implementations.
Archive format remains schema_version=1 tar with manifest.json and regular files.
All /v1 routes require a per-request Treer App identity verified against this
Soul service and workspace. X-Treer-Identity-Token takes precedence over explicit
Authorization: Bearer, then human App OAuth cookie. An invalid supplied header
never falls back. Callers must obtain fresh service tokens through Treer; never
send workload credentials. Public manuals/installers/health contain no Soul data.
Human mutations require X-Soul-Request: 1 and trusted Origin/Fetch Metadata.

## Routes

| Method/path | Result |
| --- | --- |
| GET /v1/me | {principal: {id, kind, ...}, usage: {count, bytes, max_count, max_bytes}} |
| POST /v1/souls | application/x-tar; 201 full metadata; upload options below |
| GET /v1/souls | {souls: [metadata], total, next_offset, usage}; only visible records |
| GET /v1/souls/ID | full metadata |
| GET /v1/souls/ID/archive | compatible tar; client verifies returned metadata SHA-256 |
| GET /v1/souls/by-name/NAME?version=latest | one visible metadata record; NAME URL-encoded |
| DELETE /v1/souls/ID | owner only; 200 {soul_id, deleted: true}; tombstone + unreferenced blob cleanup |
| POST /v1/souls/ID/incarnations | 410 incarnate_deprecated; never creates an Agent |

POST options are URL query parameters (not headers or caller identity):
name (defaults to manifest.name), visibility=private|workspace (default private),
reader=STABLE_PRINCIPAL_ID (repeatable; grants read access even for private),
label=TEXT (repeatable), parent_soul_id=ID, harness=TEXT,
session_id=TEXT, source_machine=TEXT, allow_secrets=true|false (default false).
Harness/session default to manifest.adapter.name/session_id, harness otherwise
generic. Unknown options/invalid types are rejected. Readers are validated in the
workspace identity directory. Parent must exist, be readable, and have the same
owner as the new snapshot; legacy workspace parents are also permitted.

GET list filters: name, harness, owner (exact stable ID or legacy), limit=1..100
(default 50), offset>=0 (default 0). GET by-name optionally accepts owner and
version=positive integer (default latest). Versions increase monotonically per
(workspace, owner, name), including deleted versions; latest across visible
owners resolves by created_at then soul_id. Specify owner for an unambiguous
lineage. No hidden metadata is disclosed through filters, latest or pagination.
Souls are immutable: create a new version to change content, labels or sharing.

## Metadata

Existing soul_id, manifest, files [{path,size}], archive_size, archive_sha256,
created_at remain. New fields: name, version, owner (verified stable ID; legacy
for migrated data), visibility, readers [IDs], parent_soul_id (ID or null),
labels [strings], harness, session_id (string or null), source_machine (string
or null), created_by (verified principal ID; legacy for migrated), size_bytes
(logical expanded bytes including manifest), contains_secrets (boolean),
secret_findings ({category: count}; never matched bytes). archive checksums may
change during v2 migration/reconstruction, so fetch current metadata; extraction
format, soul IDs, manifest and file bytes remain compatible.

New uploads default private. Workspace records and explicit readers are readable
by authenticated workspace members; only owner can delete. Owner IDs do not come
from upload metadata. Legacy records become owner=legacy, visibility=workspace,
remain readable by all authenticated members and cannot be deleted via API.
The server takes a complete sibling backup before migration; operator rollback
restores it with server stopped. No unauthenticated legacy archive exception.
Existing launchers using authenticated Treer routing can still download migrated
archives. On transports without injected tokens, reinstall the v2 client.

## Errors and retention

Errors use {error:{code,message,...}}. Invisible/missing records both return 404
soul_not_found. Invalid tokens 401/403; authenticated non-owner delete 403
forbidden. Secret upload rejected with 422 secrets_detected and
error.findings={category: count}, error.count=TOTAL; never samples, file contents,
matched values or caller-supplied filenames. allow_secrets=true explicitly accepts
and records findings/contains_secrets (it never disables scanning). Quota overflow
409 quota_exceeded. Limits configurable per-owner with server defaults 10 GiB /10000
active souls; count and bytes are logical and do not decrease through dedup.
DELETE frees active quota and cleans blobs only when no active snapshot references
them. Repeated DELETE by owner returns the same result; tombstones retain only
identity/name/version/deletion bookkeeping, not manifest/secrets/file contents.
Uploads are not request-idempotent; retry may create another version.

Incarnation belongs to the client: run treer agent admin create with the caller's
own authority and launch a client that acquires its own fresh Soul service token.
A newly created Agent has a different owner ID: a private Soul needs an explicit
reader grant at upload (when target ID already exists), or workspace visibility.
The client must explain this before creating a new Agent; server offers no bypass,
no delegation of creator credentials and no mutable ACL endpoint.

## Declarative native adapters

manifest.adapter.name accepts claude, codex or generic. Claude/Codex declare
session_id (UUID), session_file (archive path), cwd, optional cli_version.
Codex also declares session_relative_path (.jsonl), optional shell_snapshot_file
and shell_snapshot_name. Claude may declare memory_files (paths strictly below
files/memory/) and project_instructions_file. All referenced files must exist.
The archive never contains a restore script. On incarnation the client generates
a fresh native session UUID, rebases structured cwd/session metadata and imports
into the target Agent's dedicated stores; shell snapshots are never executed.


Launcher transfer adapters may instead declare `transfer_files`, an array of
{scope: session|memory|project, path: scoped relative path, file: raw archive path}.
Then session_file/session_relative_path are not required. Optional exported_at
records the launcher export time. `file` must exist below files/, scoped pairs
must be unique, and project scope only permits CLAUDE.md. Actual file bytes are
stored unencoded so server secret scanning remains effective. Soul reconstructs
the treer.agent-session/v1 bundle and current hashes for launcher import, which
allocates the native identity/storage and rebases the source paths. Legacy
session_file adapters are translated into this same transfer contract.
