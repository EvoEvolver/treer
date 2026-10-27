# Agent Hub: App-owned recursive agents (#230)

A stdlib Python Managed App example. `team` and `team/tools` are App nodes;
`team/tools/agent1` echoes and `team/tools/agent2` reverses a message. Both are
App-owned functions, with no Host child process, launcher, credential or new
Policy principal. Core owns durable delivery and authenticates the parent App.

The #230 Core successor is required: `MessagePrincipal.app_endpoint`, recursive
App addressing and `message send --from-endpoint`. The installed CLI need not
support these additions yet; use the exact CLI built with the joint local Core
fixture. No deployment or installed lifecycle operation is part of this example.

## Start and manage

Use a **new** task-owned private directory and the actual Managed App's workspace,
workload and service IDs. The App's runtime must inherit its own enrolled workload
CLI environment; never borrow an operator/another Agent's credential.

```sh
python3 -B apps/agent-hub/app.py --state-dir "$fixture_state" \
  --workspace "$workspace_id" --app-agent-id "$app_runtime_id" seed
python3 -B apps/agent-hub/app.py --state-dir "$fixture_state" \
  --workspace "$workspace_id" --app-agent-id "$app_runtime_id" serve \
  --treer "$fixture_cli" --proxy-url "$fixture_proxy_origin" \
  --service-id "$app_service_id" --host 127.0.0.1 --port "$app_port"
```

Declare the App's ordinary workspace-only HTTP service on that loopback port.
Keep it workspace-authenticated; no anonymous public service is needed. Core's
joint runner owns fresh resource allocation and cleanup. It must enable Core
Message and provide the intended Policy permissions rather than changing a live
workspace or reusing an old fixture database.

App operator commands, including while `serve` is running:

```sh
python3 -B apps/agent-hub/app.py --state-dir "$fixture_state" \
  --workspace "$workspace_id" --app-agent-id "$app_runtime_id" tree --parent team/tools
python3 -B apps/agent-hub/app.py --state-dir "$fixture_state" \
  --workspace "$workspace_id" --app-agent-id "$app_runtime_id" register team/tools/helper \
  --kind agent --name Helper --handler echo
python3 -B apps/agent-hub/app.py --state-dir "$fixture_state" \
  --workspace "$workspace_id" --app-agent-id "$app_runtime_id" remove team/tools/helper
```

These are local operator configuration commands. HTTP has no registry mutation
or process-control route. Removal disables a subtree without discarding its
identity or accepted jobs; jobs retain the handler snapshot from admission.
Re-enabling the same key keeps its kind. The App owns that reuse policy.

## Recursive Tree API

`GET /api/tree?path=team/tools` returns immediate children:

```json
{"schema_version":1,"path":["team","tools"],"nodes":[
  {"key":"agent1","name":"Echo","kind":"agent","capabilities":["message.send","message.read"]},
  {"key":"agent2","name":"Reverse","kind":"agent","capabilities":["message.send","message.read"]}
]}
```

The root has `path:[]`. An App node has `capabilities:["tree.read"]`; only that
capability permits expansion. Children are stable single-segment keys appended
to the requested path under the same owner App. There are no response-selected
URLs, arbitrary remote mounts, aliases or cycles. Nested App nodes compose
App-owned namespaces and handlers; they need not be separate Core deployments.
An empty App returns `nodes:[]`; missing/disabled/non-App paths return 404.

Keys match Core's case-sensitive ASCII grammar: 1–64 characters per segment,
start alphanumeric, then alphanumeric/underscore/hyphen; maximum eight segments.
The registry holds at most 256 identities, at most 100 children per parent;
names are at most 128 UTF-8 bytes. Disabled tombstones count toward those bounds.
The maximum response fits the shared 64 KiB bound.

Every tree request must carry `X-Treer-Identity-Token`. The App asks the configured
Proxy `/.treer/apps/identity/verify` to verify it against the App's exact service
and workspace and requires an active human. It does not decode claims, accept
cookies as identity, follow redirects or fall back after invalid assertions.
The ordinary authenticated embedded route is:
`/api/workspaces/{workspace}/apps/{app}/ui/api/tree?path=team/tools`.
`/health` and the static root manual contain no private message/identity data.

## Durable routing and replies

Send through the normal updated CLI, for example:

```sh
printf '%s' 'hello' | "$fixture_cli" message send \
  --to "$app_runtime_id/team/tools/agent1" --idempotency-key unique-request
```

Core validates the real Managed App and current Policy, accepts an immutable
addressed delivery and returns queue acceptance. This is not handler completion.
The App validates workspace, actual recipient ID and per-delivery `app_endpoint`.
It commits the sender return tuple, selected handler and message/key dedup entry
to SQLite **before** ACK. Two keys addressed by one message remain distinct jobs.
An unknown/disabled leaf produces a durable `app_endpoint_not_found` reply.

The pure handler result is saved as an immutable reply outbox intent before
sending. The App supplies `--from-endpoint`, the original message context and a
stable per-message/per-key idempotency key. Core stamps the authenticated App
actor; the endpoint is an App assertion. Replies to another App's endpoint keep
its complete return path. Human replies target the authenticated human ID.

Replies start with `[app-hub:reply:v1] `. This is an App protocol marker, never
an identity claim. Marked responses, same-App messages, and messages contextual
to a retained published reply from an Agent are consumed without another
auto-reply. An explicit authenticated human follow-up may retain the previous
reply context and still receive a new answer; a human conversation is not an
automatic responder chain. Each
authenticated sender is additionally bounded to 32 new admissions per minute
and 32 pending jobs, across all its endpoint assertions, so stripping a marker
cannot sustain an unbounded ping-pong. At most 128 current sender quota slots
exist; excess senders receive a durable aggregate refusal, not new rows.

After a timeout or restart, the same Core request/key is retried, using Core's
send idempotency; this example has no external handler side effects. A real
handler with irreversible effects needs its own uncertainty/reconciliation
contract and must not infer safe resend from this example. Pending replies are
scheduled fairly so one failed send cannot permanently starve another leaf.

SIGTERM/SIGINT stop admission and reply dispatch between CLI calls (each has a
20-second timeout), preserving pending work for restart. HTTP connections have
a five-second read timeout and cannot hold worker shutdown indefinitely.

A single process lock protects the dispatcher. Transactions use SQLite's write
lock, including concurrent local operator changes. The state directory must be
private; the DB is mode 0600. Completed rows erase request/response plaintext. The most recent 128 terminal
ID/fingerprint tombstones remain; bounded cleanup retires older terminal rows
only after every observed Core delivery ACK has been confirmed, and retains a
body-free retirement count. Pending jobs and unconfirmed ACKs are never retired.
Saved unconfirmed ACK selectors are retried independently of receive, including
after restart: Core may already have accepted the ACK while its response was
lost, leaving no redelivery to trigger the retry. Core currently never reselects acknowledged deliveries (and has no ACK-reset
path); this is the cleanup dependency, not an invented time-based redelivery
horizon. The deterministic outbound Core key also protects an unusual old
replay only while Core retains its sender/key idempotency record. Current Core
source has no deletion/retention path for that table; a future Core retention
contract requires reevaluating this dependency. This example has pure handlers
and does not promise exactly-once external effects. No source payload or token is logged.

Bounds: 1,000 pending jobs, 10,000 message/key tombstones and 10,000 delivery IDs.
At queue/tombstone/delivery capacity, or per-sender quota pressure, an
unadmitted request increments a durable body-free refusal counter and is ACKed
without executing or queuing any reply. No busy reply means no refusal amplifier.
A lost refusal ACK may be re-evaluated later; no effect existed to duplicate.
Local operator `stats` reports bounded counters. Storage/immutable-ID conflicts
remain unacknowledged for recovery. Refused input cannot fill a permanent
unacknowledged page and starve other senders. Malformed/misaddressed input with
a valid delivery selector increments one body-free durable saturating poison
counter before ACK; conflicting immutable IDs and storage errors are held.

## Validation

```sh
python3 -B -W error::ResourceWarning -m unittest discover -s apps/agent-hub/tests -v
```

These are local App tests, including actual loopback HTTP and SQLite, synthetic
identity verifier and CLI boundary checks. They cover recursive tree changes,
missing identity, wrong audience/workspace, route escape, commit-before-ACK,
crash/replay, send uncertainty, distinct leaf routing, poison and capacity.
They do **not** establish actual Core/Policy or browser acceptance. The joint
Core runner `scripts/app-tree-local-acceptance.py` owns those fresh native-stack
results; Web owns its browser client on the same stack. No real channel send,
public exposure, release or installed lifecycle change is authorized here.
