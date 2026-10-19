# Message endpoint (#159)

**Telegram candidate plus opt-in email outbox development slice.**
This branch extends the reviewed Telegram slice with owner-bound email threads,
member replies and an outbox worker. No real email transport is activated;
public HTTP lifecycle/RPC stays on its separate development branch. Default
activation remains off. No production migration/cutover is approved by this code.
User direction3479 supersedes the socket design and research's routing details.
Core3485 confirms Managed App runtime Agents can send/receive/ack; security3483
accepts the new boundary with the controls below. No Core change is requested.

## Agent binding management

Workspace Agents now use the settings UI's own JSON API with their verified
Treer identity; see [the binding API contract](AGENT-API.md). Any verified
workspace Agent may manage all bindings, including another recipient's bindings.
This explicit owner-selected trust model does not imply per-binding isolation.
Changes record the real caller and atomically queue a recipient Core notice;
identifiers are masked, denied calls audited and caller/verification quotas apply.
New bindings stay paused and activation remains operator-gated. Transport secrets,
OAuth, recovery and metadata purge remain human-owner-only. No new credential or
approval system is introduced. This code has not changed the running sandbox. The API contract above supersedes
owner-only binding statements in the historical increment notes below.

## Programs

Independent Rust package under `apps/message-endpoint`, not the Core workspace:

- `treer-message-hub`: private Managed App, Telegram polling workers, SQLite state and its own integrated React settings UI with committed
  dist. Its own runtime identity runs Core receive/send/ack and App APIs (email
  route as the actual App). Never stores or uses endpoint workload credentials.
- `treer-endpoint`: ordinary `kind=command` Agent, one per external target.
  A human creates it with fixed immutable release paths/cwd below machine root,
  then binds its stable ID to one hub and one external target. No App-side Agent
  creation, Policy writes, owner credential borrowing, or Start-as-wake.

Public HTTP and Telegram webhooks remain deferred. Email is opt-in, outbox-only
and uses the same hub and relay; see [the email contract](EMAIL.md) for setup,
member authentication and production blockers. There is no separate settings
App. Linux/macOS first; other-platform support needs validation.

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
replay can duplicate. Bounded audit10000 changes plus separate rejection ring;
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
current workspace directory membership (human owner or verified Agent for binding management). Invalid supplied token cannot fall back;
Agent binding access is described above; secret/purge/recovery access remains
human-owner-only. No-header direct access uses OAuth+PKCE, single-use expiring state
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

### Optimized relay memory observation (not full-system acceptance)

`env -i PATH=/usr/bin:/bin /usr/bin/python3 -I
apps/message-endpoint/tests/measure_relay_rss.py target/release/treer-endpoint`
measures only a newly spawned, optimized relay with its real private SQLite store
and an empty synthetic mailbox. It does not call the installed Treer CLI or touch
an enrolled machine. The synthetic Python CLI waits for one second per receive;
its RSS is reported separately, never hidden in a claimed native total.

On local Darwin/arm64, release relay SHA-256
`61bbeba5b466843721af3a2ef1f09a61cdd82470b5826a4e412978eb219a2855`
(source `7835d89e`), 25 samples reported relay RSS min/median/max
3424/3808/5184 KiB. The synthetic Python child was 8080/10736/10912 KiB.
This observation is not a memory ceiling or evidence for populated queues,
multiple endpoints, a different OS/build, or native Treer CLI + Host/Controller
total RSS. Release still owns that native acceptance. Measurements and process
cleanup address only PIDs spawned by the helper; argv/environment are not read.

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
After SQLite opens, permission checks use no-follow metadata queries rather than
opening and closing database/sidecar descriptors: closing an unrelated descriptor
can release SQLite's process-wide POSIX record locks and invalidate live WAL
coordination. Regression checks preserve those locks and verify committed audit
rows after external fixture connections close and a fresh process reopens state.
This does not authorize live manual edits or live file-copy backups; use the
stopped-App backup procedure below.

The private store also retains a validated database descriptor before SQLite
opens the same path, checks its device/inode against directory-relative
no-follow metadata after opening and before configuration writes, and closes the
pin only after SQLite closes and before releasing the cooperating-writer lock.
Missing/replaced paths fail closed and latch that store unavailable.
Configuration writes return the fixed storage-unavailable response; the
independent request-audit store uses
the same checks; request admission also checks its pinned identity synchronously
without taking the hub mutex or opening a database descriptor. Restoring a
pathname does not clear the latch: stop the App, restore a consistent complete
private backup if needed, and reopen it under operator control. No live restore
or automatic file recreation is a recovery procedure. Restart pins the current
file afresh; it never adopts or migrates rows from an orphan inode automatically.
Any recovery of orphan data is an explicit offline operator decision. These checks detect drift;
they do not provide an atomic filesystem boundary against another same-UID
process racing metadata checks and writes.

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
Paused bindings durably hold a small bounded backlog before Core ack. Beyond
that capacity the hub records visible rejections/counters and acks to keep other
endpoints moving; a pause or local hold is never successful external delivery.


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
authenticated binding-management service may call its binding/resume/pause methods; this Rust
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

Worker integration now uses owned `CoreIntent` and `ChunkIntent` values:
`prepare_telegram_forward/chunk` performs only state work, the caller releases
state access before awaiting Core/HTTP, then `complete_telegram_forward/chunk`
records accepted results. Synchronous send helpers exist only in tests. Chunk
leases last 60 seconds; token/digest/generation fencing refuses stale results.
Accepted results can commit after owner pause without admitting more work.
Intent tokens are private capabilities, never JSON API/UI fields. This split
allows independent channel tasks; actual scheduler/UI stall tests remain pending.

The Core receive/readback side follows the same boundary. Workers fetch current
metadata outside state access, then submit `record_readback` with the binding
revision; stale completions cannot override a newer owner pause or mutation.
`Readback.requested_at` is captured before the Core query, retained as
`checked_at`, and must be no more than 30 seconds old at completion. Delayed
results cannot extend freshness or replace a newer observation, and future
timestamps are rejected. This struct is never accepted from browser JSON.
`resume_after_readback` is an internal owner-service operation, never a browser
metadata upload. `admit_core_delivery` commits state without network calls; only
its successful return permits the worker to acknowledge Core. Ack failures replay
the durable receipt. `maintenance` checks private storage and prunes terminal
metadata independently from channel requests. Tests use a backend that panics on
every network operation to enforce this state boundary; they do not establish
that the not-yet-built runtime schedules independent workers correctly.

Chunk reservations persist exponential retry delays (5, 10, 20, 40, 80, 160,
300, 300 seconds) and cap at eight attempts, including reservations interrupted
by a process crash. On exhaustion the hub saves a fixed-code failure receipt and
parks the original transfer. Parked plaintext stays in the bounded pending queue
for owner inspection/retry/discard, is not sent or pruned automatically, and does
not block later transfers of that endpoint. A full bound-failure receipt budget
still persistently blocks the hub without silently discarding work. The state-only
owner recovery operations are described below; authenticated UI wiring and any
routable failure callback runtime/UI wiring are not implemented yet. Callbacks
must pass through the pinned endpoint and its recipient allowlist; an arbitrary
body-provided original sender can never expand hub send authority. Chunk lease/
retry columns migrate on hub reopen while preserving accepted results; restore a
stopped private backup with the corresponding old executable to roll back.

`recovery` provides internal owner-service operations, **not an authentication
layer**. The HTTP service must verify the current owner, CSRF, explicit
confirmation and derive the audit actor from that identity. A recovery requires a
paused binding, exact revision and no live transport lease. Retry resets only
unaccepted chunk attempts/backoff/capabilities and removes the park; already
accepted chunks remain skipped. An ambiguous prior send may still duplicate.
Discard atomically records a distinct discarded terminal state (4), erases wire
plaintext and chunk metadata, removes the park and releases pending capacity.
Per-part hashes suppress replay until the ordinary 30-day terminal pruning;
discard is never marked delivered. Neither action resumes a binding or clears
its circuit breaker. An explicit resume with fresh Core readback is still needed.

Both operations atomically append a body-free audit event with verified actor ID,
action, endpoint/transfer IDs, previous revision and time, and increment the
binding revision. A failed audit write rolls the whole recovery back. Audit is
bounded at 10000 records: further recovery fails without altering parked work;
an authenticated export/archive/clear workflow is still required before
activation. Parked-list metadata and paginated audit reads contain neither
plaintext nor lease tokens. They are internal owner-service methods and must
never be exposed without owner authorization. These changes add the audit table
on demand; stop the App and restore a complete private pre-upgrade backup to
roll back. Older draft binaries do not prune discarded-state tombstones.

## Development failure notices

A transport park atomically stores at most one fixed `failure_notice` per
endpoint/generation/transfer alongside the park. It is eligible only when the
endpoint-attested original sender is an Agent whose ID exactly matches the
human-provisioned primary recipient. Humans, unknown senders and other Agent IDs
produce owner-visible `recipient_not_pinned`; they never add recipients or widen
the hub's Core send authority. The hub sends fragments only to its pinned
endpoint. The relay rechecks its own local recipient allowlist and renders fixed
text containing only transfer ID, fixed reason code and transport attempt count.
It additionally requires its own durable record that this exact outbound
transfer was fully accepted by Core and originated from that recipient Agent.
Unrecorded or unfinished transfers are ignored. Relay notice keys use the
original transfer ID, and a durable notified bit prevents a second status even
if the hub sends a different wrapper/job ID. Relay provenance is metadata only,
capped at 10000 records. Completed provenance expires 48 hours after first Core
acceptance. At capacity the relay evicts the oldest accepted-and-notified record
first, then the oldest accepted record; it refuses before send/ack only when all
records remain unfinished. A saturating eviction count is included in its
endpoint-asserted startup diagnostic (not a delivery or security claim). Notices
for expired/evicted transfers are ignored. A draft relay's older, unrecorded forwards
cannot gain notice authority retroactively.
There is no arbitrary body, context list or `external_source` in this envelope:
hub-generated status must not impersonate an external human. It is normal Core
content sent as the endpoint itself.

Owned `NoticeIntent` values release state access before Core sends. Private
60-second leases fence stale completions; retries use identical parts and keys.
Core handoff retries back off and stop after eight reservations, visible as
`handoff_retries_exhausted`. A successful handoff is `handed_to_endpoint`, never
"delivered" or "read". Notices wait while the binding is paused or metadata is
stale; the owner still sees the parked failure immediately. Owner retry/repark
does not generate another notice or rewrite its first reason/attempt count.

The notice budget is separate from pending plaintext capacity, capped at 1000
per endpoint and 10000 globally. At full budget the original transfer still
parks, but its notice is unavailable and a per-binding skip counter increments.
It never blocks the hub or discards original payload to make notification space.
Repeating an existing park does not increment again; an explicit owner retry
followed by a new park can count another unavailable attempt. Pending
notices are not pruned. Terminal notice dedup markers remain while the original
transfer exists, then prune after the 30-day terminal retention window. This
prevents long-lived parked work generating a second notice after retention.
The table is added on hub open; pre-notice draft parks are not silently backfilled.
This remains a development protocol: old relays reject `failure_notice`; upgrade
the reviewed hub and relay artifacts together before activation. Scheduler,
authenticated status/recovery UI and actual Core callback delivery tests remain
required before deployment.

## Core backlog and recovery primitives (development)

`Core::receive_batch` preserves Core's `remaining_unacknowledged` count alongside
its bounded 10-delivery page. Backlog is not a receive cursor: only ack moves the
oldest-first window. Core currently has no sender skip, nack or recipient cap.
Leaving a paused endpoint unacked can block other endpoints; multi-endpoint
activation is gated on scheduler integration and audited owner refetch. The
hold/capacity implementation below does not establish successful recovery by
itself. The Core worker implementation below is not yet wired into a hub
executable or channel workers.

`scheduler::CoreWorkers` now runs the actual Core receive/ack, maintenance and
per-endpoint readback threads around shared SQLite state. It long-polls for at
most one second, drains subsequent pages with zero wait, limits each drain to
ten pages or 250 ms elapsed, and yields 100 ms between drains. This is a work
budget, not cancellation of in-flight CLI calls: the fixed adapter's 15-second
deadline still bounds each receive or ack. At least one delivery is processed
per page; after the budget no further ack is started, and remaining deliveries
stay in Core. State admission commits before ack, and neither receive nor ack
holds the state mutex. A separate maintenance worker runs every second and
promotes at most ten held fragments per binding.
Consecutive failed inbox drains back off 1, 2, 4, 8, 16, then at most 30 seconds;
a successful drain resets the streak. Worker status exposes the streak and a
fixed last inbox failure code (Core receive/ack, storage, binding, failure
budget, state unavailable, invalid batch or revision), without raw errors or
message text. The optional oldest-unacked age is monotonic time **since this
process first observed the current unacked delivery**, not Core creation time;
it resets when that delivery is acked or the oldest visible delivery changes.

The readback supervisor starts at most one independent worker per configured
binding (maximum twenty), including bindings added later. Each targets a query
start every ten seconds, records query time before the network call, and submits
the ordinary revision/freshness-fenced result. One blocked endpoint query cannot
starve another binding's refresh or the settings state. Missing or denied
metadata follows the existing sticky pause path; a matching result never resumes
an owner pause. Shutdown joins only these owned threads and waits for bounded
in-flight CLI calls; it does not signal or restart any Treer process.

Tests run this production scheduler against a private fake Core mailbox: an
oldest-first paused flood reaches the healthy endpoint after repeated bounded
drains, and actual blocked receive/readback threads leave maintenance and other
binding state accessible. This is not live Policy/isolation, a Bot acceptance
test or a complete hub executable. Channel workers, authenticated settings and
their integrated tests remain activation requirements.

Valid bound fragments for a paused/stale binding, or for a full active queue,
commit to a separate hold before ack. Holds are capped at 20 fragments/256 KiB
per endpoint and 400 fragments/5 MiB globally; they do not consume active queue
or failure-ledger quotas. One maximum-size 256 KiB transfer can fill an endpoint
hold (the serialized fragments also carry framing overhead). A readback older
than 30 seconds is stale, even if the owner did not pause the binding: the
runtime must refresh well inside that window, targeting every ten seconds, and
show stale readback separately from an explicit pause. Slow refresh can exhaust
the hold and cause the explicit body drops described below.
Duplicate deliveries retain their original one-hour
expiry. Promotion requires a current active/fresh binding, revalidates generation
and hashes through the normal fragment store, processes at most ten rows FIFO,
and deletes only after its durable commit. Accepted state replay is deduplicated
after a crash between commit and deletion. Expired or old-generation holds turn
into metadata rejection receipts before plaintext is removed, never into work.
Promotion can place held work behind newer live transfers; cross-transfer
ordering is not guaranteed.

At capacity, the hub **drops the fragment body by design** after durably recording
its rejection, then permits Core ack. Metadata receipts contain Core delivery ID,
message ID, endpoint/generation, fixed reason and time: at most 1000 per endpoint
and 10000 globally. Further rejection attempts only increment per-binding
counters, so individual refetch is unavailable for them; retrying an ack beyond
the receipt cap can count the same message again. Receipt-known duplicates do
not increment counters. The hub-wide hold ceiling also sets a persistent visible
capacity-exceeded flag; it still records/acks overflow rather than blocking the
Core window or poisoning the fault ledger. Metadata receipt reads paginate by
opaque delivery ID, with at most 100 per page. Receipt/counter archival, explicit
owner clear and owner-authorized refetch integration remain activation
requirements. Storage failure
returns no ack permission and preserves the prior hold; counters are not a
substitute for a successful disk commit. Holds use the same private plaintext
database, backup and encryption requirements as other message state.

`Core::get_bound_message` is a network primitive for owner-authorized
receipt recovery. It invokes only `treer message get -- <stored-message-id>` under
the hub's own identity and verifies exact message ID, workspace, bound endpoint
Agent sender and the hub Agent's presence in Core's recipient list, plus size and
context limits. It is not an HTTP API or owner authorization check.

`prepare_receipt_refetch` and `complete_receipt_refetch` now implement internal
state recovery around that adapter. The future HTTP service must first verify
the current human owner, CSRF and explicit confirmation, derive the actor ID
itself, and provide a fresh CSPRNG lease token; these Rust methods alone do not
authenticate an owner. Selection is by an existing receipt's delivery ID, never
by a browser-supplied message ID or body. Preparation requires an active, fresh,
current-generation binding and exact revision, records a body-free started
audit, and leases the attempt for 60 seconds. Only capacity/hold-expiry receipts
within the 30-day replay retention can be selected. Invalid or old-generation
receipts cannot be resurrected. Overlapping attempts for the same receipt are
rejected, and replaced/expired leases or changed revisions cannot complete.
Each receipt permits at most five preparations in a rolling hour, recorded in
SQLite separately from the audit so archiving audit rows cannot reset the rate.
Rate refusals add no audit rows. Pending leases and unsuccessful retrievals count
as attempts; there is no automatic retry.
Maintenance removes at most 1000 expired rate rows per tick, retaining recent
and future timestamps. The hub host needs a monitored clock: wall-clock steps
affect freshness, expiry, leases and rate budgets; backward steps cause refusals
rather than extending authority. Observation-age status uses a monotonic clock.

The owned intent calls the fixed Core adapter outside state access, checking
that its configured identity is the pinned hub's. Its result has no public
constructor or deserializer. Completion rechecks the binding and lease, then
uses the normal fragment store for size, generation, hash and capacity checks.
The original fragment dedup is preserved. Queue-full, unavailable, invalid and
terminal-expired results have fixed audit outcomes, without raw errors or
payloads, and permit an explicit later retry. Accepted fragments set the
receipt's `recovered_at`; this means fragment admission, **not external delivery**
or recovery of every part of the original message.
The external outcome remains `retrieval_unavailable`; internal audit codes
distinguish a pinned-hub identity mismatch, unavailable CLI, timeout, output
limit, rejected Core request and invalid adapter response. Core's CLI rejection
does not distinguish not-found from denied; the App does not inspect or persist
raw stderr to guess why access failed.

The started audit precedes fragment commit. Fragment admission and the final
receipt/audit update are separate transactions: a crash or final-audit disk
failure can leave a pending started attempt with its fragment already saved.
Retry runs normal hash dedup, rather than blindly creating new work. Audits are
append-only and capped at 10000; a full audit refuses recovery before admission.
For refetch audit entries the existing `transfer_id` field identifies the Core
delivery receipt. No automatic refetch is performed. Core content can remain
readable after ack but can still be unavailable, expired, denied or exceed the
adapter response limit. Authenticated UI, audit/receipt archival and explicit
clear remain required before activation; these primitives are not a running
owner recovery service.

## Development owner authentication (`hub` Cargo feature)

`owner_auth::OwnerAuth` now implements the outbound identity verification for
the settings service. This is not yet an HTTP router. Every
request checks `verify_app` at the configured Proxy for this service audience,
workspace, active human subject and unexpired token, then fetches the current
App directory and requires exactly one matching human with role `owner`.
Token role/name headers, remembered membership and Agent identities cannot
authorize management. The returned owner proof exposes only the verified ID.

The service must preserve all repeated header values when constructing the
input: injected identity header first, explicit Bearer second, otherwise a token
resolved from its own opaque OAuth session store. Empty, duplicate, malformed,
expired, forged or wrong-audience supplied identities fail without falling back
to a different cookie user. Callers must never supply a raw token cookie or
actor ID to bypass this selection. The OAuth primitives below still need HTTP
cookie handling and route integration.

Any current human with workspace directory role `owner` is a hub owner; this
is not a separate App-local administrator role. Management mutations require
the single exact configured settings-page `Origin`,
`X-Endpoint-Request: 1`, and, when present, `Sec-Fetch-Site` of `same-origin` or
`same-site`. Duplicate origin/custom/fetch-site headers are rejected. This
includes authenticated requests carrying ambient injected human tokens; even
an owner using curl must supply Origin. The future HTTP router must reject CORS
preflights, enforce these checks on every mutation, verify confirmation/revision
for destructive actions, and run network verification outside the state actor.
When the hub is on its own host, the Proxy origin is explicitly rejected. If
the configured settings URL itself uses the Proxy origin, that is the single
allowed origin, with a significant limitation: other scripts/pages on the same
origin can act with the user's ambient authority. Origin/CSRF checks cannot
isolate mutually untrusted Apps sharing an origin. Prefer the dedicated private
App hostname; an embedded page on a different origin must open that hostname to
edit, rather than silently broadening the origin allowlist.

Proxy/public origins come only from trusted operator configuration, never Host
or browser parameters. HTTPS is required except literal loopback HTTP. URL
credentials, query/fragment and non-root Proxy base paths are rejected, and the
service ID must be a stable `svc_` URL component. The client disables environment
proxies and redirects, uses 3-second connect/10-second request deadlines and a
2 MiB response cap, emits only fixed errors and does not log raw tokens.
Before verification it rejects missing/duplicate/empty or non-compact-JWT-shaped
credentials. A shared admission gate allows at most four verification operations
globally, two per source and twenty per source per ten-second monotonic window;
excess is a fixed rate-limit error for the router to return as HTTP 429. Pending
source records are capped at 512 and expire after sixty idle seconds. The router
must share one `OwnerAuth` instance, use the actual socket peer with
`authenticate_from`, and never take the source from JSON, a claimed identity or
untrusted forwarding headers. Behind Proxy this deliberately aggregates peers;
the fallback/internal callback API uses one stricter aggregate source bucket.
OAuth token exchange uses that same bounded gate.

Only failures are cached: at most 512 private token-hash entries, each for three
monotonic seconds. Raw tokens/hashes are never returned or logged. Positive
identity/role results are never cached, so owner revocation is checked on every
successful request. A corrected identity or restored owner role can remain
temporarily denied during the negative TTL; there is no temporary allow. These
limits reduce verification amplification; they are not complete denial-of-service
protection. HTTP request/body/header limits and bounded async workers still need
router integration, and owner-only management still needs policy/process
isolation from other Agents.
Tests use a private fake Proxy for per-request owner revocation, bad/Agent tokens,
directory membership, CSRF, duplicate headers, no redirect and response caps.
They do not establish real embedded-browser/OAuth acceptance or an authenticated
settings server; those remain activation requirements.

`oauth::OAuthVault` now implements the standard fallback's state, PKCE and
sessions around that verifier. OS randomness generates state and a separate
browser nonce (256 bits each), PKCE verifier and opaque session secrets (384 bits
each). State is hashed in process memory, expires after five minutes, binds to
the hashed HttpOnly browser nonce and is consumed before token exchange. A
mismatched browser/state does not consume a valid flow or request deletion of
its nonce cookie. A matched expired or consumed flow requests cookie deletion,
including exchange failure, to prevent replay. Only fixed registered callback
and authorization/token paths are used; no caller-supplied next/redirect URL.

The callback exchanges with S256 PKCE, then verifies the resulting human's
current owner status before storing a session. Token exchange and verification
hold neither the vault nor SQLite mutex. The browser receives only an opaque
session secret; its hash indexes an in-memory token with expiry capped by both
the verified signed claims and twelve hours. Injected Proxy tokens are never
stored by this vault. Every subsequent request still re-verifies the OAuth
token and current directory membership. There are at most 1000 pending flows
and 100 sessions. Login start checks limits before generating secrets/allocating
a flow: at most five starts and five pending flows per socket source, and at most
100 starts globally per monotonic minute. Start-source metadata is bounded at
2048 entries and expires after a minute. At global flow capacity only a different
source's oldest flow aged at least one minute may be evicted; fresh flows and
the requesting source's own flows are preserved, otherwise start refuses.
Full session capacity refuses without evicting existing sessions, and restart
deliberately signs all OAuth sessions out. The HTTP start route must use
`start_from` with the actual socket peer (not JSON/XFF); calls without a source
share one stricter aggregate bucket. Proxy/NAT peers are deliberately aggregated,
so this is a bounded load defense, not immunity from denial of service.
There is no session file, token in localStorage, local account, refresh-token
flow or credential-zeroization claim. Dedicated-user/container isolation and
host memory/swap protection remain necessary.

The future router must reject duplicate callback query/cookie values, set
HttpOnly/SameSite=Lax cookies (Secure except deliberate loopback development),
use the actual App mount path for cookie scope, and honor the callback's
`clear_browser_cookie` flag. It must also keep callbacks, cookies, access tokens,
codes and verifier bodies out of logs/HTML/JSON, run blocking Proxy calls outside
the async and state actors, and preserve header-first authentication. Library
tests with a private fake Proxy verify the actual PKCE exchange and current-owner
check; they do not replace desktop/390px or real embedded browser acceptance.

## Development Bot API adapter (`hub` Cargo feature)

The optional hub-only HTTP client uses one configured account and verified bot
ID. The relay's default build does not include reqwest/TLS dependencies. The
client pins `https://api.telegram.org`, rejects token/path injection, disables
redirects and environment proxies, caps responses at 1 MiB (including absent
Content-Length), and uses a 3-second connect/10-second overall deadline. Bot
identity must pass `getMe` before send/poll. Targets must match the client's
account; sends use plain text, explicit chat/topic and disabled link previews.
HTTP/client errors are fixed codes, never provider descriptions, URLs or tokens.
Only synthetic tokens and local fake HTTP servers are used in tests. No real bot
has been activated; token provisioning and runtime scheduling remain unfinished.

Only ordinary `message.text` from `message.from.id` with `is_bot=false` is
normalized. Bot/anonymous-admin, sender_chat, forwarded-origin, channel-post,
edited, inline/callback, attachment/caption and unsupported updates are ignored,
not used as identity. This text-only release deliberately ignores forwarded
messages too. No topic (`None` in configuration; main chat) maps to stored topic
key zero; an actual Bot API `message_thread_id` must be positive. The worker uses
only the persisted polling offset, not a caller-provided offset.

Typed HTTP 400/401 (or 403 on getMe/getUpdates)/404 rejection parks a chunk after its first failed request;
other errors remain transient up to the cap. HTTP429 returns a bounded typed
retry delay (1–86400 seconds, malformed response defaults to60). Workers must call
`fail_telegram_chunk` with that typed result after the request ends; it persists
backoff/rate delay and releases the lease. Restart cannot reset attempts or
next_attempt. Even unknown error classes remain transient, never classified from
provider text. A separate manual `release` does not claim success or clear retry
state. UI status, owner recovery and callback presentation remain activation gates.

Telegram is not end-to-end encrypted. An allowed external account takeover gives
that account whatever message authority the owner gave the endpoint. The local
allowlist is an admission control, not verification of a Treer human identity;
keep the endpoint's Core Policy isolation and external-provenance warning enabled.

Downstream faults use a persisted per-endpoint circuit breaker. Three consecutive
parks without an accepted external chunk pause only that binding with
`transport_failing`; later work waits until the owner resolves the problem and
explicitly resumes. Accepted chunks reset the counter, including a response
accepted after pause, but never auto-resume a paused binding. Other bindings and
inbound channels remain usable. Backoff at the head of a transfer preserves order;
later transfers proceed only after that transfer is accepted or parked. CSPRNG
lease generation, audited owner retry/discard and routable fixed-code failure
notices still need runtime/settings integration before activation.

Every new external text chunk repeats the endpoint-added provenance header and
prefixes every content line with `| `. Unicode bidi controls and non-text control
characters are displayed as escapes. A message body cannot emit an unprefixed
sender line, including at a chunk boundary. The original queued Core text stays
unchanged. This is presentation framing, not cryptographic proof of the nested
sender: the header explicitly says the endpoint reports it. Older development
chunk digests are never silently rewritten; a mismatching partial draft requires
owner review/drain before activation. There has been no deployed hub/Bot worker.

### Settings HTTP boundary (development)

The `hub` feature now includes an Axum router (`settings_http::Settings`) with
`GET /api/settings` (current human workspace owner), `GET /api/auth/start`,
`GET /api/auth/callback`, `POST /api/endpoint/auth/logout`, and the confirmed owner-only
`POST /api/configuration` described below. A snapshot is bounded metadata only;
health reports process liveness, not working channels. Root negotiates Markdown
and HTML; only the two named committed asset paths are served, with unknown
API/static routes remaining JSON 404s. No CORS is enabled.

The router shares at most 16 in-flight Proxy-capable requests. Health, static
assets and local logout remain available when that budget is occupied. It rejects
nonzero Content-Length outside the configuration route and all Transfer-Encoding
immediately, bounds headers to
16 KiB/128 entries and any remaining body read to three seconds, and retains its
permit until a blocking Proxy verification finishes,
even when the browser disconnects. Its source is the actual socket peer, never
X-Forwarded-For. This does not cap the HTTP listener's idle TCP connections;
keep the hub behind private Treer workspace ingress and upstream connection
limits. **Public hub settings ingress is unsupported.**

OAuth cookies are `__Host-`, Secure, HttpOnly, SameSite=Lax and Path=/; the
settings URL must be a dedicated root origin different from the configured
Proxy origin. An embedded page may inspect with the Proxy-injected identity,
but OAuth fallback must open the dedicated App URL (returned by
`GET /api/endpoint/auth/config`), so cookies are not issued as shared Proxy-origin App
credentials. Callback parameters and cookie names reject duplicates. A state
mismatch does not erase another valid login; successful exchange clears the
login nonce, sets only an opaque session cookie, and redirects to the fixed
configured App root without the code/state. Token/PKCE data stays server-side.
All responses use no-store, no-referrer and nosniff, with a self-only script/style
CSP and no inline code. Logout requires the same Origin/custom-header CSRF checks as future management
mutations, but intentionally makes no Proxy call: expired or revoked sessions can
still clear their opaque cookie and vault entry. It cannot change configuration. Upstream request
logging must not record callback query strings; this program installs no logger.

### Locally runnable hub preview

`cargo build --locked --features hub --bin treer-message-hub` embeds the three
committed React files from `dist/`; Node is build tooling only. Run the binary
with `--config /absolute/private/config.json`. The config is a 0600 owned,
single-link regular file inside a 0700 directory outside source control, read
with the same no-follow checks as relay profiles. Example field shape (not an
installation or activation instruction):

```json
{
  "hub_id": "app_RUNTIME_ID",
  "workspace_id": "WORKSPACE_ID",
  "service_id": "svc_SERVICE_ID",
  "proxy_url": "https://PROXY_HOST/",
  "public_url": "https://DEDICATED_PRIVATE_APP_HOST/",
  "access": "workspace",
  "activation_enabled": false,
  "virtual_hostname": "message-hub.treer",
  "cli": "/absolute/release/treer",
  "cli_sha256": "64-lowercase-hex-sha256-of-the-CLI",
  "state_directory": "/absolute/private/hub-state",
  "listen": "127.0.0.1:8786"
}
```

Startup requires its injected `TREER_AGENT_ID` to equal the configured App
runtime ID and requires the injected `TREER_WORKSPACE_ID` to match (no optional
workspace fallback). It verifies the CLI
hash before opening state. The listener defaults to loopback and refuses
wildcard/public addresses and `access:public`. It starts its own Core inbox, per-endpoint inbound/failure-report Core handoffs,
metadata-refresh and maintenance workers and stops only these owned threads on
SIGINT. Invalid configuration/storage/listener failures return nonzero with
fixed error codes, never raw configuration or credentials. A changed CLI hash
requires a human-reviewed config update. Confirmed owner configuration writes
can create paused bindings, pin Telegram targets, and pause bindings. Telegram account poll/send workers can run from private operator files; owner
resume is gated off by default and browser account administration is not connected, so **this is a development preview,
not ready for activation/migration**. The React page supports these confirmed configuration writes only on its
configured dedicated root URL. Embedded tunnels, forwarded preview origins and
other direct origins stay read-only and link to that URL; they never widen the
CSRF origin allowlist.

The React page follows the initial injected identity automatically, keeps OAuth
as the no-identity fallback on the dedicated App URL, and treats invalid supplied
identity as an error. It has en/zh, light/dark, keyboard-visible focus and 44px
controls. All metadata is React text, including names containing HTML. It labels
relay diagnostics as assertions and shows Core worker backlog/failure counters
without message content. Refresh is explicit; no aggressive UI polling.

UI tooling: `npm ci --ignore-scripts`, `npm run build:ui`, `npm run check`.
The deterministic check rebuilds and compares every committed artifact, enforces
a 400 KB total budget and checks source HTML/code sinks. Current dist is about
235 KB. The focused browser driver is
`node tests/browser.mjs HUB_BINARY PLAYWRIGHT_MODULE BROWSER_CACHE SCREENSHOT_DIR`.
Its real Rust hub runs with a private temporary HOME/state, a fixed fake CLI,
and a loopback fake identity Proxy; separate forwarding fixtures exercise
wildcard and the real iframe tunnel prefix. These reproduce forwarding contracts,
not production Proxy internals or real OAuth/bot delivery. Desktop/390px in both
themes, long-name/XSS text, injected identity, no-identity OAuth link and forged
identity rejection are covered. Tests do not contact real machines or bots.
`python3 tests/run.py` also runs the executable API/private-permission test in a
closed environment. Native RSS, executable Policy isolation and live bot
acceptance remain release-owned/unverified; no product-readiness claim is made.

The trusted config can name the App's registered `virtual_hostname` (`NAME.treer`).
Core replaces Host with that name for both workspace ingress and the embedded
tunnel. Settings requests accept only this explicit virtual name, the configured
dedicated public host (with its configured/default port), or the exact configured
loopback listener address. Other/missing/duplicate Host values fail before
verification; health is a fixed liveness exception. No X-Forwarded-Host is trusted.
The `access:workspace` file field is an **operator assertion**, not evidence of the
actual Proxy ingress setting: release must separately confirm the Managed App is
workspace-only; the preview does not claim an ingress self-check.

The settings snapshot has a fixed typed projection and a 20-endpoint/512-KiB
serialized-response ceiling. It includes stable endpoint/recipient/machine IDs,
expected/current Tree display paths, pause/revision/reason, relay version/state
assertions and worker counters. It excludes cwd, full Core metadata, message
bodies, transport configuration/secrets and full relay diagnostic hashes. Tree
paths remain presentation/drift information, never routing authorization. The
CLI checks now detect runtime drift as described below; a listener header-read
timeout and the dated Rust/npm dependency audit remain
pre-activation work. Versioned release files must be immutable to untrusted OS
users; neither a hash nor 0600 protects against another process under the same UID.

### Audited owner configuration

`add_binding_audited`, `bind_telegram_audited`, `pause_audited` and
`resume_after_readback_audited` are internal service boundaries, not independent
identity checks. The management route verifies the current human owner, CSRF
and confirmation, derives the actor from that proof (never JSON), and requires
the exact revision for existing bindings. A fresh Core readback must be obtained outside
the state lock before resume. These state methods perform no Core/network call.
Creation remains paused, targets remain immutable within a generation, and
metadata refresh never resumes a paused endpoint.

Every accepted configuration change and its body-free audit row commit in one
SQLite transaction. The row includes actor, action, endpoint, previous revision
and time. The shared 10,000-row audit limit refuses create/bind/resume and recovery
changes until an explicit owner archive/clear workflow is implemented; the
preview has no silent audit pruning. Safety pause bypasses the full append-only
audit through one separate row per existing binding (at most 20 rows): the latest
actor/revision/time and a saturating count commit with the pause. Repeated pauses
cannot exhaust another emergency allowance. The status page reports audit-full
and the number of bindings with such emergency records. Other SQL/storage
failures still fail closed; disk failure cannot promise durable safety actions. Audit insert failure, a later SQL error, an immutable
retarget conflict, an obsolete revision or invalid input leaves configuration
and its audit unchanged. A resume may still record its independently obtained
metadata observation (or automatically pause on drift) before the activation
transaction; an audit failure cannot activate it. The unaudited create/bind/pause/resume helper methods exist only in test builds;
production owner configuration exposes the audited forms.


`POST /api/configuration` accepts only `Content-Type: application/json`, one
positive Content-Length at most 16 KiB, no query, and no Transfer-Encoding.
Host/Origin, `X-Endpoint-Request: 1`, Sec-Fetch-Site when supplied, and current
workspace human owner verification precede JSON body reads. One shared admission
slot stays held through verification, a three-second body deadline, fresh Core
queries and the transaction, including disconnected blocking callers. Unknown
fields/actions, duplicate fields and `confirm:false` are rejected. No actor, full Core readback, raw cwd or token can be supplied in the JSON.
Create requires the untrusted expected pin values from the preview; these are
compared with a new Core lookup and never used as authoritative metadata.

Supported command objects (writes require `confirm:true`):

- `{"action":"preview_create","endpoint_id":"ag_endpoint","generation":1,"recipient":"ag_recipient"}`
  returns `status:preview` and a preview of the current endpoint path, machine,
  cwd SHA-256, recipient ID/path, and generation. It changes no binding or audit.
- `create` takes those same IDs/generation, `confirm:true`, and an `expected`
  object containing the complete preview. The server repeats the Core lookup;
  any path, machine, cwd hash or recipient-path change returns 409 before any
  mutation. The UI displays the preview before confirmation and invalidates it
  when an input changes.
  The hub queries both stable IDs through its own Core identity outside the state
  mutex, requires both Agents in its workspace and a placed command endpoint, and
  derives expected path/cwd/machine from Core. Creation is paused at revision 1.
  A recipient may be a shell, command or other Agent runtime kind: Core Message
  delivery does not require a command process. The endpoint must remain a command
  Agent running the fixed relay profile, also checked during readback/resume.
  Preview/create metadata failures return fixed 400 error codes:
  `invalid_binding_identity`, `endpoint_identity_mismatch`,
  `recipient_identity_mismatch`, `endpoint_workspace_mismatch`,
  `recipient_workspace_mismatch`, `endpoint_kind_unsupported`, or
  `endpoint_unplaced`; responses never echo raw metadata or input values.
- `{"action":"pause","endpoint_id":"ag_endpoint","revision":1,"confirm":true}`.
- `{"action":"bind_telegram","endpoint_id":"ag_endpoint","revision":2,"target":{"account_id":"bot_team","chat_id":123,"topic_id":null,"allowed_senders":[456]},"confirm":true}`.
  The binding must be paused. This pins target metadata only, never accepts a bot
  token or verifies an account. Binding does not enable receiving or sending.

Create/bind/pause writes return `{"status":"saved","revision":N,"paused":true}`.
The separately gated owner resume returns `paused:false` (see below).
Reload `/api/settings` for the current complete projection. Stale revisions return 409; malformed input 400; unavailable Core,
storage or audit capacity 503, all with fixed body-free error codes. Owner proof
expiry is rechecked after Core queries and mutex waits before committing. These
routes expose resume only behind the default-off operator gate described below.
They do not expose retarget, deletion, generation replacement, transport
secret writes, receipt refetch or audit erasure. Owner role is checked once per
request through Proxy; there is no cross-system transaction with role revocation.


The owner forms provide stable endpoint/recipient IDs and generation for paused
creation, immutable Telegram target metadata, and pause at the displayed revision.
Each action needs an explicit confirmation checkbox; no implicit browser confirm
or optimistic activation is used. Target IDs are exact safe integers, and sender
IDs must be positive and distinct. Inputs/errors are rendered as text, labels
remain visible, and forms wrap at 390px. A stale save shows the fixed conflict
error and requires refresh/reconfirmation rather than silently retrying. The UI
does not accept a bot token in this older snapshot; the separate write-only token settings increment below supersedes that restriction. Live bot
readiness and gated owner resume are described below; no release acceptance is
implied by these controls.


Create and target-binding commands also have a per-verified-owner limit of ten
attempts per 60 seconds, counted before state/Core execution, including rejected
commands. The fixed error is `owner_write_rate_limited` (429); slots for at most
128 owner IDs expire after 60 seconds. Preview is read-only and uses the normal
verification/admission limits. Safety pause deliberately bypasses the write quota
and the append-only audit capacity, while still requiring current owner, CSRF,
confirmation and an exact revision. Verification/storage failure can still stop
any request; this is not an OS-level emergency stop or a durability promise under
disk failure. System pause reasons are visible as current state, but a separate
historical system-audit stream is still not implemented.


### Owned Core handoff loops

The executable now starts a handoff supervisor and at most one worker per bound
endpoint (20 maximum). Every worker alternates the inbound queue and body-free
failure notices. It persists a fresh private CSPRNG lease before sending only to
that stable endpoint through the hub's own Core identity. It does not impersonate
the endpoint or the original sender. The endpoint relay still applies its own
local pins before authoring any onward message. Outer Core sends have no borrowed
contexts or external-author metadata. Only after **all** fixed-key fragments have
been accepted by Core is the queue/notice handoff committed; this is not endpoint
or recipient delivery/read confirmation. Ambiguous Core outcomes retry the same
idempotency keys after the durable lease/backoff, never mark them delivered.

All Core CLI calls run outside the state mutex. One slow endpoint cannot stop
another endpoint, metadata refresh, maintenance or HTTP settings. Each fragment
checks current active/fresh readback, unchanged revision and failure-budget state
before its send. Pause/stop prevents subsequent fragments, but an already-started
Core call may still be accepted; accepted final results may commit after pause,
fenced by generation and private lease. A partial handoff remains uncompleted.
Core CLI calls have their existing 15-second deadline; stop is checked between
fragments and only joins owned threads. A healthy partial transport may require
multiple bounded CLI calls; shutdown is not a recall of accepted messages.

No-work checks yield; failures back off 1..30 seconds independently per endpoint.
Failure notices retain their durable eight-attempt/backoff policy. Unexpected
worker termination or a poisoned actor stops this owned worker group rather than
hot-respawning. The owner snapshot/UI shows inbound handoffs, notice handoffs,
failures and stopped state; no raw CLI errors or message bodies are exposed.
These workers connect Core handoff from already admitted hub state. The next
section describes Telegram account loops. Owner resume is gated off by default;
browser account setup and complete recovery/retirement workflows remain unfinished.
No live channel is approved for
activation.


### Operator-file Telegram account loops (development; not activated)

The private hub config optionally contains `telegram_accounts` (default `[]`,
at most 20). Each entry is a trusted operator setting, not browser input:

```json
{"account_id":"bot_team","bot_id":12345,"token_file":"/private/hub/secrets/team.json"}
```

The token file is strict JSON with one `token` string (the real BotFather token).
Never paste a real token into shell argv/history, tickets or the repository.
Create it with a private editor/secret provisioner: its directory must be owned
0700 and the regular, single-link file owned 0600, outside VCS and without symlink
ancestors. The same no-follow bounded reader used for hub configuration applies.
Unknown account fields, duplicate account or bot IDs, alternate API origins,
wrong token format/bot-ID prefix and unsafe files fail startup with one fixed
configuration error. Tokens are only in these private files/process memory, not
SQLite, snapshots, Core Messages, responses, logs or browser storage. Both hub
state and token/config files require protected backups, dedicated OS user or
container, encrypted disks/backups where appropriate; same-UID Agents can read
0600 files, so that mode is **not** Agent isolation.

Configuring an account is an explicit operator action that can start real
getMe/getUpdates requests; **do not configure a live account before release
review and legacy-poller drain approval**. There is no real-account test or
migration permission in this change. A file is loaded at startup; direct operator file changes
or deletion do not revoke the in-memory token. The later write-only settings
increment stops and drains workers before its own file changes. Revoke compromised credentials at
Telegram, then have the authorized releaser update the private file and restart
the versioned App. No live file reload or hidden token-recovery path exists.
Changing tokens for the same bot is supported; after successful getMe, the App
persists account ID → bot ID, capped at 20. Changing a known account to another
bot fails closed before polling/sending, even if its new token verifies. Use a
new account ID and reviewed drain/new bindings; pin retirement is not implemented.

Each account has independent owned poll and send threads. First, getMe must verify the configured bot identity; that check expires after
five minutes and requests wait for a fresh check before resuming. Both directions
are gated by that bounded identity check. Account status exposes only account/bot
IDs, fixed phase codes and counters; getMe does not verify Policy isolation,
recipient permission, or message delivery. The only production origin remains
https://api.telegram.org; redirects and environment proxies stay disabled, with
3-second connect/10-second total and 1-MiB response caps. No URL is accepted from
configuration, messages, API parameters or webhook payloads.

Polling uses only the durable offset and existing strict normalizer/allowlist
admission. Queue/offset writes happen before the next offset is sent. A paused or
full target causes visible `backpressure` and preserves its offset, so it can
block that **account**, not other accounts. Telegram retains updates for a limited
period; this is not indefinite external-mailbox durability. No raw response body
is reported. Sending prepares one leased chunk per endpoint per round outside
network waits, checks active/current revision/budget again before HTTP, and
commits accepted results only under the generation/private lease. One slow target
can delay others on that account by the 10-second request bound; other accounts,
Core workers, state and settings remain independent. Telegram sendMessage has no
idempotency key: losing the accepted result before commit can duplicate an
external message, as documented for the chunk state machine.

429 retry_after (1..86400 seconds) gates both account loops. 401 (or 403 on getMe/getUpdates) clear the
client identity check and require a new getMe; credential failures show a fixed
phase and back off. 409 is sticky `duplicate_poller`, with no automatic retries
until an operator resolves the competing poller and an authorized restart.
Permanent chunk rejection parks through the existing bounded retry policy;
network errors reverify/back off. Wrong bot/pin mismatch, poisoned account state
or worker panic fail the account closed; other accounts continue. SIGINT requests
stop of all owned worker groups before joins and checks between operations; an
already-started HTTP/CLI request can still be accepted and recorded. No worker
restarts another App/Agent or changes a remote machine.

The page now separates the pause control and its own confirmation from Telegram
target fields, and embedded cards deep-link to that endpoint's dedicated pause
control. Browser token/account editing, capacity receipt recovery, full-audit clearing,
header-read timeout, dated dependency audit, Policy evidence and live acceptance
remain unfinished. These limitations must be closed before migration/activation.


Private store checks also run in the common admission/handoff budget gate, not
only the maintenance timer. Unsafe directory/database/WAL metadata therefore
refuses new Core or Telegram admission and subsequent sends immediately on that
gate. Repairing storage remains an operator action; the App never chmods an unsafe
live file into apparent safety. Already in-flight network operations cannot be
recalled, and no file mode can undo data that another OS process already read.

### Persistent inbound Core failure limit

An actual failed Core send for Telegram inbound work increments a durable streak
once per private queue lease. Empty turns, failure-notice alternation and process
restart do not reset it. At least eight failed attempts spanning ten minutes
pause only that binding with `core_handoff_failing`, increment its revision and retain its pending plaintext
queue for owner recovery; metadata refresh does not resume it. Successful complete
handoff resets the streak. A crash after queue completion but before resetting the
streak conservatively retains failures. Replayed or stale lease callbacks cannot
increment the streak or clear a later failure. The bounded health table has at most
one row per binding, and worker settings expose only `core_send_unavailable`, never
CLI stderr or message bodies. Automatic pause history remains a known limitation:
this records the current durable safety state, not an append-only owner audit.

A `sendMessage` 400/403/404 is a permanent target rejection: park that transfer
without invalidating the bot identity or delaying unrelated chats. A 401 from
any operation, or 403 from getMe/getUpdates, invalidates account readiness.

### Owner recovery interface (development)

The dedicated owner settings page now offers **Recovery and audit** on each
endpoint. `POST api/configuration` accepts `inspect_recovery` (`endpoint_id`,
optional `before_audit`) and `recover_parked` (`endpoint_id`, exact `revision`,
`transfer_id`, `operation: retry|discard`, `confirm: true`). Both require current
owner verification and the same dedicated-origin CSRF checks as configuration.
Read-only inspection uses the bounded HTTP verification budget, not the owner
write quota. It returns at most 100 oldest parked transfers and 25 recent failure
receipts for that endpoint, and a global hub owner audit page of 25 records with
`next_audit_before`. No message body, transport error, digest or lease is returned.
For more than 100 parked items, handle the oldest items and inspect again; arbitrary
parked pagination is not implemented. Audit pages are not a stable export snapshot.

Recovery requires a paused binding, matching revision, explicit confirmation and
no live transport lease. Retry preserves already accepted chunks and can duplicate
an ambiguous external send; discard irreversibly clears remaining plaintext and
records a distinct discarded state, never delivered. Each operation and revision
change commit atomically with a server-derived owner audit row. A full audit blocks
recovery writes until an eventual archive workflow; safety pause remains available.
These parked-transfer controls never resume the binding or clear failure receipts.
The later sections add separately gated owner resume and terminal receipt clear.
Capacity receipt recovery, audit retention recovery, account editing, isolation
evidence and release acceptance remain unfinished activation requirements.

A successful recovery returns the updated bounded recovery projection (including
its audit row) in the same verified request. The UI does not issue a second
verification just to discover the outcome. A failed inspection clears stale local
results and offers a manual reload; verification limits are unchanged.

The failure-window start is persisted alongside the capped count. A short Core
outage cannot trip this breaker just because attempts are fast; both eight actual
attempts and ten elapsed minutes are required. The private 60-second queue lease
already spaces retries, independently of the worker backoff. The old health schema
is upgraded transactionally; an old streak without a timestamp starts its window
at the next actual failed attempt, without resetting the count. A delayed duplicate
callback cannot start, extend or trip the window. A completed handoff clears both
count and timestamp. Clock rollback delays elapsed-time eligibility; it does not
silently reset the streak or resume a paused endpoint.

Retry and irreversible discard have independent per-transfer confirmations in the
UI; confirming retry never enables discard. Confirmations clear on inspection or
a successful mutation. The discard checkbox explicitly states that deletion cannot
be undone, separate from the warning about potentially duplicate retries.

### Running CLI pin checks

Both programs construct Core clients using the operator-configured CLI SHA-256;
there is no unpinned production constructor. Before each CLI operation they check
regular-file metadata (including inode/device, length, mode, mtime and ctime).
A changed stamp triggers a full bounded hash. With unchanged metadata, the next
Core operation once the previous hash is at least 30 seconds old rehashes it. Clones share a sticky failure latch.
Missing, unreadable, nonregular, oversized or mismatched files prevent further CLI
execution; restoring bytes does not clear the latch. Hub admission and external
handoff gates also check it, and owner status displays a fixed integrity failure.
The settings HTTP server and safety pause remain available. Already in-flight
network operations can finish; an authorized artifact/config recovery and process
restart must be arranged by the operator, not an App button.

Hashing opens nonblocking and rejects nonregular files before reading, so replacing
a CLI with a FIFO cannot wait forever for a writer. This is installation drift
detection, **not OS isolation or race-free execution attestation**: a hostile same-OS
principal can race path checks against spawn, and a cached stamp is not a new hash
on every invocation. Use an independent OS user/container and owner-enforced Policy;
this does not prove any endpoint's current Core command declaration or argv.

### Owner metadata export and terminal receipt clear (development)

**Recovery and audit → Archive and clear metadata** exports the oldest 1,000
owner audit rows (hub-wide, export-only) or terminal failure receipts (one endpoint,
all generations). Audit includes the bounded emergency-pause summary. Private
metadata includes actor/endpoint/delivery IDs, fixed reasons/actions, size, revision
and timestamps. No message body, rejected-body hash, transport error, lease or
credential is exported. Store downloads in private backups.

The server supplies `canonical_jsonl`: UTF-8 without BOM, fixed struct field order,
compact `serde_json` escaping, one JSON object per LF-terminated line (including
last line). The first line is the version-1 header: `version`, `hub`, `workspace`,
`kind`, `endpoint_id`, `revision`, `from`, `through`. Next come records ordered by
ID; audit emergency summaries follow as `{"emergency_pause": ...}` lines ordered
by endpoint. SHA-256 covers exactly these downloadable bytes; check offline with
`shasum -a 256 FILE.jsonl`. The displayed file name contains the closed ID range and
full hash. The HTTP envelope also contains records for UI counts; do not hash that
envelope. Empty ranges use 0–0. Exports are capped; audit pagination beyond the
first batch is not implemented.

`POST api/configuration` accepts `export_metadata` (`kind: audit|receipts`,
`endpoint_id: null|ID`) and `clear_metadata` (receipts only: `archive` containing
kind/endpoint/revision/through/checksum, `archive_saved: true`, `confirm: true`).
Current owner and Origin/CSRF checks apply to both; clear consumes the owner write
quota. Clear recomputes the exact metadata prefix and checksum with the hub mutex
held, in the deletion transaction. New rows beyond the prefix remain; changed or
pruned snapshots require a new export. All bindings must be paused and there must
be no live refetch, transport, inbound handoff or notice lease. Exact binding revision
is mandatory. No arbitrary filesystem path is accepted.

Two independent confirmations become available only after requesting a download:
the owner declares it saved and accepts irreversible clearing. A browser download
is not proof of persistence. The retained owner audit event records actor, time,
previous revision and fixed JSON metadata in its legacy `transfer_id` field: range,
row count, SHA-256, `archive_saved` declaration and whether the terminal receipt
budget latch was released. Latch release requires every endpoint and the global
terminal receipt count to be below their ceilings in that same transaction.

Audit records, emergency pauses, capacity receipts (including unrecovered message
handles), their capacity latch, pending messages, circuit-breaker and refetch rate
history are never cleared. Bindings remain paused. There is no audit-clear API:
hash-chain anchors and minimum evidence-tail retention are not yet implemented.
A full 10,000-row owner audit therefore still blocks configuration/recovery writes;
safety pause remains available. Export is not yet a complete full-audit recovery
workflow. Resume and capacity receipt recovery remain separate unfinished work.

### Core failure streak expiry

The durable Core handoff circuit stores first and last failure timestamps. A new
actual failed attempt after more than 30 minutes without one starts at one failure
and a new ten-minute window. A clock rollback also starts a new window. Delayed
duplicate callbacks do not extend the streak. Migration adds `last_failure_at` in
the existing transaction; historical rows without that timestamp start a fresh
window on their next distinct failure. The eight-attempt/ten-minute threshold,
private lease fencing, sticky paused state and successful-completion reset remain.
No expiry automatically resumes a paused binding.

### Gated owner resume and declarations (development; default off)

The private runtime configuration has `activation_enabled:false` by default.
Only the operator's reviewed private configuration can turn it on; no browser or
HTTP request can change it. Turning it on requires an explicit operator/release
decision and installed-profile checks; it never resumes a binding by itself.
The owner subsequently records a truthful isolation choice before resume. A
same-UID functional smoke is not verified isolation or production acceptance.

**Resume endpoint** first calls `preview_resume` with the endpoint ID. The server
reads current bindings, then queries Core by endpoint and recipient stable ID
outside the hub mutex. The preview includes their current paths, machine,
working-directory SHA-256, generation, revision and the immutable Telegram target.
It refuses mismatched endpoint placement or a changed/active binding. An additional
fresh endpoint observation is kept internally; it cannot be supplied in JSON.
A preview is not authority and can be inspected while the operator gate is off.

`resume` accepts endpoint ID, the exact preview under `expected`, `confirm:true`
and `evidence:{profile_reviewed:true,isolation:"reviewed",reference:"..."}`.
The reference is a bounded 1–256-byte ticket/manifest reference, never a credential.
The server repeats Core queries, compares the current preview, rechecks current
owner proof, paused state, exact revision and immutable target under the state
mutex, and requires current readiness for the startup-configured account. Readiness
means its pinned bot identity passed getMe within five minutes, with no cooldown,
fatal error or stopped workers. No global worker-list mutex or network request is
held across the activation transaction. Workers independently recheck their live
health/readback gates before each handoff.

The owner must separately confirm the fixed relay launch profile/release and
either reviewed OS/container separation and enforced Policy isolation, or explicitly accepted unverified isolation; provide the
evidence reference, and then confirm sending the pending queue. These are **owner
declarations, not App/Core attestation**: Core metadata does not prove startup argv,
OS isolation or Policy enforcement; getMe does not prove target reachability.
The App never probes Policy, starts an Agent or installs a profile. Same-OS-user
0600 files cannot isolate a hostile Agent. Core metadata can change immediately
after a query; periodic readback and Core authorization remain necessary.

Resume and its owner audit commit in one transaction. For `resume_binding`, the
legacy audit `transfer_id` field contains fixed JSON evidence: declaration kind,
profile/isolation confirmations, reference, canonical preview SHA-256 and target.
Actor/time/previous revision are the ordinary audit columns. Audit failure cannot
activate; an independently obtained metadata observation may still be retained.
Incomplete confirmation, default-off gate, unavailable/moved Core objects,
stale preview/revision or unready account fail closed. Active pending queues,
existing capacity receipts, refetch rates and circuit-breaker history remain;
the existing consecutive transport-park counter is reset. Held content follows
the explicit choice below. This never means downstream delivery succeeded.

The preview also includes held fragment count plus a metadata-only snapshot hash
and parked transfer count. `evidence.held_action` is `discard` by default; `release`
is an explicit alternative. Discard deletes the held text in the same activation
transaction and retains bounded `owner_discarded_hold` metadata tombstones to
prevent Core redelivery from admitting it again. These tombstones cannot be
refetched; a full receipt budget refuses the discard/resume. Release leaves the
held fragments eligible for the existing promotion/expiry checks. The choice,
count and snapshot hash are included in the audit evidence. Parked transfers are
not automatically recovered: `evidence.parked_count_ack` must equal the displayed
count (with a separate UI acknowledgement when nonzero). Any backlog change
between preview and commit requires another preview.

The commit checks the CLI integrity latch and private storage, the audit cap and
a durable per-owner resume budget (10 successful resumes per 60 seconds). The
HTTP admission quota applies separately. Evidence is plain text rendered as
text, never fetched as a URL or logged; the form warns against pasting secrets.

Before starting workers, a false operator gate forces existing active bindings
paused with `operator_activation_disabled` and a `system_activation_gate` audit
row; full audit capacity uses the existing bounded emergency pause evidence.
Telegram transport workers do not start at all with this gate off. With the gate
on, a legacy active binding lacking an activation timestamp is still forced
paused (`activation_required`). Restarting never silently enables those bindings.

Telegram message dates are required. Updates older than the current activation
minus five seconds, older than ten minutes at admission, or more than thirty
seconds in the future are discarded, counted and skipped in the durable poll
offset; they are never forwarded. A small allowed skew can admit messages from
up to five seconds before activation. The status shows the activation time,
newly admitted Telegram updates, accepted outbound chunks and stale-update
observations since activation (not final delivery). Duplicate callbacks do not
increment the accepted counters; repeating a discarded update after a crash can
count another observation before the account offset commits. Held Core content
is a different queue with the explicit release/discard choice above.


### Referenced receipts and export read budget

A receipt still referenced by a parked transfer or pending failure notice is not
clearable, even after export. The whole requested clear is refused (no partial
delete) until those references have been recovered/resolved. TTL pruning keeps
the same references. Metadata export remains available and does not acknowledge
or resolve the work. Preserved receipts continue consuming the bounded failure
budget; the safety pause path remains available.

`export_metadata` has a separate per-owner limit of six attempts per minute per
hub process, after authentication and before taking the hub state mutex. It
returns HTTP 429 `owner_export_rate_limited` and does not consume the write/pause
budget. Restart resets this RAM read limiter. The 8 MiB export budget counts the
complete serialized export object, including the compatibility `records` copy,
JSON string escaping and emergency evidence; 1 KiB is reserved for the fixed HTTP
outcome wrapper. Canonical download SHA-256 continues covering only the exact
JSONL bytes. Audit deletion/hash-chain pruning is still not implemented.


### Positive runtime fixture and clock/held-age limits

The `cfg(test)` positive fixture invokes the **same runtime assembly** as the hub
executable, with real loopback owner HTTP/router, Bot API HTTP client, SQLite,
pinned Core CLI subprocess fixture, account/worker threads, activation, inbound
Core handoff, accepted outbound chunk, and an active restart. It then restarts
with the gate disabled and checks the durable pause and absence of Bot calls.
It waits for durable commits, not just observation of requests at fake services.
The Core fixture stands in for the endpoint's Core messages; this test does not
claim final recipient Agent delivery or actual bot acceptance.

A second runtime test now runs the real Rust `Relay` logic with its own private
SQLite store and pinned CLI subprocess against the same isolated Core message
bus. A fake Telegram update traverses hub → relay → recipient under the endpoint
identity with external-source metadata. A normal recipient reply traverses
relay → hub → the bound fake chat, retaining context and without a second send
on the next relay turn. It does not inject prebuilt outbound transport fragments.
The native relay executable has a separate process test; neither test substitutes
for the [disposable release-owned Telegram smoke](tests/TELEGRAM-SMOKE.md).


Only `cfg(test)` can construct the fake Bot origin/account factory; no Cargo
feature, config field or environment variable can enable it. The App gate scans
both debug and optimized release hub executables for the test origin/factory markers and verifies the
fixed production Telegram origin remains. Running an unmodified production
binary against a fake Bot API is intentionally impossible without changing
system DNS/trust; tests do neither. Actual release acceptance still needs a
throwaway bot/chat and real installed profile/Policy evidence, never a user's bot.

Timestamp filtering uses the **hub** clock against Telegram's date. A hub clock
more than thirty seconds behind can drop fresh updates, and downtime exceeding
ten minutes drops older queued messages; the status warns when stale observations
are nonzero. The resume preview includes the oldest held creation timestamp and
shows its approximate age using the browser clock. Explicit release can send
content up to the one-hour hold TTL; the default remains irreversible discard.
Age/status/evidence are display/declarations, never authorization decisions.

### Telegram candidate checks and recovery

The [disposable Telegram acceptance checklist](tests/TELEGRAM-SMOKE.md) defines
release-owned provisioning, owner bind/resume, identity/context checks, negative
controls, drain, rollback and token revocation. No live test or deployment is
implied by local checks. The [dependency audit](DEPENDENCY_AUDIT.md) records the
Rust TLS patch carried into this slice; its dated result is not a perpetual
security guarantee.

The App author runner accepts explicit non-secret tooling paths:
`python3 tests/run.py --cargo /absolute/cargo --cargo-home /private/tool-cache --rustup-home /absolute/rustup-home`.
It clears runtime identities and credentials from child environments, keeps the
build target in this worktree, and limits Rust test concurrency to two threads.
Build the debug hub before the graded `unittest discover` check; discovery tests
must not treat their runner arguments as binary paths. UI checks remain
`npm run check` and the focused browser driver documented above.

A stale resume preview is a conflict: the page discards the old preview and
confirmation, asks for a fresh preview and never silently retries the POST.
The deterministic positive fixture gates only its unrelated test readback
scheduler; production still requires every fresh Core observation. No fake Bot
origin or test scheduling control exists in production configuration.

## Telegram candidate provenance

See [the file provenance table](TELEGRAM-SLICE.md) for inherited review scope and
newly changed files; prior verdicts do not approve this extracted candidate.

## Inbound Telegram source label

At admission the hub prepends a fixed `External Telegram message` warning with
validated account/chat identifiers and `untrusted`. The label and text share
the 32 KiB UTF-8 body budget; oversized text is ignored with the update cursor
advanced and the fixed ignored counter incremented, never silently truncated.
The complete labeled body is stored durably before forwarding, so retries use
the same bytes. Structured external_source metadata remains attached. This is
a readable source label, not prompt-injection isolation or proof of a human's
Core identity. A normal outbound reply gets its own endpoint presentation and
does not inherit this label automatically.

Use fresh state for the disposable slice. Earlier local-development queued rows
are not silently rewritten to add a label: their bytes and retry keys remain
unchanged, and they must be drained/discarded before enabling this candidate.

### Write-only Telegram token settings (separate increment after 4caaf273)

The owner can verify/store, replace or remove a token in the dedicated hub
settings page. The operator first configures the account ID, **public numeric
bot ID**, and absolute `token_file` under an existing 0700 directory. The file
may initially be absent: that slot reports `token_missing`, creates no worker
and makes no Telegram requests. Paths are never accepted from the browser.
The configured bot ID remains the identity anchor for first store and replace;
the durable account pin also prevents rebinding existing queues to another bot.

The password field is write-only and clears on each submission. Only account ID,
bot ID, presence, last four characters and restart status are returned. Use a
disposable bot for sandbox acceptance and revoke its token in BotFather when
finished. A pasted token travels over HTTPS through Treer ingress to the App;
the hub does not log request bodies, URLs containing tokens, or provider errors.
This does not make the credential safe from compromised App code or another
process under the same OS user. Use a separate OS user/container and private
backups for real deployment.

`POST /api/telegram/token` accepts exactly one of:

```json
{"action":"set","account_id":"sandbox_telegram","token":"<write-only>","confirm":true}
```

```json
{"action":"remove","account_id":"sandbox_telegram","confirm":true}
```

Both require current verified human owner identity, the dedicated App Origin,
CSRF header and a bounded JSON body, just like configuration changes. Invalid
identity never falls back to OAuth. Set attempts (including malformed/failed
submissions) are limited to five per owner and twenty globally per minute, and
share the ordinary owner write budget. Removal bypasses these mutation budgets;
identity verification and HTTP resource limits still apply. A token must have a
3–20 digit positive bot ID and a 35-character ASCII secret; whitespace is rejected
before network access. `getMe` uses only the fixed Telegram API host, with a
10-second timeout, redirects disabled and a bounded response. It must confirm
the configured bot ID. Every error is a fixed code; provider descriptions and
request URLs are never returned.

**Conservative first-version lifecycle:** pause all endpoints before set/replace.
Remove pauses all bindings itself as containment. Either operation stops and
joins *all* Telegram workers before changing a file, and keeps them stopped until
an authorized operator restarts the hub. The join wait is bounded to 12 seconds;
failure leaves the old file untouched, workers stopped, bindings paused and
`restart_required` visible. A file failure also leaves workers stopped and reports
`storage_unavailable`; inspect/recover through an authorized operator action.
An already accepted Telegram send cannot be recalled. Restart never bypasses the
startup activation gate or the separate exact owner resume. Hot credential reload
is intentionally not implemented in this increment.

Files are written using an exclusive no-follow 0600 temporary file in the same
0700 directory, synced, renamed atomically and followed by a directory sync.
Existing symlinks, hard links, unsafe permissions and unsafe ancestors are
refused. Replace keeps the old file until the new token passes verification.
Remove unlinks the private file; this is not secure erasure of backups or memory,
and revoking at BotFather is still necessary. SQLite stores only metadata. The
ordinary owner audit records a confirmed **attempt** before filesystem I/O
(owner/action/account, bot ID and last four); it is not proof of a successful
filesystem commit. Removal additionally keeps one bounded latest containment
record and count per configured account, so full ordinary audit capacity cannot
block removal. Filesystem and SQLite commits are not one distributed transaction.

Focused token UI checks: `node tests/token-browser.mjs <debug-hub> <playwright-module>
<browser-cache> <screenshot-directory>`. These use local synthetic identities and
an explicitly mocked successful UI submission; actual getMe rejection, atomic
storage and secret-persistence checks are separate Rust/real-handler tests.

`restart_required` is an in-memory process status and resets after the hub is
restarted. The durable binding pause and bot identity pin survive restart; a
cleared restart hint never means that an endpoint resumed automatically. Set
also checks pause state before getMe and again before stopping workers/committing,
so an already-active binding causes refusal without a Telegram request.


### Truthful isolation choice at resume (#159, decision 4441)

Resume requires a current verified workspace owner, a fresh exact preview, the
operator activation gate, ready bot identity, the fixed-profile declaration,
and an explicit isolation choice. Nothing is preselected. The owner chooses
`evidence.isolation: "reviewed"` (OS/container separation and enforced Policy
isolation were reviewed) or `"unverified_accepted"` (running without verified
isolation; risk accepted). Both are owner statements, never App attestation.
The second option is available for general deployments, not only sandboxes.
Same-UID processes can read the token file, SQLite state and held content,
and act as the endpoint even with 0600 files. A compromised same-user process
defeats hub confinement.

The choice and the bounded free-text `reference` commit atomically with resume
in the existing owner audit (and its JSONL exports). A reference is untrusted plain text,
not HTML or executable input; exports must preserve that treatment. Cc controls
and Unicode 15.0 Cf format characters (including bidi controls) are rejected,
not escaped into a potentially misleading audit display. It must contain no secrets. For this functional smoke the truthful
reference is `functional smoke, sandbox folder, same user, isolation untested`.
The profile declaration remains required and must also be truthful.

Bindings expose the latest committed `isolation` in owner settings and retain
an **Isolation unverified · risk accepted by owner** badge after pause/reload/
restart. No declaration gives an unverified/no-declaration badge; it never
implies risk acceptance. No destructive schema migration is required: legacy
audit entries with `isolation_reviewed:true` display as the historical reviewed
owner declaration, while absent/unrecognized legacy evidence stays unknown.
New resume JSON requires the enum field; old boolean/missing/null/unknown
choices are rejected, so an old client cannot silently accept risk. Existing
bindings are not automatically resumed or modified. Auth, CSRF, rate limits,
Core metadata, preview revision, bot readiness, held-content choice and audit
capacity checks remain enforced for both isolation choices.

The hub uses `/api/endpoint/auth/config` and `/api/endpoint/auth/logout` for
App discovery and logout. The Proxy reserves `/api/auth/config` and
`/api/auth/logout` even on wildcard App domains; those routes must not be used
for hub operations. Dedicated-origin editing still requires a verified owner
and the existing Origin/CSRF checks; embedded views remain read-only.

## Next channel and migration work

[Telegram migration](MIGRATION.md) describes the read-only inventory tool and
cutover/rollback blockers. [Email channel contract](EMAIL.md) describes the
planned scoped-member thread flow. Neither document enables a transport or
authorizes a production migration.

## Email outbox development slice (#159)

Leave `email` absent to preserve the existing Telegram-only runtime. To test the
new slice in an isolated development profile, a reviewed private hub config may
include `"email":{"outbox_directory":"/absolute/private/email-outbox"}`. The
leaf is exclusively created as 0700 when absent; ancestors must already exist,
be trusted and outside source control. Existing private-directory/no-follow
checks also apply on each publication. This is not a deployment instruction.
There is **no production Proxy mail switch** in runtime configuration: unknown
fields, including `email.mode`, fail closed. The Proxy sender remains an unused
library helper until a later reviewed real-send increment.

On the dedicated owner settings page, create the ordinary paused endpoint
binding, then bind an exact address and current workspace human stable ID under
Configure endpoint. Email and Telegram targets are mutually exclusive and cannot
be changed within that generation. The owner explicitly asserts the address-to-
human association; OAuth does not prove address ownership. Nonmembers cannot be
bound for replies in this phase. Preview/resume keeps the existing operator gate,
fresh Core pins, isolation declarations, revision checks and owner audit. The
operator gate defaults off; a browser cannot turn it on. No old binding is
migrated automatically.

With explicit activation, fixed-counterpart Agent messages produce a private
outbox notification with a fixed summary and `#email-thread=<id>` link. The
bound member signs in using the existing Treer App OAuth flow or injected human
identity, reads the thread and queues a text reply to the fixed Agent. The
actual endpoint Agent remains the Core sender; human attribution is App-asserted.
The UI says queued/outbox, never delivered by email. No incoming SMTP, webhook,
nonmember login, magic-link credential or arbitrary recipient is supported.

The complete migration procedure and its unresolved importer/reverse-journal
blockers are in [MIGRATION.md](MIGRATION.md). Retained email plaintext, quotas,
missing purge/TTL and private outbox reconciliation remain production blockers,
detailed in [EMAIL.md](EMAIL.md). Never remove live SQLite rows or outbox files
to bypass a capacity or ambiguous-send condition.
