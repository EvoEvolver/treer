# #230 implementation record: recursive App-owned trees

The initial design-only instruction was superseded by #230/7955: deliver working
source and local integration. #230/7958 further superseded a flat agent list:
the App API returns recursively composable App and Agent nodes.

The maintained contract is [App-owned trees](../app-tree.md). Core owns protocol,
authenticated address resolution, durable message storage and browser human
message routes. Apps owns `apps/agent-hub`, its node management, durable inbox
and outbox, handlers and tree API. Web owns recursive lazy Tree rendering and
the child conversation, using the same authenticated App UI tunnel and actual
Core Message routes. No per-child Host process or separate credential is added.

Stable identities are owning workload ID plus a validated relative path. App
nodes can delegate internal implementations without exposing arbitrary browser
URLs. Root, child and grandchild targets are never inferred from display names
or treated as authority. Directory capabilities enable explicit UI actions;
Core Policy remains tied to the authenticated parent App principal.

Implementation and focused checks proceed in the isolated Core branch
`core-app-defined-agents-230`. Local acceptance uses fresh identities, private
state and loopback ports only. The final ticket handoff binds the composed source,
actual receipt and cleanup status; source checks alone are not E2E acceptance.
There is no deploy, release, enrolled update or existing installation lifecycle
change in this task.

## Actual local completion

Executable composition `2535ff055d8fedf159a2f770f83fcf746619933d` passed the
maintained local entry against fresh PostgreSQL, Proxy, Host/Controller and the
bundled App. The authenticated directory actually returned `team` App → `tools`
App → Echo/Reverse Agent leaves; anonymous directory access was denied. A real
managed Agent used the CLI to address the leaf, retry the same message, receive
its authenticated owner-plus-endpoint reply, preserve that address with `reply`,
reject a forged logical sender and observe an explicit missing-handler failure.

Chromium, Firefox and WebKit each expanded the live recursive API and exchanged
two consecutive human messages with actual App replies, including the second
request's context, refresh recovery, explicit child author and a 390px viewport.
No API responses were mocked. Stopping the private App left one accepted message
durably queued; an identical retry kept its ID, and the restarted App consumed
and replied. Cleanup reaped all roots, observed all 12 captured PID/birth
identities gone and all five listener ports absent, with no cleanup errors.

Safe receipt: `.treer/reviews/core230-local-completion/final/result.json`, SHA256
`e9e1e9046c8b5c7ccad84d6db88f5e65951a4004a681b7220a964417b39ce262`.
The separately built production Console HTML is bound by SHA256
`7c565a4bb910d449e750e28bdcf7a9596a3adeffdefe29f8482568037c7bfd42`;
the actual browser runner is bound by SHA256
`9345e5be22d5bf89fa34d8bb1625092cbc91d13e72569a6b3beb741cce0a979c`.
This is a local source/artifact binding, not a reproducible-build claim.

The earlier `7aab9f49` attempt remains recorded as failed: generic App reply-loop
suppression also suppressed a legitimate contextual human followup. The App now
uses Core's authenticated sender kind to preserve Agent reply-chain termination
while answering human followups; focused Store tests and the actual two-send
browser checks cover the correction. The failed run's independent cleanup also
passed. Neither run's private database, session binding or logs are distributable.

Focused native checks included 18 actual PostgreSQL-backed Message tests,
protocol path validation, targeted subtree migration/retry regressions, native
build and strict Clippy. App and Web owners recorded their scoped regression,
accessibility and build checks on #230. No full gate, deployment, public ingress,
installed update or external business side effect was exercised or required.
