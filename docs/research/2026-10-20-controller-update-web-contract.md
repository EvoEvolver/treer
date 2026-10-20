# Controller update from the Machine page (#222)

Status: proposed contract for Core, Web and Security review. This document does
not add routes, advertise a capability, or enable machine updates. The owner
request is issue #222; #111 remains the updater and native acceptance boundary.
The matching inert wire models live in `treer_protocol::controller_update`.
They reject extra fields during decoding and provide explicit `validate()`
methods for shape/outcome consistency. Future adapters must call validation
after bounded decoding, then independently verify authorization, catalog trust,
token binding and native evidence. Model validation supplies none of that
authority and cannot attest to a health check or signed receipt.
Nullable response fields must be present: JSON null means unknown, whereas an
omitted key is malformed. Browser decoders must enforce the same distinction.
Before deserializing, future HTTP adapters cap request bodies at 1 KiB and
response bodies at 64 KiB and bound collection visitors to 16 targets/operations
and six plan steps; post-decode validation is not an allocation limit. Oversize
body/vector/string negatives are required at that adapter boundary. Reject
created/updated/observed/finished times beyond server now plus 30 seconds; model
validation checks only internal chronology and cannot consult server time.

## Existing implementation boundary

The provider CLI installer already has human-session authorization, membership
and `machine.exec` checks, prepare admission, a 120-second confirmation token,
atomic operation insertion and token consumption, one-shot dispatch and GET
observation. Reuse those patterns and small presentation helpers; a Controller
update is neither a provider recipe nor a generic command job.

At #111 `c9af1252`, `self_update::Receipt` records workspace, server, UUID job,
revision, release/source identities, Controller/CLI hashes, publication state,
source trust and phase. The Unix worker owns transferred locks and has a
one-shot Core execution seam. `CoreHealthy` means `LauncherRequired`, not whole
update completion. There is no production Controller dispatch/reconciliation
entry. Windows retained-decision work is test-only. None of these facts permits
advertising browser update support yet.

In particular, `Source::Signed` currently leaves freshness unavailable until
P4; `Source::Pin` is an explicit transition acknowledgment, not a signature, and
its current validation requires acknowledgments unsuitable for silently
inventing browser consent. A catalog cannot manufacture those acknowledgments
or bypass updater validation. Production enablement needs a reviewed trust
adapter and usable #111 entry, recovery and native acceptance for that platform.

## Scope and capability

Proposed capability: `machine.controller-update.v1`, separately advertised by
Proxy and by a supported Controller. Unknown or absent capability refuses the
mutation, irrespective of version strings. Existing GET discovery must remain
available while the Controller is offline or replaced by an older binary.
Advertisement is per platform: no native acceptance for that platform means
no capability there, even if the shared types compile. Discovery precedes
capability gating in the UI, so a durable operation never disappears offline.

All routes below are human-session routes under
`/api/workspaces/{ws}/machines/{machine}/controller-update`. Require current
workspace membership and the existing `machine.exec` machine-resource Policy
check on every request, including observation. No App, service or machine
credential access. Do not add a new Policy action in this slice: #222 explicitly
selects the existing action. Creator-only operation access returns 404 for a
foreign operation after ordinary scope authorization.

Strict request models reject unknown fields. No browser request accepts argv,
command, path, URL, hashes, worker flags, trust overrides or rollback commands.
No endpoint provides cancellation; closing the dialog stops observation only.

## Proposed API surface

| Method and suffix | Request | Response |
| --- | --- | --- |
| GET `catalog` | none | `Catalog` |
| POST `prepare` | `{target_id}` | `Preview` |
| POST `confirm` | `{confirmation_token}` | `Operation` |
| GET `operations` | none | `Discovery` |
| GET `operations/{operation_id}` | canonical UUID | `Operation` |

Catalog, prepare and confirm use `Cache-Control: no-store`; observation does
too. GET never dispatches, restarts, activates, consumes a token or runs an
update probe with side effects. A browser refresh starts with discovery, not
prepare. Confirm is sent once by the UI, even if its response is lost.

Wire fields below are the minimum proposed shapes, not existing API promises.
All timestamps are UTC RFC3339 strings. IDs are opaque bounded strings; job and
operation selectors are canonical CSPRNG-generated v4 UUIDs, never filesystem
paths. Enforce a unique operation ID constraint, binding scope immutably to that
ID. An expired or abandoned preview ID can never be rebound to another target,
session or machine, including after cleanup.

`Identity`:

```text
{ release_sha, artifact_source_sha, controller_sha256, cli_sha256 }
```

SHA fields are canonical lower-case hex (40 for source/release SHA, 64 for
artifact hashes). A version label can be shown separately but is not identity.
Platform is a closed existing release-platform enum.

`Catalog`:

```text
{ support: supported | manual_update_required,
  availability: ready | offline | busy | catalog_unavailable,
  current: Identity | null, observed_at: timestamp | null,
  platform, catalog_revision, targets: Target[] }
Target = { target_id, identity: Identity, platform, version_label }
```

At most 16 targets, each from a Proxy-owned immutable pinned catalog rooted in
the updater's reviewed artifact trust. No unverified remote "latest" response
is authority. A channel alias resolves to an immutable target before preview;
the first implementation can simply omit aliases. Same-version, downgrade,
unknown ordering, mismatched platform or unavailable current identity cannot
mint a token. Release labels are untrusted display-only ASCII text, bounded to
128 bytes (printable characters and spaces only). This excludes Unicode bidi,
format and zero-width characters; localized UI copy stays in UI dictionaries.
Target IDs and labels never replace the artifact identity shown in details.
Permission refusal is HTTP 403, not an empty catalog that looks unsupported.
Catalog bytes change only through a reviewed Proxy release, not a runtime fetch.
`catalog_revision` is a positive monotonic integer no greater than 2^53-1; persist
the accepted revision floor so an older release cannot silently roll it back.
An older Proxy with a catalog below the floor refuses new preparation; existing
operation lookup remains available. Current identity has no semantic-version
claim: display its short release SHA as "Build", with full identity in details.

`Preview`:

```text
{ operation_id, current: Identity, target: Target,
  plan_digest, steps: UpdateStep[], confirmation_token, expires_at }
UpdateStep = verify_artifacts | stage | replace_controller_and_cli |
             verify_health | reconcile_launcher | finalize
```

Steps are fixed categories, not commands or prose supplied by the Controller.
The plan includes launcher reconciliation because #111 does; it never restarts
the Host or agents. `operation_id` is allocated at prepare and becomes the
stable #111 job ID at acceptance. Preview allocation does not claim that any
update has started. Expired/unconfirmed previews are not returned as operations
by discovery. A no-op prepare returns a typed refusal, not a success operation.

`Operation`:

```text
{ operation_id, current: Identity, target: Target,
  state: accepted | updating | succeeded | failed | rolled_back |
         rollback_failed | partial | unconfirmed,
  phase: preparing | prepared | activating | core_healthy |
         launcher_installing | rolling_back | complete | failed |
         rolled_back | rollback_failed | partial | null,
  observation: available | waiting_for_reconnect | unavailable,
  observed_identity: Identity | null, restored_identity: Identity | null,
  health: unknown | healthy | unhealthy,
  receipt_revision: integer | null,
  created_at, updated_at, observed_at: timestamp | null,
  finished_at: timestamp | null, error_code: UpdateError | null }
Discovery = { operations: Operation[], blocked_by_other_operation: boolean }
```

Separate durable outcome from observation availability. An offline machine does
not erase a known outcome; an unavailable GET does not change durable state to
failure. `accepted` is a committed Proxy intent, not proof that dispatch reached
the worker. The phase is the last validated receipt phase, or null before a
receipt exists. `core_healthy` alone must never produce `succeeded`.

No raw receipt, output tail, path, exception message or token enters operation
responses, browser persistence, audit or Proxy logs. The receipt is validated
privately and projected to these fields. Diagnostic codes are fixed categories.
Accepted intent has no error, observed/restored identity, health evidence or
finished time. Updating has no error/finished result. Unconfirmed, partial and
rollback-failed project health as unknown or unhealthy, never healthy: this
minimal wire shape has no separate correlated healthy-component evidence.

## Confirmation, duplicates and conflict ownership

Reserve prepare attempts before Controller work, reusing installer ceilings of
10 attempts/minute and four live tokens per user/machine; TTL is 120 seconds.
Bind the hashed token to workspace, server, creator, session hash, operation ID,
current identity, immutable catalog target including hashes/platform, catalog
revision and canonical plan digest. Revalidate capability, Policy, catalog,
current state and plan before acceptance. The Controller revalidates the bound
plan/current installation under its updater locks before any mutation.

One database transaction consumes the token, claims the machine mutation slot,
inserts the durable intent and records acceptance audit. Network dispatch occurs
after commit, once; PostgreSQL and a Controller socket send are not atomic.
Do not use a replayable generic outbox for activation. An interrupted dispatcher
leaves intent/dispatch uncertainty for read-only reconciliation, not resend.

For the same authenticated creator/session and still-authorized scope, an
already-consumed token returns its existing operation without dispatching again.
This bounded consumed-token mapping is observation, not reusable authority;
fresh tokens expire as usual. A replay in another scope/session is refused.
Confirmation tokens contain 32 to 256 ASCII alphanumeric/underscore/hyphen
characters. Actual generation uses cryptographic randomness and server lookup
is hashed or constant-time; this structural length floor is not an entropy proof.
The consumed-token lookup expires 420 seconds after original preparation (TTL
plus five minutes); it never extends the 120-second acceptance deadline. After
lookup expiry, refuse the token and use authenticated GET discovery. Foreign
and nonexistent operation IDs have the same 404 response/code and query path;
the busy boolean has the same membership/Policy authorization as discovery.
Two different previews racing for one machine cannot both claim the slot.

The mutation slot must also exclude a provider CLI install. Integrate the claim
with the existing installer transaction; checking two independent busy flags is
racy. Do not change existing installer execution or confirmation semantics.
Unconfirmed, partial and rollback-failed updates retain the conflict claim until
explicit, validated recovery proves ownership safely resolved. Timeout or
retention expiry cannot free it. Definitive failures also have a 60-second
per-machine cooldown before a new prepare. There is no UI retry for ambiguity.
Recovery is a separate explicit operator action outside this browser API,
audited with operator identity, job and evidence category. It must prove the
old worker cannot mutate, reconcile installation locks/journal/publications,
validate active artifact identity and health, and obtain the matching durable
native recovery result before releasing the Proxy claim. A job-not-found report
or database edit alone is insufficient. This operator recovery path is not yet
implemented in #111; its absence blocks production enablement, not continued
model/UI development. Do not add an ad-hoc clear-slot endpoint here.

## Restart-safe observation and retention

Store intent, frozen identity/plan binding, creator, timestamps, dispatch state
and last validated receipt projection durably in Proxy storage. Bind the native
job to exactly the same UUID/workspace/server/target. The new Controller reports
the existing job after restart; it must not invent a new job from an online bit.
Reject receipt identity mismatch, revision regression, conflicting equal
revisions or invalid phase transitions. A later authenticated valid report can
resolve `unconfirmed`; only that path can clear its claim when safe.
After a dispatcher dies between acceptance commit and send, reconciliation
queries the selected Controller for that exact job UUID without dispatch. A
not-found result waits within the report grace, then becomes unconfirmed and
keeps the claim. It is never proof that sending is safe or required.

Success requires the matching whole-update `Complete` receipt, target artifact
identity and correlated updater health evidence. `RolledBack` needs validated
restoration and restored health/identity evidence; otherwise show unconfirmed.
`Partial` and `RollbackFailed` remain distinct, with unknown health explicit.
Uncertainty after dispatch, worker loss or missing receipt is never success.
After ten minutes without a report, show unconfirmed and stop automatic polling;
manual Check status remains GET-only. This timer grants no recovery authority.

Discovery returns at most the creator's 16 newest operations on that machine,
always including its unresolved operation. Another creator's active slot is
exposed only as `blocked_by_other_operation`, without identity or job details.
Order is descending by created_at, then descending canonical operation UUID
for ties. At most one unresolved claim (accepted, updating, unconfirmed, partial
or rollback-failed) may appear. A malformed list refuses whole instead of asking
the UI to choose an active operation. Non-ready or unsupported catalogs return
no selectable targets, independently of durable operation discovery.
Keep resolved observations seven days, capped at 64 per creator/machine; prune
only resolved, safely reclaimable records. Unresolved records and slot ownership
cannot be evicted to satisfy a cap; refuse new acceptance if durable capacity
is exhausted. Audit and native journal retention retain their separate rules.

Fixed audit categories cover prepare/refusal, acceptance, dispatch attempt,
validated outcome and observation uncertainty. They carry actor/scope, job,
from/to identities, plan digest and category, never confirmation tokens or
process diagnostics. No cancellation event is fabricated for dialog closure.

## Typed refusals and incremental delivery

Proposed `UpdateError` values: `unsupported`, `offline`, `busy`,
`catalog_unavailable`, `target_unavailable`, `target_not_upgrade`,
`preview_expired`, `preview_changed`, `confirmation_invalid`, `rate_limited`,
`capacity_exhausted`, `verification_failed`, `health_failed`, `receipt_invalid`,
`recovery_required`, `result_unconfirmed`. Prefix HTTP error codes with
`controller_update_`; enum response values stay unprefixed. Authentication,
membership and Policy errors retain the existing standard responses.

Delivery order: review this contract; implement strict shared protocol models
and synthetic Web flow; implement Proxy intent/confirmation/discovery with
transaction and replay tests; add the reviewed #111 production adapter and
restart reconciliation; then exact combined gates, native platform acceptance
and deployed-browser acceptance before enabling capability. No fake successful
adapter or version-inferred support bridges an unfinished stage.

Required negatives include substituted target/extra fields, stale current or
plan, wrong user/session/machine, double-tab tokens, consumed-token replay,
install/update slot races, lost POST response, death before/after dispatch,
Proxy/Controller restart, missing/regressing/conflicting receipts, health or
identity mismatch, partial/rollback-failed results and GET-only browser recovery.
The existing installer browser and Proxy regression checks remain required.
Stale-current tests must exercise the prepare-to-confirm window at the Proxy
and the confirm-to-mutation window under Controller updater locks separately.
