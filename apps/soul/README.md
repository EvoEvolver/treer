# Soul v2

Soul is a Managed App for immutable, versioned Claude/Codex session snapshots.
Humans browse their visible snapshots, inspect manifests, copy incarnation
commands and delete their own snapshots. Agents use a standalone `treer-soul`.
It never creates Agents with the Soul service's authority.

## Deploy and configure

Deploy only after backing up and reviewing the migration below. This repository
change does not deploy, migrate or restart the existing `/infra/soul` App.
Use Workspace access, never `--public`:

```sh
treer app create --machine self --name soul --cwd . --port 9420 \
  env -- SOUL_DATA_DIR=.treer/apps/soul SOUL_PROXY_URL=https://YOUR_PROXY \
  python3 apps/soul/soul.py
```

`SOUL_SERVICE_ID` and `SOUL_WORKSPACE_ID` may explicitly configure identity.
Otherwise the server discovers its own Managed App by matching its injected
runtime Agent ID in `treer app list`; it does not accept an audience from clients.
Configure `SOUL_PUBLIC_URL` for a direct origin or embedded tunnel root when
explicit identity is used. Auto discovery uses the App public URL or canonical
`/api/workspaces/W/apps/ID/ui/` fallback. OAuth callbacks require a registered
callback origin; forwarded human identity works without wildcard ingress.

| Variable | Default | Meaning |
| --- | --- | --- |
| SOUL_LISTEN | 127.0.0.1:9420 | HTTP listener |
| SOUL_DATA_DIR | .treer/apps/soul | Private database, blobs, temporary uploads |
| SOUL_PROXY_URL | TREER_PROXY_URL | Trusted Proxy origin; required |
| SOUL_SERVICE_ID | own Managed App discovery | Verify audience |
| SOUL_WORKSPACE_ID | TREER_WORKSPACE_ID / discovery | Workspace scope |
| SOUL_PUBLIC_URL | http://soul.internal / discovery | Installer/OAuth root |
| SOUL_MAX_UPLOAD_BYTES | 67108864 | Compressed/tar request limit |
| SOUL_OWNER_MAX_COUNT | 10000 | Active snapshots per owner |
| SOUL_OWNER_MAX_BYTES | 10737418240 | Logical expanded bytes per owner |
| SOUL_CLIENT_PATH | bundled client.py source | Source for standalone artifact |
| TREER_BIN | treer | Own identity discovery only; never Agent creation |

Run one server process per data directory. An exclusive lock prevents concurrent
servers; a process lock and SQLite transactions serialize versions/quota/deletion.
The server uses Python standard library only, no frontend build step.

## Save and incarnate

Install the actual single-file CLI (generated with the small session/scanner
modules embedded, SHA-256 checked before atomic installation):

```sh
curl -fsSL http://soul.treer/install.sh | sh
treer-soul save --harness claude --name leader-state
treer-soul save --harness codex --session self --name leader-state
# New Agents have different identities: sharing must be explicit.
treer-soul save --harness claude --name leader-state --visibility workspace
treer-soul incarnate leader-state --machine target-machine
```

`save` accepts `--session UUID`, `--cwd DIR`, `--parent SOUL_ID`, `--label TEXT`,
`--reader STABLE_ID`, `--visibility private|workspace`, `--redact/--no-redact`
and `--allow-secrets`. Redaction defaults on, changes only private temporary
copies, preserves JSONL structure and reports aggregate counts. `--no-redact`
still encounters server secret rejection unless `--allow-secrets` is explicit.
`capture-codex` remains an alias. `upload --manifest ... --file PATH=LOCAL` remains
for generic environment-bound archives. `list`, `show NAME_OR_ID[@VERSION]` and
`delete NAME_OR_ID[@VERSION]` output JSON. Default name resolution is latest;
ID pins a specific immutable snapshot. List supports `--name`, `--harness`,
`--owner`, `--limit`, `--offset`; follow next_offset until null.

Self selection first asks its own Controller-registered AIS for the current thread,
then reads its own Agent-private saved thread or confirmed native session
environment ID. It never infers self from a shared cwd or selects the newest
session; missing authority needs an explicit --session. Claude captures project JSONL, project `memory/`, and `CLAUDE.md`.
Codex captures its rollout; the current launcher transfer allowlist deliberately
excludes shell snapshots because they can contain machine credentials. Legacy
archives retain their snapshot file, which is never imported or executed.
Codex save output explicitly reports omitted=[shell_snapshot], and the legacy
--include-shell-snapshot flag fails clearly instead of silently ignoring it. No auth/config credential
files are exported. Launcher consistency checks reject changing source files; original sessions
are never modified. `soul_sessions.py` is a thin bridge to the launcher-owned session export/import
CLI. Native file selection, consistency and rebasing belong to launcher adapters.
Soul validates the transfer bundle, decodes files into the tar so server scanning
sees actual bytes, and reconstructs checksums after redaction before import.
A launcher with session-transfer support must be installed on source and target.

Incarnation resolves the version first, then uses read-only `treer exec --machine`
to inspect harness executable/version, existence of login material, installed
launcher and free disk. It never reads or displays credential contents. Missing
requirements/old versions refuse creation; `--force` explicitly proceeds with
warnings. Current minimum tested contract: Codex 0.149.0 / Claude 2.0.0; this is
a conservative compatibility floor, not proof of native session portability.
Creation runs `treer agent admin create` under the caller's own authority.
`--folder F` is optional; omitting it retains the caller's current tree folder.
A private Soul cannot be handed to a brand-new unknown identity: the client
refuses before creating and suggests an explicitly workspace-visible new version.

The target downloads the single-file client, acquires its own fresh Soul token,
verifies archive size/hash and restores declaratively from manifest.adapter.
Each incarnation receives a new native session UUID (the source UUID remains in
snapshot metadata), preventing global resume from selecting an unrelated original.
Each Agent has its own `~/.local/state/treer-soul/AGENT_ID/workspace` and restore
marker. Startup after completed restoration never downloads/reimports or overwrites
new learning. Claude uses the dedicated cwd's unique project directory and memory;
Codex/Claude storage selectors come from the launcher import result and must
remain inside the dedicated Agent directory, except Claude may select exactly
the target's already configured local Claude home while creating a separate
project keyed to the new cwd. Unrelated external config paths are rejected. Provider login remains machine-local
and is handled by the launcher adapter; Soul never exports or reads credentials. Shell snapshots are
archival only; Soul never executes them. Generic souls support an explicit command after `--`.

Launcher path comes from `TREER_AGENT_LAUNCHER`, or
`TREER_AGENT_INSTALL_ROOT/current`, or
`whoami.machine.root/.treer/launchers/agent/current`. It invokes
`launchers/agent/scripts/treer-agent.sh --harness H --session-id ID --cwd DIR`.
Install the unified launcher on target first; no source-machine path is embedded.
`TREER_SOUL_STATE_DIR` overrides restore storage; `TREER_SOUL_SERVICE` defaults to
`soul` for `treer token create`; `TREER_SOUL_URL` overrides the installed URL.

## Permissions, scanning and API

The complete wire contract is [API.md](API.md). Every `/v1/` request verifies the
supplied token through `/.treer/apps/identity/verify`, requiring active identity,
this service audience/workspace and current directory membership. Authentication
order: injected X-Treer-Identity-Token, explicit Bearer, then human OAuth session.
Invalid supplied headers never fall back to cookies. OAuth uses PKCE plus a
separate 10-minute HttpOnly/SameSite=Lax browser correlation cookie, stored hashed
alongside state; callback validates the initiating browser before consuming code.
Missing/wrong cookies cannot consume another browser's flow or clear its valid
correlation cookie; successful or matched-expired callbacks clear cookies and
replay fails. Unbound old OAuth flows are
invalidated on upgrade. No local account system and
no trusted caller ID/name/role fields. Human mutations require X-Soul-Request: 1;
all browser writes check Origin against configured App/Proxy origins and reject
cross-site Fetch Metadata. Manuals, installer, client assets and health are public;
all snapshot data, lists, archives, manifests and usage require authentication.

New snapshots default private. Owner can read/delete; workspace visibility grants
members read access; readers grants selected directory principals read access.
Only owner can delete, including when other humans have admin roles. Content,
labels and ACL are immutable: upload a new snapshot to change them. Parent must
be readable and owned by the caller or legacy. Versions monotonically increase
per owner/name, including deleted versions. Listings expose only visible records
and show the caller's active logical usage. Quotas charge expanded bytes including
manifest, independently of physical deduplication.

Server scans every regular file, including manifest and unbound files. Patterns
cover Treer workload/enrollment/machine credentials, sk-/GitHub tokens, Bearer,
JWT, private keys, AWS key IDs and password/secret/api_key assignments. A hit
rejects with 422 and category/count only, never matched content. Explicit
allow_secrets still scans and permanently marks contains_secrets plus counts.
This conservative pattern scanner can have false positives and cannot detect
all secrets (encoded, fragmented or unknown formats); redaction is best effort.
Treat transcripts and server state as sensitive even when no hits are detected.

DELETE retains a small ID/owner/name/version/deletion tombstone, frees quota and
removes unreferenced blobs. Shared blobs remain until their last active reference
is deleted. No undelete or background retry. Server `/incarnations` always returns
410 incarnate_deprecated to authenticated callers and never invokes create.
Remove all agent.admin.create permissions from the service's Policy.

## Migration, backup and rollback

Leader/operator performs deployment and migration with the old server stopped.
Never migrate a live directory. On first v2 start with legacy `souls/ID.json` and
`ID.tar`, the server creates a complete sibling `DATA.pre-v2-UUID` backup **before
creating its database or altering files**, prints its path, and imports all records
in one SQLite transaction. IDs, manifest and file bytes remain compatible. Records
become owner=legacy, visibility=workspace, created_by=legacy. Existing authenticated
Agent download/startup routes remain usable; legacy snapshots cannot be deleted
through API. Current metadata supplies reconstructed archive checksum/size.
Post-commit cleanup removes original archive/metadata files and unreferenced blobs;
interrupted cleanup is safe to resume. Failed migration preserves originals and
backup; restore the backup before retrying rather than manually editing SQLite.

Rollback: stop v2, move its data directory aside, copy the complete announced
pre-v2 backup to the original data path, deploy the old code and start it. New v2
uploads/deletes are not represented in the old backup; retain the moved directory
for recovery. Backups contain secrets and are deliberately not cleaned by DELETE.
Restrict backup access and retention separately.

Routine backup: stop the server, copy **the entire data directory** and deployment
configuration to private backup storage, then restart. Database-only backups omit
blobs. Restore the complete copy while stopped. SQLite uses secure_delete for
metadata cleanup; filesystem snapshots/backups/SSD retention are outside the App's
control. This is logical cleanup, not a secure-erasure guarantee.

## Checks and limits

```sh
python3 apps/soul/tests/test_soul.py
python3 apps/soul/tests/test_client.py
SOUL_PLAYWRIGHT_MODULE=/path/to/playwright/index.mjs node --test apps/soul/tests/browser.mjs
SOUL_TEST_SESSION_CLI=/path/to/launchers/agent/scripts/session.sh \
  python3 apps/soul/tests/test_launcher_integration.py
```

Tests allowlist subprocess environments, use temporary HOME/TMP/state, fake Treer
and local identity Proxy. They never create a real Agent or access a real machine.
Three opt-in tests also use the actual launcher session CLI with temporary fake
Claude/Codex files: export/redaction/import/restart and legacy Codex migration,
without a provider process or real login. Browser tests reproduce automatic forwarded identity in direct ingress and embedded
tunnel paths at desktop/390px with light/dark themes, manifest text/XSS, owner delete
and screenshots. Mock identity verification is not production issuer coverage;
Proxy identity signing is covered by the separate core tests. Real `/infra/soul`
deployment/migration/remote-machine acceptance remains the leader's responsibility.

Native session formats vary by harness/version; JSONL includes historical source
paths even though structured cwd is rebased for restore. Claude project storage is
isolated by cwd, not an OS sandbox. Provider login remains target-local and may change independently of the snapshot. Missing self session identity needs explicit --session.
No cross-owner mutable sharing, refresh tokens, background migration, scheduling,
secure disk erase, or remote download cache. Response/archive materialization and
list filtering currently use bounded-request in-memory processing; quotas are
single-process and per owner, not a workspace-wide disk cap. File blob dedup is
implemented; append-only JSONL delta chains are deferred to keep snapshots simple.
A failed first import can leave partial Agent-local native storage; the launcher
refuses overwrites. Inspect/move that dedicated native directory aside before
retrying; a successful marker prevents all later reimport. Restore targets must
be App-owned private directories, not user-supplied symlinks.
