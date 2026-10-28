# App-owned trees and message addresses

A Managed App can implement its own tree of Apps and Agents. These are logical
nodes: Treer does not create a Host process, PTY, credential, or update job for
each child. The App owns its internal structure, implementations, delegation,
and business failures. Core owns authenticated addressing, durable delivery,
mailbox visibility, Policy, and the parent App's existing lifecycle.

## Recursive directory

The Console expands a Managed App by reading its own `GET /api/tree?path=` API
through the authenticated App UI tunnel:

```text
GET /api/workspaces/WORKSPACE/apps/APP/ui/api/tree?path=team%2Ftools
```

The response contains the requested relative path and its immediate children:

```json
{
  "schema_version": 1,
  "path": ["team", "tools"],
  "nodes": [
    {"key": "agent1", "name": "Echo", "kind": "agent",
     "capabilities": ["message.send", "message.read"]}
  ]
}
```

Nodes may have `kind: "app"` or `kind: "agent"`. Only `tree.read` enables
expansion; only explicit message capabilities enable conversation actions.
Unknown capabilities confer no action. An App may delegate a child App's page
to another implementation while keeping this root-relative contract. Responses
cannot choose URLs, credentials, another owning App, or an ancestor mount.

Each key is one case-sensitive ASCII segment matching
`[A-Za-z0-9][A-Za-z0-9_-]{0,63}`. A full relative path has at most eight
segments (519 bytes). Directory pages contain at most 100 children and 64 KiB.
The Console rejects duplicate keys, wrong echoed paths, excessive depth and
invalid pages; it displays empty, denied and unavailable states separately.
Collapse/reopen refreshes the App-owned page. Strictly increasing relative
paths and the depth bound prevent response-driven recursion cycles.

## Messages

```sh
printf '%s\n' 'Review this change' | treer message send \
  --to /infra/agent_hub/team/tools/agent1 --idempotency-key review-42
```

Core first resolves an ordinary existing Tree target. If no such target exists,
it resolves a real Managed App prefix and validates the remaining relative
path. Ordinary Agents and folders cannot own App subaddresses. Stable addresses
use `OWNER_WORKLOAD_ID/team/tools/agent1`; the runtime workload binding is
authoritative, including legacy Apps whose App ID differs from their runtime ID.

`MessagePrincipal.app_endpoint` records the relative path. Its `kind/id` remains
the authenticated owning App, and `name/path` remains a snapshot of that App.
The Console combines both for display. Moving or renaming an App does not change
accepted mailbox identities; deleting/recreating the same name creates a new
owner. App-local key reuse and renaming are the App's responsibility.

Core does not claim that a key exists or that a handler has run. Send success
means durable acceptance. The App receives root and child deliveries through
its normal `treer message receive`, persists acceptance before ACK, and routes
each `delivery.recipient.app_endpoint` itself. Distinct child paths and the root
mailbox are distinct deliveries, even when they share the same owner. ACKs remain
delivery-specific and authenticated to the owning mailbox.

An App replies with its explicit logical source:

```sh
printf '%s\n' 'Review complete' | treer message send --to USER_ID \
  --from-endpoint team/tools/agent1 --context MESSAGE_ID \
  --idempotency-key app-review-result-42
```

Only the authenticated Managed App can use its own `--from-endpoint`; a human
or ordinary Agent cannot impersonate this identity. Core cannot attest which
internal model or function executed. `treer message reply MESSAGE_ID` preserves
a received logical sender's full stable subaddress. App-to-App replies use the
same protocol. Bodies may use `--stdin-envelope` to stay out of argv.

The owning principal remains the Policy and mailbox authorization boundary.
Internal nodes do not gain independent credentials or Policy subjects, and an
App can read its entire mailbox. Directory capabilities describe available UI
operations; they never bypass Core Policy. App code owns any finer internal
authorization and must verify current service-scoped forwarded human identity.

Browser conversations use authenticated human routes `GET/POST
/api/workspaces/WORKSPACE/messages` and `GET .../messages/MESSAGE_ID`. They check
current workspace membership and the same Message Policy/visibility rules as
other transports. Mutations retain existing cookie Origin protection. Optional
paired query fields `app_owner_id` and `app_endpoint` filter an already-visible
conversation **before** pagination; they never grant access to other messages.

## Offline, failure and retry

The durable App deployment keeps its mailbox addressable while the runtime is
offline. Messages wait for the App to receive them. App directory requests may
be unavailable during that interval; the Console does not fabricate a tree.
An unknown/removed internal key is an App-level failure, not a fallback to the
root mailbox or a similarly named Host Agent.

Core idempotency includes logical sender and recipient addresses. Repeating an
identical request with the same key returns the original IDs; changing a child
path under the same sender/key conflicts. Keys are scoped to the authenticated
owner, not each internal endpoint; Apps should namespace them. Delivery and App
execution are separate: Apps must use a durable inbox/outbox and cannot infer
exactly-once external effects from ACK or a network timeout.

The bundled `apps/agent-hub` example persists local nodes, receipt tombstones,
immutable outbox replies and retry keys. Its pure echo/reverse handlers have no
external side effects. Queue/storage bounds refuse further admissions. Confirmed
terminal rows may be retired in bounded batches; pending work and unconfirmed
ACKs remain. This depends on Core's current ACK finality and retained sender/key
idempotency, as documented in the example's README. Removing a key stops its future local
handling; already accepted work follows the App's persisted job contract.

Message routes require `TREER_ENABLE_CORE_MESSAGES`. Clients must not strip a
logical address and retry against the root mailbox when a send fails. Apps
without the directory API are shown without children.
