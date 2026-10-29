# Agent Hub: App-owned recursive agents

A stdlib Python Managed App example. `team` and `team/tools` are App nodes;
`team/tools/agent1` echoes and `team/tools/agent2` reverses a message. Both are
App-owned functions, with no Host child process, launcher, credential or new
Policy principal. Core owns durable delivery (`MessagePrincipal.app_endpoint`,
recursive App addressing, `message send --from-endpoint`) and authenticates the
parent App. See [the shared tree/message contract](../../docs/app-tree.md).

## Start and manage

Use a private state directory and the Managed App's actual workspace, runtime
and service IDs. The App's runtime must use its own inherited workload CLI
environment; never borrow an operator's or another Agent's credential.

```sh
python3 -B apps/agent-hub/app.py --state-dir "$state" \
  --workspace "$workspace_id" --app-agent-id "$app_runtime_id" seed
python3 -B apps/agent-hub/app.py --state-dir "$state" \
  --workspace "$workspace_id" --app-agent-id "$app_runtime_id" serve \
  --treer "$(command -v treer)" --proxy-url "$proxy_origin" \
  --service-id "$app_service_id" --host 127.0.0.1 --port "$app_port"
```

Run `serve` as a Managed App and declare the tree contract so the Console shows
its subtree without probing:

```sh
treer app create --machine self --name agent_hub --cwd treer --port 8791 \
  --ui-capability app-tree.v1 python3 -- -B apps/agent-hub/app.py \
  --state-dir "$state" --workspace "$workspace_id" --app-agent-id "$app_runtime_id" \
  serve --treer "$(command -v treer)" --proxy-url "$proxy_origin" \
  --service-id "$app_service_id" --host 127.0.0.1 --port 8791
# An existing deployment: no restart.
treer app update /agent_hub --ui-capability app-tree.v1
```

Keep the App's HTTP service workspace-authenticated; no public access is
needed. The workspace must allow Core Message and the intended Policy
permissions.

Local operator commands (safe while `serve` runs):

```sh
hub() { python3 -B apps/agent-hub/app.py --state-dir "$state" \
  --workspace "$workspace_id" --app-agent-id "$app_runtime_id" "$@"; }
hub tree --parent team/tools
hub register team/tools/helper --kind agent --name Helper --handler echo
hub remove team/tools/helper
hub stats
```

HTTP has no registry mutation or process-control route. Removal disables a
subtree without discarding its identity or accepted jobs; jobs keep the handler
snapshot from admission. Re-enabling the same key keeps its kind.

## Recursive Tree API

`GET /api/tree?path=team/tools` returns immediate children:

```json
{"schema_version":1,"path":["team","tools"],"nodes":[
  {"key":"agent1","name":"Echo","kind":"agent","capabilities":["message.send","message.read"]},
  {"key":"agent2","name":"Reverse","kind":"agent","capabilities":["message.send","message.read"]}
]}
```

The root has `path:[]`. An App node has `capabilities:["tree.read"]`; only that
capability permits expansion. Children are stable single-segment keys under the
same owner App; there are no response-selected URLs, remote mounts, aliases or
cycles. An empty App returns `nodes:[]`; missing/disabled/non-App paths return 404.

Keys follow Core's case-sensitive ASCII grammar: 1–64 characters per segment,
starting alphanumeric, then alphanumeric/underscore/hyphen; at most eight
segments. The registry holds at most 256 identities and 100 children per parent;
names are at most 128 UTF-8 bytes. Disabled tombstones count toward these
bounds. Responses fit the shared 64 KiB bound.

Every tree request must carry `X-Treer-Identity-Token`. The App verifies it
through the Proxy's `/.treer/apps/identity/verify` against its exact service and
workspace and requires an active human. It does not decode claims, accept cookies,
follow redirects or fall back after an invalid assertion. The embedded route is
`/api/workspaces/{workspace}/apps/{app}/ui/api/tree?path=team/tools`.
`/health` and `/` contain no private data.

## Durable routing and replies

```sh
printf '%s' 'hello' | treer message send \
  --to "$app_runtime_id/team/tools/agent1" --idempotency-key unique-request
```

Core validates the Managed App and current Policy, accepts an immutable addressed
delivery and returns queue acceptance (not handler completion). The App validates
workspace, recipient ID and per-delivery `app_endpoint`, and commits the sender
return tuple, selected handler and message/key dedup entry to SQLite **before**
ACK. An unknown or disabled leaf produces a durable `app_endpoint_not_found` reply.

The handler result is saved as an immutable reply intent before sending. The App
supplies `--from-endpoint`, the original context and a stable per-message/per-key
idempotency key; Core stamps the authenticated App actor. Replies start with
`[app-hub:reply:v1] ` (a protocol marker, not an identity claim). Marked
responses, same-App messages and Agent messages in the context of a published
reply are consumed without another auto-reply; an authenticated human follow-up
still gets an answer.

Each sender is bounded to 32 admissions per minute and 32 pending jobs across all
its endpoints; at most 128 sender quota slots exist. Global bounds: 1,000 pending
jobs, 10,000 message/key tombstones, 10,000 delivery IDs. Over-capacity or
over-quota requests increment a body-free refusal counter and are ACKed without a
reply. Malformed/misaddressed input increments a poison counter before ACK;
conflicting immutable IDs and storage errors stay unacknowledged for recovery.
`stats` reports these counters.

After a timeout or restart the same Core request/key is retried using Core's send
idempotency; handlers here are pure, so this is not an exactly-once contract for
external side effects. Unconfirmed ACKs are retried independently of receive.
Completed rows erase request/response plaintext; the latest 128 terminal
tombstones remain, and older terminal rows are retired only after their ACKs are
confirmed. Pending replies are scheduled fairly across leaves.

SIGTERM/SIGINT stop admission and dispatch between CLI calls (20-second timeout
each), preserving pending work. HTTP reads time out after five seconds. One
process lock protects the dispatcher; the state directory must be private and the
database is mode 0600. No payload or token is logged.

## Validation

```sh
python3 -B -W error::ResourceWarning -m unittest discover -s apps/agent-hub/tests -v
```

The tests use real loopback HTTP and SQLite with a synthetic identity verifier
and CLI; they do not exercise a real Core, Policy or browser.
