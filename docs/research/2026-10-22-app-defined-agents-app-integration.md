# App-defined agents: App integration (#230)

This is the App-owned section of Core's single #230 implementation. The user
superseded design-only scope in 7955 and required a recursive App/agent tree in
7958. A flat list or a mandatory Core child registry is not the contract.

The runnable [Agent Hub example](../../apps/agent-hub/README.md) implements the
App side. Core owns shared wire, path resolution, identity, Policy, durable
Message APIs/CLI and the joint native-stack runner. Web owns lazy recursive Tree
and the conversation view against that same stack. Local tests are distinct
from actual Core/Policy and browser acceptance; no release is included.

## Address and discovery

The identity tuple is `(workspace, stable owner App workload ID, full relative
endpoint)`. For `/infra/agent_hub/team/tools/agent1`, Core authenticates the real
App and persists `app_endpoint:"team/tools/agent1"` on the addressed recipient.
It does not create a Host process, child credential, PTY or independent Agent
principal. The App alone decides how its internal endpoint runs.

App discovery uses the fixed authenticated App route `GET /api/tree?path=...`.
The response is `{schema_version:1,path:[...],nodes:[...]}` with immediate
single-segment child keys, `kind:"app"|"agent"`, display names and explicit
`tree.read`, `message.send`, `message.read` capabilities. Only `tree.read`
permits expansion; no operation is inferred from a name or kind alone.

The complete owner-ID/path tuple identifies a node across fetches and restarts.
Only one fixed owner App URL is used; nested Apps compose local namespaces and
may delegate internally. No response-selected remote URL can escape that root.
The example has two App levels (`team/tools`) and actual agent leaves. It uses
SQLite-backed declarations and live operator register/remove commands rather
than a static Tree list. Removing a subtree preserves identity tombstones and
accepted job handler snapshots.

The agreed bounds are eight path segments, 64 ASCII bytes per segment, 100
immediate children and 64 KiB response. The example additionally caps total
registered identities at 256. Duplicate keys and path traversal are impossible
in its registry; Web must still reject malformed/stale/untrusted App responses.
Empty, missing, forbidden and unavailable are separate presentation states.

The App verifies the fresh service-audience human assertion from the normal
workspace-authenticated App UI tunnel. Invalid/missing assertions fail closed;
no synthetic claim or cookie fallback is used for acceptance. Public HTTP
visitors gain no Core message authority.

## Responsibility transfer

1. The App's existing workload identity polls its own Core mailbox. Core applies
   current existing Policy semantics to receive, send, read and ACK operations.
2. The App validates the exact delivery recipient, workspace and endpoint, then
   commits a local dedup/job or durable rejection before ACK.
3. It chooses its own handler, execution model and concurrency. Queue admission
   snapshots the handler; later edits cannot silently retarget accepted work.
4. It persists a reply intent before network send, including the complete return
   endpoint, original context and stable idempotency key.
5. It retries the identical Core send/key after ambiguous network completion,
   using Core send idempotency. Real external effects require a separate
   App-owned uncertainty contract; the example's handlers are pure functions.

A message addressed to two keys creates independent jobs and replies. Dedup
includes the full endpoint, rather than collapsing recipients to owner App ID.
The App sender remains the authenticated actor, with its structured local
sender endpoint asserted through `--from-endpoint`. It cannot select another
App principal or grant a child independent Policy authority. Replying to another
endpoint preserves its complete tuple; context edges do not grant history read.

An unknown key is an App failure after Core queue acceptance, not a silent
fallback to a different handler. Malformed addressed data has bounded body-free
poison accounting. Storage failure/conflicting immutable IDs prevent ACK.
Offline Apps leave deliveries in Core under its usual expiry/backlog semantics.
Restart restores jobs/outbox before responsibility is forgotten. Capacity
refusal commits a body-free counter and ACKs without execution; it cannot poison
the oldest receive page. A bounded recent terminal window retires only after
all observed Core ACKs are confirmed, relying on Core never reselecting an
acknowledged delivery. Pending/unconfirmed evidence is retained. Reply framing,
same-App/context suppression and per-authenticated-sender quotas stop automatic
reply amplification. Failed replies cannot starve all siblings.

## Existing endpoint boundary

The #203/#204 endpoint assembly at `1de7d7fb` provides reusable patterns:
commit-before-ACK, outer identity checks, private durable inbox/outbox, exact
idempotency, known-context correlation, poison isolation and conservative
uncertainty. Its `treer.endpoint/v1` fragments and channel-specific receipts are
**not** the generic recursive App address contract.

Telegram/email secrets, allowlists, single-poller ownership, native correlation,
migration and history visibility remain channel-App responsibilities. Do not
substitute a local endpoint for a real relay Agent ID or bypass the relay's
metadata/generation checks. A future adapter needs explicit local mapping and
channel tests; #230 does not authorize activation, real sends or migration.

## Evidence ownership

The App tests exercise actual local SQLite and HTTP with explicit synthetic
Core identity/CLI boundaries. Core owns fresh real Proxy/Host/Controller/CLI
integration, including addressed send, two-leaf dedup, Policy rejection,
offline/restart and sender identity. Web owns real browser lazy expansion,
empty/unavailable/permission states and selected-leaf message/reply display.
One combined handoff must cite exact commits and separate actual results from
not-run cases; App source tests alone do not complete #230.

See [Core Message architecture](../architecture.md#core-message),
[App identity](../../apps/README.md), and the
[Agent Hub executable/commands/tests](../../apps/agent-hub/README.md).
