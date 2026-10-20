# #158 final tool retention: leader-only compatible update

Prepared after incident7243. Rust PID91662/gen25 is historical, not a target
identity to act on without fresh verification. The same saved thread is
01a12e09-93e9-79a1-9e66-45a25bac49d8. Migration remains provisional. This plan
executes nothing and authorizes no restart, acceptance resend or third agent.
Core/Security must review the exact patch, gate and frozen artifact; the leader
must separately decide whether/when to execute one update after author idleness.

The scoped sanitized evidence proves completed commandExecution then omission
from the same final projected turn. It does not identify which raw producer
snapshot omitted it. No private provider-native files were read. The original
pwd observation remains tool UNKNOWN; the separate README observation proves
its own real execution only. Never replay either acceptance.

The pinned public Codex0.162.1 protocol uses ThreadItem (including its string id)
in ItemStartedNotification, ItemCompletedNotification and Turn.items:
[item schema](https://github.com/openai/codex/blob/092d3acd6bec3e3a14bdc7e7a2810ab628ab759d92c603/codex-rs/app-server-protocol/src/protocol/v2/item.rs).
The same version's [Turn schema](https://github.com/openai/codex/blob/092d3acd6bec3e3a14bdc7e7a2810ab628ab759d92c603/codex-rs/app-server-protocol/src/protocol/v2/thread_data.rs)
explicitly labels itemsView as summary/notLoaded/full; fixtures use these labels
without inferring full-history authority from them.
This is a same-item identity contract, not proof that an arbitrary producer
never changes an id. Differing IDs are never merged by position/text; an
apparent duplicate message refuses except for existing receipt-owned user echo
canonicalization. Other distinct items remain distinct. Missing/invalid stable
identity refuses final reconciliation while preserving the observed projection.
A legitimate repeated identical message under a different id, or a missing id
in a summary, can therefore pause the queue with the fixed conflicting-final
error; do not bypass that availability refusal or replay automatically.
Only dynamicToolCall.contentItems and mcpToolCall.result are extendable result
fields. Other existing nonempty terminal tool metadata (type, tool, namespace,
server, arguments/input, status/error, duration and unknown fields) must remain
equal; omitted/empty summary metadata retains the previous value. A changed
duration therefore deliberately pauses just like a changed tool name, rather
than silently accepting contradictory terminal detail. File path/kind entries
stay immutable within the retained prefix. Protection of same-ID detail applies
after item termination; an in-progress item's incoming payload still follows
the existing replacement behavior. This is not a new partial-output archive.

## Preparation and stop boundary

New immutable artifact directory:
/Users/dooms/.local/opt/treer-rust-launcher/final-retention-v1.
Use only hashes/bytes from its exact reviewed freeze and SOURCE.md. Preserve
cursor-semantics-v1, attempts1–5, prior manifests, Node backups and live fences.
Immediately before STOP and immediately before START, verify SHA256SUMS itself
against the freeze, then every manifest entry. Check the compiled provider path,
public executable hash and installed package metadata against SOURCE.md too;
the reported providerVersion alone is not binary attestation. No provider probe
or private home/auth/config/transcript reads. Read-only modes are accident
prevention, not same-user containment.

Leader declares a quiet window and records fresh full supported startup,
launcher/provider/tool PID plus birth identities, same thread, ready/idle/open,
queue0 and no pending notification/ack/goal/attachment mutation. Held-delivery
view UNAVAILABLE must remain explicitly UNAVAILABLE, not fabricated zero. Save
only sanitized IDs/counts publicly; startup and backups remain private0600.
Use a NEW absent audit directory for this update, not attempt5. No prompt to
establish idleness. If eligibility is ambiguous, STOP the procedure unchanged.

Leader stops the exact agent through supported lifecycle only, verifies every
recorded launcher/provider/tool identity exited, and checks no open holders on
its private state before replacement. Never kill by name or guessed PID. If an
identity or holder is ambiguous, halt and reconcile. Make and verify a private
byte-for-byte backup of the stopped Rust state in the new audit, recording only
hashes/size/modes in sanitized reports. Do not clear/import/archive live journals,
fences, Node backup, or source files. The current deployment is already Rust:
**no --import-node-state and no --restore-node-backup for this update.**

## Startup replacement and rollback

Save the exact prior startup (including all argv/env and explicit thread) as the
rollback specification. Build the candidate specification by replacing ONLY the
old frozen binary and public asset paths with final-retention-v1 equivalents;
all other argv/env/thread/cwd settings must remain byte-identical. No generic
startup template may silently drop settings. Leader reviews the before/after
delta, updates saved startup while stopped, and starts ONCE. Fresh hashes again
immediately before start. Record new actual PID/birth/generation.

Before any separately approved NEW observation require same thread, ready/idle,
open transport, queue0, no fence, valid identity/order/full-request predicates,
four numeric predicate counters0 and the existing pinned cursor gate from
CURSOR-SEMANTICS.md. Resumed/imported thread status must remain explicit.
No retry or prompt if any check refuses. A missing finalProjection before the
first new completion is expected (null), not proof of retention.

If startup/eligibility fails, leader stops and verifies exact new identities and
all holders exited. Retain failed-state evidence privately. Restore the exact
prior startup and a separately validated stopped backup only if necessary;
never delete/edit a new dispatch fence to manufacture eligibility. If state
requires reconciliation, halt instead of automatically restoring/replaying it.
Node rollback remains a separate leader decision with the already reviewed
Node backup/source eligibility procedure, not an implicit fallback here.

## Acceptance and limits

Leader may authorize ONE distinct read-only observation, watched live through
supported AIS scoped to its new turn. Require exactly one completed tool with
matching public output, then same-turn final /api/state retaining the item and
output once after settlement and repeated reads. Compact /v1/transcript retains
its id/kind/command once; that route intentionally omits detailText/output.
Record only fixed
kinds/statuses/match booleans and IDs, not raw frames/commands/private output.
Check finalProjection counts/refusals and no fault, plus itemized real-browser
embedded/standalone/older-page acceptance and per-PID RSS samples. HTTP tests
are not browser acceptance. No unchanged retry, old acceptance resend or third
agent before the leader's distinct decision.

The bounded union preserves only observed items in the current retained window.
No full-history completeness or restart recovery of omitted native items is
claimed; the journal persists receipts, not a new tool transcript. This fix
cannot reconstruct the tool already missing in attempt5. An over-budget union
fences with uncertainty, not truncation or clean success. Existing terminal
receipts never reopen on conflicting later summaries. Native Windows and live
rollout acceptance remain separate from the local hermetic gate.
