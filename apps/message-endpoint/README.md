# Message endpoint — implementation contract (#159)

Status: **P1 review draft + hermetic library slices; not runnable or deployed**. Implements research
[design E](../../docs/research/2026-10-17-message-endpoint-hub-design.md) approved
in #159. The contract below is normative for the new implementation; existing
Telegram, HTTP and email Apps remain unchanged until reviewed migration.

## Programs and scope

An independent Rust package/workspace under `apps/message-endpoint` owns two
programs: `treer-message-hub` and `treer-message-endpoint`. No root Cargo workspace
or Core changes. The hub is a private Managed App containing its React settings
UI (committed `ui/dist`, relative asset/API URLs). It owns transport credentials,
bindings and SQLite queues, never endpoint workload credentials or a Core receive
loop. Its own Managed App credential may be used only for App APIs, including
Proxy email send. It never uses another Agent's identity.

Each external target has one real `kind=command` Agent running the endpoint
program. Only that process inherits its own Treer runtime environment and invokes
`treer message receive/send/ack`. The human creates it using a reviewed immutable
launch command; settings binds an existing stable ID, never creates Agents or
writes Policy. One hub handles multiple Telegram accounts/targets and later
email/HTTP targets. Telegram uses polling; webhook is deferred.

Public HTTP data ingress is a separate process mode of the hub executable
(`treer-message-hub public-http`), exposing only the existing four HTTP routes.
It has no hub SQLite, admin API, Treer CLI, transport secrets or arbitrary socket
RPC. A distinct scoped queue socket is designed/reviewed for P3. Do not reuse the
endpoint socket for public requests. Same UID is not filesystem isolation.
Initial platforms: Linux/macOS local Unix sockets. No Windows peer-check bypass.

## Local socket protocol v1

### Authentication and framing

Hub creates an owned private state directory (0700), DB/sidecars (0600), and
endpoint Unix socket (0600), rejecting symlinks, unexpected owners and unsafe
ancestors. Do not unlink a live socket or an unverified path. Endpoints verify
socket ownership/mode and server peer UID; hub checks client peer UID. Use safe
platform peer-credential APIs, fail closed if unavailable. Peer UID is a necessary
local-account check, **not endpoint identity**.

Human local provisioning creates 32 random bytes per binding generation in a
private exclusive no-follow file, without returning it through settings HTTP.
Both hub and endpoint retain the raw binding key in separate owned 0600 files
under 0700 directories; the DB holds a fingerprint and key-file reference only.
This is a hub-specific credential, never a Treer workload credential. Encryption
at rest is an operator storage responsibility; a stolen hub key authenticates
as that endpoint. Hash-only key storage is withdrawn because mutual HMAC needs
a secret verifier; do not treat a public fingerprint as that verifier.

Hub sends a random 32-byte challenge; endpoint supplies its own random 32-byte
nonce, endpoint ID, generation, and HMAC-SHA-256 of the domain-separated,
length-prefixed transcript (protocol version, hub challenge, endpoint nonce,
endpoint ID, generation). Hub checks client proof and returns a differently
domain-separated HMAC over the same transcript; endpoint verifies before sending
any delivery. Challenges are single-use, connection-bound, expire after 5 seconds.
No raw key is transmitted. Derive distinct client/server frame-MAC keys from the
transcript and binding key; authenticate direction, strictly increasing u64
sequence and exact frame bytes. Reject replay, counter overflow and tampering;
reconnect with new nonces, never resume sequence state. A fake hub cannot harvest
a credential or obtain data without proving key possession. Mutual proof does
not protect against same-UID file theft or an already-compromised real hub.
Tokens/keys never enter URL/argv/env/log/browser responses; zero key buffers where
practical, no claim of complete memory zeroization. Every transaction rechecks
binding generation. Later frames cannot override authenticated endpoint identity.
No JWT or Core workload token is accepted as a hub binding credential.

The endpoint verifies the hub peer/account and private socket path; this is not
mutual cryptographic authentication against a malicious same-UID process. A
same-UID process can steal files or impersonate either side. Critical deployments
need separate users/containers, scoped mounts and a controlled socket broker.

Each frame is a big-endian u32 byte length followed by one UTF-8 JSON object,
maximum 128 KiB; body maximum 60,000 UTF-8 bytes, context IDs maximum 32. Reject
zero/oversize frames before allocation, duplicate keys, unknown fields/operations,
invalid UTF-8, trailing JSON, version mismatch and non-finite numbers. Five-second
frame deadline; one response per request, no pipelining; one authenticated
connection per endpoint, replacing a stale connection only after reauthentication.
`pull` waits at most 250 ms so pending submits cannot be starved. Core receive
runs independently of the socket scheduler; bounded queues, global 24 connection
cap, authentication-rate cap and bounded worker tasks prevent unbounded spawn. Unknown/revoked/wrong-generation credentials
have one generic error. No bodies/tokens/raw upstream errors in logs.

### Operations and crash boundaries

All requests have bounded `request_id`; replies echo it with either `result` or
`error:{code,message,retry_after_ms?}`. IDs are not authorization. Requests use the
authenticated binding generation, never an arbitrary channel/account/target.

| Operation | Request | Durable response / effect |
| --- | --- | --- |
| `submit` | Core `delivery_id`, `message_id`, verified-delivery sender reference, body, context IDs, creation time | Transaction inserts outbound intent and dedup digest, then returns stable `receipt_id`; only then may endpoint ack Core |
| `pull` | `wait_ms` (0–250) | Oldest eligible inbound job, fixed stable recipient ID, body/source, context, stable Core idempotency key, opaque lease token; or empty |
| `complete` | job ID, lease token, successful Core message ID | Atomically records Core acceptance and context mapping; retry of the same result succeeds |
| `defer` | job ID, lease token, allowlisted temporary error code | Releases lease with bounded backoff; never accepts caller-provided exception text |
| `status` | no fields | pause/generation/queue counters; no credentials or other bindings |

`submit` is unique on `(endpoint_id,generation,delivery_id)`, with a canonical
payload digest. Identical retry returns the same receipt; changed content gives
`conflict`. A receipt promises durable queue acceptance, **not transport delivery**.
Duplicate message IDs cannot create duplicate sends within that generation.
Hub does not verify the Core delivery signature: a stolen binding credential can
forge submissions as that endpoint. Sender references are App metadata, not a
Core-authenticated human/Agent claim. Relay has no generic exec/RPC operation.

Inbound is durably admitted/deduplicated before advancing Telegram update offset
or returning public acceptance. `pull` leases one job for 60 seconds; only one
live lease per endpoint, FIFO by committed sequence. Expired leases replay the
same job/idempotency key after reconnect. Endpoint checks the recipient against stable IDs in its human-provisioned local
binding file (owned regular no-follow 0600 file); a hub-supplied ID cannot extend
that allowlist. Mismatch pauses the binding and reports a static audit error.
Endpoint executes a fixed argv vector:
`treer message send --to ID [--context ID] --idempotency-key KEY --stdin-envelope`,
body and external source exclusively over stdin. It returns success only after
validating the CLI JSON Core message ID. An ambiguous send retries the same key;
`complete` records the mapping before the next job. Core idempotency retention
and actual duplicate semantics must be tested, not called exactly-once.

Paused/revoked/drifted bindings reject new submits/pulls. Revocation/rotation
invalidates connections before their next transaction (generation checked in each
transaction), not just at handshake. Old in-flight CLI sends/transport calls may
already have succeeded: no immediate recall guarantee. Lease completion can record
an already accepted send after pause but cannot change recipient or create work.
Rotation pauses and drains the old generation, then invalidates credentials;
never silently resend ambiguous old-generation work under a new key.

## Queues, transports and retention

SQLite WAL with foreign keys and `synchronous=FULL`; acknowledge only after commit.
A sole hub writer owns these tables; short transactions, no network/CLI while
holding a transaction. Private online backup includes pending jobs/mappings,
configuration and token files. Backups are plaintext: use encrypted storage and
0700/0600 copies, never repository/log artifacts; remove after the approved
rollback window.

Schema v1: `schema_migrations`; `accounts` (channel, secret-file reference,
admission configuration); `endpoints` (stable ID, account/target, admitted inbound
recipient IDs, generation, credential hash, paused state, verified configuration
revision/time); `outbound` (Core IDs, receipt, digest, payload, state, attempts,
next retry); `inbound` (external ID, sequence, digest, payload, stable recipient,
idempotency key, lease token/expiry, Core ID, state); `contexts` (endpoint-scoped
external/Core IDs); `transport_parts` (durable chunk intents/results);
`audit` (actor, action, old/new nonsecret references, outcome). Unique indexes
scope all external IDs/contexts by endpoint, account and binding generation.

Initial hard caps: 20 endpoints; 1000 pending jobs and 16 MiB pending payload per
endpoint; 10,000 jobs / 128 MiB globally. Each adapter has bounded concurrency and
fair per-endpoint scheduling, with independent deadlines/backoff. Reject before
acceptance at capacity, retaining existing unacked work. Accepted undeliverable
work transitions to explicit terminal failure with a visible reason; do not call
it delivered or silently evict it. Pause on disk failure. Dedup tombstones retain
30 days after completion; UI/API states that replays beyond this horizon can
duplicate. Accepted pending work is not removed by that horizon. Audit retains
latest 5000 changes plus a separate rejected-request ring; not tamper-proof.

Telegram account polling has one owner; allowlisted sender/chat/topic and fixed
binding admission before enqueue. Durable update dedup/offset; blocked queue does
not advance past an unpersisted admitted update. Outbound chunk intentions commit
before send; mapping commits per returned message. Transport timeouts can cause
external duplicates; show ambiguous state rather than claim exactly-once. Incoming
replies resolve only this endpoint's context mapping; unsolicited input uses only
the human-configured recipient. External text never changes binding/recipient.
Email keeps summary+thread link, OAuth member binding, actual hub Managed App
email identity and real-recipient allowlist (`doomspec@outlook.com`); tests outbox
only. P3 preserves HTTP four-route auth/admission/queue semantics. No inbound mail.

## Launch profile and Policy preconditions

Human creates an ordinary command Agent with immutable absolute release paths:

```text
/immutable/release/treer-message-endpoint
  --socket /private/hub/endpoint.sock
  --credential-file /private/endpoint/binding.key
  --binding-file /private/endpoint/binding.json
```

The local binding file pins endpoint ID, generation and allowed recipient IDs;
changes require the human provisioning step, never socket instructions.
Only file paths enter argv. Runtime supplies its own ID/Controller/workload
credential; never configure another Agent's ID/credential. No shell interpolation,
remote download, mutable checkout or user-controlled command arguments. Relay
spawns only the configured absolute `treer` binary with fixed operation argv;
CLI output and child lifetimes are capped (receive 30s, send/ack 15s; own children
only). At most one receive and one send/ack child per relay. CLI must support
stdin-envelope; no legacy argv-body fallback. No harness/launcher needed.

Relay first drains its mailbox in bounded non-wait batches, then interleaves
inbound sending with outbound receive (neither direction may starve the other).
It reconnects with jittered bounded backoff and never voluntarily exits zero.
Human Stop/Delete remain authoritative. Auto-sleep/wake is not implemented;
the hub must not use Start (`agent.create`) as a wake substitute.

Human-installed schema-v2 Policy templates must restrict stable endpoint IDs and
folder members. Core 3477 confirms the real message resource shapes:

| Action | Policy resource | Additional store boundary |
| --- | --- | --- |
| `message.receive` | `message.mailbox`, ID = receiving principal | Only caller mailbox; selector JSON `"self":true` compares resource ID to subject ID |
| `message.ack` | `message.delivery`, ID = delivery ID, no Tree facts | Store refuses deliveries not addressed to the caller |
| `message.read` for send contexts | `message`, ID = context message ID, no Tree facts | Store requires the caller to have sent/received the context |
| `message.send` | Each recipient's `message.mailbox`, with recipient Tree facts | Per-endpoint stable recipient ID allowlist; local relay allowlist is independent |

Planned template: priority-100 allows for own receive, delivery ack, visible
context reads, and fixed per-ID send recipients; priority-10 deny rules for all
other supported actions, matching both the endpoint folder and every stable
endpoint ID so moving out cannot remove the deny. Humans maintain it, hub never
writes/publishes Policy. Higher-priority unrelated allows can defeat it;
monitor/default allow is not isolation. Non-message command/Tree/creation rights
remain denied, including for the hub except explicit read-only needs.

**Do not use `actions:["*"]` as a catch-all:** at baseline `f43a4411`,
`policy.rs` compiles/indexes actions by exact string and looks up only the request
string. Syntax acceptance does not prove wildcard evaluation. Enumerating the
existing actions leaves future actions default-allow unless the Core contract
changes; final template and evaluation tests remain required before activation.

**Blocking Core clarification before activation:** design E requires ongoing
read-only verification of actual Policy/enforce mode, placement and immutable
command, pausing both directions when unverifiable/drifted. Core 3476 confirms
Policy and startup-command readback are unavailable to an ordinary hub runtime;
placement can use `agent.tree.read`. Static checks at baseline `f43a4411` also
show the App diagnostic bridge requires BOTH current human owner and bound
Provider runtime (`api/policy_diagnostics.rs`, `app_owner_session`), so an ordinary
hub's owner OAuth session cannot use it. The explain input accepts only
agent/app/folder/agent.tree resources, not the message resources above. Substituting
an Agent resource would test a different authorization request.

Owner-session verification with a 24-hour fingerprint (Core's suggested option)
is not adopted: security3470 rejects stale human attestations as a substitute
for unattended verification; allow-more drift remains invisible. Likewise, denying
Agents `agent.startup.manage` does not detect a human changing the declaration.
No long-lived owner token, Provider binding or Policy-write grant is a workaround.
Ask leader/Core/security to settle a minimal App-scoped read-only contract or
explicitly approve changed guarantees; any Core change needs leader approval.

Security3470 permits hermetic protocol/store/relay work with fake transport only.
No live activation, real bot or migration until this boundary and the exact
template are reviewed. A proposed read-only route is not an existing API. The
hub does not invent Core facts from local policy files or signed-looking data.

## Integrated private settings

Hub serves `/` as negotiated HTML/Markdown, `/health`, relative built React assets,
and `/v1/...` JSON. Settings lists accounts/endpoints, canonical path for display
only, stable IDs, status/capacity/failures, pause/resume, binding generations and
nonsecret audit. Binds human-created existing nodes; never creates/deletes Agents
or edits Policy. Credentials are locally provisioned, not copied through browser.
No external command/config execution UI. Destructive changes require confirmation;
concurrent edits use revision preconditions, actor IDs come from verified identity.

Per-request injected App token verification against own service/workspace first;
invalid supplied token never falls back. Current directory membership and owner/
admin role required for settings data/mutations; Agents denied even if they can
open the App. No header: standard OAuth+PKCE fallback, single-use expiring state
bound to HttpOnly browser nonce, no open redirects. Never persist injected tokens.
Exact Origin + custom-header protection for all unsafe methods, including
injected humans; browser writes require `Sec-Fetch-Site: same-origin`, no CORS; configured Proxy/ingress origin allowlist, no forwarded-Host trust.
Strict CSP (no inline script/eval/third-party assets, direct `frame-ancestors
none`; authenticated Treer tunnel rewrites framing as documented), nosniff and
no-store; escaped React text, no raw HTML/markdown execution. Desktop/
390px, keyboard, light/dark, en/zh, long IDs/errors and tunnel prefixes tested.

## Delivery gates

P1 Core + security review: protocol, profile, Policy/readback, schema and UI scope.
P2 hermetic Rust protocol/store/CLI tests, fake Telegram transport, integrated UI
browser tests and RSS including real waiting CLI; then release-owned disposable
canary-e2e bot validation. P3 independent public HTTP socket review + channel tests,
OAuth/email tests with fake outbox. P4 reviewed offline backup/drain/cutover/reverse
journal/rollback with no dual polling; no old App retirement before acceptance.

Pin an independent Cargo.lock; audit dependencies, forbid unsafe in App code,
review transitive build scripts and keep reproducible React dist build inputs.
Record SHA-256 of both release binaries in the release manifest; the human
provisioning step verifies the installed immutable files against that manifest
before creating a profile. Display the pinned release/hash in status; separately
verify command and release integrity during drift checks. Hashes establish
artifact identity, not reproducibility; retain #151 provenance limitations.
Parser depth/length caps apply to both socket and transport JSON; adapter failures
are caught per adapter and surfaced without exposing raw request/response/exception
text. Test a stalled and a failed adapter while settings and another adapter
remain responsive; no panic crosses a handler, no unbounded retry/spawn.

Use private worktree Cargo target and closed test environment. No shared target,
real credentials or production transport in tests. No push/deploy/restart is
part of author work. Root workspace/CI packaging changes, if needed, go to owners.

## Current development slice

The Rust library currently implements pure framing/session authentication and
an in-memory SQLite queue test harness. There are no program entry points,
production file opener, socket listener, transport worker, settings UI or
activation API yet. The in-memory store tests transaction semantics; they do not
prove filesystem crash durability. Production WAL/recovery/permissions, generation
rotation, context maps, audit and retention remain implementation work.

Run the focused offline checks with `python3 apps/message-endpoint/tests/run.py`.
The runner strips inherited Treer/harness credentials, gives tests a private
HOME/temp directory, and uses this worktree's target directory. It needs existing
Rust tooling/dependency cache; it does not download dependencies or invoke Treer.
