# Message endpoint (#159)

**P1 revised contract; hermetic library work only, not runnable/deployed.**
User direction3479 supersedes the socket design and research's routing details.
Core3485 confirms Managed App runtime Agents can send/receive/ack; security3483
accepts the new boundary with the controls below. No Core change is requested.

## Programs

Independent Rust package under `apps/message-endpoint`, not the Core workspace:

- `treer-message-hub`: private Managed App, Telegram polling/email/HTTP channel
  workers, SQLite state and its own integrated React settings UI with committed
  dist. Its own runtime identity runs Core receive/send/ack and App APIs (email
  route as the actual App). Never stores or uses endpoint workload credentials.
- `treer-message-endpoint`: ordinary `kind=command` Agent, one per external target.
  A human creates it with fixed immutable release paths/cwd below machine root,
  then binds its stable ID to one hub and one external target. No App-side Agent
  creation, Policy writes, owner credential borrowing, or Start-as-wake.

Public HTTP is a separate `treer-message-hub public-http` process exposing only
#142's four routes, never opening hub state or invoking Treer. Its narrow queue
boundary remains P3 review work. There is no separate settings App. Telegram
webhooks are deferred. Linux/macOS first; other-platform support needs validation.

## Authentication and forwarding

**Core Message is the only hub–endpoint transport.** Delete the old socket/HMAC
protocol from the active implementation; git history preserves that research.
Core authenticates/stamps each sender. No binding secrets, challenge protocol,
local credential exchange or self-reported identity headers exist.

The human-provisioned endpoint file (owned 0600 regular no-follow file in 0700
directory) pins its own Agent ID, ONE hub runtime Agent ID, binding generation,
allowed recipient stable IDs and release SHA. IDs, never paths, authorize routing.
The hub pins bound endpoint IDs, generations, target, and expected kind/path/cwd.
Only the human provisioning step changes the endpoint file; hub messages cannot
change pins, execute commands, create Agents or expand recipient permissions.

Endpoint dispatches on `delivery.message.sender.kind/id` from Core, never a body
field. Sender == pinned hub Agent means strictly parsed inbound-delivery envelope;
any other admitted sender is ordinary outbound text, even if its body imitates an
inbound command. Hub accepts forward envelopes ONLY from bound endpoint IDs.
Unknown senders/invalid envelopes are counted and durably rejected, not executed.
Body-provided original sender metadata is App-attested: Core proves which endpoint
forwarded it, not that every nested assertion is true. External metadata is not a
Core-verified human identity. Hub compromise reaches all its bound endpoints,
within their local recipient allowlists and whatever Policy the owner installed.

Outbound: receive X → send its envelope parts to pinned hub with deterministic
keys → ack X only after ALL parts accepted by Core. Hub receives part → durable
SQLite commit/dedup → ack Core → assemble and queue → transport worker. A hub
receipt is not external delivery. Inbound: transport admission → durable SQLite
job → hub sends parts to pinned endpoint → endpoint verifies authenticated hub,
reassembles, checks recipient against its local allowlist, sends as itself using
stable key and `--stdin-envelope` → acknowledges hub deliveries after acceptance.
Hub retries ambiguous sends with the same keys; no exactly-once transport claim.

Core limits: body 1..32768 UTF-8 bytes, ≤32 contexts/recipients, receive ≤100 items
and ≤30s wait. Wrapper overhead/JSON escaping can exceed the original body limit.
Use bounded versioned fragments: `protocol:"treer.endpoint/v1"`, direction,
generation, transfer ID, part index/count, base64 data. Serialized logical envelope
≤256 KiB, at most16 parts, raw fragment data ≤18000 bytes; serialized fragment
must remain ≤32768 bytes. Duplicate keys/unknown fields/invalid UTF-8/base64,
conflicting fragments and oversized/deep JSON fail closed. Transfer IDs and Core
idempotency keys derive from endpoint ID/generation/source Core delivery ID and
part index; retry keys/payload must be identical. Store/ack each hub fragment
before advancing; incomplete sets consume bounded capacity. The relay has bounded
in-memory reassembly; unacked Core deliveries remain durable across a restart.
Only complete sets may cause a send. Never ack rejected content without a durable
failure record. Multi-part ordering/interleaving/crash replay require tests.

Core receive and delivery remain at-least-once; idempotency protects retries for
Core's retained key lifetime, not infinite dedup. External content now persists
in Core on each hop as well as App SQLite, governed by Core retention and
`message.read` visibility. No body, source metadata, raw error or credentials in
logs. Bound backlog/queue sizes and show rejection/terminal failure in settings.

## State, limits and lifecycle

SQLite WAL/foreign keys/`synchronous=FULL`, one writer, short transactions; no
network or CLI while a transaction is open. Tables: schema versions, accounts
(secret-file refs), endpoint bindings/generations/pins, inbound/outbound intents,
fragment sets, transport parts/results, context maps, audit. Dedup namespaces
include endpoint, generation, direction, source IDs; reject same key/new content.
Inbound leases expire after60s, FIFO per endpoint, retry the identical Core key.
At most one active inbound job per endpoint; adapter concurrency is separately
bounded with independent deadlines, fair scheduling and fault isolation.

Initial cap20 endpoints, 1000 pending items/16 MiB per endpoint; 10,000 items/
128 MiB globally (includes incomplete fragments). Pending accepted work never
silently evicted. Reject before admission at capacity; terminal failure retains
visible nonsecret reason/ID. Completed dedup tombstones30 days; warn that older
replay can duplicate. Bounded audit5000 changes plus separate rejection ring;
not tamper-proof. Disk failure pauses, never reports delivery success.

Profiles use absolute immutable hub/relay/CLI paths, fixed argv and private HOME.
Human provisioning verifies installed SHA-256 against the release manifest;
status shows pinned release/hash. Same-UID can alter files/read credentials:
0700/0600 is not adversarial OS isolation; use separate users/containers, scoped
mounts/network, no Host/Docker sockets. Hash identity is not reproducible-build
proof (#151). Endpoint invokes only fixed CLI operations; bodies/source over stdin,
never argv. Refuse old CLI without stdin-envelope. Cap child output/time/count.
Relay drains mailbox on restart in bounded batches before normal waits; both
forwarding directions remain schedulable. Never voluntarily exit0 (ordinary
Agent completion disables startup); reconnect or fail nonzero. No sleep/wake
feature exists yet; never wake human-Stopped/deleted Agents.

Hub redeploy/runtime-ID change: pause affected bindings, human verifies new hub
release/ID and regenerates each endpoint's pin via its reviewed provisioning step,
drains old queues, then resumes. No message instructs relay to trust a new hub;
never dual-poll old/new bot consumers. Preserve original idempotency namespaces
while draining; don't re-key ambiguous work during re-pin.

## Policy and readback: owner's responsibility

**Standing UI/operator warning: hub does NOT verify Policy mode/document or
continuous Policy correctness.** A missing/mis-edited template can leave endpoints
and stolen endpoint credentials with ordinary Agent reach on the machine. This
is an explicitly accepted user risk (3479/3483), not equivalent to continuous
verification. Command/argv/profile drift is not detectable through `agent show`.
No owner token cache, fake Provider binding or long-lived verified-at guarantee.
The command-integrity chain is therefore also unverified by the hub: preventing
non-human command changes depends on the owner's Policy denial of
`agent.startup.manage`, and that Policy is itself not checked. The human-created
immutable profile and provisioning-time binary hash do not prove that a later
declaration still points to that binary. Show this chain in settings as well as
the Policy warning; never display a green continuously-verified-command badge.

Activation checklist (human owner session, outside hub authority): inspect the
actual enforce template, negative/positive message decisions and each endpoint's
declared command/argv/cwd through the admin startup route; compare installed
binary hashes to the release manifest; verify pinned hub/recipient IDs. These
checks do not become unattended verification after the owner closes the page.

Owner installs/checks schema-v2 enforce rules once, records a dated owner-attested
note, and remains responsible for subsequent changes. The UI labels that note
as human attestation, not ongoing verification. The exact template and test evidence
are required before activation. Priority100 allows beat priority10 denials unless
other higher-priority rules interfere; do not use unimplemented action wildcards.

Core message resource shapes confirmed3477/3485:

| Action | Resource / boundary |
| --- | --- |
| receive | `message.mailbox` ID=self; subject JSON `"self":true` checks own ID |
| ack | `message.delivery` delivery ID; store independently enforces recipient |
| context read | `message` context ID; store requires sent/received visibility |
| send | `message.mailbox` recipient ID; recipient Agent Tree facts available |

Per-stable-ID rules allow endpoints to receive/ack/context-read, forward to hub,
and send only to local configured recipients. Hub receives/acks/context-reads and
sends only to bound endpoints. Low-priority deny `message.send` TO hub for all
subjects except bound endpoints; similarly deny sends TO endpoint except pinned
hub and human-approved senders/groups. Deny other supported actions for endpoint
folder AND every endpoint ID so Tree movement cannot remove the guard. Enumerated
actions do not automatically cover future Core actions; owner must update them
on upgrade. Hub may read only per-bound-ID `agent.metadata.read`, not broad Tree.

Hub periodically uses own-identity `treer agent show ID`: verify existence,
kind=command, exact path/folder/cwd and expected binding; pause on mismatch or
unavailable data. Core3485 says command/argv/profile/credentials/Policy are absent;
never claim to check them. No global `agent.tree.read` when per-ID show suffices.
Optional future App-scoped Policy read model and kind hard-limit remain separate
leader-approved Core work, not this task.

## Integrated private settings and transports

Workspace-auth ingress only. `/` negotiated HTML/Markdown, `/health`, relative
React assets and `/v1/` JSON; no SPA fallback for unknown API paths. All settings
routes require per-request App token verification (own audience/workspace) and
current owner/admin directory membership. Invalid supplied token cannot fall back;
Agents denied. No-header direct access uses OAuth+PKCE, single-use expiring state
bound to HttpOnly browser nonce, validated local returns. No forwarded token
persisted. Exact Origin/custom header/Sec-Fetch-Site same-origin on unsafe browser
requests, no CORS, strict CSP/no eval/inline/CDN, nosniff/no-store. Credentials are
write-only, never returned; only set/rotation metadata. Revision preconditions,
actor-stamped audit, confirmation before destructive/pin changes. No command UI.
Desktop390px/keyboard/light/dark/en-zh/tunnel-prefix tests; ui-dev visual review.

Telegram polling: admitted sender/chat/topic before enqueue; durable update offset,
external-ID dedup, bounded text chunks and committed chunk intents/results. Never
advance offset past an admitted update not persisted. Ambiguous Bot API sends can
duplicate. No arbitrary URL fetch or redirects with tokens. Email remains Proxy
App-runtime send, summary+thread link, bound-member OAuth replies, no inbound mail.
Real allowlist `doomspec@outlook.com`, tests fake outbox only. HTTP P3 keeps #142's
four-route contract and separate public process; new narrow boundary gets review.
Adapter stall/fault tests must prove UI/other channels remain responsive.

## Migration, checks and current implementation

P1 revised contract reviewed by Core/security; P2 relay/hub/Telegram/React fake
transport tests, real waiting-CLI RSS, then release-owned disposable canary-e2e
App receive smoke and disposable bot. No real user bot. P3 public HTTP/email;
P4 reviewed backup/drain/cutover/reverse-journal rollback, preserve mappings,
offsets/pending work/URLs, no alias authority transfer or old-history rewriting.
Retire old Apps only after acceptance. Backups include SQLite/config/secrets,
0700/0600, encrypted storage, never repository/log; delete after rollback window.
No author push/deploy/restart. Cross-owner packaging/CI requests go to owners.

The active protocol library now contains Core Message envelopes/fragments, not
sockets/HMAC. Queue tests cover in-memory and private on-disk SQLite, including
committed WAL recovery after an unclean child-process exit. This does not prove
power-loss durability on every filesystem. Core relay, transports and UI remain
unfinished; there is no production daemon entry point yet.
Offline focused checks: `python3 apps/message-endpoint/tests/run.py` (stripped
Treer/harness environment, private HOME/temp, private worktree target). Independent
Cargo.lock pinned; review/audit dependencies and build scripts. No root Core gate.

The separate [App-runtime smoke procedure](tests/RUNTIME-SMOKE.md) is a disposable
Python test helper for verifying the existing App-kind Core messaging path before
the Rust hub exists. It is not a production entry point or third product program.

Security follow-ups3504: assemblies pin the local endpoint/generation/direction
and recompute transfer identity from the reassembled source ID. Inbound Core
idempotency keys derive from endpoint/generation/external ID, never SQLite row IDs;
a rebuild can reorder rows without changing the key. Relay startup must report
its own binary hash to hub status (diagnostic only, never trust/authorization).
Incomplete-transfer expiry/cleanup and capacity accounting, completed tombstone
pruning, durable poison-message failure+ack and actual relay startup reporting
remain P2 work. None may be
inferred as complete from the current in-memory queue tests.

The Unix persistent store opener creates only the final dedicated state directory
(0700); all parents must exist. Configure an absolute physical path with no
symlink components, outside any `.git`/`.hg`/`.svn` ancestor. Existing state
directories must already be 0700 and owned by the effective UID; unsafe permissions
fail closed rather than silently changing a shared directory. Ancestors must be
root/current-UID owned and not group/world-writable unless sticky. New database,
lock and SQLite sidecar files are exclusively created at 0600; existing files
must be regular, single-link, 0600 and owned by the effective UID. A lifetime
nonblocking lock excludes another cooperating hub writer. SQLite opens the
database with NOFOLLOW, uses WAL/FULL synchronization and checks sidecar permissions.
Unknown database application/schema versions are rejected. This slice currently
supports Unix only; Windows storage provisioning is not implemented.

State is plaintext, including message bodies. Keep state and backups on encrypted
storage. Stop this App before copying its complete state directory (including any
WAL/SHM files); preserve private permissions and test recovery in isolation. Do
not copy only the main database while it is running. These checks do not defend
against another process running under the same OS UID; use a dedicated user or
container. Do not run the App with elevated privileges.
