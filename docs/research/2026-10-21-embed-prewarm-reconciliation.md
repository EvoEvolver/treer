# Embed loading: reconcile prewarm with accepted availability behavior

Offline scope for issue #198, following ticket 7025. This document proposes a
minimal successor; it does not enable prewarm or authorize a live run.

## Compared sources

- `42d00a35`: accepted availability correction; only check status 204 permits UI.
- `9c57bc44`: earlier Core/Web prewarm candidate, not the accepted correction.
- `da2b9304`: accepted Tree Add machine union used as this document's base.
- `0f9875fd:web/scripts/measure-embed.mjs`: existing opt-in timing runner.

The direct Web diff from 42d to 9c57 adds the prewarm hook, Tree intent handlers,
capability gating, bootstrap rendering helper and prewarm tests. It also removes
`embed-availability.spec.ts`. Therefore a wholesale merge would lose the accepted
availability regression matrix. The production `embed-ui.ts` has no diff between
those two sources; most work is at the hook and Core bootstrap boundaries.

## Required compatibility changes

The old Core v2 probe mints after every non-204 result and after a network error.
The accepted correction instead permits a single handoff on the ordinary empty
200 miss or legacy 401, and stops on other statuses, redirect or network failure.
Both foreground and hidden prewarm must retain that partition. A hidden failure
ends quietly; it must not create a compatibility bypass, retry or load Agent HTML.
After exchange, only an exact 204 check can finish. A check result is availability,
not cached authorization: the UI request continues to enforce current audience,
user/workspace/Agent binding, membership, Policy, logout and revocation.

Core must reconcile the earlier purpose-bound code/session storage, short unused
session lifetime, one-time document rotation, session-row eviction and authoritative
per-user quotas with the accepted correction before advertising
`agent.interface-embed-prewarm.v1`. Existing embed capability alone is insufficient.
Do not infer that earlier branch tests establish these properties on the successor.

## Minimal Web migration

Port only the prewarm hook and its capability/Tree/logout integration onto the
then-current accepted source. Preserve subsequent Tree hit-area and Add machine
changes; do not replace the Tree file from 9c57. Retain the entire accepted
availability suite and add the prewarm cases alongside it. Update the shared test
bootstrap extractor to execute the successor Core template and fail if expected
branches cannot be extracted; never silently substitute a second bootstrap.

Retain the old proposal's explicit local bounds: 400 ms deliberate mouse hover or
Tab focus; two simultaneous attempts; ten frame starts and six mint attempts per
minute per account/document; 60 s per-Agent cooldown; 15 s attempt timeout. Core
quotas remain authoritative across documents/devices. Touch/drag/arrow navigation,
Save-Data, hidden documents, Apps, offline machines and already-open tabs do not
start background work. A new tab already starts a foreground handoff; do not add
a second hidden handshake for it.

Keep exact source/origin/hash/state checks and cancel listeners, frames and pending
requests on leave/blur/click/logout/user/workspace/interface-generation changes.
Cancellation after exchange may retain only Core's bounded unused session; it
cannot claim that the session was revoked. Foreground click always checks again.
Check session identity changes as well as user ID when reconciling the hook scope;
the old hook takes user/workspace only and must not retain an old session's attempt.

## Verification before a candidate

Run the retained availability matrix first: initial 200/401 hand off once, initial
204 reuses, other statuses/network/redirect stop; post-exchange non-204 never
permits UI. Apply the same negative matrix to hidden prewarm. Retain prewarm tests
for cold/warm sessions, cookie refusal, session loss between warm and click,
concurrent source/hash isolation, cancellation, quotas, hidden layout and keyboard
focus. Add same-user session reset and expired/evicted-session cases where absent.
Report response fixtures separately from actual browser/cookie behavior.

For Chromium, Firefox and WebKit, a later explicitly reserved disposable canary
fixture must compare cold and warm opens with prewarm disabled, then deliberate
hover/focus and click on the successor. Cover blocked third-party cookies and
partitioned-cookie support honestly; unsupported reuse must take the normal
handoff or refuse according to the existing security contract. Use no broad
browser-policy changes to manufacture compatibility.

## Timing evidence and release boundary

Audit the existing opt-in runner against the exact successor before reuse. Its
fixed-label marks cover frame creation, bootstrap script/load, ready receipt,
code receipt and available paint entries; endpoint timings cover mint, session
exchange, check and UI document. Add a fixed-label code-delivery observation if
needed, without collecting the code. UI visibility and paint are separate facts.
Retain only numeric stage/status/size data and source/artifact/runner hashes, with
no cookies, nonce/code values, URLs, response bodies or user content.

Use three cold/warm pairs per engine initially, bounded by the reviewed runner's
maximum five pairs. Cold means fresh browser/context, not flushed OS DNS or edge
caches. Record unavailable DNS/TLS/paint entries as unavailable, not zero or an
inferred improvement. Prewarm must show reduced click-to-visible latency without
increased unauthorized navigation or unbounded session/mint work.

The accepted corrective canary is functional evidence, not this performance
comparison. No live measurement is run under this document, especially while the
#202 exclusive Worker window is held. A future package requires exact then-current
served identity, reviewed Core/Web bytes, bounded resources/session cleanup and
its own execution scope. Round-trip reduction and Production rollout remain
outside this reconciliation.

## Reconciliation with the current metrics source

The Release diagnostic union `c04f6011` combines current `961dd64e` with the
accepted `da2b9304` baseline before applying the prewarm successor. The missing
availability suite was not an intentional deletion or structural move: the
current lineage had not carried the accepted foreground reuse negotiation,
availability error partition, or its browser regression cases. Applying only the
five prewarm commits to that lineage cannot preserve those dependencies.

The accepted Web lineage separates into concrete groups:

- `18366136`, `b4e1c61e`, `0f9875fd`, `42d00a35`: foreground v2 negotiation,
  fenced outcomes, real-template tests and availability partition. These are
  the prerequisite group for the successor, not optional prewarm hunks.
- `5c52a267` through `eb96fc68`: the bounded measurement runner and its
  refinements. Retain as diagnostic tooling; it does not prove hover treatment.
- `da2b9304`: accepted Tree Add machine entry/focus behavior. Preserve while
  composing Tree prewarm handlers; it is not a handshake prerequisite.
- `fb915a47`, `fb8ef0d5`, `8d94d9e3`: accepted audit labels/charts, their tests,
  translations and contrast tokens. These are independent of prewarm and must
  be listed separately in the Release source inventory, not called prewarm fixes.
- `6be9a4f1` and `6e97383b`: accepted startup navigation and terminal test setup
  fixes. Keep their existing behavior, without claiming new prewarm coverage.

In this diagnostic union, the foreground interface, prewarm hook, capability
consumer and Tree match `ed1080bb` byte for byte. The metrics components, parser
and browser suite match `961dd64e` byte for byte. The accepted `42d00a35`
availability scenarios and assertions remain unchanged; only their Rust-template
extraction uses the shared successor extractor. Keep the accepted Add machine
entry and focus handling alongside the hover/focus handlers.

The shared `WorkspaceAppContent.logout` is the actual composition point:
`embedPrewarm.cancel()` must precede the logout request, and
`setMainView("overview")` must also run before that request to unmount metrics.
Keeping either side alone loses accepted behavior. The combined caller browser
regression holds the logout response while metrics are visible and a prewarm
mint is pending, then requires both the metrics panel and hidden frame to
disappear before releasing the response. Late mint completion must not navigate
or restart prewarming after an inventory update.

This records the Web dependency boundary, not approval of every unrelated
accepted-baseline path or a deployment selection. Core/session/ingress lineage
and the Release source inventory require their owners' reconciliation. These
local HTTP fixtures do not establish live cookie reuse or a latency improvement.

## Minimal source selection after the diagnostic union

The Web port on Core `25994a1a` carries the foreground/caller/template/browser
prerequisites, timing diagnostic and accepted Tree Add machine/focus boundary.
It preserves the current metrics files unchanged. It intentionally excludes the
independent accepted audit-summary UI/labels/tokens and delayed mobile startup
change, along with their specific tests/documentation; the diagnostic union's
results do not establish those excluded surfaces on this selected source.
Core owns shared operator, quality and security documentation in its parent.
The diagnostic 285-case proof remains bound to its original source, with final
port checks reported separately.
