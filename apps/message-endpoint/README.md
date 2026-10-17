# Message endpoint (#159)

**Development implementation; relay is locally runnable, hub is unfinished. Not deployed.**
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
power-loss durability on every filesystem. The Unix relay executable is now implemented and tested only with synthetic
mailboxes/fake CLI. The hub, transports, settings UI, reviewed install profile and
full-system activation checks remain unfinished.
Offline focused checks: `python3 apps/message-endpoint/tests/run.py` (stripped
Treer/harness environment, private HOME/temp, private worktree target). Independent
Cargo.lock pinned; review/audit dependencies and build scripts. No root Core gate.

The Unix CLI adapter invokes only fixed send/receive/ack operations using this
process's own injected identity. Sends use stdin envelopes for bodies and external
metadata; contexts are separately validated CLI options. It caps combined child
output at 1 MiB, uses nonblocking stdin/stdout/stderr and a 15-second deadline,
and kills/reaps only its own exact child on failure. Responses must identify the
expected receiver/sender runtime. Errors are fixed codes without raw output.
Fake CLI tests validate each operation against the repository's real clap snapshot.
The relay uses this adapter; hub transport implementation remains unfinished.

The separate [App-runtime smoke procedure](tests/RUNTIME-SMOKE.md) is a disposable
Python test helper for verifying the existing App-kind Core messaging path before
the Rust hub exists. It is not a production entry point or third product program.

Security follow-ups3504: assemblies pin the local endpoint/generation/direction
and recompute transfer identity from the reassembled source ID. Inbound Core
idempotency keys derive from endpoint/generation/external ID, never SQLite row IDs;
a rebuild can reorder rows without changing the key. Relay startup must report
its own binary hash to hub status (diagnostic only, never trust/authorization).
Durable fragment sets now count against the shared pending count/byte limits.
Incomplete sets expire 600 seconds after their first part (duplicates never
extend it); ready complete sets remain pending until downstream acceptance.
Periodic cleanup removes expired plaintext, retains completed/expired per-part
hash tombstones for 30 days, then prunes them. Completed payloads are removed
after downstream durable acceptance; their hashes still reject conflicting
replays. Reopen tests cover partial-set recovery and completed-set deduplication.
The relay invokes cleanup and drains ready transfers after restart. Hub daemon
integration is still pending. The store also provides durable
terminal rejection receipts (fixed reason, Core delivery ID, timestamp, body length;
no rejected plaintext) and a bounded recent-failure view. The receiver must save
that receipt before acknowledging poison; storage/full errors prohibit ack.
Failure receipts have a separate 1000/endpoint, 10000/global budget so a full
pending queue can still record poison without unbounded growth. Queue completions
and failure receipts prune after 30 days; pending/leased work is never pruned.
Relay receive/ack and startup hash reporting are implemented; the hub settings
view, owner clear/resume action and executable isolation template tests remain
activation gates. None may be
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
Closing the store explicitly unlocks after SQLite closes, so a forked CLI child's
inherited descriptor cannot keep the writer lock alive until exec/exit. A
regression holds that shared descriptor in a private child while reopening;
another genuinely live store must still be refused.
Special-file opens use NONBLOCK/NOCTTY before rejecting non-regular files. The
periodic runtime status tick must recheck any sidecars SQLite later recreates;
a failed check pauses admissions and is reported as a fixed storage error.
Unknown database application/schema versions are rejected. This slice currently
supports Unix only; Windows storage provisioning is not implemented.

Schema v3 retains terminal queue timestamps and rejection receipts, replacing
scaffold v2 rejection digests with body lengths to avoid confirming guesses of
short rejected content. Legacy receipts preserve IDs/reasons with unknown length
(-1); Core delivery IDs are immutable, so these rows deduplicate by ID/reason.
Same-length content changes cannot be detected by rejection receipts; only
Core-authenticated, receiver-derived delivery IDs may be recorded. New rows never
store rejected plaintext or its digest. Old database pages/WAL/backups may retain
the previous digests; this migration does not promise forensic erasure. Scaffold
v1/v2 state upgrades transactionally without deleting bindings, pending work or
fragment state. Back up the stopped complete state directory before any
upgrade; to roll back, stop the new process and restore that private backup with
the corresponding old executable. Never open upgraded v3 state with older code.

State is plaintext, including message bodies. Keep state and backups on encrypted
storage. Stop this App before copying its complete state directory (including any
WAL/SHM files); preserve private permissions and test recovery in isolation. Do
not copy only the main database while it is running. These checks do not defend
against another process running under the same OS UID; use a dedicated user or
container. In particular, descriptor checks and SQLite's later path-based open
are not atomic against same-UID path replacement. Do not run the App with
elevated privileges. Lifecycle APIs require Unix epoch seconds (at least 30 days
after epoch for cleanup); a bad clock must surface a visible cleanup error.
Paused bindings retain unacked Core deliveries and their visible backlog until
the human resumes them; a pause is not successful delivery.


## Development relay executable (Unix)

`treer-endpoint --config /absolute/private/endpoint.json` runs indefinitely; invalid
startup waits 30 seconds before exiting nonzero, limiting repeated configuration
failures under Controller restart. Transient Core/storage failures use fixed errors
and a two-second retry delay. There is no voluntary successful completion after draining.
No real endpoint has been activated by the author. The test suite launches only
this executable against a private fake CLI which checks the real command contract.

The human-provisioned JSON file must be 0600, effective-UID owned, single-link,
regular and no-follow in a 0700 physical directory outside a repository (16 KiB
maximum, unknown/duplicate fields rejected). Required fields are `endpoint_id`
(the injected own `TREER_AGENT_ID`), `hub_id`, positive `generation`, `recipients`
(1–32 unique stable Agent/App runtime IDs, no names/paths), absolute `cli` and
`state_directory`, `release_sha256` (reviewed relay binary's lowercase hex
SHA-256), and `cli_sha256` (reviewed Core CLI binary hash). Before invoking any
Core CLI operation, startup hashes both executables and refuses either mismatch.
Use immutable versioned executable paths; startup checks are accidental-drift
guards, not protection against same-UID replacement after checking. It contains no other Agent's credentials. State pins hub/endpoint IDs,
generation and recipient list; changing them requires a human-controlled drain
and fresh state, never a message-driven re-pin.

Each loop checks storage, runs both cleanup APIs, drains at most one ready inbound
job, receives up to ten Core deliveries, then drains at most one more inbound job.
This is at most two completed inbound sends per tick; the receive wait is up to one
second, and slow Core calls reduce throughput. A test starts with 100 ready inbound
jobs and verifies an outbound message still forwards during the same tick. Failed inbound sends
leave their durable jobs ready while ordinary outward messages can still proceed.
Non-hub content is always outward, even if it imitates inbound control JSON.
Outbound originals are acked only after every identical-key fragment is accepted
by Core; inbound fragments only after durable storage, and recipient sends use
stable transfer-derived keys. Poison is committed as a terminal failure before
ack. Per-database lifetime dedup/retention limits still apply.

Startup sends `treer.endpoint.status/v1` to the pinned hub with generation, pinned
release hash, actual own-executable hash, version and a fixed state. The hub must
use the outer authenticated sender to associate it; hashes are diagnostics only,
not proof of running command/profile integrity. A full failure-receipt budget
persists `failure_budget_exhausted`, stops further mailbox work without acking,
and reports that state. It does not silently resume after pruning. Owner viewing,
explicit receipt clearing/resume and hub-side status/backlog presentation are
still to implement before activation. The hub must make Policy/command-integrity
limitations visible alongside those states.


## Development hub receiver library

`hub::Hub` now owns the bounded durable Core fragment inbox and per-ID metadata
checks. It is not yet an HTTP server or complete hub program. Only a future
owner-authenticated service may call its binding/resume/pause methods; this Rust
API does not perform browser authentication. New bindings start paused. Resume
requires an exact revision and fresh per-stable-ID `agent show -- ID`, matching
workspace, command kind, canonical path, cwd and machine. It never enumerates the
Tree or reads command/argv/credentials/Policy. A refresh failure, hidden/missing
path or mismatch pauses; the old expected and current observed values remain
visible, and later matching data never automatically resumes. Metadata older than
30 seconds prevents work admission and transport job access until refreshed.

Outer Core sender kind/ID selects the binding. Unknown/human senders get body-free durable aggregate counters before ack-and-drop:
at most 128 sender/kind rows plus one overflow counter. Counts are receive attempts,
so ack retries may count again. Unknown floods cannot exhaust an exact-delivery
ledger and block bound work. Bound poison uses the existing bounded failure ledger;
its exhaustion still persistently blocks without ack. A previously blocked
scaffold database is not silently unblocked, since its cause may be bound poison.
Valid outbound fragments commit before ack and reassemble into durable transport
work; Core ack is not external delivery. Partial inboxes and binding/pause state
survive reopening. Changing the hub runtime ID/workspace refuses existing state;
relay and hub state directories cannot be shared.

Endpoint status JSON is strictly parsed and stored at most once per binding per
60 seconds, including while paused. It is only an endpoint-asserted diagnostic:
matching hashes, `running`, or repeated status messages cannot resume, re-pin or
authorize anything. This implements the receiver side of security follow-up3570
item6; its UI labeling remains an activation gate. Owner receipt clearing/resume,
worker scheduling, external transports and authenticated settings are unfinished.


## Development Telegram worker mechanics

`telegram` implements target admission and durable worker steps against a fakeable
transport. The Bot API network adapter, secret configuration, scheduler and UI are
not implemented yet; no bot is polled or sent to in these tests. Each immutable
binding generation pins one account/chat/topic and 1–32 explicit sender IDs;
(account, chat, topic) must be unique. A sender, chat or topic mismatch cannot
create a message job. Unsupported/non-text updates advance the durable offset
with an ignored counter, without storing their content. Invalid batch ordering,
duplicate update IDs and numeric overflow are rejected before processing.

Admitted text is durably queued with its binding-owned recipient and source
metadata before offset advancement. Crash before offset commit replays the same
external ID; enqueue deduplication prevents a second job. A full/paused/stale
admitted target holds the account's offset, preventing loss; it can block other
chats sharing that bot until resolved. Use separate bot accounts when independent
availability is required. Other accounts and the settings server must have
independent scheduling; stall/fault acceptance remains pending the runtime.

Hub-to-endpoint delivery leases one FIFO job and sends deterministic Core
fragments as the hub itself. It records completion only after all parts are
accepted. A failed or ambiguous part repeats identical keys after lease expiry.
External source account/conversation/message are transport assertions, not
Core-authenticated human identities. Received body text cannot select a recipient.

Telegram output uses plain text (no Markdown/HTML parse mode), lossless chunks
of at most 3500 UTF-16 units and one external chunk per worker invocation. Chunk
intent commits before sending; each accepted result commits separately. Retries
skip committed parts, including recovery after the final result commit but before
transfer retirement. Lost external responses can still duplicate messages; there
is no exactly-once Bot API promise. The worker must periodically call both store
retention functions followed by `cleanup_telegram`, which frees only metadata
whose parent receipt/job has been pruned. Pending sources and polling offsets
are retained. Pending queue limits do not bound all historical metadata volume
within the 30-day retention window; monitor disk use and use encrypted backups.
