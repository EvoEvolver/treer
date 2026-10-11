# Unified App and Agent workloads

The approved task is to unify lifecycle, present both kinds in a persistent
folder tree, and expose the same start/stop/restart operations in CLI and UI.
This change keeps Treer's scope at identity, transport, and process recovery.
It does not add a sandbox, transparent networking, or custom domains.

## Model and ownership

- One workload has a stable Agent ID, credential, resolved startup command,
  machine, desired state, and monotonically increasing process generation.
- An App adds service and browser-access metadata to that workload. New App
  records use the same identity as their process; restart never allocates a
  second Agent identity. Existing active legacy identities remain unchanged.
- The Controller alone owns process launch and recovery. Proxy reconnect
  declares legacy App startup specifications and adopts surviving processes.
  Proxy exit-triggered replacement and independent App restore are removed.
- Enabled Apps retry exited processes; ordinary Agents retain their completion
  behavior. Explicit start is idempotent, restart replaces a process once,
  stop persists disabled recovery, and deletion revokes the identity.
- Host inventory, persisted generations, and a shared Controller lock fence
  concurrent commands and delayed exit events. Explicit start/restart first
  declare missing App specifications without launching an extra process.

## Migration

Keep durable service IDs, platform domains, access mode, and App-owned data.
Preserve active runtime credentials when adopting legacy Apps. If a cold legacy
App has lost its plaintext credential, rotate only its active machine-bound
credential. An old stopped App whose prior runtime credential was revoked
migrates once to its canonical App identity; revoked credentials stay revoked.

Keep `treer app` as a compatibility interface for deployment/service settings.
Use `treer agent start/stop/restart/read` and the shared Agent API for lifecycle
and logs. Capability negotiation rejects new lifecycle commands on older
Controllers without disconnecting their machine; update the Controller to use
new start/restart behavior. Stop remains a baseline command.

## Workspace tree

Store folders and workload placements in PostgreSQL. Support nested folders,
drag/drop, a keyboard/touch move dialog, rename, delete, and reload. Folder
deletion promotes children to its parent. Moves change presentation only;
machine ownership, filesystem paths, credentials, and Policy stay unchanged.
Membership checks, workspace-scoped foreign keys, serialized mutations, and
cycle validation enforce tree consistency.

## Verification and release

Verify real Host cold recovery, hot Controller adoption, stable credentials and
IDs, generation fencing, concurrent starts, and stopped intent. Run Proxy
PostgreSQL migration/auth/tree tests, protocol/CLI checks, strict Rust Clippy,
and desktop/mobile browser workflows. Logs remain bounded PTY replay rather
than a durable archive; successful restore does not prove endpoint readiness.

Deploy a clean source commit to Canary and record health, exact Railway and
Worker versions, and downloaded artifact revision under `.treer/releases/`.
The user explicitly excluded multi-machine testing, so record that omission
and use `canary_deployed`, not `canary_passed`. Existing installed Controllers
must be updated separately; publishing new artifacts does not update user
machines automatically.
