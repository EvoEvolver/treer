# App-owned trees and message addresses

A Managed App can implement its own tree of Apps and Agents. These are logical
nodes: Treer does not create a Host process, PTY, credential, or update job for
each child. The App owns its internal structure, implementations, delegation,
and business failures. Core owns authenticated addressing, durable delivery,
mailbox visibility, Policy, and the parent App's existing lifecycle.

## Recursive directory

An App that implements this contract declares the `app-tree.v1` UI capability
(`treer app create|update ... --ui-capability app-tree.v1`, or
`ui_capabilities` on the App create/update API). Core persists the declaration
and returns it as `AppDeployment.ui_capabilities`; it is presentation metadata
and grants nothing. The Console makes a declaring App expandable without a
request. For an App that declares nothing, the Console asks each running App
once per page session for its root page (`Accept: application/json`, 4 s timeout). Only a `schema_version: 1`
JSON answer, or a 401/403, makes the App row expandable; 404/405/501, an HTML
SPA fallback or any other non-tree reply leaves it a plain leaf with no
disclosure, and 5xx/timeouts are asked again after 30 s instead of guessed.
An expandable App reads its own `GET /api/tree?path=` API through the
authenticated App UI tunnel:

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

### Per-endpoint inbox

With `message.endpoint-inbox.v1` (Proxy `/api/health` and Controller
capabilities), an App can poll and acknowledge one endpoint instead of its
whole mailbox:

```sh
treer message receive --endpoint team/tools/agent1 [--after CURSOR] [--limit 50] [--wait MS]
treer message ack DELIVERY_ID... --endpoint team/tools/agent1 [--operation-id ID]
```

The same JSON goes to `POST /api/messages/receive` and `/api/messages/ack`
(Controller), `/agent/workspaces/WS/messages/receive|ack` (Proxy, machine
credential) and `/api/apps/SERVICE_ID/messages/receive|ack` (service token):

```json
{"app_endpoint":"KEY","after":"CURSOR","limit":50,"wait_milliseconds":0}
{"app_endpoint":"KEY","delivery_ids":["dlv_..."],"operation_id":"ID"}
```

`app_endpoint` only narrows the caller's own mailbox: deliveries are owned by
the App runtime identity, so another workload's endpoint returns nothing and
its delivery IDs are `message_delivery_not_found`. Receive orders by delivery
time and ID, never consumes, and reports `remaining_unacknowledged` for that
endpoint. An endpoint or `after` page also returns `next_cursor` (omitted on an
empty page); a cursor only advances the scan, so omit `after` to see every
still-unacknowledged delivery again. A scoped ACK fails entirely when any
delivery belongs to another endpoint. Retrying the same `operation_id` returns
the saved response; reusing it with another endpoint or delivery set is
`message_ack_idempotency_conflict`. Whether an item is a prompt or a message is
App metadata, not a Core field.

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

The bundled [Agent Hub](../apps/agent-hub/README.md) uses this contract for
real work: `templates/`, `jobs/` and `handoffs/` are App nodes, and a running
`jobs/<id>` is an `agent` node whose Messages the hub forwards to the job's
real worker Agent with `treer agent prompt`; once archived it becomes an App
node. It records each handled message ID before ACK, so a redelivery after a
crash is not forwarded twice, and creates at most one job per root Message.

Message routes require `TREER_ENABLE_CORE_MESSAGES`. Clients must not strip a
logical address and retry against the root mailbox when a send fails. Apps
without the directory API are shown without children.
