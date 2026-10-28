# Unified endpoints — design E, research #155

> The `apps/message-endpoint` prototype of this design (#159) was never cut over
> and has been removed; `apps/telegram` remains the Telegram channel. The code is
> in git history before the apps consolidation commit.

2026-10-17; **research, not implementation approval**. User correction 3410:
endpoints ARE real command Agent processes. D/D-lite/D-lite+ are withdrawn
(historical research in `4359f482`); no processless identities or Core delegation. Core 3422 confirms existing
`kind=command`; a dedicated endpoint kind is not zero-change E.

## Inventory, cost and alternatives

Read-only mac_m2 snapshot 19:13Z: Telegram bridge 40.5 MiB + CLI 6.2 MiB,
settings 46 MiB; email 30.4 MiB. HTTP absent in this workspace snapshot. RSS
fluctuates/double-counts shared pages; “six processes/250 MB” projects all channels.
No private config/database contents or real credentials read. Mail is out of scope.

| Current channel | State/secrets | Identities/processes and Core |
| --- | --- | --- |
| [Telegram](../../apps/telegram/README.md) | registry/offsets/mappings/chunk retries in SQLite; bot-token file | bridge Agent + private settings App, one Core receive loop |
| [HTTP](../../apps/telegram/http_app/README.md) | data/admin SQLite, token/pair hashes, cursors/maps | bridge Agent + public data/private settings Apps, one Core receive loop |
| Email (`apps/email-endpoint`, since removed) | contacts/thread IDs/digests/intents/OAuth sessions, private outbox; provider secrets stay Proxy-side | one private App, no receive loop; caller-authored Core messages/replies, App-runtime email |

A (single public/admin process) rejected; B (three shared processes) saves memory
but hub-authored messages fail endpoint identity requirement; C (separate Apps,
shared library) saves little; D variants need Core work. **E reuses Agent IDs,
mailboxes, Tree/Policy/pins/audit, with N real lightweight relays plus fixed shared
transport/settings/public processes.** Not a fixed two/three-process system.

Hermetic idle experiment, mac_m2 arm64 19:38Z, three samples each at 1s spacing:

| Receiver scaffold | Parent RSS | Actual waiting CLI RSS | Sum |
| --- | --- | --- | --- |
| tiny C fork/exec/wait loop | 1152 KiB | 6288 KiB | 7.27 MiB |
| `/bin/sh` loop | 1968 KiB | 6304 KiB | 8.08 MiB |
| Python subprocess loop | 13200 KiB | 6224 KiB | 18.97 MiB |

Installed CLI SHA256 `87e2fd520714fbbc468cc5a5f8d01dda0cf97459f6d94da1ee7383d804e15d65`.
Fake loopback HTTP/WS Controller, synthetic identity, stripped environment/private
HOME; no real Treer connection. Temporary harness/results recorded in ticket;
all own processes stopped. **Idle scaffolds only:** no parsing/IPC/queue/load peaks,
real Host or end-to-end benchmark. Complete relay RSS remains unknown.
20 C scaffolds+CLI ≈145 MiB, 100 ≈727 MiB, before hub/PTY/send/ack peaks.
Python misses “few MB”; native/shell needs a full bounded-parser prototype.

## E: routing, identity and local contract

Each endpoint `/contacts/alice-email` runs a fixed tiny relay with **only its own**
Treer workload identity; never distribute other Agents' credentials. Hub maps
stable endpoint ID to approved channel/account/target. Outbound ordinary Core
Messages addressed to the node retain internal sender/context in hub metadata;
external replies become new Core Messages sent by the endpoint itself. No hub
POC table as author/routing authority; explicit per-endpoint `deliver_to`
Agent/folder scope for unsolicited inbound, no target supplied by external text.

Peer UID alone cannot identify a node. Human-approved bind generates a random
per-endpoint hub credential; relay gets owned 0600 file in 0700 directory, hub
stores hash/scope/generation. Unix socket peer check plus token challenge/session
auth; request IDs/sequences scoped by binding generation, never trust claimed
Agent ID/channel. Bind/rotate/revoke from private human settings, not incoming
messages. No secrets in argv/logs; endpoint has no transport token. Same UID can
read both files: this is not adversarial OS isolation.

Bounded versioned JSON RPC, fixed operations and deadlines: relay submits its
Core deliveries; hub commits/dedups → receipt → relay Core ack. Reverse: hub
commits inbound → relay lease → own-identity CLI send with stable idempotency key
→ durable hub completion/Core ID. Replay same IDs after ambiguous send/reconnect;
reject conflicting payloads/cross-node contexts. Per-node FIFO, not global order
or exactly-once transport. Hub queues offline; cap per-node/global bytes/items/
retention, fair scheduling, lease recovery, visible backpressure, no accepted
unacked eviction. Receive wait cannot block reverse sends. Initial planning cap
20/machine (not load-tested); hundreds need a new capacity decision.

Relay reconnects with bounded backoff. Per Core 3422: exit 0 disables ordinary
Agent startup (completed); human Stop disables it; Delete revokes. Nonzero exit/
kill with enabled startup is restored by the ~5s heartbeat after Proxy-active
validation; cold restore reattaches or respawns enabled declarations. Timing is
conditional, not a recovery SLA. Keep ordinary idle relay alive; unexpected conditions reconnect or exit nonzero,
never exit 0 as a retry strategy. Durable queues survive a fresh run generation.

**Future auto-sleep requirement (3415/3418), not built here:** distinguish sleep
from human Stop/Delete. Authorized Core send should wake the recipient; hub
needs narrow, audited, ownership-checked wake-only authority for managed endpoints
(no arbitrary start/argv/Policy changes), with admission, rates/backoff before
wake. Core must enforce that future ownership check; App names are insufficient.
On wake drain Core mailbox first in bounded batches, then resume hub traffic;
keep workload/binding credentials and idempotency state unchanged. Hub queues
inbound durably/capped during sleep, with visible rejection/failure at limits.
Never wake deleted/human-stopped endpoints or silently revive revoked bindings.
Until platform auto-sleep ships, E operates awake; no promise of existing
wake-on-message or safe App-owned wake capability. Core 3435 confirms current
Start requires machine-level `agent.create`: the hub must not use it to wake nodes.

## Transports, settings and security

Hub owns transport secrets, endpoints only workload/binding credentials.
Prefer channel/trust-domain workers. Public HTTP stays separate, no admin DB,
four routes; queue RPC needs review, not pair-only IPC unchanged. Private
settings owns admin state, workers data. Email send must retain actual Managed
App runtime/service/machine ownership; no command-hub credential borrowing.
Email remains summary/link + workspace-auth web reply, no inbound mail/nonmembers.
Web reply checks bound member, then endpoint authors: hub audit records member/
App/endpoint, but Core proves only Agent author and sender-asserted metadata.

**Creation default:** human creates endpoint via Console/CLI using a fixed,
reviewed relay launch command/profile under their own authority; private settings
only binds that existing stable ID. It neither holds `agent.create` nor borrows
owner login authority. UI can provide a reviewed creation guide, not silently
execute as the App. No unauthenticated auto-creation; count/rate caps fail visibly.
App-driven creation is deferred: allowed only if Core/Policy can constrain one
fixed profile/argv/cwd and hard quota (not just machine/folder); compatibility
is absent today (Core 3435: Policy cannot match profile/argv/cwd; profile launches
still require machine-level `agent.create`). Generic creation is machine-user code execution,
not an acceptable default for browser-facing settings.

Hub compromise can command all bound endpoints. Relay permits only fixed
message operations/targets, never arbitrary exec. Per Core 3422, **enforce**
Policy can restrict own receive/ack and intended sends while denying all other
actions; stable-ID rules or restricted Tree-manage prevent move escape. Monitor,
default-allow and unplaced gaps are not protection. Verify Policy/mode, placement
and fixed command before activation and periodically; pause both directions and
flag drift/unverifiable state. Verification/readback contract needs review.
Humans maintain templates/per-ID rules; transport/settings have **no Policy write
authority**, only read/verify. Never give them publish rights to repair drift.
No Core-independent message-only guarantee: mis-edited Policy restores ordinary
Agent reach. Optional kind hard-limit requires Core work, outside E.
Unlike D, no Core owner/delegate boundary, immutable assurance or via/human proof.
Stolen workload credentials have Agent reach. 0700/0600 files aid secrecy, not
same-UID isolation; dedicated users/containers, minimal Policy/mounts/network,
no Host/Docker sockets, separate complex parsers. Real-email allowlist remains.

Telegram polling default; optional separate webhook process/socket.
[Bot API](https://core.telegram.org/bots/api#setwebhook): modes exclusive,
updates ≤24h, bounded retry of unsuccessful webhooks; constant-time secret header,
durable update-ID before 2xx, caps, controlled HTTPS hostname, Railway IP unverified.
Webhook does not remove N endpoint Core receivers or guarantee RAM savings.

## Migration and decision

Human approves target/folder/bindings. Pause affected consumers; private full
SQLite/config/token/outbox backup (0700/0600, no repo/log, delete after rollback
window). Preserve offsets/intents/token hashes/cursors/URLs. Drain old mailboxes
or explicit new-context cutoff; aliases cannot transfer authority, never dual-poll.
Do not rewrite historical authorship. Post-traffic rollback needs drain/reverse
journal, not snapshot restore. Test full RSS/peaks, crash/sleep/reconnect/replay,
backpressure, cross-node spoof/revoke, Policy drift, migration/rollback and
390px/desktop before retirement; fake transports/outbox only.

**Next:** zero-Core E confirmed (3422/3435); security accepted the research at
`98442947` (3436), with socket, Policy/readback, relay/profile and migration
contracts requiring separate review before code. User decides residual authority/provenance
tradeoffs. No implementation/deployment or future wake feature authorized here.
