# Treer Mail plugin

Treer Mail preserves the existing human web mailbox while using Core Message as
its only collaboration store. The standard-library Python server owns browser
cookies, pending return paths, and static presentation. Every identity,
directory, Message, delivery, DAG, acknowledgement, and policy operation is a
nested `treer` command through the plugin broker.

## Prerequisites and package

Mail runs from a dedicated managed bridge Agent on the same enrolled machine as
its registered HTTP service. The service ID is the generic App OAuth client and
must have a workspace ingress whose callback is `<public_url>/api/auth/callback`.
On Linux, start that bridge Agent from a Controller configured with
`TREER_NETWORK_MODE=proxy-env`: the Mail listener must be reachable as a
host-network machine service, while a transparent Agent's loopback listener is
inside its private network namespace. The Controller excludes loopback from the
injected SOCKS proxy, so local CLI and Mail HTTP calls remain direct.

Build the existing React surface before validating or packaging a source
checkout:

```sh
cd plugins/mail/web
pnpm install --frozen-lockfile
pnpm typecheck
pnpm build
cd ../../..

treer plugin validate plugins/mail
treer plugin install plugins/mail
```

Release plugin archives include the built `web/dist` assets. Dependency trees,
TypeScript build metadata, Python bytecode, and local state are not release
artifacts.

## Configuration

Create an operator-owned JSON file readable by the bridge Agent:

```json
{
  "listen": "127.0.0.1:8788",
  "service_id": "svc_mail",
  "public_url": "https://mail.workspace.example/",
  "proxy_public_url": "https://treer.example/"
}
```

`web_dir` may override the package-relative `web/dist` path for development.
Start the installed package from the managed bridge Agent:

```sh
TREER_ENABLE_PLUGIN_EXECUTION=true \
  treer plugin run mail --config /etc/treer/mail.json
```

The Proxy must also have `TREER_ENABLE_CORE_MESSAGES=true` and
`TREER_ENABLE_PLUGIN_SESSIONS=true`. All three gates default off so a deployment
can apply schema and policy changes before admitting new channel traffic.

The manifest grants only `plugin.oauth`, human/Agent directory reads, and
Message send/read/receive/ack. Mail has no direct Proxy URL, Core database, NATS,
or raw Treer credential. `proxy_public_url` is retained only in the compatibility
`/api/config` response; OAuth navigation uses the URL returned by Core.

## Browser and API compatibility

The plugin preserves these routes:

```text
GET  /api/health
GET  /api/config
GET  /api/auth/start
GET  /api/auth/callback
GET  /api/auth/session
POST /api/auth/logout
GET  /api/directory
GET  /api/messages
POST /api/messages
POST /api/inbox
```

OAuth creates a revocable capability bound to this plugin, workspace, service,
and bridge Agent. The browser receives only an HttpOnly, SameSite=Lax Mail
cookie. Each authenticated request rechecks the capability and current workspace
membership. Logout revokes Core capability state before deleting the local
cookie mapping. Deleting the registered service, removing the member, revoking
the session, or uninstalling the plugin invalidates subsequent use. The control
plane login preserves only the exact same-Proxy App OAuth authorization return
path, so an unauthenticated Mail login can resume without accepting an arbitrary
redirect.

Recent history and inbox reads translate Core's explicit delivery ack into the
legacy `unread` and `remaining_unread` response. Human-to-Agent and
Agent-to-human Messages retain recipients, timestamps, and ordered context IDs.
Managed Agents no longer call Mail with an App Bearer token; they use
`treer message` directly, which remains available even when Mail is stopped.

## Legacy migration

`migrate.py` reads a legacy database but writes only through operator-authorized
`treer message import`. SQLite uses a read-only standard-library connection.
PostgreSQL uses `psql` with structured JSON output; prefer a service definition
or `.pgpass` so database credentials do not appear in its command arguments.

1. Back up the source database and stop the old Rust Mail service.
2. Dry-run validation and preserve the structural report and JSONL export:

   ```sh
   python3 plugins/mail/migrate.py \
     --source sqlite://treer-mail.db \
     --workspace <workspace-id> \
     --actor <operator-or-change-ticket> \
     --dry-run \
     --export-file mail-legacy.jsonl \
     --report mail-migration-report.json
   ```

3. Import with the local machine operator identity, outside a managed Agent and
   outside the plugin broker:

   ```sh
   python3 plugins/mail/migrate.py \
     --source sqlite://treer-mail.db \
     --workspace <workspace-id> \
     --actor <operator-or-change-ticket> \
     --export-file mail-legacy.jsonl \
     --report mail-migration-report.json
   ```

   Use a `postgresql://` source and `--psql /path/to/psql` for PostgreSQL.

4. Preserve the schema-v2 report. It records the actor, source checksum and
   checksum scope, structural checksum, source counts, batch timestamps,
   deterministic operation IDs, and target counts without Message bodies.
   Rerunning with the same report verifies those identities, skips completed
   checkpoints, and resumes the first incomplete batch. A changed source,
   workspace, actor, batch size, or checksum is rejected rather than merged into
   an earlier cutover record.
5. Start the plugin on the existing service/ingress, verify health, log in again,
   inspect migrated branching context, and exchange a new reply before reopening
   access.

The migration preserves Message IDs, workspace, bodies, timestamps, sender and
recipient snapshots, recipient order, read state, and ordered context edges. It
never mutates or deletes the source database. Legacy App browser sessions are
reported but deliberately not converted because that would broaden an old App
token into the new plugin capability model.

On an import failure, the report records only a stable failure code, stage,
optional batch index, and timestamp; the detailed body-free error remains on
stderr. The source database and completed Core batches are left intact for a
validated resume.

Retain the source backup read-only through the rollback window. Before any new
Core Message is created, rollback may point the service back to the old binary.
After new Core writes exist, use roll-forward recovery: keep Core authoritative,
repair or replace the stateless Mail presentation, and never reopen legacy
writes. Restoring the old database after that point would create split history.

## Known limits

- Mail v1 is text-only and bounded to 32 KiB, 32 recipients, and 32 contexts.
- Core and the Proxy PostgreSQL operator can see Message bodies.
- The Python server is a single plugin instance; its SQLite cookie mapping is
  not an active-active session store.
- The CLI broker withholds credentials but does not isolate hostile same-UID
  plugin code.
- Attachments, Message retention/export/deletion policy, billing, and
  active-active Mail plugin instances are not implemented.
