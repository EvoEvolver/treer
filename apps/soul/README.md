# Soul

Soul is an experimental Managed App for immutable, versioned Claude/Codex
session snapshots. Humans browse their visible snapshots, inspect manifests,
copy incarnation commands and delete their own snapshots. Agents use the
standalone `treer-soul` client. The server never creates Agents; incarnation
runs under the caller's own authority. The wire contract is [API.md](API.md);
the Agent manual is [AGENT.md](AGENT.md).

## Deploy and configure

Use Workspace access, never `--public`:

```sh
treer app create --machine self --name soul --cwd . --port 9420 \
  env -- SOUL_DATA_DIR=.treer/apps/soul SOUL_PROXY_URL=https://YOUR_PROXY \
  python3 apps/soul/soul.py
```

Without `SOUL_SERVICE_ID`/`SOUL_WORKSPACE_ID`, the server discovers its own
Managed App by matching its injected runtime Agent ID in `treer app list`; it
never takes an audience from clients. Discovery uses the App's public URL or the
`/api/workspaces/W/apps/ID/ui/` tunnel root. Set `SOUL_PUBLIC_URL` when identity
is configured explicitly. Forwarded human identity works without wildcard
ingress; the OAuth fallback needs a registered callback origin.

| Variable | Default | Meaning |
| --- | --- | --- |
| `SOUL_LISTEN` | `127.0.0.1:9420` | HTTP listener |
| `SOUL_DATA_DIR` | `.treer/apps/soul` | Private database, blobs, temporary uploads |
| `SOUL_PROXY_URL` | `TREER_PROXY_URL` | Trusted Proxy origin; required |
| `SOUL_SERVICE_ID` | own Managed App discovery | Verify audience |
| `SOUL_WORKSPACE_ID` | `TREER_WORKSPACE_ID` / discovery | Workspace scope |
| `SOUL_PUBLIC_URL` | discovery, else `http://soul.internal` | Installer/OAuth root |
| `SOUL_MAX_UPLOAD_BYTES` | `67108864` | Compressed/tar request limit |
| `SOUL_OWNER_MAX_COUNT` | `10000` | Active snapshots per owner |
| `SOUL_OWNER_MAX_BYTES` | `10737418240` | Logical expanded bytes per owner |
| `SOUL_CLIENT_PATH` | bundled `client.py` | Source for the served client |
| `TREER_BIN` | `treer` | Own identity discovery only |

Run one server per data directory (an exclusive lock enforces it). The server is
Python standard library only, with no frontend build step.

## Save and incarnate

```sh
curl -fsSL http://soul.treer/install.sh | sh
treer-soul save --harness claude --name leader-state
treer-soul save --harness codex --session self --name leader-state
treer-soul save --harness claude --name leader-state --visibility workspace
treer-soul incarnate leader-state --machine target-machine
```

`save` accepts `--session UUID|self`, `--cwd DIR`, `--parent SOUL_ID`,
`--label TEXT`, `--reader STABLE_ID`, `--visibility private|workspace`,
`--redact/--no-redact` and `--allow-secrets`. Redaction is on by default and
changes only private temporary copies. `capture-codex` is an alias for
`save --harness codex`. `upload --manifest ... --file PATH=LOCAL` stores generic
archives. `list` (`--name`, `--harness`, `--owner`, `--limit`, `--offset`; follow
`next_offset`), `show NAME_OR_ID[@VERSION]` and `delete NAME_OR_ID[@VERSION]`
print JSON. A name resolves to its latest version; an ID pins a snapshot.

`--session self` asks the Agent's own registered Agent Interface for its current
thread, then its saved native session ID; it never guesses from a shared cwd.
Claude snapshots contain project JSONL, project `memory/` and `CLAUDE.md`; Codex
snapshots contain the rollout. Shell snapshots and auth/config credential files
are never exported. Source sessions are not modified.

`incarnate` resolves the version, then uses read-only `treer exec --machine` to
check the harness executable/version (minimum Codex 0.149.0, Claude 2.0.0),
presence of login material, the installed launcher and free disk, without
reading credential contents. Missing requirements refuse unless `--force` is
given. It then runs `treer agent admin create` as the caller (`--folder F`
optional, `--name` optional). A private Soul cannot be handed to a new identity;
save a workspace-visible version instead.

The new Agent downloads the client, gets its own Soul token, verifies archive
size/hash and restores into `~/.local/state/treer-soul/AGENT_ID/workspace`
(`TREER_SOUL_STATE_DIR` overrides) with a new native session UUID. After a
completed restore, restarts never re-import or overwrite later work.
`TREER_SOUL_URL` overrides the server URL (default `http://soul.treer`);
`TREER_SOUL_SERVICE` (default `soul`) names the service for `treer token create`.

Native session export/import is delegated to the Agent launcher. The client
locates it through `TREER_AGENT_LAUNCHER`, `TREER_AGENT_INSTALL_ROOT/current`, or
`<machine root>/.treer/launchers/agent/current`, and looks for
`treer-agent.sh` with a sibling `session.sh`. The current launcher ships
`scripts/treer-agent.mjs` (with a `session` subcommand) instead, so native
save/incarnate works only when `TREER_AGENT_LAUNCHER` points at an executable
that provides that layout. Generic souls (`upload`) are unaffected.

## Permissions and scanning

Every `/v1/` request verifies its token through `/.treer/apps/identity/verify`
(active identity, this service and workspace, current membership). Order:
injected `X-Treer-Identity-Token`, explicit Bearer, then human OAuth session;
an invalid supplied header never falls back to a cookie. OAuth uses PKCE plus a
10-minute HttpOnly/SameSite=Lax browser nonce. Human mutations require
`X-Soul-Request: 1`; browser writes check Origin and Fetch Metadata. Manuals,
installer, client and health are public; all snapshot data requires
authentication.

Snapshots default to private. The owner can read and delete; `workspace`
visibility grants members read access; `--reader` grants selected principals
read access. Only the owner can delete. Content, labels and ACL are immutable.
Versions increase monotonically per owner/name. Quotas charge expanded bytes,
independent of blob deduplication.

The server scans every file for Treer credentials, `sk-`/GitHub tokens, Bearer
values, JWTs, private keys, AWS key IDs and password/secret/api_key
assignments. A hit returns 422 with categories and counts only. `allow_secrets`
still scans and marks the snapshot `contains_secrets`. Scanning and redaction are
heuristic; encoded or unknown secret formats can be missed. Treat all snapshots
as sensitive.

DELETE keeps a small tombstone, frees quota and removes unreferenced blobs.
`/v1/souls/ID/incarnations` always returns 410 `incarnate_deprecated`; the
service needs no `agent.admin.create` permission.

## Storage and backup

Stop the server, copy the **entire** data directory and deployment configuration
to private backup storage, then restart; a database-only copy omits blobs.
Restore the complete copy while stopped. A data directory from the earlier
file-based Soul is imported on first start after a sibling `DATA.pre-v2-UUID`
backup is written; never import a live directory. Backups are plaintext and may
contain secrets: restrict and encrypt them. Deletion is logical cleanup, not
secure erasure.

## Limits

Tar preflight bounds the complete decoded stream (plain, gzip, bzip2, xz; no
zip) before parsing. Archive processing is in memory per bounded request; quotas
are per owner, not a workspace disk cap. A failed first import can leave partial
Agent-local native storage; move it aside before retrying. Same-UID Apps and
administrators can read data regardless of App ACLs; use a dedicated
unprivileged user or container/VM on critical-service machines.

## Checks

```sh
python3 apps/soul/tests/test_soul.py
python3 apps/soul/tests/test_client.py
SOUL_PLAYWRIGHT_MODULE=/path/to/playwright/index.mjs node --test apps/soul/tests/browser.mjs
SOUL_TEST_SESSION_CLI=/path/to/session-cli python3 apps/soul/tests/test_launcher_integration.py
```

Tests use temporary HOME/state, a fake Treer and a local identity Proxy; they
never create a real Agent or contact a real machine.
