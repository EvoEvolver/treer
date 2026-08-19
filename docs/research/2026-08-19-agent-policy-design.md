# Agent communication policy design

- Status: enforcement foundation implemented
- Reviewed against source: 2026-08-19

## Implementation progress

Implemented on `codex/agent-policy-design`:

- versioned shared policy document, selector, group, rule, mode, effect, and
  persisted-record models;
- one `workspace_policies` JSONB row per workspace with optimistic revision,
  actor metadata, document bounds/validation, and transactional
  `treer_policy_changed` notification;
- a standalone `WorkspacePolicyStore` with typed get/replace operations;
- managed-Agent identity validation and propagation from the CLI, through the
  Controller, to Proxy HTTP and terminal WebSocket requests for discovery and
  Agent control operations as well as the existing mail/service paths.
- Proxy-side workload credential hashing, validation, and machine/workspace
  binding;
- machine-only same-machine guards and Agent-token requirements for
  cross-machine control;
- action-indexed JSONB policy compilation and five-second per-workspace caches;
- enforcement for Agent discovery, metadata, prompt, input, output, terminal,
  lifecycle, creation, and machine mutation routes.

Not implemented in this slice:

- check/explain and policy-management APIs;
- PostgreSQL notification-driven cache invalidation (the current cache uses a
  five-second freshness bound);
- policy administration UI, delegation, or durable decision audit.

## Goal

Let workspace administrators control, independently, which humans and Agents a
managed Agent may discover, inspect, mail, prompt, or control. The design must
remain explainable, work with one Proxy or many Proxy replicas, and add no
PostgreSQL query to an ordinary authorization decision.

This is a control-plane authorization system. It does not turn the current
shared-host runtime into an adversarial sandbox.

## Current behavior and gaps

Treer already has the correct basic authorization shape in
`crates/treer-proxy/src/policy.rs`:

- a workspace-scoped `PolicyRequest`;
- structured `PolicySubject`, action, and `PolicyResource` values;
- an asynchronous evaluator chain;
- policy calls around mail, inbox, human-directory, service, virtual-host,
  network, and workload-identity operations.

Production constructs the engine with a durable evaluator. It lazily loads and
compiles each workspace document, indexes rules by action, and reuses the
immutable result for five seconds. Agent control routes now provide distinct
actions for discovery, metadata, prompt, raw input, output, terminal, stop,
create, update, and delete. Browser calls retain membership authorization and
do not become machine or Agent policy subjects.

Remaining gaps are policy management/check/explain APIs, notification-driven
cache invalidation, pinned revisions across a whole batch, and durable decision
audit. A mail send still resolves the complete workspace directory before
authorizing each recipient, so denial behavior must avoid leaking hidden names.

## Security boundary

The first version provides reliable authorization between authenticated
control-plane principals. A managed Agent request must carry its stable Agent
ID and workload credential to the Controller; the Controller validates the
credential and forwards an authenticated Agent principal under its machine
credential. A local human/operator request must instead carry a Controller
operator credential that is never injected into an Agent environment. The
Proxy then verifies that an Agent belongs to the authenticated machine and
workspace; operator requests use the separate machine principal.

The current local runtime is not strong isolation. Agents and the Controller
can share an OS account and coding Agents run with permission-bypass flags. A
hostile same-account process may steal the owner-only operator or machine
configuration even though simply omitting Agent headers is now rejected by the
Controller. Strong enforcement against that attacker requires filesystem/process
isolation around the distinct local operator credential, ideally a container
or microVM. Until then the product claim is governed coordination for trusted
hosts, not a sandbox boundary.

The policy implementation must still close accidental and remote control-plane
bypasses:

1. Managed-Agent CLI calls authenticate as the Agent on every governed route.
2. Missing Agent credentials never silently upgrade a request to machine
   authority: governed local operations require either a valid Agent workload
   credential or the separate local operator credential.
3. Browser requests authenticate as a human; machine requests remain a
   separate principal class.
4. Workspace membership and machine binding are checked before policy.

## Policy vocabulary

Discovery, observation, communication, interruption, and lifecycle control are
different permissions. Do not collapse them into `agent.access`.

| Action | Resource | Meaning |
| --- | --- | --- |
| `agent.discover` | `agent` | Include a target in list/search/name resolution |
| `agent.metadata.read` | `agent` | Read status, machine, cwd, and other metadata |
| `agent.output.read` | `agent` | Read replayed terminal output |
| `agent.prompt` | `agent` | Submit semantic text to the target PTY |
| `agent.input` | `agent` | Send raw terminal bytes or attach interactively |
| `agent.stop` | `agent` | Stop a running Agent |
| `agent.update` | `agent` | Rename or otherwise mutate Agent metadata |
| `agent.delete` | `agent` | Delete an Agent record/process |
| `agent.create` | `machine` | Create an Agent on a target machine |
| `mail.send` | `agent.mailbox` or `human.mailbox` | Add durable pull-only mail for a recipient |
| `mail.read` | own mailbox | Read and acknowledge the caller's deliveries |
| `human.discover` | `human` | Include a human in the Agent-visible directory |

`mail.send` remains pull-only and never wakes, prompts, or writes to a target
Agent. `agent.prompt` is active prompt injection. `agent.input` is stronger
because it permits arbitrary key sequences and interactive terminal input.

Names are mutable display labels and never appear in stored policy selectors.
The management API may accept a name for convenience, but it resolves and
stores a stable `ag_...`, `usr_...`, or machine ID.

The principal enum should be expanded now to cover `Human`, `Agent`, `Machine`,
and later `Service`. Keeping humans outside the engine would create a second
authorization language as soon as human-to-Agent restrictions are needed.

## Durable document

Use PostgreSQL as a KV-like document store: one atomic JSONB policy document
per workspace, addressed by `workspace_id`. Do not query JSONB during request
authorization and do not add a GIN index; the only online lookup is the primary
key when loading or refreshing a document.

```sql
CREATE TABLE workspace_policies (
    workspace_id TEXT PRIMARY KEY
        REFERENCES workspaces(workspace_id) ON DELETE CASCADE,
    revision BIGINT NOT NULL CHECK (revision > 0),
    schema_version INTEGER NOT NULL,
    mode TEXT NOT NULL CHECK (mode IN ('monitor', 'enforce')),
    document JSONB NOT NULL,
    updated_at TEXT NOT NULL,
    updated_by_kind TEXT NOT NULL,
    updated_by_id TEXT NOT NULL
);
```

Updates use optimistic concurrency:

```sql
UPDATE workspace_policies
SET revision = revision + 1, document = $document, ...
WHERE workspace_id = $workspace AND revision = $expected_revision
RETURNING revision;
```

Reject an update unless the complete document validates and compiles. Cap the
encoded document and rule count, initially 256 KiB and 1,000 rules, so a policy
cannot become a memory or compilation denial of service.

An example document is:

```json
{
  "schema_version": 1,
  "defaults": {
    "agent.discover": "deny",
    "agent.metadata.read": "deny",
    "agent.output.read": "deny",
    "agent.prompt": "deny",
    "agent.input": "deny",
    "mail.send": "deny",
    "mail.read": "allow"
  },
  "groups": {
    "reviewers": {
      "principals": [
        { "kind": "agent", "id": "ag_review" },
        { "kind": "human", "id": "usr_owner" }
      ]
    }
  },
  "rules": [
    {
      "id": "builder-can-see-reviewer",
      "priority": 100,
      "effect": "allow",
      "subjects": [{ "kind": "agent", "id": "ag_builder" }],
      "actions": ["agent.discover", "agent.metadata.read", "mail.send"],
      "resources": [{ "principal_group": "reviewers" }]
    },
    {
      "id": "builder-can-prompt-review-agent",
      "priority": 110,
      "effect": "allow",
      "subjects": [{ "kind": "agent", "id": "ag_builder" }],
      "actions": ["agent.prompt"],
      "resources": [{ "kind": "agent", "id": "ag_review" }]
    }
  ]
}
```

Version 1 selectors should be intentionally small: principal kind, stable ID,
machine ID for Agents, explicit groups, and the `self` relationship. A resource
that represents a principal-owned object, such as `agent.mailbox`, carries a
typed target-principal reference; `principal_group` matches that reference
rather than overloading the resource kind. Avoid an embedded expression
language. New typed selectors can be added behind a new schema version after
their invalidation dependencies are understood.

## Evaluation semantics

Evaluation is deterministic:

1. Reject a workspace mismatch before evaluating rules.
2. Pin one immutable compiled policy revision for the whole request.
3. Collect candidates from exact and wildcard indexes.
4. The matching rule with the highest priority wins.
5. At equal priority, deny wins over allow.
6. If no rule matches, use the action's explicit default.
7. In `monitor` mode, report the computed decision but preserve legacy allow
   behavior; in `enforce` mode, apply it.

The engine should return a `PolicyOutcome`, not only `Result<()>`. It contains
the effective decision, policy revision, matching rule ID or default, and
monitor/enforce mode. Normal API errors expose only a stable denial code;
owners/admins may use a separate explain API to see rule details.

Existing first-explicit-evaluator behavior can remain for composing independent
hard guards, but the durable rule evaluator itself follows the priority model.
Workspace isolation, authenticated-principal binding, and immutable system
limits are guards and cannot be overridden by a JSON rule.

## Discovery and non-disclosure

Policy must filter at the collection boundary rather than fetch everything and
hide it only in the UI.

- Agent list/search filters each resource by `agent.discover` in one batch.
- Agent get returns the same generic not-found response for absent and hidden
  targets.
- Name-based target resolution uses only discoverable candidates, preventing a
  hidden name from causing an ambiguity leak.
- A stable target ID may be used without discovery if the requested operation
  itself is allowed. A denied or nonexistent ID returns the same external
  result to an Agent.
- Reading metadata, output, prompting, and raw input each receive their own
  authorization check after target resolution.

Mail preserves the independence of discovery and sending. Name-based mail uses
the filtered directory. Stable-ID mail may resolve the full workspace directory
and then authorize `mail.send`, but failure is reported as a generic unresolved
recipient. All recipients are resolved and authorized before the message
transaction starts, so a multi-recipient send remains atomic.

Historical mailbox delivery is not re-authorized against the sender on every
read. The recipient's durable delivery is the access grant; `mail.read` governs
access to the recipient's own mailbox. Context IDs continue to reveal no body
unless the referenced message is already visible to that mailbox.

## Hot path and cache

PostgreSQL is authoritative but is not on the decision hot path.

Each Proxy owns a `PolicyStore` with a bounded, sharded map from workspace ID to
an immutable `CompiledPolicy`. Compilation expands groups and builds indexes by
subject kind/ID, action, and resource kind/ID. A typical exact decision probes
a fixed set of exact/wildcard buckets and a small candidate vector. Directory
filtering uses `authorize_batch` against the same pinned compiled value rather
than awaiting one policy future per Agent.

Expected request flow:

```text
authenticated request
  -> workspace hard guard
  -> cached compiled policy pointer
  -> indexed or batch evaluation (no await, no SQL, no NATS)
  -> operation
```

Cache misses use one coalesced PostgreSQL load per workspace. Entries have a
bounded idle TTL; workspaces without a document share one immutable legacy
policy object rather than allocating a copy.

Policy updates execute in PostgreSQL and call `pg_notify` in the same
transaction. PostgreSQL delivers the notification after commit. Every Proxy
keeps a dedicated `PgListener`, fetches the named workspace revision, compiles
it, and atomically swaps the cache entry. The writer swaps its local entry
before returning success.

`LISTEN/NOTIFY` is invalidation, not storage. On listener reconnect, a Proxy
invalidates and reloads its active entries before declaring policy health
restored. While invalidation health is unknown, cross-principal prompt, raw
input, terminal attach, and lifecycle operations fail closed; self-only inbox
access may continue from the cached policy. This prevents a missed revocation
from becoming an indefinite stale allow.

NATS is unnecessary for policy evaluation. A future NATS invalidation adapter
may reduce fan-out load at very large replica counts, but PostgreSQL remains
authoritative and the same compiled evaluator is reused.

Do not synchronously insert one audit row per authorization check. Rule changes
are durably attributed in their update transaction. Decision outcomes carry
revision/rule metadata into structured logs and later append-only audit events;
sensitive allowed operations and denials can be queued asynchronously without
adding a database round trip to list or terminal traffic.

## Alternatives considered

- **Normalized ACL rows:** useful for ad hoc SQL reporting, but a policy edit
  becomes a partial multi-row state and compilation still needs a revision
  boundary. A single JSONB value gives atomic replacement and optimistic
  concurrency. Materialized reporting tables can be derived later.
- **PostgreSQL row-level security:** useful for database-owned rows, but most
  Agent status, PTY output, and control commands live in Proxy/Controller
  memory. RLS cannot be the common enforcement point.
- **NATS as policy storage:** broker subjects are routing, not authorization
  semantics. NATS may broadcast invalidations later, but it should not decide
  policy or become mandatory for a single-Proxy installation.
- **A separate general policy service:** premature for the current vocabulary
  and adds a network hop/failure mode to every decision. The existing evaluator
  trait keeps that adapter possible if policy complexity later justifies it.
- **An embedded expression language:** flexible but difficult to validate,
  index, explain, and bound. Typed versioned selectors cover the immediate
  source/action/target problem while preserving a migration path.

## API and identity changes

Introduce a request principal at routing middleware rather than rebuilding a
subject independently in each handler:

```text
RequestPrincipal::Human { user_id, organization_role }
RequestPrincipal::Agent { agent_id, server_id }
RequestPrincipal::Machine { server_id }
```

The Controller must generate and validate a local operator credential, validate
Agent workload credentials, and propagate Agent identity for discovery,
list/get, prompt, input, output, attach, stop, create, rename, and delete, as it
already does for mail and inbox. An unauthenticated local request is rejected
rather than forwarded as a machine. WebSocket terminal setup must carry the
same authenticated principal through the upgrade and authorize both attach and
input before the stream opens.

Policy management is human-only in the first release and requires organization
owner/admin membership:

- `GET /api/workspaces/{id}/policy` returns document and revision.
- `PUT /api/workspaces/{id}/policy` validates, compiles, and conditionally
  updates by expected revision.
- `POST /api/workspaces/{id}/policy/check` evaluates hypothetical requests.
- `POST /api/workspaces/{id}/policy/explain` returns matching-rule details.

Agents receive a smaller check surface for their own principal so tooling can
avoid doomed operations, but they cannot read the full policy or enumerate
hidden targets. Policy administration by Agents and delegated capability
tokens are later features.

## Rollout

### Phase 0: close attribution gaps

1. **Pending:** Add `Human` to Proxy `PolicySubject` and introduce
   `RequestPrincipal` middleware.
2. **Complete for machine/Agent principals:** Managed-Agent workload
   credentials are validated by both Controller and Proxy; local operator
   credentials prevent header omission from becoming machine authority.
3. **Complete:** Prompt, raw input, attach, output, lifecycle, discovery, and
   machine mutation actions are authorized by the Proxy.
4. **Partial:** Credential, binding, precedence, and operator tests exist. Full
   two-machine HTTP and terminal denial tests remain pending.

### Phase 1: durable policy in monitor mode

1. **Partial:** The JSONB row, typed store, validator, revision handling,
   compiler, TTL cache, and transactional invalidation notification are
   implemented. The notification listener remains pending.
2. Add owner/admin policy `get`, `put`, `check`, and `explain` APIs.
3. Instrument every governed route and batch-filter copies of Agent/human
   directories, while preserving results in monitor mode.
4. Surface computed denials and cache health in diagnostics.

### Phase 2: enforcement

1. Enable enforcement per workspace, never globally by surprise.
2. Start with Agent discovery, mail send, and prompt.
3. Add output read, terminal attach/raw input, stop, create, update, and delete
   only after each route has identity and denial tests.
4. Provide UI presets that compile to the same document: isolated, team,
   supervisor-only, and unrestricted.

### Phase 3: delegation and audit

Add expiring, narrowing capability grants only after base decisions are stable.
Delegation must intersect with the issuer's authority and be revocable by
revision. Add a durable audit pipeline/outbox separately from evaluation so it
cannot make terminal or directory requests database-bound.

## Verification gates

Correctness tests must cover:

- stable IDs rather than mutable names in stored rules;
- workspace isolation before policy;
- same-priority deny precedence and explicit defaults;
- multi-recipient mail atomicity;
- no hidden-resource leakage through list, get, name ambiguity, or errors;
- separate mail, prompt, raw input, output, attach, and stop decisions;
- source Agent credential validation through Controller and Proxy;
- one pinned revision across batch/list and multi-recipient operations;
- notification refresh across two Proxy processes;
- listener disconnect/reconnect and fail-closed sensitive actions;
- malformed/oversized documents rejected before persistence;
- monitor decisions matching later enforce decisions.

Performance tests should run with at least 1,000 Agents, 1,000 rules, and 100
concurrent list/control clients. Record p50/p95/p99 decision latency, batch-list
latency, compile time, cache size, notification-to-swap latency, and database
queries per request. The acceptance criterion for a cache hit is zero database
queries and no network I/O inside the compiled evaluator.

## Current performance assessment

The added hot-path work is one SHA-256 digest and constant-time comparison for
Agent authentication, one in-memory credential-cache lookup, one workspace
policy-cache lookup, and action-indexed selector matching. Machine-only and
browser requests do not hash an Agent credential. Policy cache hits perform no
PostgreSQL query; a workspace produces at most one refresh query per five-second
window per Proxy under steady load.

The pre-existing machine authentication remains more expensive than the new
checks because each HTTP or WebSocket handshake performs a PostgreSQL lookup
and Argon2 verification. A short-lived Proxy-signed machine session or a
revocation-aware machine-auth cache should be benchmarked before optimizing the
SHA-256 Agent check.

Agent credential entries currently use the same five-second freshness bound so
cross-replica deletion becomes effective without relying on a notification
listener. That is deliberately conservative but is the main scaling cost: `N`
continuously active Agents can produce up to `N / 5` credential lookup queries
per second on each Proxy. Before operating thousands of simultaneously active
Agents, add PostgreSQL `LISTEN/NOTIFY` invalidation for credential revocation and
policy changes, retain a longer fallback TTL for missed notifications, and use
single-flight refreshes so concurrent expiry causes one query. The next
optimizations, in order, are:

1. Pin one compiled policy revision for batch discovery and multi-recipient
   operations instead of reacquiring the cache for every target.
2. Replace cloned per-action rules with shared compiled rule indices and
   precomputed principal-group membership sets.
3. Issue short-lived Proxy-verifiable Agent session tokens after workload
   credential exchange if database refresh traffic remains material.
4. Benchmark 1,000 rules and 1,000 visible Agents before adding more selector
   types.

## Recommendation

Build this as a compiled workspace ACL evaluator behind the existing generic
`PolicyEngine`, persisted as one versioned PostgreSQL JSONB document per
workspace. Start by fixing principal propagation; then ship monitor mode and
explainability; only then allow workspace-by-workspace enforcement.

This shape supports the immediate communication controls without baking mail
semantics into a permanent special case. The same subject/action/resource and
cache can later govern services, network, tasks, artifacts, and UI actions.
