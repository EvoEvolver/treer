# Windows self-update: first native checkpoint

Issues #111, leader3158, security3159 and launcher3161. This is a proposed
Windows-specific contract for review, not implementation or live-probe approval.
The [transaction design](2026-10-16-controller-self-update.md) still defines
verification, receipts, rollback and acceptance. Windows does not depend on the
Unix socketpair/SCM_RIGHTS/pidfd work. Darwin remains unavailable for Rust
authority-bearing self-update; its existing updater is unchanged.

## Current scope: PowerShell updater parity (leader5134, 2026-10-19)

This decision supersedes the boot-epoch acquisition experiment and the proposed
native registration/permit prerequisites below. Do not implement an HKLM epoch,
privileged initializer, registry provisioning or reboot/hibernate/Fast Startup
experiment for this work. The inert registration models remain test history,
not requirements for the first Windows updater. They grant no production
authority. The goal is the transaction already implemented by
`scripts/update-machine.ps1`, expressed in Rust, with the previously selected
supervisor inside the stable Host Job. This plan does not authorize a live update.

The smallest complete transaction is:

1. Validate the local detached installation, exact workspace/server identity,
   loopback endpoint and binary paths. Refuse an unexpected shared installation
   unless explicitly acknowledged. The Controller daemon starts a copy of the
   installed Rust image as supervisor in the Host Job, outside the Agent PTY Job;
   no breakaway, Host restart, service or scheduled task is involved.
2. The supervisor takes the installation's exclusive, non-inherited file lock
   before mutations and keeps it until a terminal result. Use a new private
   per-attempt directory and refuse existing terminal or ambiguous attempts;
   never automatically resume an interrupted transaction. The initiating
   Controller returning or exiting does not release this supervisor-owned lock.
3. Fetch the explicitly pinned SHA256SUMS and verify its out-of-band SHA-256
   before interpreting it. Download Controller and CLI only, verify their hashes
   and reported versions, and refuse redirects, URL credentials and ambient
   proxies. Preserve the PowerShell bounds (8 KiB manifest, 256 MiB per artifact)
   and match the per-artifact 256 MiB bound for Windows Controller/CLI downloads
   (leader5245). The inert Rust image/copy verifier uses that same bound, checked
   from metadata before copying and during streaming; its buffer stays 64 KiB.
   Local retained Windows
   artifacts at `169259f` measure 20,885,504 bytes (Controller) and 5,185,024 bytes
   (CLI); these are historical size evidence, not the next candidate. W3 must
   record sizes of its exact frozen artifacts and refuse an oversized candidate.
   Keep normal TLS certificate validation; no custom trust or HTTP downgrade.
4. Snapshot readiness, Controller epoch/build, Host build and Agent
   ID/PID/generation tuples. Back up Controller, CLI and configured startup store
   into private storage, check backup hashes, and retain the Host binary only as
   evidence without changing it. Write the activating receipt before swapping.
5. Copy and verify the staged Controller beside its installed destination; move
   the running image to a unique `.prev-*` name, then move the verified copy into
   place. If the second move fails, restore the first immediately. Keep the old
   running image; two moves are not an atomic multi-file transaction. Invoke the
   fixed Rust `service ... restart-controller` command, never a shell.
6. Bound health waiting and require the selected workspace/server, new
   Controller epoch, intended build, Proxy connection, required capabilities,
   unchanged Host build and the PID/generation of every previously live Agent.
   Match the current script's inventory semantics (`09b14458`): Exited records
   not relisted and newly appeared Agents are reported, not treated as live-Agent
   loss. Only then replace the CLI with the same checked rename sequence and
   verify final installed hashes/live-Agent tuples. Persist `complete` only
   after all checks succeed.
7. On failure, restore changed binaries in reverse order from checked backups;
   if activation occurred, restart the restored Controller and verify its old
   build and live-Agent tuples. Persist `rolled_back` only after verified recovery;
   otherwise persist `rollback_failed` or leave interrupted evidence for an
   operator. Startup state is backed up but this parity transaction does not
   change it. Launcher installation and signed-channel anti-rollback are later
   increments, not hidden conditions for this first parity run.

Known limits are explicit: the hash pin is integrity pinning, not publisher
authentication or release freshness. There is no OS boot-epoch proof,
cross-reboot replay proof or exactly-once recovery claim. An exclusive lock
serializes cooperating live workers; it does not defeat a same-account actor,
snapshot replay or all stale/duplicated requests. Refuse an ambiguous receipt
and require operator inspection rather than redispatch. Host/OS death kills the
supervisor and can interrupt rollback; do not report that as successful recovery.
The existing script's Host-build comparison is not proof of Host-process identity.
The PowerShell script validates target SHA/version equality but has no ordering
or downgrade check. Git SHA lexical order cannot provide one. For the Rust
manual-pin path, W3 must require an explicit downgrade/unordered-pin acknowledgement
when ordering against the running build cannot be established; this is not
signed-channel rollback protection. Refuse a new attempt while an unfinished
receipt exists until an explicit operator recovery action classifies it.
Retain private backups and failed copies until an operator reviews the receipt
and explicitly selects exact files for cleanup; there is no silent rotation.
Status/logs never disclose startup content, credentials, artifact URLs or argv.
For failure recovery, an operator must classify the unfinished attempt and move
its evidence aside within private storage before authorizing another attempt;
never silently delete or overwrite it. Explicit cleanup remains a separate
operator decision on exact paths.

Implementation increments toward the first `win_native` run:

- W1: checked Windows file copy/backup/rename and private receipt primitives;
  disposable NTFS tests cover sharing denial, failed second rename and reverse
  rollback. Reuse download/checksum code without porting Unix-only mechanics.
- W2: minimal supervisor launch from the daemon, supervisor-owned file lock and
  fixed local restart/health path. Test Controller replacement inside a disposable
  Host Job; simultaneous submissions and supervisor death must not report success.
- W3: wire the pinned manual transaction and status receipt end to end; hosted
  Server 2022/2025 cover successful update, forced health failure/rollback and
  interrupted-state refusal with disposable installations. Freeze the exact SHA,
  native artifact hashes, gates and recovery procedure for review.
- W4: request leader-coordinated first `win_native` execution using only approved
  Treer paths and the existing Host, preserving a verified old executable and a
  local-terminal recovery command. No reboot or privilege experiment. Actual
  Controller restart and update wait for that explicit operational authorization.

Initial estimate: 3-5 development days for W1-W3, excluding review/CI queue and
machine scheduling; W4 is one supervised maintenance session if hosted results
are green. The Windows production path is not wired today, so this is an estimate,
not a claim that the inert hosted tests already implement the updater. Release
owns script/coordinator cutover and script removal after native acceptance.

### Next W1 slice: receipt and interruption classification contract

This is a design checkpoint, not an implemented Windows journal, recovery
command or permission to replay a mutation. The Unix journal in
`treer-agent-server/src/self_update.rs` uses Unix private-directory mechanics;
its `publication.rs` reconciles one atomically replaced destination. Windows
must not reuse that reconciliation unchanged: moving the old image aside and
publishing its replacement leave an observable interval without an active name.

Keep the transaction identity, pinned artifact provenance, monotonic revision
and bounded fixed diagnostics from the shared receipt design. Add an explicit
Windows layout discriminator before accepting Windows records; an old Unix
reader must refuse it rather than interpreting a Windows rename as atomic.
Whether this becomes a new schema version or a tagged publication model must
be settled when the common model is separated from the Unix storage adapter.
The current inert ordering model proposes both: version 2 plus an explicit
Windows two-rename layout, present independently in the marker and receipt.
The model checks the marker header before inspecting receipt content. These
typed fields are not yet a serialized schema; the existing Unix version-1
reader is unchanged. The subsequent schema increment must exercise the actual
Unix reader refusing Windows bytes, including a Windows marker presented alone.
The next test-only Controller module `windows_update_envelope` fixes a draft
binary envelope before the inner receipt schema is implemented:

| Offset | Width | Meaning |
| --- | --- | --- |
| 0 | 8 | ASCII magic `TRUPDWIN` |
| 8 | 2 | Little-endian schema version, exactly 2 |
| 10 | 2 | Little-endian layout, exactly 1 (Windows two-rename) |
| 12 | 2 | Little-endian kind: 1 receipt, 2 marker; must match expected kind |
| 14 | 2 | Reserved, exactly zero |
| 16 | 4 | Little-endian total byte length, including the 20-byte header |
| 20 | Remaining | Nonempty opaque payload, not yet parsed |

The supplied buffer is capped at 65,536 total bytes before header access; the
declared total must exactly match, so truncation, trailing bytes and concatenated
records are refused. Decode borrows the payload with no allocation or inner
parser; encode checks the payload bound before allocating. The subsequent
test-only `read_file` adapter requires an already-open regular file positioned
at zero, checks its handle metadata length is 21..65,536 bytes, then reads the
fixed 20-byte header on the stack. It validates every header field and agreement
with the observed length before allocating the bounded buffer. It reads exactly
the remaining bytes, probes at most one extra byte to refuse growth/trailing
data, and rechecks the handle length. Tests cover a real disposable file, every
truncated read, invalid headers refusing before body reads, oversized metadata
refusing before any read, and I/O failures at every byte and at the EOF probe.
This is a bounded byte transport, not a trusted file opener: NTFS/ACL/object
identity checks, exclusive ownership and mutation fencing remain prerequisites
for a future native caller. Same-size in-place changes are not detected by the
length checks; the independent pair digest check is still mandatory. No I/O
deadline or power-loss durability guarantee is provided by this adapter.
The native caller must read through the same handle that passed NTFS, ACL and
identity checks, without reopening by path, and deny write/delete sharing for
the complete read and digest interval. Compute the digest over the exact returned
bytes, never a second read. The local fixed-disk NTFS assumption must be checked
by the native caller, including refusal of pipes/devices; `is_file` alone is not
that native admission check. The random `create_new` shared-temp opener is a
disposable test fixture only, not the production private-record opener.
Portable tests cover every truncated length, mutation of every header byte,
wrong kind/endianness, overflowing declared length and the exact total cap.
On Unix, a disposable real `Journal` is given a Windows receipt with a sibling
Windows marker, then the Windows marker alone in the receipt slot. Both read
and reopen must refuse with existing bytes and directory entries unchanged.
The legacy Unix parser cannot treat the binary magic as JSON; no migration or
Windows payload parsing is added to it. These tests establish rejection, not
Windows inner-field/schema validation, native storage or production support.
This separation is an invariant of the format: every envelope starts with
uppercase `T`, outside all legal JSON value starts and JSON leading whitespace.
Keep that exclusion if the magic changes; both receipt and marker encodings
have a regression check independent of their opaque payloads. Unix refusal
currently comes from this JSON parse failure, not a new Unix discriminator
reader. The envelope itself has no integrity protection: before any payload
use, the independent marker/receipt binding, revision, header and exact-byte
digest agreement remain mandatory. A structurally valid envelope alone must
never authorize payload use or hide torn/in-place payload damage.

The next test-only Controller module `windows_update_record` proposes only the
inner binding/provenance/object fields; it is not the complete receipt schema.
Both envelope kinds contain strict JSON objects, with unknown, duplicate and
missing fields refused at every typed object level. Their fields are:

| Payload | Required fields |
| --- | --- |
| Marker | `binding`, `receipt_sha256` (SHA-256 of the entire receipt envelope) |
| Draft receipt fields | `binding`, `provenance`, `images`, `state` |
| Binding | `workspace_id`, `server_id`, canonical nonnil `job_id`, positive `revision` |
| Provenance | Lowercase hex `release_sha`/`artifact_source_sha` (40), `sums_sha256` (64) |
| Images | Exactly `controller` and `cli`, each with exactly `old` and `new` objects |
| Object | `filesystem` (`ntfs` only), `volume_serial` (u32), `file_id` (u64), `bytes` (u64), `sha256`, `links`, `attributes`, `acl_policy` |
| Claimed state | `transaction`, `controller_step`, `cli_step`, `new_health_recorded`, `final_health_recorded`, `old_health_recorded` |

Parsing requires an independently supplied expected binding. It validates the
marker envelope/fields/binding first, then the receipt envelope and whole-byte
digest, and only then invokes the draft receipt JSON parser. A test with invalid
receipt JSON and a mismatched marker asserts the digest refusal happens first.
The parsed binding must match that expectation again. All four recorded object
identities must differ; equal hashes remain valid for different identities.
Object lengths range from 1 byte through 256 MiB, with lowercase SHA-256, one link,
and the sole `owner_system_protected` policy label. The attribute allowlist is
exactly NORMAL alone, ARCHIVE alone, READONLY alone or ARCHIVE | READONLY;
zero, all other bits and NORMAL combined with any other flag are refused.
Uppercase digest spellings are refused in both the marker and receipt objects.
The identity shape deliberately uses the native adapter's NTFS-only boundary:
ReFS and every other filesystem are unsupported. The serialized `ntfs` label
alone proves nothing; a future observer must independently verify NTFS on the
pinned volume handle before using the BY_HANDLE volume/file identity, and
refuse if it cannot do so. There are no caller-supplied paths, SIDs or arbitrary
diagnostics.

These checks validate serialized field shapes and pair consistency only. They
do not authenticate the expected binding, supply the external durable anchor,
prove provenance or ACLs, reopen objects or compare them to real NTFS state.
The next increment adds strict claimed-state spellings for the existing seven
transaction phases and nine image steps in the inert platform classifier.
All three health-record flags are required booleans. Parsed claims are compared
against separately supplied synthetic slot observations by the same classifier
source, included only in Controller tests; observed slots are never accepted
from receipt JSON. Tests parse and digest-bind claims before checking forward
and rollback order, stray health flags, and unknown/unreadable observations.
Parsing a claim alone deliberately does not mean it is consistent with disk.
Neither parsing nor layout consistency authenticates the health flags, validates
a prior-to-next state transition, proves completion or authorizes mutation.
Actual health evidence, archive disposition, transition validation and integration
with the marker-first model still require separate increments. No production
reader, file I/O adapter or recovery command uses these parsed fields.
Before a production reader may rely on health, each flag must be replaced by an
evidence reference bound to the observed image identity/hash and receipt revision,
and checked against an independently authenticated observation from the new
process. A digest or reference manufactured only by the receipt writer is not
sufficient; the authenticated channel and evidence ownership still need a
separate design/review. `Consistent` must never become an alias for complete.
Transition validation must jointly check the prior receipt, the exact next
revision and an allowed phase/step edge (including explicit rollback edges).
It must be tied to the pair-external durable anchor decision: an internally
consistent old pair replay is still accepted by the current layer. No monotone
revision or replay-protection claim is made until that external ownership and
restart contract exists and is verified.
The subsequent test-only `check_successor_fields` is only a partial predecessor
guard: both record pairs must pass the envelope/digest/field parser; the previous
receipt bytes must match a separately supplied expected-head digest and binding;
the candidate revision must be exactly checked `previous + 1`; and provenance
plus all four image records must remain identical. Job/workspace/server retargets,
revision skips/replays/overflow and otherwise valid image/provenance changes
refuse. The expected head is synthetic, with no protected storage, restart
ownership or atomic compare-and-swap implementation. Tests explicitly show that
replacing both pairs and the synthetic head can pass: this is not replay
protection. The guard also permits an unchanged claimed state at the next
revision; it does not validate phase edges or health evidence, authorize a
publication or make the `Consistent` result mean complete. Those additional
checks and integration with the marker-first publisher remain separate work.
The guard destructures every `DraftFields` field without a remainder pattern:
adding any field must fail compilation until its immutable or transition-owned
status is explicitly chosen. Binding is handled by exact pair parsing; state
remains reserved for the phase/evidence guard. That next guard must refuse a
revision bump with no state change unless a separately specified typed no-op
exists; the current field guard does not implement that requirement.
The named W4 external-head gate requires storage outside both records, a durable
compare-and-swap advance before publishing the new pair, and defined restart
ownership. Replaying an older pair together with its old head is precisely the
case this layer cannot detect. Storage ownership, crash reconciliation after
head advance and before pair publication, and its protected authorization remain
unimplemented and require separate review before W4.
The next inert `check_forward_successor` composes the exact predecessor/revision
and immutable-field guard with layout checks before and after one forward edge.
Its explicit graph is Prepared -> Activating, then Controller Original ->
MoveOldIntent -> OldMoved -> PublishIntent -> Published; only then may a new
health claim enter CoreHealthy. CLI follows the same five steps while the
Controller remains Published, and only then may the final health claim enter
Complete. Each publication step and each phase/health change is a separate
revision; unchanged state, skipped/reversed edges and any exit from Complete
refuse. All rollback edges deliberately refuse in this forward-only guard.
An independent 12-state trace tests every ordered state pair, all eight health
flag combinations at each legitimate edge, and Unknown/Unreadable in every
slot of either image before and after each edge. The observations remain
synthetic and health fields remain bare claims: passing this graph establishes
only model consistency, not authenticated health, head freshness, durable
publication, completion or mutation/recovery authority. Recovery/rollback graph
edges, evidence ownership and integration with the publisher remain open.

#### Forward-edge observation contract (design gate)

The current graph accepts two synthetic snapshots; that does not establish when
an adapter may obtain or use them. A future adapter must apply the following
timing contract under the same verified installation ownership and pinned
objects. `O/N/-` means active old, candidate new, backup absent; `-/N/O` is the
active-name gap; `N/-/O` is the published new image. The table applies first to
Controller in Activating, then to CLI in CoreHealthy. The other image must also
be independently checked in its required unchanged layout at every edge.

| Receipt edge | Snapshot used to validate old and candidate claims | Operation ordering |
| --- | --- | --- |
| Prepared -> Activating | Same fresh current snapshot; both images `O/N/-` | Record phase before any image move |
| Original -> MoveOldIntent | Same fresh current `O/N/-` for both claims | Publish/read back intent before moving old aside |
| MoveOldIntent -> OldMoved | Fresh post-move `-/N/O` for both claims; old intent permits this result | Verify the move before recording its result |
| OldMoved -> PublishIntent | Same fresh current `-/N/O` for both claims | Publish/read back intent before moving candidate into active |
| PublishIntent -> Published | Fresh post-move `N/-/O` for both claims; old intent permits this result | Verify the move before recording its result |
| Activating -> CoreHealthy | Same fresh current snapshot; Controller `N/-/O`, CLI `O/N/-` | Verify new Controller health before recording evidence and unlocking CLI |
| CoreHealthy -> Complete | Same fresh current snapshot; both images `N/-/O` | Verify final health before recording final evidence |

Do not manufacture candidate snapshots or combine a stale pre-operation snapshot
with an imagined result. A result edge uses an actual post-operation observation;
an intent edge uses the current pre-operation observation and must not perform
the rename until its record publication/readback contract succeeds. Interrupted
intent records may classify either physical layout during diagnosis; that is
not permission to publish a new intent after its operation has already occurred.
The current inert graph does not enforce this timing or persist observations.

The production health-edge signature must accept verifier-constructed evidence
types, not caller-supplied booleans or deserialized receipts as evidence. Only
the health verifier may construct those values after checking the authenticated
process observation, image identity/hash, job/revision and channel ownership.
Serialization must not provide a constructor that bypasses verification. New
Controller health and final health require their respective evidence types;
the verifier/channel design remains unimplemented and needs separate review.

#### Exit coverage from forward states (proposed, not executable)

The following is coverage for a future rollback graph, not accepted recovery
commands or implemented state transitions. All exits require trusted head,
verified objects, exclusive/quiescent ownership and appropriate health evidence.
`C` and `L` denote Controller and CLI; `RC` and `RL` denote the proposed
RollbackController and RollbackCli phases. A restored image still needs old
Controller health before RolledBack can be recorded.

| Forward phase and durable step | Current moving-image layout | Proposed exit |
| --- | --- | --- |
| Prepared (both Original) | `O/N/-` on both | RC with C Original; no move; verify old health |
| Activating, C Original | `O/N/-` | RC with C Original; no move; verify old health |
| Activating, C MoveOldIntent | `O/N/-` | RC with C Original; old move did not occur |
| Activating, C MoveOldIntent | `-/N/O` | RC with C RestoreOldIntent; restore old directly |
| Activating, C OldMoved | `-/N/O` | RC with C RestoreOldIntent; restore old directly |
| Activating, C PublishIntent | `-/N/O` | RC with C RestoreOldIntent; new publication did not occur |
| Activating, C PublishIntent | `N/-/O` | RC with C MoveNewAsideIntent; move new aside, then restore old |
| Activating, C Published | `N/-/O` | RC with C MoveNewAsideIntent; move new aside, then restore old |
| CoreHealthy, L Original | `O/N/-` | RL with L Original; no CLI move, then RC reverses C |
| CoreHealthy, L MoveOldIntent | `O/N/-` | RL with L Original; old CLI move did not occur, then RC |
| CoreHealthy, L MoveOldIntent | `-/N/O` | RL with L RestoreOldIntent; restore CLI directly, then RC |
| CoreHealthy, L OldMoved | `-/N/O` | RL with L RestoreOldIntent; restore CLI directly, then RC |
| CoreHealthy, L PublishIntent | `-/N/O` | RL with L RestoreOldIntent; new CLI publication did not occur, then RC |
| CoreHealthy, L PublishIntent | `N/-/O` | RL with L MoveNewAsideIntent; move new aside, restore CLI, then RC |
| CoreHealthy, L Published | `N/-/O` | RL with L MoveNewAsideIntent; move new aside, restore CLI, then RC |
| Complete, both Published | `N/-/O` on both | Terminal evidence-preserving stop; no revision or rollback in this completed transaction |

When RL finishes, RC first records MoveNewAsideIntent for the still-published
Controller, then NewAside, RestoreOldIntent and Restored. A direct restoration
from a gap never pretends the new image was published. Any unknown/unreadable
object, layout conflict, missing evidence, uncertain publication or untrusted
head instead reaches an explicit diagnose-and-stop outcome: preserve all records
and objects, do not rename/delete/archive or retry automatically. That outcome
is a design classification, not a new serialized phase or an implemented human
recovery channel. The rollback graph must prove these branches and interruption
points before being wired; this table alone provides no recovery capability.

The rollback actor is not implemented. Before any exit can execute, its design
must name a separate executable with pinned object identity and hash, rather
than depend on the image being rolled back. It must establish exclusive
installation ownership and verified quiescence, including how it stops a live
new Controller and proves that a crashed Controller no longer owns the work.
Neither a crash nor a receipt grants that ownership. Until that actor and its
ownership proof exist, every row above remains coverage only.

The inert rollback slice must mechanically enumerate every combination of
transaction phase, Controller step, CLI step and observed layouts accepted by
the shared classifier, including its health-claim combinations. Each consistent
case must select exactly one proposed exit or diagnose-and-stop; overlapping
exits and missing cases must fail the test. This checks coverage, not the
authenticity of observations or the authority to execute an exit.
The shared model now checks this coverage by enumerating all single-image
slot combinations (including unknown/unreadable objects), all seven phases,
both steps and all eight health-claim combinations. Each consistent forward
tuple must match exactly one of the 16 design rows; existing rollback-phase
tuples explicitly select diagnosis only. This is a coverage test, not an exit
executor or proof of rollback ordering.

An additional private inert entry guard now checks the proposed first rollback
record against the predecessor field guard and the same synthetic current
snapshot for both records. Prepared/Activating enter RollbackController;
CoreHealthy enters RollbackCli. Original layout selects Original, a gap selects
RestoreOldIntent directly, and published layout selects MoveNewAsideIntent.
The other image step and all health claims must remain unchanged. Complete
and all rollback phases refuse entry. Tests exercise all 16 forward layout
cases, candidate phase/step alternatives, health changes and uncertain slots.
This is not rollback continuation, an executable exit or a substitute for the
missing pinned actor, quiescence proof, authenticated health or trusted head.

Rollback entry is a downgrade and must require the same trusted external head,
exclusive installation ownership and pinned old-object identity/hash as a
forward update. A future entry API must also take a verifier-constructed typed
reason: authenticated new-Controller failure, expiration established by the
deadline monitor, or human abort through the protected authorization channel.
Record the verified reason with the successor revision, but never let a bare
receipt field or its deserialization construct that authorization. Neither the
reason verifier nor that channel exists; the current entry guard validates no
downgrade reason and cannot authorize a downgrade.
The deadline owner must be the independent pinned rollback actor or its
supervisor, not the Controller process whose health it judges: a dead or hung
Controller cannot reliably run its own deadline monitor. The constructed reason
must bind the job, the relevant revision and the time source used. This names
the required actor role, not an existing executable or implemented clock proof.

The continuation graph below now has a private `cfg(test)` structural model;
no row authorizes execution. Both claims must match the same fresh current observation
for intent/handoff edges and the real post-operation observation for result
edges. The other image remains checked in its required unchanged layout.

| Proposed edge | Required observation and ordering |
| --- | --- |
| RL/RC MoveNewAsideIntent -> NewAside | Fresh post-move gap; verify new image moved aside before recording result |
| RL/RC NewAside -> RestoreOldIntent | Same current gap; publish/read back restoration intent before restoring old |
| RL/RC RestoreOldIntent -> Restored | Fresh post-restoration original layout; verify old identity/hash before recording result |
| RL Original/Restored -> RC MoveNewAsideIntent | CLI original/restored and Controller still published; publish/read back Controller intent before its move |
| RC Original/Restored -> RolledBack | Both images original/restored; verifier-constructed old-Controller health evidence required before recording terminal state |

The continuation preserves `new_health_recorded` as a historical statement
about the replaced Controller. It never means the restored Controller is
currently healthy. `final_health_recorded` stays false throughout rollback;
`old_health_recorded` becomes true only on the final edge to RolledBack after
fresh verified old-Controller health. The broad layout classifier can accept
some RC claims with old health already set; the future graph must explicitly
diagnose-and-stop on those claims rather than treating them as a shortcut.
Complete and RolledBack are terminal. The continuation test enumerates all seven
phases, both nine-step sets, all three physical layouts per image and eight
health-claim combinations. Each classifier-consistent tuple must match at most
one independent continuation-table row or an explicit diagnostic stop; every
row must be exercised. The stop inventory is independently closed: final/old
health already set, non-rollback phases, terminal Complete/RolledBack, RL
Published, and RC MoveOldIntent/OldMoved/PublishIntent/Published. The test
requires row absence to equal membership in that explicit inventory, and every
stop class must occur. A missing edge cannot hide as an unclassified stop even
if both the implementation and the edge table omit it.
An intent still awaiting its result has a structural
next claim but cannot record it until a real post-operation observation also
satisfies the successor claim. Record-guard traces check all ordered pairs,
health changes and uncertain slots for six branches: the full nine-state
RL/RC/terminal chain, an untouched CLI handoff, direct CLI gap restoration,
direct Controller gap restoration, and no-move Controller exits with the CLI
untouched or already restored. Every branch is exercised with historical new
health both false and true, and every claim is independently layout-checked.
The older exit-inventory test remains conservative and classifies rollback
phases as diagnosis only; it is not the continuation selector. No executable
adapter uses either model. Verified reason custody and typed old-health
evidence remain separate gates; these synthetic record-pair tests do not prove
a durable multi-revision publication sequence or authentic process health.
A separate chain test serializes all six branches with increasing revisions
and advances a synthetic in-memory exact-byte head only after each accepted
edge. At every revision it rejects replayed predecessors, repeated/skipped
revisions, a valid rewritten predecessor and a changed pinned old-image hash;
the terminal head rejects another revision of RolledBack. This exercises
cross-revision guard composition, not durable head storage, CAS, restart
ownership, real publication or protection against rewriting the head itself.

After restoration, starting and verifying the old Controller is an operation
outside this record graph. A crash or restart between Restored and RolledBack
leaves the durable state Restored with the original layout; it does not imply
health success. Old-health evidence must bind the restored image hash and the
start time of the process actually started from that restored object, excluding
any process predating restoration. No cached pre-restore observation or other
evidence may authorize RolledBack. The no-move Original exit likewise requires
fresh health of the verified old-image process; it cannot borrow historical
new-Controller health.

RolledBack must carry or reference the verified entry reason from the entry
revision, preserving its chain of custody rather than inventing a new reason
at completion. It is terminal for that job ID. A later attempt requires a new
job; the rolled-back job must never automatically retry the same candidate.
The private inert record now preserves the reason reference through RolledBack;
the evidence verifier and authenticated health API remain unimplemented gates.

#### Rollback-reason lifecycle (private inert model only)

The following is the target draft lifecycle and immutability contract. The
required `rollback_reason` key is explicitly null
in Prepared, Activating, CoreHealthy and Complete; omission is not equivalent
to null. RollbackCli, RollbackController and RolledBack require a strict object
with exactly these fields; duplicate, missing and unknown fields are refused:

| Field | Draft constraint |
| --- | --- |
| `kind` | Exactly one of the six kinds in the entry table below |
| `entry_revision` | Integer greater than one and no greater than the enclosing revision |
| `predecessor_receipt_sha256` | Lowercase 64-hex digest of the exact receipt envelope preceding entry |
| `evidence_sha256` | Lowercase 64-hex reference to independently verified reason evidence |

The private `cfg(test)` schema requires the key, parses null or the strict
six-kind object, and bounds its revision/digests. Forward phases require null;
rollback phases including RolledBack require the object. The partial successor
guard permits creation only at the candidate revision with the exact prior
head digest; the entry graph also checks kind/phase/step compatibility.
Continuations preserve the entire reference. Tests reject missing keys, wrong
phase presence, clearing/replacing each reference field and otherwise valid
entry references with stale revision or a different predecessor digest. Four
synthetic branches run from a forward predecessor through rollback entry to
RolledBack with the same reference; six standalone rollback histories test
preservation and field-mutation refusal through their terminal edges. These
advance a synthetic in-memory head only after successful checks. Intermediate
rollback fixtures are not relabeled as forward entry points. This implements
claim lifecycle checks, not reason authorization, a durable head or a verifier
capability.

On entry only, null may become an object, with `entry_revision` equal to the
candidate revision and `predecessor_receipt_sha256` equal to the trusted prior
head digest. On every continuation edge, including RolledBack, the entire
object is immutable. Clearing it, replacing its kind/digests/revision or
creating it during continuation is refused. The exhaustive `DraftFields`
destructure must explicitly classify this field: the partial successor guard
may allow only null-to-object or exact preservation; the entry and continuation
graphs must respectively enforce creation and preservation, so the partial
guard alone never accepts an entry. Forward edges require null throughout.

The entry guard must also enforce this closed kind/phase/step table against the
verified predecessor claim. All rows remain subject to the independent layout,
ownership and evidence gates; a matching label alone proves no failure.
The private test-only entry guard uses a compatibility helper enforcing the table
against a classifier-consistent predecessor and both synthetic layouts. Its
test crosses all seven phases, both nine-step sets, three layouts per image,
all eight health claims and all six kinds against an explicit row inventory;
each kind must have accepted and refused cases. Unknown/unreadable slots are
also refused. Separate entry tests cross all six kinds with the canonical
forward trace and an explicit index inventory, independently of that helper.
Successful label compatibility is not a verified reason capability.

| Kind | Allowed predecessor phase and moving step | Required evidence meaning |
| --- | --- | --- |
| `controller_activation_failed` | Activating, Controller MoveOldIntent/PublishIntent | Checked Controller rename failed with unchanged layout; post-move uncertainty diagnoses and stops |
| `new_controller_unhealthy` | Activating, Controller Published | The started new Controller failed its authenticated health check |
| `cli_activation_failed` | CoreHealthy, CLI MoveOldIntent/PublishIntent | Checked CLI rename failed with unchanged layout; post-move uncertainty diagnoses and stops |
| `final_health_failed` | CoreHealthy, CLI Published | Final post-activation health failed before Complete |
| `deadline_exceeded` | Prepared, Activating or CoreHealthy, any classifier-consistent step | Independent monitor established expiration for the bound job/revision/time context |
| `human_abort` | Prepared, Activating or CoreHealthy, any classifier-consistent step | Protected human channel authorized this bound abort |

No kind is accepted from Complete or any rollback phase. Prepared only permits
Original steps and has not attempted image activation, so activation/health
failure kinds are refused there; only deadline or human abort can enter from
Prepared. A CLI at Original likewise cannot claim that a CLI activation
operation already failed. The Controller failure kinds are distinct so that
rename/verification failures need not be falsely labeled process illness.

The serialized object is an untrusted claim, not a verifier-constructed reason.
Neither digest reference is an authentication mechanism. A future verifier
must independently obtain and authenticate the referenced evidence, then bind
its kind, workspace/server/job, entry revision, predecessor digest and pinned
old/new objects before constructing a non-deserializable reason capability.
An unhealthy-process reason also binds the observed process identity/start;
a deadline reason binds the independent monitor and its time source/context;
a human reason requires the protected channel that does not yet exist. Missing
or unavailable evidence must stop the update, not fall back to trusting JSON.
The evidence store, authentication scheme and capability API need separate
design review; this table creates none of them and adds no public format.

The inert format tests cover required-null versus missing, strict-object
refusals, entry revision/head binding, each reason-field mutation independently,
and reason preservation through RolledBack. Every kind has matching and
mismatched phase/step cases, including Prepared/Original, pre-final CLI steps,
Complete and existing rollback phases. Rollback fixtures carry explicit reason
references; there is no permissive default or compatibility claim for an
installed format. A subsequent verifier model must demonstrate that valid
JSON and digest references alone cannot construct a verified reason.

#### Proposed independent reason-verifier boundary (design only)

This is the next inert API contract for review, not a native evidence format,
an implemented source of authority or permission to enable rollback. The
existing parser and graph return structural acceptance only. A future adapter
must separately require a verifier-produced reason before entering rollback.

The caller supplies a locally constructed entry context from its owned attempt:
workspace/server/job, candidate entry revision, exact predecessor receipt digest,
both pinned old/new image records, and the independently checked predecessor
state/current layouts. Do not construct this context from the candidate reason
reference. Recheck the expected head and exclusive ownership at the mutation
boundary; constructing a reason must neither advance the head nor extend a
deadline. The current synthetic ExpectedHead has no protected durable source.

The proposed verifier performs these steps in order:

1. Check the reference against the entry context, including exact revision,
   predecessor digest and the existing kind/phase/step table. Refuse terminal
   and rollback predecessors, unknown layouts and mismatched pinned objects.
2. Obtain evidence through a separately supplied trusted source. The source
   selects one fixed attempt-local evidence slot; the receipt contains no path,
   URL, executable or source selector. An inert test source can return a typed
   synthetic observation or an error, but its successful return proves no
   native authentication. Production source construction remains unavailable
   until actor/process ownership and the observation channel are reviewed.
3. Independently bind the observation to the same workspace/server/job,
   entry revision, exact predecessor digest, reason kind and complete pinned
   image set. Compare the bounded evidence bytes with evidence_sha256, but do
   not treat that digest match as source authentication. A source from another
   attempt with identical artifact hashes is still a mismatch.
4. Check kind-specific semantics below. Only then construct a private,
   non-deserializable verified-reason value. It must have no public field
   constructor, Default or conversion from ReasonReference. Passing a JSON
   parser result to the entry graph cannot substitute for this value.

| Kind family | Required independent observation |
| --- | --- |
| Controller/CLI activation failure | An attempted operation on the selected pinned image and fixed slot, with its intent revision and a fixed failure stage; caller-supplied error text or an absent success flag is insufficient |
| New Controller/final health failure | A completed failed health evaluation bound to the process handle identity, start instance, intended pinned image and required health predicates; a missing reply alone requires an independently established deadline instead |
| Deadline exceeded | The independent supervisor/rollback actor's original bounded budget and a coherent monotonic sample in that same live monitor instance; Controller timestamps, wall-clock arithmetic and a restarted monitor do not renew or reconstruct it |
| Human abort | Explicit authorization for this attempt and predecessor through a protected human channel; no such channel exists, so a native adapter must refuse this kind |

The monitor must be independent of the Controller being evaluated. This live
monitor contract makes no boot-identity or cross-reboot claim and adds no
registry, reboot experiment or privileged initialization. After monitor loss,
unknown ownership or restart, preserve evidence and diagnose; do not create a
fresh timeout budget and continue. A valid reason proves the reason only:
rollback still requires a pinned independent actor, exclusive installation
ownership, quiescence, checked file observations and a trusted exact head.
Old-Controller health after restoration remains a separate verifier result.

Before publishing the entry receipt/marker, any retained evidence it references
must be stored and read back through a bounded private checked handle. A failed
write, sync, readback, source acquisition or authentication stops the attempt
without fallback, automatic retry or a different kind. An interrupted evidence
write cannot become success by reparsing the receipt. This proposes ordering
only; evidence serialization, storage slot, authentication and native acquisition
remain unimplemented and need review before an adapter consumes them. The
source must never accept arbitrary receipt-provided bytes as authenticated input.

The next test-only verifier increment should independently mutate every bound
context field, source kind and process/monitor instance; refuse absent, failed,
oversized and hash-mismatched evidence; reject a well-formed receipt with no
independent observation; and exercise every kind's success/refusal semantics.
Tests must also show that verification neither mutates records/head nor grants
ownership, and that a reference preserved to RolledBack is not automatically
reverified as a new rollback entry. Those tests would establish synthetic
composition only, without a production verifier constructor or real I/O.

The private `cfg(test)` child module `windows_update_record::reason` now
exercises that synthetic composition. Its evidence source has one fixed slot,
an 8 KiB limit, exact declared-length reads, strict typed events and no path or
free-text diagnostic field. A verified value borrows its exact live context,
has no Clone/serde/Default implementation, and is moved into the inert entry
check. Entry consumption latches the attempt so that another value verified
before consumption cannot enter again. Lost liveness or a replacement context
refuses even when all serialized fields and predecessor bytes are identical.
The latch represents in-memory consumption only, not durable publication or
restart protection. Synthetic numeric process/ownership/monitor identities
are not OS handles or authenticated ownership proof.

Health-failure observations compare handle identity, start instance and the
running-image hash against the separately supplied context and intended new
Controller hash. Failure stages and health predicates use fixed vocabularies;
argv, environment, tokens, credentials and file contents are not diagnostic
fields. A future native source must acquire these identities and completed
failure observations independently; the synthetic constructor authenticates
nothing. Human abort remains refused. Deadline observations use the original
context budget without renewal, and source failures never select a fallback.
Operation failures compare the independently supplied intent revision against
the intent-state predecessor's own revision. Only `move_old` at
`move_old_intent` and `publish` at `publish_intent` are accepted. A result-state
predecessor (`old_moved` or `published`) cannot turn a failed postcheck into an
operation-failure reason. The former `verify_image` stage is refused by schema;
relabeling it as either remaining stage also refuses. Post-success uncertainty
must diagnose and stop rather than authorize rollback.

The inert health-failure vocabulary uses this explicit requirement table:

| Failed predicate | `new_controller_unhealthy` | `final_health_failed` |
| --- | --- | --- |
| Intended Controller build, Proxy connection, unchanged Host, live Agent tuples | Required | Required |
| Installed CLI image hash | Not required; refuse this reason | Required after CLI publication |

The latter predicate refers to the CLI new-image record already bound in the
complete image set; the failed evaluation still binds the same live Controller
handle/start/hash. The future native health source must actually evaluate the
required predicate, rather than accept a caller label as failure evidence.
These are synthetic failure observations, not implemented native health checks.

Consumption must occur before any durable entry publication. A publication
failure after consumption keeps the attempt consumed and ends in
diagnose-and-stop: preserve the old head and evidence, with no retry, replacement
reason or reconstructed attempt. The synthetic regression injects that failure
after entry acceptance and proves that both an already verified second value
and a new verification still refuse. This establishes latch ordering only; no
native publication or durable restart latch is exercised.

A native source must enforce the 8 KiB limit while reading the private handle
bound to the attempt, reading at most limit plus one and refusing before larger
allocation. Hash comparison, schema decoding and binding must use those same
single-read bytes. The test-only portable transport now enforces a 1..8192
declared length before reading, allocates exactly declared length plus one,
and reads into that fixed buffer without growth. EOF at the declared length
is mandatory; truncation, the extra byte and any I/O error (including after
all declared bytes but before EOF) refuse without returning partial evidence.
The synthetic source feeds this transport from an already allocated fixture
vector; digest, parse and binding use its one returned buffer. Streaming tests
cover chunked input, endless input, exact cap and failure at each read boundary.
This establishes portable transport bounds only, not private-handle identity,
attempt custody, native source acquisition or a bound on fixture construction.

#### Next native fixture: fixed private evidence slot (proposed)

Implement this below `windows_update_bootstrap::checked_files`, confined to
the existing disposable `Attempt` fixture on local NTFS. Do not reuse
`replace_record`: its retained readers deliberately permit delete sharing for
replacement. Reason evidence instead needs one immutable, exclusively created
slot and a retained read handle that denies writes and deletion. No installation
path, Controller caller, reason verifier constructor or rollback actor is added.

The proposed private API is `Attempt::store_evidence(bytes, expected_digest,
fault) -> EvidencePin`. The fixed filename is `reason.evidence`; there is no
caller filename, source selector, overwrite or replacement mode. The input is
opaque fixture bytes, not an authenticated observation. Reject empty or larger
than 8192 bytes and a digest mismatch before creating anything. A successful
result borrows its live `Attempt`, retains the read handle and verified identity,
and exposes neither Clone/serde nor an owned raw handle. It is storage custody
only: no conversion to the Controller's verified-reason value is provided.

1. Recheck the retained attempt directory's protected owner/SYSTEM-only ACL,
   current path identity and local NTFS boundary, retaining its ancestor pins.
   Derive the one fixed slot from that pinned directory, never receipt data.
2. Create with `CREATE_NEW`, a protected owner/SYSTEM-only descriptor present
   at creation, non-inheritable handle and OPEN_REPARSE_POINT. Reject an
   existing file, link, directory or uncertain lookup without repair or removal.
   Retain read/write access only for this preparation phase and allow at most
   read sharing. Write the exact bounded bytes, sync, and read back through this
   same handle. Verify regular-file attributes, single link, volume/file ID,
   private ACL, exact size and expected hash. A failed operation leaves the
   attempt unusable and preserves whatever evidence was created.
3. Record the verified identity, close the writer, then reopen read-only with
   FILE_SHARE_READ only. Recheck directory binding, final fixed path, identity,
   single link, attributes and ACL. Read at most observed length plus one into
   a fixed allocation (maximum 8193), require exact EOF and compare bytes/hash.
   Recheck identity/size/ACL after reading. A replaced object, open writer,
   changed bytes, sharing conflict or failed readback refuses the result.
   The close/reopen gap is not an atomic handle downgrade: identity and content
   checks detect substitution, and the final share mode excludes surviving
   writers/deleters. It is not isolation from a malicious same-user process
   capable of changing ACLs or injecting into the owner.
4. Return the retained read pin only after all checks. A second store always
   refuses, including identical bytes. An error never retries, deletes, chooses
   another slot, relaxes sharing or claims durable publication. Loss of the
   `Attempt` or read pin ends custody; reopening after restart cannot recreate
   it. The caller must retain the pin through the entry publication lifetime.

Use fixed fault stages after creation, after sync and after writer close to
test preservation and no reusable result. A narrow readback seam may inject
I/O/truncation failure; report it as injection rather than an observed OS
failure. Fixture success proves sync/readback behavior on the tested OS, not
power-loss durability, protected head storage or an authenticated failure.

Hosted acceptance must run the exact SHA on Server 2022 and 2025: ordinary
tests plus explicit native evidence tests for the 1-byte/8192-byte boundaries,
zero/8193 refusal before creation, wrong digest, duplicate store, restrictive
ACL/non-inheritance, write/delete/replacement denial while pinned, preserved
bytes after injected failures and refusal to retry. Attempt-local fixtures may
also inject a replacement during the writer-close seam and must observe
identity/content rejection. Cleanup is explicit test-owned cleanup after all
handles close; error paths never remove retained evidence themselves.

This proposal needs security review before the native fixture is implemented.
It does not enable W2/W3 transaction publication or W4 activation; authenticated
observation acquisition, external head, durable consumption and health/actor
ownership remain separate contracts. A real win_native installation run still
requires the leader's coordination at its approved milestone.

The first fixture implementation is private to `cfg(all(windows, test))` in
`checked_files::evidence_file`. Preparation requests only FILE_READ_DATA,
FILE_WRITE_DATA, FILE_READ_ATTRIBUTES, READ_CONTROL and SYNCHRONIZE; the final
pin requests only FILE_READ_DATA, FILE_READ_ATTRIBUTES and READ_CONTROL.
Neither requests DELETE, WRITE_DAC or WRITE_OWNER. FILE_SHARE_READ during
preparation prevents another opener from acquiring write/delete access before
writer close; the native tests check this both after creation and after sync,
then again while the final pin lives. The handle-inheritance bit is checked.

Only the pin API reads the slot; there is no public path reader, recovery
constructor or conversion into a reason source. A failed store returns no pin,
so a future verifier must receive a source error, never read a leftover file by
path. The live Attempt latches the first valid-input store invocation before
native I/O; later calls refuse even after a pre-creation failure. Invalid
length/digest inputs are rejected before this invocation and create nothing.
Three stage-fault tests retain the empty/complete slot and reject later stores;
a close-gap replacement test substitutes identical bytes with a different file
identity and requires refusal. Tests alone may inspect failed fixture files to
assert preservation; that does not establish a production evidence reader.

Retention remains a future operator workflow: after verified terminal state and
the minimum seven-day retention, the installation owner may explicitly invoke
a separately reviewed cleanup operation as that same OS user (or SYSTEM for a
SYSTEM-owned installation), verifying the exact attempt/slot and ownership
before removal. There is currently no cleanup command or timer. No store/read
error path, drop handler or ordinary update job may perform that cleanup;
uncertain/nonterminal jobs remain retained. Fixture tests remove only their
exact test-owned paths after dropping all pins. Native sharing, ACL, sync and
readback behavior still require exact-SHA hosted execution on both Windows
images; cross-target compilation is not evidence of runtime success.

For the future private evidence store, the proposed readers are the owning OS
user and SYSTEM only. Keep referenced evidence read-only through terminal state
and for at least seven days afterwards, then require an explicit reviewed
cleanup policy; uncertain/nonterminal attempts must retain it. Exhausted
retention quota refuses new attempts rather than evicting required evidence.
This is a retention proposal, not an implemented timer, quota, deletion or new
cleanup authority. Checked storage, sync/readback, authenticated acquisition,
durable single-use publication and production wiring remain separate gates.

#### Next acquisition slice: native operation outcome (proposed, test-only)

The hosted evidence-pin fixture is accepted as storage custody. Its bytes are
still caller-supplied: storing and hashing a claimed failure does not establish
that the failure occurred. The next slice should capture the result inside the
disposable checked rename fixture, before any evidence encoding. Keep it under
`cfg(all(windows, test))`, with no installed paths, Controller adapter, rollback
execution, head publication or recovery entry point.

Today `rename_checked` returns a fixed-stage `io::Error` and prints a numeric
Win32 error immediately after a failed `SetFileInformationByHandle`. Neither
that error's `PermissionDenied` kind nor the printed line is a reason source.
Do not parse diagnostics, accept a caller-supplied error code, or turn every
`Err` into an operation-failed observation. Introduce an internal outcome type
whose constructors remain inside the checked operation boundary:

| Captured outcome | Meaning | Eligible for a failure observation in this fixture |
| --- | --- | --- |
| Precondition refused | ACL, source binding, digest or preparation failed before the native rename | No; diagnose and stop |
| Native call failed | The one checked native rename returned failure; capture `GetLastError` immediately | Only after unchanged-layout verification below |
| Native call succeeded, postcheck refused | Rename returned success but object/path verification failed | No; state is uncertain, diagnose and stop |
| Verified success | Native rename and all postchecks succeeded | No failure observation |

The acquisition function borrows the same live disposable `Attempt` and retains
the checked source handle. Bind a private, non-Clone, non-serializable outcome
to that attempt, source object identity/hash and fixed from/to slots. The
operation selects the slots before calling Windows; no evidence file, receipt
or caller error label selects them afterwards. A local invocation latch must
be consumed before the native call; a second invocation on that acquisition
object refuses even if the first fails. This is in-memory fixture ordering,
not a durable attempt latch or proof of production installation ownership.
Derive the failure stage from the operation's selected slot transition and
step, never a supplied label. Represent the captured native status only as a
bounded unsigned integer, not an OS error string.

A failed native return alone does not prove that the intended old layout is
intact. Before minting a fixture observation, reobserve the retained source
identity, content and source slot, plus the destination's expected state using
the existing checked-slot rules. An unreadable destination is not absence.
Any change or uncertainty yields no observation and preserves all objects.
Hold the same retained handles and sharing restrictions through both the
native call and re-observation: deny source write/delete sharing and retain
the test-owned destination pin. Add an injected interference case between the
failed return and re-observation that must produce no observation; injection
must be explicit and must not weaken the ordinary acquisition path's pins.
For the deterministic negative fixture, hold a test-owned destination object
at the fixed target slot, attempt the non-replacing rename once, and prove
both objects remain the same after refusal. This deliberately blocked operation
is a native test result, not evidence of a real updater failure. Do not assume
one Win32 error number across OS versions; require native failure with a
captured nonzero status and verified unchanged objects.

Only the captured eligible result may produce bounded fixture evidence bytes
and pass them to the fixed `reason.evidence` store. The resulting composite
borrows the operation outcome and the evidence pin; it cannot be reconstructed
from file bytes, a digest, a numeric status or a reopened path. A storage error
returns no composite and never reruns the rename. Production job bindings,
intent revisions and predecessor-head authority are not supplied by this
fixture, so it must not construct the Controller's `VerifiedReason` or claim
an authenticated production rollback reason.

Acceptance on exact-SHA hosted Server 2022/2025 must cover verified success
without a failure value, an occupied-target native refusal with unchanged
identities/bytes, precondition refusal without entering the native call,
injected post-success verification refusal without a failure value, a second
acquisition refusal, and evidence-store failure without a retry or path-read
fallback. Report injected failures separately from observed Windows failures.
The existing evidence-pin, rename, observer and running-image fixtures remain
regressions. Security review precedes implementation of this new outcome
boundary. Durable consumption/CAS, actual operation-intent binding, health
authentication, actor ownership and W4 authorization remain separate gates.

The first implementation lives in the private Windows test-only
`checked_files::operation_outcome` module. The shared checked-rename fixture
now preserves a typed before-call/native-status/post-call distinction internally;
its older `io::Result` interface remains a fixture convenience, not an evidence
constructor. The acquisition borrows the attempt and two retained
`copy_for_rename` handles and snapshots their identities, sizes and digests.
It supports the deterministic occupied-target negative case only, deriving
move-old/publish stage from Active-to-Backup/Candidate-to-Active selection.
It does not implement an absent-destination failure acquisition or supply
production intent/head bindings. Successful native rename and injected
post-success refusal are tested directly at the internal capture boundary.

An eligible collision outcome borrows its acquisition; its evidence is a fixed
five-byte fixture encoding (one stage byte plus little-endian unsigned Win32
status), not a Controller reason-schema payload. A stored composite borrows
that outcome and retains its evidence pin; reading rechecks both image pins
and evidence. No byte/path constructor or verified-reason conversion exists.
Acquisition is consumed before revalidation/native I/O, and evidence storage
uses the attempt's existing single-invocation latch. Tests inject a move using
an already retained fixture DELETE handle between failure and re-observation:
the changed destination slot must yield uncertainty and no evidence. This
does not relax sharing or simulate an external opener bypassing it. A second
case injects creation-stage storage failure, retains the empty evidence slot
and proves neither storage nor acquisition can retry. Exact-SHA hosted Windows
execution is still required; cross-target lint is not runtime acceptance.

Hosted run 39320282022 verified exact
`dcdfe5e49808ce9160f86d7e1f050cfe846e4bb4` on Server 2022 build 20348 and
Server 2025 build 26100. Each image passed all three `native_operation_` tests
with zero failures/ignored tests; the actual occupied-target calls reported
nonzero status 183. Ordinary platform tests reported 61 passed and four ignored;
the separate outside-runner-Job owner round trip and running-image rename each
passed with exit zero. Core independently read the retained raw log (ticket
5873), and security accepted this fixture slice. This does not close W1-W3 or
establish a Controller reason source.

The following decoder-only increment parses that existing fixture format with
exact length, stage tags 1/2 and a nonzero unsigned little-endian status. The
stored reader compares the decoded pair with its retained native failure;
decoding alone creates no accepted evidence or authority. Its pure test covers
all 256 stage bytes, zero and boundary statuses, every truncated length and an
extra trailing byte. The test remains in the Windows-only fixture: compilation
on another target does not execute it. Independent context composition and
single-use consumption described below are not implemented by this decoder.

#### Proposed composition slice: independent context plus native custody

The next design boundary joins two still separate fixtures: the native
`operation_outcome::Stored` borrows a real failed rename and evidence pin, while
the Controller `windows_update_record::reason` verifier accepts only synthetic
JSON observations and numeric ownership/monitor stand-ins. Neither can simply
be cast into the other. The five-byte native encoding has no job, predecessor,
image-role or intent-revision binding; the synthetic JSON has no native custody.
No digest, diagnostic line, caller error code or successfully parsed record may
bridge that authority gap.

Keep the next implementation private and test-only. First exercise composition
inside the native fixture, without exporting its handles/types or constructing
the Controller's `VerifiedReason`. Supply a separately created immutable fixture
context before acquisition: attempt identity, image role, operation-intent
revision and expected source/destination object identities, sizes and digests.
Synthetic job/predecessor labels, if included, remain explicitly synthetic and
must be independently supplied rather than reconstructed from evidence bytes.
The context is not an installation lock, durable head or production owner proof.

Acquisition must check that independent image expectation against its retained
objects before consuming the operation, derive the stage from selected slots,
and retain the context and both image handles through capture, storage and
verification. Do not learn the expected destination digest from whichever file
is found there and then call the comparison an independent role binding. The
existing deterministic occupied destination remains a negative fixture only;
it does not prove the production classifier permits a rollback from that layout.

The first consumer should parse the existing exact five-byte format strictly:
only stage tags 1/2, exactly four little-endian status bytes and nonzero status;
reject truncation, trailing data, unknown tags and zero. Compare the decoded
stage/status with the retained actual failure and the independently selected
context. Read only through the existing fixed-slot evidence pin, rechecking
the attempt and image custody before and after the read. A malformed, replaced,
unreadable or mismatching record yields no accepted fixture value. This parser
is not the eventual Controller observation schema; adding native status to that
schema and defining a production adapter require a separate reviewed change.

Any accepted composition value must borrow this exact context, acquisition and
pin, have no public/byte/path constructor or Clone/serde implementation, and be
consumed once by a private fixture sink. Consume the verification invocation
before reading evidence; a read/refusal/sink failure spends it without retry or
reopening. The sink only records test acceptance in memory: it cannot publish a
head, rename an image, execute rollback or reconstruct authority after restart.
The original acquisition and evidence-store latches remain independent; a new
consumer must not renew either of them.

Acceptance must include independently changed expected identity/hash/size,
wrong role/stage/revision/context, distinct old/new fixture bytes, and an
identical-content replacement with a different identity; each refuses without
minting a value. Cover malformed five-byte inputs using a pure parser test,
but never use that parser's success alone as native provenance. Native tests
must cover context mismatch before any operation, retained-handle interference
after storage, the single read/consume path, failure without retry and no
cross-attempt substitution. Run exact-SHA Server 2022/2025 with all prior native
regressions. No installed paths, production schema migration, health evidence,
durable once-only publication, recovery channel or W4 action is included.

The private native fixture now implements this composition in
`operation_outcome/composition.rs`. Its disposable images have distinct fixed
bytes; their expected hashes and lengths come from those fixture inputs, and
their expected identities are retained when the checked copies are created,
before acquisition. Context and intent equality, attempt identity and both
image expectations are checked before constructing the acquisition. Verification
borrows that exact composition and stored failure, reads only its evidence pin,
and checks custody again after reading. A latch on the stored evidence also
prevents a newly constructed verifier from accepting it twice. The accepted
value can only be moved into a private in-memory counter sink; refusal does not
restore any latch. Three new native tests cover expectation mutations, both
operation roles, distinct bytes, cross-attempt substitution, identical bytes
with a different identity, post-read interference, and spent verification or
sink failures. Windows target lint is compile evidence only: exact-SHA hosted
2022/2025 execution remains required before calling this increment accepted.
This is still not a Controller reason adapter, durable consumption or W2/W4
authorization. Synthetic job/head/owner evidence has not been added.

#### Next native slice: context-bearing evidence, without restart authority

Hosted run 39328765493 exercised exact `6def7e8f` on Server 2022 and 2025:
each reported seven native-operation tests passed, 65 ordinary library tests
passed with four ignored, and the prior separate owner/running-image fixtures
passed. Core read the retained raw log (5988); security accepted the inert slice
(5989). This closes fixture composition only. Security5976 identifies three
production gaps: pointer equality does not survive restart, `Cell` is not a
durable consumption latch, and expectations must originate in a previously
committed record rather than in the operation being checked.

The next increment should address the first gap as a **private native test
format**, leaving the existing five-byte fixture and the Unix record unchanged.
It must not construct a Controller reason or claim to close the other two gaps.
Use a separately versioned fixture record in its own disposable attempt and the
existing fixed evidence slot, with a strict bounded encoder and decoder, not an
optional extension of the old five bytes. The record binds:

- A domain/version tag and exact total length, with no trailing bytes, alternate
  representations or unsupported tags accepted.
- An independently supplied attempt identifier and predecessor-record digest;
  for the disposable test these are explicitly synthetic committed-record
  inputs, fixed before acquisition. They are not generated from outcome bytes.
- A component selector (`Controller` or `Cli`) independently of the image-version
  role (`Old` or `New`), the selected native operation, and its nonzero committed
  intent revision. The current fixture's Old/New role is not a component binding.
- Expected source and destination volume/file identity, byte length and digest,
  plus the actual immediately captured nonzero Win32 failure status. Expected
  object records come from the independent context, never a fresh observation
  relabelled as an expectation.

Write those fields only from the retained acquisition and its independently
validated context. Keep the real failure object, protected attempt directory,
both image handles and read-back evidence pin alive through verification. Read
the bounded fixed-slot record and check custody before and after the read.
Decode into inert data, compare every binding with an independently supplied
expected context, then compare operation/status with the actual retained native
failure. A matching digest or successful parse alone must never mint acceptance.
The byte-format binding is not a MAC, a secret, a file-lock proof or protection
against a same-UID writer; its trust still depends on protected record custody
and on an independently trusted predecessor. Arbitrary caller-provided context
does not become trusted by being hashed or serialized.

Acceptance should rebuild an equal expected context as a distinct allocation
(no pointer equality for serialized binding comparisons), reject identical
failure bytes from a different attempt, predecessor, component, revision or
image identity, and exercise every field mutation, truncated/extra input,
unsupported version, zero status and post-store interference. Keep the existing
single-use in-memory verifier/store latches and test that failures cannot retry.
Passing these tests proves live native custody plus explicit byte bindings, not
that a reconstructed context or pin after process death is authoritative.

Before any Controller adapter, a separate increment must obtain the predecessor
from the validated committed receipt/head under the supervisor-owned lock,
bind the actual attempt directory and intent to it, and implement durable
consume-plus-CAS ordering. A crash after native failure, after evidence sync,
after acceptance or before head publication must remain diagnose-and-stop until
that ordering is implemented and tested. Never automatically reconstruct a
failure capability from bytes on restart. Recovery stays disabled; this plan
adds no health proof, rollback actor, installation mutation or W4 authority.

The first implementation increment is the private test-only
`operation_outcome/context_record.rs` codec, **not native binding yet**. It uses
225 bytes in fixed order: `TRCTX001`, u16 little-endian total length, 32-byte
attempt identifier, `PREDE001`, 32-byte predecessor digest, one byte each for
component/role/operation, u64 intent revision, source then target image records,
and u32 status. Each image is u64 volume serial, all 16 file-id bytes, u64 length,
and 32-byte digest. Integer fields use little endian; identifiers and digests
are exact opaque byte arrays. Zero/all-ones volume or file identity is refused;
attempt/predecessor sentinels, zero/oversized images, identical object identities,
unsupported selectors, inconsistent role/operation, zero revision/status and
non-exact framing are refused too. Byte equality is allocation-independent.
Tests cover every-byte binding mutation, equal-width attempt/predecessor swaps,
truncation/trailing bytes, invalid fields and mutual old/new decoder refusal.
No old decoder or Unix bytes changed. The field values in these tests are
synthetic, including status: this does **not** prove actual-error acquisition.
The next native increment must obtain `FILE_ID_INFO` from retained handles
(the existing 64-bit file-index tuple is insufficient), fix independent expected
identities before acquisition, and implement the pinned store/read comparison
against the actual captured failure. Cross-operation status substitution and
post-store interference remain native test requirements, as do all custody and
durable-publication conditions above. Hosted execution of this codec increment
is pending; Windows cross-target lint alone is compilation evidence.

The following private `composition/native_binding.rs` increment connects that
codec to live disposable collision acquisitions. It obtains the u64 volume
serial and all 128 file-id bits using `GetFileInformationByHandleEx(FileIdInfo)`
on retained image and attempt-directory handles, refusing zero/all-ones values.
It fixes image identities and independent expected length/digest before
acquisition. The synthetic attempt identifier is SHA-256 over the explicit
`test-only-native-attempt-v1` domain, volume and directory file id; the synthetic
predecessor is a domain-separated fixed fixture-head digest. These constructors
exist only under the Windows test module, not in a production adapter.
The expected binding has no caller-provided status: a zero placeholder must
remain zero, and the serialized status comes solely from the retained actual
Failure. For this occupied-slot fixture only, that status must be
`ERROR_ALREADY_EXISTS`; this is not a production operation error allowlist.
The store retains the exact composition/failure and the read-back evidence pin.
Consumption compares a separately allocated expected binding, populated with
the retained actual status, and rechecks old custody plus full file ids before
and after the read. Every verification attempt spends its private in-memory
latch, including read-hook refusal, context mismatch and sink refusal.

Three tests exercise both operations, Controller/Cli selectors, context
substitution, foreign failures, coincident statuses from distinct operations,
wrong-status bytes, attempted external evidence write/delete, retained-handle
rename after read, and non-renewable store/acquisition/consumption. The read-hook
error is synthetic, not an actual ReadFile failure. Both real occupied-slot
operations produce the same error; no distinct-error native fixture is claimed.
The final sink is still just an in-memory test counter. No native state is
reconstructed after process death; committed-head acquisition, durable
consume/CAS, production health, rollback actor and W4 remain absent. Exact-SHA
hosted 2022/2025 execution is required before accepting this native increment.

Hosted run 39333496173 exercised exact `7ce17fb1` on both Server 2022/2025:
native-operation filter 13 passed, ordinary platform library 71 passed with four
ignored, previous regressions and separate CIM owner/running-image tests passed.
Core read the raw log (6034); security accepted this inert fixture (6030).

The next test adds a genuine distinct-error negative case without broadening
acceptance. It duplicates the verified source handle with `FILE_GENERIC_READ`
only, retaining the same full file id but removing DELETE rights before fixing
the expected context and constructing acquisition. The native rename must then
return `ERROR_ACCESS_DENIED` for both operation roles. A separate ordinary
collision acquisition must return `ERROR_ALREADY_EXISTS`. Substitution of the
actual permission-failure status or the foreign failure object into the
collision evidence is refused; the collision itself still consumes once. The
permission failure cannot pass the collision-only binding policy and creates no
evidence slot, but its acquisition is spent. Exact-SHA hosted execution remains
required to establish the native error ordering on both supported images; cross
compilation alone is not proof. This closes no actual ReadFile failure coverage,
production error classification, committed-head binding or durable consume/CAS
gap, and does not add Controller/W4 authority.

For a future adapter, diagnose-and-stop must latch a fixed reason and refuse
all new update jobs, including after restart. Clearing that latch requires a
protected human-authorization channel, which does not exist yet. No manual
rename sequence is supported; preserved records are diagnostic evidence, not
recovery input. Recovery remains disabled. Neither the latch nor an operator
clearing mechanism is implemented by the current inert model.

### Next inert fixture: distinguish read failure from positioning refusal

The remaining read-error gap concerns the retained evidence pin, not rename
status acquisition. The existing post-read hook cannot establish that the OS
refused a file read. Reducing the pin to metadata-only access is also insufficient
by itself: the current reader seeks before reading, and that positioning call
may fail first. A failed overall consume must not be labelled a `ReadFile`
failure without observing which native operation actually failed.

Keep the next fixture private under the Windows test module. Preserve the
original pin until a reduced-rights duplicate exists; establish that both handles
refer to the same full volume/file identity. Retain directory custody, ACL and
metadata validation. The negative case must capture the read operation's actual
error immediately, separately from any seek error, and prove that the read was
attempted. A deterministic implementation may use a bounded explicit-offset
native read to avoid requiring an earlier positioning operation; it must retain
the existing length-plus-one bound, exact-length/hash comparison and post-read
identity checks. Do not bypass those checks merely to reach the injected fault.

For both operation roles, create genuine collision evidence first, then exercise
the reduced-rights pin through the same verifier read path. Require zero sink
acceptances, a spent consumption latch, and refusal of a second consume even if
the original readable handle is restored. Include a normal readable-pin control
using the same reader. Diagnostic output may contain only fixed stage names and
numeric native errors, never evidence bytes or private paths. Hosted 2022 and
2025 execution must show the read-stage failure and successful control on the
exact commit; a seek/ACL/identity refusal does not satisfy this experiment.

The contract checkpoint alone added no reader changes or native tests.
Actual read-failure coverage, production
committed-head binding and durable consume/CAS remain open; W4 stays disabled.

The subsequent inert implementation uses one synchronous `ReadFile` reader with
an explicit offset for preparation readback, pin readback and verification. The
first read must return the exact observed length; short or zero reads refuse.
A separate one-byte probe at that length must establish EOF, followed by the
existing metadata/identity recheck and digest comparison. Allocation remains
bounded to length plus one, at most 8193 bytes. `Win32_System_IO` is enabled only
as a Windows dev dependency for the `OVERLAPPED` offset structure; the entire
bootstrap fixture remains absent from production builds.

The new composed fixture runs both operation roles. Its readable control
asserts successful prevalidation, two native read calls, postvalidation and hash
comparison. It then duplicates the retained evidence handle with only
`FILE_READ_ATTRIBUTES | READ_CONTROL`, proves the same full file identity, and
replaces the pin handle while preserving the original. Failed consumption must
report successful ACL/identity/length prevalidation, exactly one read call and
actual `ERROR_ACCESS_DENIED`, with no postvalidation or hash comparison and zero
sink acceptance. Fixed `Read` and `ReadControl` stage diagnostics expose no bytes
or paths. Restoring the original must reproduce the unchanged evidence bytes
through the same checked reader, but cannot renew consumption or evidence-store
authority. Compilation is not native execution: exact-SHA hosted 2022/2025
results and security acceptance remain required for this new coverage. This
adds no durable latch, production reason adapter, restart recovery or W4 caller.

### Next binding boundary: retained committed head under fixture ownership

The read-failure fixture passed exact `5a99ade8` on hosted Server 2022 and 2025
(run 39340099443). Both emitted the real `Read win32=5` and readable-control
lines, with 15 filtered tests and 73 ordinary tests passed, four ignored. Core
and security read and accepted the raw evidence (6158 and 6157). This closes the
read-error fixture only; the current predecessor remains a fixed synthetic hash.

The next increment must obtain expectations from an independently published,
retained record before acquiring an operation. Keep it wholly inside a
disposable native fixture. First specify and test ownership: a fixed protected
lock file in the pinned fixture directory, exclusively opened without write or
delete sharing, retained for the whole head/acquisition/evidence/consume
sequence. Verify its ACL, single-link regular-file type, full file identity and
directory binding; a competing opener or changed directory must refuse. A
process-local `Cell`, a lock pathname or a digest cannot substitute for this
retained object. This fixture lock is not the production installation lock, Host
ownership or attested quiescence, and must never be exposed as such.

Only while holding that fixture ownership may a writer publish the initial
head through the existing fixed record replacement primitive. Use a separately
versioned, strictly bounded head encoding with no error-status field: operation
failure status cannot already exist in the predecessor. Its committed fields
must bind the actual attempt directory, component, image role, operation,
nonzero intent revision and full expected image identities, lengths and digests.
These values originate in the independent fixture setup before the attempted
operation; do not learn expectations from failure evidence or whichever current
image is found. Fix the exact encoding before implementing it, with golden
bytes and refusal of every truncation, trailing bytes and unsupported fields.

After publication, retain the exact read-back head handle and its full file id.
The context predecessor is the digest of these validated committed bytes, not
the current fixed label. Revalidate the fixed head name against that retained
object, bytes, ACL and directory before acquisition and before/after evidence
consumption. A same-byte replacement with a different identity, changed revision
or component, a foreign attempt, lost lock custody or a head advance must refuse
without renewing acquisition or consumption. No reopen may reconstruct authority
after process death. Tests must distinguish live fixture ownership and pinned
head checks from an atomic compare-and-swap: the existing replacement helper is
explicitly not a CAS against a non-cooperating writer.

This checkpoint specifies the boundary only; no lock, head codec or adapter is
implemented by it. Native lock exclusion, head codec and composed acquisition
need separate review and hosted evidence. Durable consume-plus-head publication,
crash ordering, real supervisor-owned installation locking and W2/W3 transaction
assembly remain open. W4 and recovery remain disabled.

The first lock-only implementation adds a private `checked_files::ownership`
fixture. It exclusively creates or opens the fixed `fixture.lock` with zero
sharing and a non-inheritable handle in the retained attempt directory. It
checks the protected owner/SYSTEM ACL, single-link regular-file attributes,
full volume/128-bit identity and final-path binding against the pinned
directory. Tests require actual sharing violation 32 from both a competing
same-process opener and a bounded ten-second child test process, repeated
validation while held, denied deletion, and successful reacquisition of the
same object after close. The child performs only a competing open, never
reconstructs ownership from a pathname or head record, and cannot inherit the
lock handle. This is not a restart-authority test: no head capability exists yet.

The slice has no head writer, codec or operation integration. Those subsequent
slices must repeat lock validation around publication/acquisition/consumption,
prove cooperating publication without custody refuses, and test directory/head
substitution. A before-and-after hash/identity comparison alone cannot detect an
in-place rewrite followed by restoration between checks. The later head pin must
either prevent that write for the whole relevant interval or use an independently
durable revision mechanism; no detection of invisible intervening writes is
claimed here. Native same/child-process exclusion and release still require
exact-commit hosted 2022/2025 execution; cross-target lint is compilation only.

The lock-only slice `c31c36d2` passed hosted run 39347200600 on both Server
2022 and 2025: actual same-process/child sharing errors 32, reopen result 0,
15 operation fixtures and 74 ordinary tests passed, four ignored. Core read
the raw log in 6259. This supplies no head or recovered-operation authority.

#### Fixed committed-head codec, fixture v1

Before adding a writer, fix the head format independently of the outcome
codec. It is exactly 181 bytes, with little-endian integer fields:

| Offset | Bytes | Meaning |
| --- | --- | --- |
| 0 | 8 | ASCII `TRHEAD01`, including the only supported version |
| 8 | 2 | Total length, exactly 181 |
| 10 | 32 | Attempt-directory binding, neither all zero nor all ones |
| 42 | 1 | Component: 1 Controller, 2 CLI |
| 43 | 1 | Image role: 1 old, 2 new |
| 44 | 1 | Operation: 1 move-old, 2 publish; only role/operation 1/1 or 2/2 |
| 45 | 8 | Nonzero intent revision |
| 53 | 64 | Expected source image |
| 117 | 64 | Expected occupied target image |

Each image contains volume serial u64, file ID 16 bytes, length u64 and SHA-256
32 bytes, in that order. Volume and file ID reject zero/all-ones sentinels;
length is 1 through the shared 256 MiB cap. The two full object identities must
differ. Hash bytes have no reserved sentinel. This first negative fixture
describes an occupied target, not an authenticated absence claim. There is no
predecessor, failure status, timestamp, path or free-form field in the head.
No outcome codec bytes are accepted as a head, or vice versa.

The codec-only slice checks literal golden bytes, every truncation, trailing
data, unsupported framing/selectors, invalid lengths/identities and exact
expected-field comparison. Parsing grants no trust: attempt binding and image
expectations still require independent setup, live custody and native checks.
It adds no head publication, retained handle, head hash in the acquisition,
reopen authority, durable consumption or production caller. Subsequent writer
and retained-pin slices must prevent write/delete sharing for the whole interval
and prove actual competing-write refusal before using a head as a predecessor.

The codec `9d897d1b` passed exact hosted run 39349608192 on both Server
2022/2025: three codec tests and 77 ordinary tests, four ignored, with prior
regressions preserved. Core 6308 and security 6306 read the raw logs; security
also accepted the static format as inert data. Digest bytes remain unrestricted
data: a real composition must independently compute and compare SHA-256, not
treat a digest sentinel policy as proof of provenance.

#### Cooperative initial publication and retained read sharing

The next private native fixture borrows its `FixtureLock` for the entire
`HeadPin` lifetime. Only that live lock can call initial publication; a missing
ownership argument cannot be constructed from a path or record. It validates
lock/directory custody before and after the existing fixed record replacement
helper, then retains the exact full identity and validated bytes through the
transition from the helper's delete-sharing reader to a `FILE_SHARE_READ`-only
handle. It checks the protected ACL, regular single-link allowed attributes,
non-inheritance, fixed `receipt.json` path and full identity/bytes of both the
retained and current-name reader on each validation. Existing initial records
refuse rather than replacing or clearing them.

Tests require actual sharing violation 32 for write and delete access requested
by permissively sharing second openers, a successful read opener, and successful
write/delete opens after the retained pin closes. Invalid codec bytes must
refuse before creating a record; a second initial publication must preserve the
original full identity/bytes and create no staged record. These native assertions
require exact-commit hosted 2022/2025 evidence; cross-compilation is not execution.

This slice uses synthetic codec fixture values only. It establishes storage
custody, not that encoded image/attempt fields match independently acquired
native objects. No fresh-process constructor or acquisition consumes a HeadPin.
The next composition must derive actual attempt and image expectations before
the operation, bind the committed head digest into context, and refuse foreign
attempts, same-byte/different-object substitutions and head advance. Neither a
reopened lock nor a parsed record can rebuild that authority after process death.

#### Partial retained-head/context composition

The cooperative publication/pin slice `98d47e50` passed hosted run
39351238820 on Server 2022/2025: two pin tests with actual write/delete
sharing violations 32 and released write-open 0, plus 79 ordinary tests and
four ignored. Core 6321 and security 6322 independently read the raw logs.

The next test-only composition builds the 181-byte expected head from the
independently captured native attempt-directory binding, full source/target
identities, hashes, sizes and fixed intent before native failure acquisition.
It publishes under the live fixture lock and derives the context predecessor
SHA-256 from the retained pin only after exact expected-byte and same-attempt
checks. The pin remains borrowed through evidence storage and consumption;
the wrapper checks it before evidence read and again after that read, before
the existing single-use fixture counter may advance. A failed precheck spends
the evidence too. No decoded record, digest or reopened path constructs a pin.

Two new native tests exercise both move-old/publish occupied-target failures,
the retained digest in the existing strict context/evidence codec, single use,
foreign-attempt refusal, and independent revision/component/hash/object-field
mismatch refusal before evidence creation. These tests require a new exact-SHA
hosted run; Windows cross-compilation alone does not establish their execution.
The expected-field mismatch cases do not simulate an actual file replacement
or a committed head advance. Same-bytes/new-object replacement, head advancement
and a fresh-process diagnose-only rehearsal remain separate acceptance work.
The older operation fixtures still explicitly use their synthetic predecessor;
this partial fixture adds no production adapter, recovery or durable consume.
The cooperative publish-to-pin gap and leader-held W4 boundary remain unchanged.

The partial composition `c7b84cd5` passed exact hosted run 39353008966 on
Server 2022/2025: retained-head 2, operation 17 and ordinary 81 tests passed,
four ignored, with earlier native regressions preserved. Core 6345 and
security 6344 independently read the raw logs; this accepts only the partial
inert composition, not the complete predecessor boundary.

The next two negative validator fixtures deliberately drop the restrictive
head pin, retain the old object through a delete-sharing reader, and use the
existing native record replacement helper under the same cooperative lock.
Keeping that old reader alive prevents ID reuse while the replacement is
created. They prove that a real same-byte replacement has a different full
object identity and a revision-8 replacement has changed bytes. They then
inject a reopened handle into a stale in-memory snapshot solely inside the
negative test: validation and digest extraction must refuse. For the revision
case, even injecting the new object identity must still refuse the old bytes.
No reason evidence is created and the complete replacement remains intact.

These are fault-injection tests of stale-snapshot refusal after real native
replacement, not replacement while a restrictive pin lives, a supported
committed-head advancement API, or a constructor that recovers authority.
The previous live-pin tests prove competing write/delete access is excluded.
Fresh-process diagnose-only, end-to-end refusal during evidence consumption,
and actual committed-head advancement remain acceptance work. Exact-SHA
hosted Windows execution is required for these two new validator tests.

The subsequent fresh-process rehearsal spawns the exact diagnostic test twice
as a bounded, non-inheriting native child: once while the parent retains its
lock and restrictive head pin, once after both have closed. The child receives
only the fixed head path and returns the unit diagnostic `ParsedUntrusted`
after bounded syntactic parsing (or `Refused` on a read/format error). Its branch
never calls ownership acquisition, constructs a `HeadPin`/context, performs
native rename, publishes evidence or invokes a consume sink. A parsed file is
untrusted data even if its original publisher was valid.

The parent requires successful child exit within ten seconds, kills/reaps only
its own child on timeout or unwind, and checks the retained pin after the first
probe and the original full identity/bytes after the second. No reason evidence
or staged record may appear. This proves execution of the deliberately limited
diagnostic entry point, not that arbitrary future code cannot misuse a path,
that a restarted updater has authenticated state, or that crash recovery is
implemented. Native hosted evidence is required; end-to-end consumption refusal
and a real committed-head advancement protocol remain separate work.

The next composed negative fixture changes only security metadata on the
disposable head while its restrictive read pin remains live. A separately
opened `READ_CONTROL | WRITE_DAC` handle retains the original non-null private
owner/SYSTEM ACL. `SetSecurityInfo` reapplies that ACL while clearing its
protection flag, and `GetSecurityDescriptorControl` verifies the actual flag.
The supplied ACL is never null or world-granting, but clearing protection can
transiently inherit additional ACEs from the parent directory and broaden access.
This fault is confined to the synthetic object created in the private fixture
directory, and restoration is explicitly checked; it is not safe as a generic
operation on an existing installation. Data-writer/deleter sharing exclusion is unchanged.
Owner security-metadata override is a test fault, not a newly claimed adversary
boundary or a production security repair operation.

For each move-old/publish collision the test stores real context-bound failure
evidence, then clears the flag either before consumption or in a narrow hook
after evidence read. The pre/post retained-head checks must refuse, the private
sink stays zero and the evidence becomes spent. A hook-execution flag distinguishes
the pre-read refusal from the post-read branch. After restoring the original
protected ACL, native custody checks must succeed again, but consumption must
still refuse and evidence bytes remain unchanged. An RAII guard also attempts
restoration on unwind, targeting only that newly created fixture object.

This covers composed refusal on an actual head ACL-custody failure, not an
atomic head-advance/consume commit, arbitrary concurrent ACL rewrite detection,
durable once-only consumption, or production recovery. The hook is private to
the inert composition. Exact-SHA hosted tests on both Windows images must
confirm the actual ACL transition and zero-sink/spent assertions before this
slice is accepted; successful cross-target lint alone cannot prove them.

#### Next boundary: write-ahead consumption decision (design only)

The fixture head checklist is accepted through `cfdb53da` (hosted run
39358295439, Core 6384, security 6381). Actual ACL-custody loss before/after
evidence read is accepted through `341fc476` (hosted run 39358825446, Core
6392, security 6391); `8fab7160` changes comments/docs only. Neither replaces
the in-memory consumption latch with a durable decision. The next independent
format review must precede a decision writer or Controller adapter.

Propose one immutable fixed `consume.intent` file inside the already pinned
attempt directory, separate from the head and `reason.evidence`. Creation is
exclusive, never replace, append, truncate, delete-and-retry or open-existing
as permission to consume. After security review 6402, the proposed binary record
is exactly 177 bytes, including an installation/lock-lifetime binding:

| Offset | Bytes | Meaning |
| --- | --- | --- |
| 0 | 8 | ASCII `TRCONS01`, distinct from head/context/pipe domains |
| 8 | 2 | Little-endian total length, exactly 177 |
| 10 | 32 | Installation and live supervisor lock-lifetime binding |
| 42 | 32 | Independently committed attempt-directory binding |
| 74 | 24 | Retained predecessor head full volume-u64/file-id-128 identity |
| 98 | 32 | SHA-256 of the exact retained predecessor head bytes |
| 130 | 32 | SHA-256 of the exact retained 225-byte context evidence |
| 162 | 3 | Component, image role, operation selectors |
| 165 | 8 | Nonzero committed intent revision, little-endian |
| 173 | 4 | Actual retained nonzero Win32 failure status, little-endian |

Use the existing identifier/selector/role-operation rules. Digest bytes have
no reserved sentinel; a valid digest is not authority. The evidence digest
transitively binds both full image identities, lengths and hashes, but only
after the separately pinned strict context decoder and retained actual failure
match all independently committed expectations. A successful parse never
constructs an accepted/consumed token. Component and revision must originate in
the prior committed intent, not a caller's failure classification or a fresh
observation. This format remains a proposal, not an implemented file contract.
The installation/lifetime identifier rejects all-zero/all-ones values and must
eventually come from the real supervisor's independently retained ownership,
not from the directory or its decision file. A codec fixture may use an
explicitly synthetic 32-byte constant; it proves neither installation identity
nor lock ownership. Copying a decision to another installation/lifetime must
fail exact comparison with that separately supplied expectation.

The intended live ordering under one supervisor-owned installation/enrollment
lock lifetime is: validate committed head and retained failure/evidence; spend
the in-memory attempt; exclusively create the complete private decision; sync,
close its writer and retain a restrictive read-back pin; revalidate exact
identity/bytes/ACL and all custody; only then permit one private continuation.
Any error after starting decision creation refuses continuation and preserves
all records, including partial files. The lock order and real supervisor entry
still need their own implementation review; the disposable `FixtureLock` does
not satisfy this production requirement.
During writing, the new decision handle has write access with read sharing
only, never write/delete sharing. The restrictive read-back pin then stays open
for the entire continuation. The writer-to-reader transition still needs a
separately tested custody/ACL contract; a gap cannot be hidden by calling it
continuous pinning. After the continuation closes its pin, the file remains as
the burn marker, protected by the attempt directory's ACL rather than an open
handle. That protection does not prevent an authorized owner deleting it.

Every interruption from decision creation through continuation and subsequent
head publication is diagnose-and-stop on a new process. An existing decision
is a burned attempt even if its bytes are complete and valid; a missing,
truncated, malformed or conflicting decision never grants restart authority.
The new process cannot infer that continuation did or did not run from this
file. A decision that reached read-back is not proof of survival across power
loss. No atomic consume-plus-head CAS, automatic retry/recovery, or completed
update claim follows from this write-ahead shape. A separate committed-head
publication protocol must resolve that boundary before production wiring.
In particular, deletion/replay protection requires independently protected
consumption state outside the attempt directory, bound to the committed head
by the real supervisor-owned installation protocol. A consumed attempt must
not become fresh when its intent file or directory disappears, is copied or is
restored from an older snapshot. A supervisor-maintained monotonic attempt
generation is one possible future design, not an implemented or approved epoch
mechanism here. The format and intent file alone supply no such protection.
This requirement adds no HKLM operation, registry/reboot experiment or recovery
authorization; its storage and head publication contract require separate review.

First implement only the strict codec with an independent literal golden
vector, all truncations/trailing data and cross-domain refusals, every bound
field mismatch and invalid selector/identity/revision/status coverage. Then
review the private native decision writer, interrupted-stage fixtures and
read-back/write-sharing tests separately. The existing native head fixture
results cannot stand in for that evidence. W4 remains leader-held.

The first codec implementation is private `checked_files::consume_record`,
inherited `cfg(all(windows, test))` only. Its fields and strict 177-byte framing
follow the amended contract accepted in security 6408. It has no writer, path,
file handle, supervisor acquisition, public conversion or consumed token.
The fixture lifetime/attempt values are explicitly synthetic. The three tests
compare an independent literal hex vector, mutate every encoded byte against
the independent expected record, reject copied installation/attempt bindings,
all truncations and appended data, old length 145, unsupported domain/version
tags, sentinel identifiers and invalid selector/revision/status fields. They
also retain unrestricted digest data and maximum nonzero revision/status.

The same standalone Rust test file can execute as pure codec logic on the
development host; passing that harness does not establish Windows file sharing,
ACL, flush durability or production integration. Exact-SHA hosted Windows
library testing remains separate evidence. The native exclusive writer and its
interruption cases are still unimplemented, as are external deletion/replay
protection and atomic consume/head publication.

The codec `dd5bfd86` passed hosted run 39361643226 on Server 2022/2025:
consume codec 3 and ordinary library 88 tests passed, four ignored, with prior
native regressions preserved (Core 6416, security 6417, raw logs read).

The first private writer slice is exclusive staging only. It borrows the live
disposable `FixtureLock`, validates the strict codec before touching a file,
creates `consume.intent` with an explicit private owner/SYSTEM DACL,
non-inheritable read/write handle and read sharing only, writes/syncs the bytes,
then verifies ACL, regular single-link attributes, full identity, fixed path,
non-inheritance and bounded exact read-back while retaining the writer. It
returns only that private staging guard, not a continuation or consumed token.
Complete existing files refuse both before and after closing the writer.

The two new native tests require actual sharing violation 32 for permissively
sharing write/delete probes, compatible read success, and released access 0.
An injected interruption after writing/syncing exactly 17 bytes closes its
writer and leaves the same partial object and bytes intact; exclusive creation
must refuse both immediately and after fixture-lock reacquisition. This is an
injected return, not process death or a real write/sync failure. Tests delete
only their exact disposable files after assertions; writer errors never clean
up or overwrite decision evidence. Native Windows execution remains required.

This does not implement the writer-to-restrictive-read-pin transition, prevent
writes after guard closure, validate independently committed production fields,
bind a real supervisor lifetime, dispatch a continuation or persist once-only
consumption across deletion/replay/power loss. Those remain separate reviewed
slices; an exclusively created file alone grants none of those guarantees.

The staging slice `47dee255` passed hosted run 39363419251 on Server
2022/2025: two stage tests, actual competing write/delete 32 and released 0,
partial 17-byte preservation/retry refusal, and ordinary 90 passed/four ignored
(Core 6446, security 6444, raw logs read).

The next private transition consumes the original `Stage`, validates it,
closes its writer, then opens the fixed name with read access and read sharing
only. It retains the original full identity and bytes, borrows the same live
fixture lock, and validates lock/directory/ACL/type/non-inheritance/path and
exact read-back before returning a private `ReadPin`. Each later validation
also independently opens the current fixed name and compares identity/bytes.
There is no constructor from an existing path, parsed record or restarted lock.
This type supplies storage custody only, no continuation or consumed token.

The writer-close/reopen gap has only cooperative-lock protection. Tests inject
either an error there or an actual in-place write/sync changing status 183 to
182 (still syntactically valid). Both must refuse, preserve the existing object
and exact resulting bytes, and refuse another exclusive creation. This is not
process-death or sync-failure evidence, nor detection of an invisible rewrite
that restores its bytes before reopen. The successful transition test requires
real write/delete sharing errors 32 while the read pin lives and released
access 0 afterwards, with the unchanged burn file still refusing creation.
The read-pin transition `093e55c4` passed hosted run 39365180780 on Server
2022/2025 (two pin tests, ordinary 92 passed/four ignored; Core 6461 and security
6460 independently read the raw logs).

The next test performs a real same-bytes replacement during the close/reopen
callback: rename the original to a private fixture-only archive name, create
and validate a separate protected `consume.intent`, then close its writer.
Keeping the original file alive prevents file-ID recycling. The original
transition must refuse the new full identity even though both objects contain
the exact same valid 177 bytes. Both objects and their bytes must remain intact,
the fixture lock must remain valid, exclusive creation must still refuse, and
no reason evidence may appear. This is deterministic same-process interference
inside the existing private test hook, not a concurrent adversary, crash,
invisible rewrite-and-restore or production replay fence. Continuation
integration remains separate work. All new Windows assertions require exact-SHA
hosted execution; no production locking or replay/CAS claim follows.

#### Next inert composition: bind the decision before the private sink

The same-bytes gap replacement fixture `b279e51d` passed hosted run
39367207855 on Server 2022/2025: read-pin three, ordinary 93 passed/four ignored
(Core 6479 and security 6475 independently read raw logs). The next slice should
compose the accepted codec, exclusive writer and restrictive read pin with the
retained-head/native-failure fixture. This is a design contract for review, not
permission to wire a Controller adapter or perform a rollback.

At design checkpoint `279cd85b`, the `BoundStored::consume` callback ran after
evidence read but before its exact context comparison. It must not be reused as
a decision-publication hook in that order. The composed private path must first spend the in-memory
evidence latch, validate the retained head and live operation binding, read the
strict context evidence and match every field against the independently supplied
fixture expectations plus the retained actual failure. Only after this match
may it build or create a decision. Any earlier error must leave the sink at zero
and create no decision file. Existing read-failure and ACL-loss assertions must
continue to hold after adjusting the private callback order.

Build the 177-byte decision from separate sources: explicit synthetic fixture
lifetime (not a real supervisor lifetime); the pre-acquisition binding's attempt,
component, role, operation and revision; the validated retained head's original
full identity and exact digest; the digest of the just-validated 225-byte context;
and the retained native failure status. Never accept these expected values from
an existing decision. A narrow private head-snapshot accessor, if needed, must
validate the live head and its expected bytes before returning identity/digest.
This snapshot is fixture data, not a standalone authority or restart constructor.
Codec field visibility must stay inside the Windows test-only checked-files
boundary; no public record-to-capability conversion is introduced.

Under the same borrowed `FixtureLock`, exclusively create the decision, sync,
close the writer and perform the already reviewed restrictive read-pin
transition. Revalidate that pin, the retained head, operation binding and context
evidence after the transition, immediately before the sink. Keep the decision
pin alive for the entire synchronous private sink invocation. The sink receives
no pathname, parsed record, token or continuation capability; it only increments
the existing test counter. Sink refusal or any write/read-back/revalidation
failure spends the same in-memory attempt and preserves whatever decision bytes
exist. A complete existing decision must refuse creation without reaching the
sink; dropping guards must not delete it or make the same evidence fresh.

Initial composed assertions should cover both move-old and publish collisions:
one successful sink visit with exact independently assembled decision bytes and
write/delete exclusion while the pin lives; a second invocation refused with
the sink unchanged; pre-existing complete and partial decisions preserved with
zero sink visits; injected failure after decision creation preserving the burn
file and spending the evidence; and post-transition head/context custody refusal
with zero sink visits. A flag must prove each injected boundary was reached.
Retain existing actual ReadFile-error and ACL-loss coverage. Review the exact
test matrix before implementation; exact-SHA hosted native execution is required
afterward, and compile-only checks cannot establish the composition.

Security review 6491 accepted this order with seven implementation conditions:
hash the exact first matched context bytes once and compare later read-back to
that digest; include independent per-field mismatch controls before any decision
creation; observe actual write/delete 32 inside the sink and release afterward;
prove the old ReadFile/ACL fault boundaries still execute; isolate the distinct
synthetic lifetime and all capabilities to Windows test builds, including a
non-test compilation/source-boundary check; preserve both a replaced complete
decision object and a 17-byte partial decision; and keep the validated head
snapshot accessor private and data-only. The field matrix must independently
cover operation, role, component, revision, head identity/digest, context digest
and actual failure status, with spent evidence, zero sink and no decision file.
Those decision-composition checks remain pending until their code lands.

The first preparatory code slice only reorders `BoundStored::consume`: spend,
binding check, evidence read, exact expected-context comparison, callback,
post-callback binding check, sink. Its existing ten foreign-binding controls
for each operation now panic if the callback runs, assert spent evidence and
no decision file, then retry the correct binding and require refusal. The
five matching-context scenarios explicitly assert callback entry, including
injected failure, post-read native slot mutation and evidence-sharing probes.
The actual ReadFile-error fixture retains its panic-on-callback and native read
trace checks; the head ACL fixture retains pre/post hook-execution flags.
No decision is written and no new accessor, digest handoff, sink capability or
production API exists in this preparatory slice. Exact-SHA hosted evidence is
still required to accept its reordered native controls.

This adds no durable consume-plus-head transaction, external deletion/replay
fence, independently committed production expectations, authenticated health,
real installation lock or real supervisor lifetime. A fresh process remains
diagnose-and-stop regardless of decision-file presence. Process death, actual
sync failure, invisible rewrite-and-restore and power-loss durability remain
unproven. Conditions 5976 and leader-held W4 are unchanged.

Reject unknown fields, unsupported versions, invalid transitions, duplicate
image records and a workspace/server/job binding mismatch before any mutation.

Private metadata records Controller and CLI independently: expected old/new
hashes and byte lengths, verified object identities, fixed active/candidate/
backup slot selectors, and the last durable step. Paths are derived from the
validated installation and private attempt, never from public receipt fields.
For each old/new object persist the NTFS volume serial and file ID, byte length,
SHA-256, expected single link, and the validated owner/protected-DACL policy.
Reopen and reverify every property and the non-reparse directory binding before
classifying after a restart. A matching digest with a different file ID is an
unknown object, including after reboot; a copied backup is not the original O.
For each image, persist and read back each intent before the corresponding
rename; verify all retained objects and the resulting layout before recording
its completion:

`original -> move_old_intent -> old_moved -> publish_intent -> published`.

Reverse restoration needs two separately recorded moves too:

`published -> move_new_aside_intent -> new_aside -> restore_old_intent -> restored`.

If publication never happened, `old_moved` or `publish_intent` may enter
`restore_old_intent` directly after verifying the missing active slot and intact
old backup. Moving the new image aside uses its now-vacant candidate slot and
never overwrites an existing object. A hash match alone is insufficient: retain
the checked object identity, size, private ACL, one-link/non-reparse checks and
directory binding through each move. Same-version updates can have equal
old/new hashes; distinguish their objects by identity, not by digest inequality.

The table below classifies observations made under a newly acquired installation
lock after the previous worker is proved quiescent. `O` and `N` mean the recorded
old and new objects, with all identity/content checks satisfied; `-` means an
explicitly verified absent slot. A read/sharing/ACL error is not absence.

| Last durable step | Active / candidate / backup | File-layout classification |
| --- | --- | --- |
| `original`, `move_old_intent` | O / N / - | Original layout; no completed move observed |
| `move_old_intent`, `old_moved`, `publish_intent` | - / N / O | Old moved; active-name gap |
| `publish_intent`, `published`, `move_new_aside_intent` | N / - / O | New image published; health remains unproved |
| `move_new_aside_intent`, `new_aside`, `restore_old_intent` | - / N / O | New image aside; old restoration pending |
| `restore_old_intent`, `restored` | O / N / - | Old file layout restored; old health remains unproved |
| Any other step/layout pairing, unknown object or unreadable slot | Any | Conflict; preserve evidence and require operator inspection |

An intent can therefore match either side of its single rename. Observing the
post-rename layout does not advance the durable record, repeat the operation,
authorize rollback or prove which process executed it. These classifications
are inspection results only; an interrupted attempt still blocks new attempts.
Do not infer quiescence from a missing PID or an obtainable lock alone, or use
recorded process identifiers after reboot as authority. Missing, corrupt,
partially written or uncertain-sync receipts require inspection, not discovery
of a plausible layout followed by automatic adoption.

For this first parity implementation, quiescence after interruption is an
explicit local operator attestation, not an automated process-death proof.
The operator must establish that the old supervisor and any dispatched restart
helper can no longer mutate the installation, using local process/service
inspection and retained process handles where available. Record PID plus
creation time as diagnostic evidence, never as sufficient recovery authority.
After reboot, require a fresh operator assessment of current processes and
installation state; historical PIDs, elapsed time and the file lock cannot
substitute for it. If the operator cannot establish quiescence, preserve the
blocked attempt. Automated quiescence proof is outside this slice.

Transaction-level progress remains separate from per-image moves. Persist
`activating` before the Controller's first rename and a restart intent before
dispatching its fixed restart operation. Record `core_healthy` only after the
new Controller passes the full baseline checks above; only then start CLI moves.
Persist a rollback intent before reverse CLI/Controller restoration. Record
`complete` only after both installed images and final health/inventory checks;
record `rolled_back` only after both old images and independently checked old
Controller health. If a restart response or health-record write is lost, file
layout alone cannot resolve it: leave recovery required and never redispatch
from the intent. A failed recovery while the supervisor is alive may record
`rollback_failed`; worker death is interrupted evidence, not a fabricated
terminal result. This parity path has no `launcher_installing` step.

Cross-image validation applies after both candidates and their original-image
records have been prepared. In the following table, original/restored refer to
the corresponding durable step and verified O / N / - layout, published to
N / - / O, and a moving state includes an intent observed on either side of its
one rename. The per-image table must also accept each record independently.

| Transaction state | Controller | CLI |
| --- | --- | --- |
| `prepared` | Original | Original |
| `activating`, before durable new-health success | Original, forward moving or published | Original |
| `core_healthy`, CLI publication | Published with recorded new-health success | Original, forward moving or published |
| `complete` | Published | Published, with final health/hash/inventory success recorded |
| `rolling_back`, CLI restoration | Published | Original, published, reverse moving or restored |
| `rolling_back`, Controller restoration | Original, published, forward gap, reverse moving or restored | Original or restored |
| `rolled_back` | Original or restored | Original or restored, with old-health success recorded |

Persist a rollback substep identifying which image is being restored. CLI must
be verified original/restored before a Controller restoration intent; its
remaining candidate is evidence, not an installed image. Any other combination
is a conflict. `failed` is permitted only before installed-image mutation;
`rollback_failed` preserves the last validated rollback substep and layouts,
without asserting restored health. Preparing records may describe incomplete
downloads, but cannot be fed into this publication classifier or authorize moves.

Proposed local operator recovery interface, not shipped CLI syntax:
`self-update --recover JOB_ID --action classify|archive --ack-quiescent`.
It accepts no paths or executable arguments and is never invoked automatically.
The acknowledgement is a statement of fact, not authentication. Neither the
flag, a same-account credential nor the absence of Agent environment variables
proves that a human authorized recovery. Both actions write transaction state
and must remain unavailable until a trusted local human-authorization mechanism
is specified, implemented and tested. They are absent from Proxy and managed
Controller command surfaces; direct invocation of the installed executable must
also enforce this restriction, not merely hide help or check argv/environment.

At minimum, the future entry must refuse callers inside any Job (including the
Host and Agent PTY Jobs), noninteractive logon sessions and callers without a
local interactive console. These are necessary filters, not sufficient proof of
a human: a same-account process may obtain an interactive console or escape a
Job. An ordinary console confirmation code is not trusted merely because it
was printed there. Before enabling either action, review how the confirmation
is bound to this job, expected revision, exact action and one use, and how its
delivery/acceptance channel excludes managed Agents. No such protected channel
is claimed here. Until that boundary is resolved, interrupted attempts remain
blocked and require separately authorized local maintenance; the updater does
not provide an archive bypass. Native negative fixtures must invoke both actions
from an Agent Job, the Host Job and a noninteractive process and observe refusal
without receipt/marker writes, archiving or process mutation.

Each invocation acquires the installation lock, validates job/enrollment and
receipt/marker binding, freshly observes both image layouts and requires the
operator attestation above. `classify` appends one inspection event; it cannot
rename images, restart processes or convert interrupted work to success.
`archive` is a separate invocation requiring the expected classified revision;
it recomputes the classification and refuses changes or conflicts. Only then
may it move the exact attempt evidence into private archive storage and record
that disposition in the installation marker. A replacement attempt additionally
needs a verified healthy installed baseline; archiving an active-name gap does
not make that installation eligible. Restoring images or restarting a Controller
after interruption remains a separate explicitly authorized local repair, not
an implicit recovery action. Cleanup is separate again.

Recovery records contain the fixed action/result, job/enrollment binding,
prior/next revision and record digest, classification, operator-attestation flag
and timestamp; no arbitrary operator text, paths or process output. Fixed results
are `inspection_recorded`, `evidence_archived`, `recovery_conflict`,
`quiescence_required` and `recovery_required`. Persist successful inspection
events in the receipt and retain the archived receipt unchanged thereafter;
record archive disposition separately in the marker. If a refusal prevents a
safe receipt write, return its fixed code without overwriting the evidence.
Public status exposes only these fixed result codes, never operator text or
confirmation material. No confirmation material is stored in the public receipt.

The Windows storage adapter must exclusively create private attempt storage,
bound receipt reads to 64 KiB, and compare the entire expected prior record and
revision under the installation lock before writing. Write and sync a new
private temporary file, atomically replace the receipt without deleting the
old file first, then revalidate owner/protected ACL and exact record readback.
Any uncertain result stops further mutations and preserves available evidence;
no power-loss durability claim follows merely from a successful API return.
Additionally keep a separate protected installation marker outside the attempt
and its archive, bound to the enrollment/job and highest reserved revision plus
exact next-record digest. Reserve the fixed installation-private location
`self-update/installation-marker.json` under the selected enrollment state root,
as a sibling of `attempts/` and `archives/`. The enrollment installer must create
its private parent and initial marker with a protected owner/SYSTEM-only DACL;
the updater validates their owner, ACL and non-reparse binding, refuses a missing
or unexpected marker and never deletes or recreates it as recovery. Existing
enrollments need a separately reviewed explicit provisioning step before this
updater is enabled; production parent ACL provisioning remains unresolved.
Before replacing a receipt, durably replace and read
back that marker with the next revision/digest; only then publish the exact
receipt bytes. A crash between these two publications leaves a mismatch and
requires operator inspection; it never accepts the older receipt as current.
Readers require exact marker/receipt revision and digest agreement, including
terminal reads; a lower revision, same-revision changed digest, missing marker
or uncertain marker write blocks further mutations. Recovery inspection uses
this same protocol. Archive disposition is a separate marker field and cannot
lower the high-water revision. Test failure at every marker/receipt boundary.
This detects accidental restoration of an older receipt while the marker
survives; it cannot detect a consistent rollback of both files or resist a
same-account actor rewriting them. A hash chain alone would not detect removal
of its latest links, so it is not a replacement for the independent marker.
Before W4, admission also needs a separately held expected job/revision anchor
outside this pair, to detect an accidentally restored directory or snapshot.
No existing Controller/Proxy field is assumed to provide that guarantee. Its
storage owner, enrollment binding and restart behavior must receive a separate
design review; missing, stale or unavailable anchor evidence must block
admission. This does not authorize new Proxy state or claim resistance to a
same-account actor rolling back or rewriting all anchors.
Native tests must exercise each pre/post-rename boundary, receipt replacement
failure, equal-digest/different-object images, conflicting occupied slots and
lost restart/health acknowledgements; also truncated/oversize/unknown-field,
duplicate-image and wrong-binding records, and Unix-reader refusal of Windows
records. They must prove that an interruption
never produces success, automatic replay, cleanup or acceptance of a new job.
Production parent ACL policy, the storage adapter and these fault tests remain
unimplemented; the hosted running-image fixture does not establish them.

The subsequent private `cfg(test)` module `update_record_order` models only
marker-first ordering in memory. Opaque records carry synthetic enrollment/job
bindings, a nonzero revision and 1-65,536 bytes; the independent marker binds
the exact bytes with SHA-256. Replacement requires the entire expected previous
record, an unchanged binding and a checked revision increment. It models ten
acknowledged boundaries (create, write, sync, publish, readback for the marker,
then the receipt) and injects interruption before and after each one. Tests
check the retained names/values, the marker/receipt mismatch interval and that
the same worker cannot retry after uncertainty, even when both published values
already agree. Additional cases reject stale/same-revision modified receipts,
wrong bindings, missing markers, oversize/empty records and revision overflow
without starting publication. A consistently restored marker and receipt is
explicitly undetectable, as required by the threat model.
The marker additionally carries the proposed version/layout header. Tests
reject a mismatched marker header even with matching binding, revision and
digest. A further injected corruption flips one bit of the staged marker
digest: publication preserves those corrupted bytes, marker readback returns
its specific mismatch, receipt staging never begins and the worker remains
uncertain with retries refused.

These are synthetic acknowledgments, not real sync/readback calls. The sticky
uncertainty bit is volatile worker state, not a persistent recovery fence.
Inspection yields record consistency only; it cannot authorize a new job,
establish completion/health/quiescence or recover after a restart. Bytes are
opaque and bindings are supplied separately: this is not a schema parser and
does not establish that serialized fields match the binding. Windows schema
discrimination, Unix-reader refusal, native marker storage, enrollment
provisioning and fault tests against actual file APIs remain separate work.

For the real adapter, each modeled file-sync acknowledgment must correspond to
a successful [`FlushFileBuffers`](https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-flushfilebuffers)
on the retained writable staged file, followed by exact readback/identity/ACL
checks; publication uses the reviewed handle-based POSIX-semantics rename.
Neither a successful file flush nor readback is presented as an acknowledged
parent-directory metadata flush. No equivalent parent-directory flush is
implemented or validated here, and an administrative volume-wide flush is
outside this user-mode updater's scope. Therefore ordering can be tested under
process interruption, but durable rename ordering across power loss remains
unproved. A future adapter must report uncertainty rather than label that gap
as synced; W3 evidence must distinguish process interruption from power loss.

`treer-platform/src/windows_update_layout.rs` now exercises only the per-image
table as a private, test-only pure model on all platforms. Its tests cover
both sides of every rename intent, direct restoration after failed publication,
and refusal of unreadable/unknown slots, occupied destinations, missing objects
and duplicate object references. Inputs labelled old/new are assumed verified;
there is no native observer or identity verifier in this model. The model also
checks the cross-image table, requiring durable original/published/restored
steps at the corresponding boundaries, CLI restoration before Controller
restoration, and explicit new/final/old health evidence for the relevant states.
Health inputs represent assumed validated records, not probes or authenticated
evidence supplied by this model. Additional tests reject early CLI replacement,
missing health evidence, a forward intent disguised as restoration, a CLI
forward gap in the CLI rollback substep, and either invalid/unreadable image
even when all health inputs claim success. It never reads/writes a receipt or
marker, grants recovery authority, or implements a production command. Native
storage/identity and fault-injection tests remain necessary.

Health evidence must also belong to the durable transaction state: prepared and
activating accept no health records; core-healthy accepts only new-health;
complete requires new/final-health and refuses old-health; rollback substeps and
rolled-back refuse final-health. Rollback may retain new-health evidence, and
CLI rollback refuses old-health because the Controller is still published.
Controller rollback accepts old-health only once its durable step and layout
are original/restored; rolled-back still requires old-health. The pure model
enumerates all eight flag combinations for each state on otherwise valid layouts. Stray evidence is a
conflict, not ignored metadata that might conceal a reordered receipt write.

The next native fixture adds `Attempt::observe` in `checked_files.rs`. This is
still private test-only code over an exclusively created disposable attempt,
not a production recovery observer. In-memory records capture the volume/file
identity, attributes/link count, size and digest after checking a protected
owner/SYSTEM-only ACL. Observation rechecks the retained directory binding and
private ACL, opens only its three fixed slot names without write sharing, and
checks each object again before labelling it old/new. Equal hashes with a
different file ID are unknown. Only `ERROR_FILE_NOT_FOUND` means absent;
sharing/read/ACL errors mean unreadable, including a directory occupying a file
slot. A foreign read grant is refused under this private-attempt policy too.

Its native regression observes equal-hash/different-ID originals and candidates
through the actual rename gap, publication and reverse restoration, injects a
third equal-hash object, an exclusive-reader conflict, foreign read ACL and an
occupied directory. Hosted run 39220955841 checked exact code commit
`0b17854c1f49c5892d901a824b480327a260c94f` on Server 2022 and 2025. Each
reported the observer test alone 1 passed/0 failed (`OBSERVER_EXIT=0`), rename
tests 2 passed/0 failed, and full platform library 47 passed/0 failed/4 ignored.
The separately CIM-launched outside-Job owner round trip and running-image
rename each reported 1 passed/0 failed, exit 0 and their raw-result marker true.
Raw log: `.treer/releases/boot-111-0b17854c/run.log` (Issues #111, event 5332).
These are disposable fixture results, not installed/live or `win_native`
acceptance. Observation
returns labels and drops its read pins; it is not an atomic multi-file snapshot
or mutation authority. Production use still needs lock/quiescence enforcement,
persisted authenticated records and marker binding, installation-specific ACL
policy and retained pins through any authorized operation. No receipt/storage
adapter, recovery command or installed-file access is added by this fixture.

The following native storage increment is an opaque-byte fixture in
`checked_files/record_file.rs`, not the receipt schema or installation marker.
`Attempt::replace_record` accepts 1-65,536 bytes and an optional retained previous
record handle, validates the exact current object's identity/content and private
ACL, then exclusively creates `receipt.next` with a protected owner/SYSTEM DACL.
It writes, syncs and reads back before a same-directory full-path handle rename
to `receipt.json`, with replacement allowed only for the expected old record.
It never deletes the old name first. After replacement it rechecks identity,
ACL and bytes, closes the original writer object and returns a freshly opened
read pin with delete sharing so later replacement can retain the old reader.

The fixture rechecks the prior record before publication, but this is not an
atomic compare-and-swap against another writer: production must serialize it
under the installation lock and enforce the independent marker protocol.
Injected failures before publication and after publication return an error and
retain the old or new record respectively; complete staged evidence is retained
on pre-publication/native-sharing refusal and blocks silent retry. Tests cover
the 64 KiB boundary and oversize/empty refusal before temporary creation, stale
previous-handle refusal, old-reader retention, both injected boundaries and a
real delete-sharing conflict. These tests
do not establish schema validation, revision/marker ordering, child quiescence,
production ACL provisioning or power-loss durability. No production journal or
recovery command uses this primitive.

Initial hosted code `170c8d9f` failed both new storage tests on Server 2022/2025
(run 39222920553 and single-thread diagnostic 39223181983). Initial creation
succeeded, but replacement with an old reader failed and the after-publication
fault test still observed old bytes. The code used ordinary `FileRenameInfo`;
the successor uses `FileRenameInfoEx` with replace/POSIX flags when a previous
record is expected, and zero flags for initial creation. Microsoft's
[rename contract](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/ntifs/ns-ntifs-_file_rename_information)
documents that POSIX replacement preserves existing handles while new opens
resolve to the replacement. The flags contract is gated at Windows 10 RS1
(version 1607) in the SDK; the updater's initial supported server floor is
Server 2022, with Server 2022/2025 as the measured targets. Production wiring
must refuse unsupported builds or unavailable extended-rename semantics before
replacement, never substitute ordinary rename. Earlier systems are not validated
by these fixtures.

Exact successor `3ef57b6a` passed hosted run 39223838525 on Server 2022 (20348)
and Server 2025 (26100). Core read the raw log
`.treer/releases/boot-111-3ef57b6a/run.log`: each image reports both record tests
passing, full platform library 49 passed/0 failed/4 ignored, earlier observer
and rename tests passing, and outside-Job owner round trip and running-image
tests passing with zero exit codes. The single `rename_ex` Win32 32 line per
filtered run is the intentional non-delete-sharing reader refusal. This accepts
the disposable opaque-record fixture only; the earlier failed run remains
evidence under its original SHA.

The subsequent diagnostic delta logs fixed stages and numeric Win32 codes at
each record validation and I/O refusal, without paths, SIDs or record contents.
Code zero denotes a semantic refusal or an upstream verifier that intentionally
discarded the native error; it does not mean OS success. Native-call errors are
captured immediately. This diagnostic delta is separate from the hosted SHA
above and needs its own execution evidence. There is no ordinary
rename, delete-first or sharing-check bypass fallback. The delete-sharing
blocker assertion remains unchanged. A separate observed concurrent fixture
directory collision is addressed by OS-random selectors instead of timestamps;
existing directories are still refused, never adopted or removed on collision.

### W1a: private directory and checked-copy fixture

`windows_update_bootstrap/checked_files.rs` is private under `cfg(windows, test)`.
It retains non-delete-sharing local-directory ancestors, requires a fixed local
NTFS volume with persistent ACLs and a parent with no foreign writers, creates a
new attempt directory with a protected current-user/SYSTEM DACL at creation,
and refuses an existing attempt. Its fixed candidate file is CREATE_NEW with
the same DACL and a non-inherited handle. A separately reopened pinned source
is bounded to 256 MiB before copying and during a 64 KiB-buffer stream
(updated from 128 MiB by leader5245). The
copied bytes are hashed, synced and read back through the same destination
handle; the returned handle disallows other writers/deletion. Failed evidence
is preserved and cannot be overwritten by retrying with a corrected digest.
The existing-parent/source DACL helper only reads security information and
rejects foreign mutation grants; it does not change existing permissions or
exclude foreign read grants. Newly created attempt/candidate objects instead
receive the stricter protected owner/SYSTEM-only descriptor at creation.

The deterministic attempt names are test inputs only. W2 must generate the
128-bit name suffix from the operating system random source inside the trusted
coordinator, expose no caller-selected name in the production command, and test
that caller-supplied names cannot select or adopt an attempt. A syntactically
valid name alone does not establish freshness or authorize execution.

Native fixture tests cover an independent source offset, copy checksum failure,
existing directory/file refusal, oversized source refusal before candidate
creation, unsafe names, foreign parent writers, non-inherited handles and
write/rename sharing denial. This is not publication, a complete W1 transaction,
production ACL authorization or a real installed Controller update. Same-account
attackers remain outside the boundary. Hosted Windows execution is required;
cross-target clippy only establishes compilation. The next W1 slice must add
checked rename/backup/reverse-rollback behavior and receipt-state handling.

Hosted run 39208522339 exercised W1a commit
`116986d5b2cd51151a09dcda7d7d2dd109e2de24` on Server 2022 and 2025: each
reported 39 platform library tests passed, zero failed, two ignored, including
all three new native checked-copy/attempt tests. The separately launched
`native_owner_round_trip` passed on each runner outside the ambient runner Job.
These are disposable native fixtures, not a production update or `win_native`
acceptance. The later `2d3be41a` changes only comments and documentation; it was
not the hosted checkout. Release retained the raw log under
`.treer/releases/boot-111-116986d5/run.log` (Issues #111, event 5177).

### W1b first slice: retained-handle rename and reverse-rollback fixtures

The test-only checked-file module now also creates a verified copy whose retained
read handle has DELETE access for renaming, without write access or write/delete
sharing. The original W1a read-only copy API is unchanged. Rename accepts only
three fixed slots inside the retained attempt directory: `candidate.exe`,
`controller.exe`, and `controller.prev.exe`. It rechecks owner/DACL, file identity,
single-link/non-reparse status, current slot and SHA-256 before calling
`SetFileInformationByHandle(FileRenameInfo)` against the same file handle, using
a NULL RootDirectory and the retained directory handle's final full path joined
with the fixed destination slot. The directory and all ancestors remain pinned
without delete sharing, preventing path redirection; no caller path or cwd is
used. ReplaceIfExists is false. It verifies
identity and final slot afterwards. Caller paths, external install destinations,
and replacement of occupied destinations are unsupported.

Two native tests cover a successful two-copy activation and its reverse with
both handles held, wrong source slot/hash, occupied destination refusal, a failed
second rename, and backup restoration after removing the exact test-owned empty
collision directory. Recovery itself refuses an occupied destination, preserving
both images rather than deleting evidence. The fixture explicitly retains the
failure until that test injection is removed; this is not automatic recovery.
These calls do not yet persist a receipt, make the multi-rename sequence atomic,
establish power-loss durability, or update a running installed executable. Hosted
Windows execution initially failed: hosted run 39210214944 on `ba665a24` reported
39 passed, two failed, two ignored on both Server 2022 and 2025. Both new tests
refused their first candidate-to-active rename; the fixed generic error does not
identify which check or native call failed. W1a's tests still passed; its hosted
result does not establish rename support. The follow-up added fixed check-stage
codes and, only for a failed native call, numeric Win32 status; no paths, file
content or OS error strings are printed. No validation is relaxed and no fallback
is attempted. Hosted run 39210740394 on `aa50cd59` narrowed both failures to
`set_file_information`, Win32 87 (ERROR_INVALID_PARAMETER), after the ACL,
identity, link-count and hash checks passed. The next correction changes only the
destination addressing from a directory handle plus relative name to the full
path form described above, retaining all checks and permissions. Hosted run
39211335874 verified exact commit `2f71c4ac0768ef914eb35989611dfc6e6c84510a`
on Server 2022 and 2025: both rename tests passed; the full platform library
reported 41 passed, zero failed, two ignored; the separately invoked
`native_owner_round_trip` outside the runner Job passed with exit zero. The
rename step, full library step and round-trip step all exited zero on both
images. Raw evidence: `.treer/releases/boot-111-2f71c4ac/run.log`.

Each rename-only run prints three Win32 183 refusals, matching the three
intentional occupied-target calls in the collision test (existing active image,
injected directory for candidate activation, and that same directory for backup
restoration). Test output is concurrent: a diagnostic before the other test's
`ok` line does not attribute it to that test. All successful moves unwrap their
result. This verifies only the disposable rename/reverse fixture, not an
installed update or crash recovery. Receipt states and recovery classification
remain the next W1b slice; no real `win_native` machine was used.

### W1b running-image fixture (hosted verified, still inert)

The ignored `windows_update_bootstrap::running_image::native_running_image_rename`
test adds a separate disposable live-image probe. It refuses an ambient Job and
launches only its private checked copy of the platform test executable, with
explicit executable/cwd, empty environment, no inherited handles, no breakaway
and one fixed ignored child test. Before resuming the child it compares the
kernel-reported process image with the retained file identity and digest. The
owned process handle and creation time bind all subsequent liveness checks.

The initial hosted run 39213059404 on `723f772d` failed on both Windows versions
at `create_process`, Win32 32 (sharing violation); the running-image rename was
never reached. The old rename tests, full platform library (41 passed, four
ignored) and separate owner round trip passed on that SHA. The cap-only
`5fd9f0a6` rerun 39213575115 reproduced that failure. Lowering the granted
access through DuplicateHandle may leave the original write-open file object's
sharing record alive (security5251); that is the correction's mechanism
hypothesis, not an isolated measurement of Windows internals.

The successor fixture tightens only its freshly created private active image's
DACL to protected owner/SYSTEM read/execute/delete before dropping the last
write-open object. It derives a metadata handle through
[ReOpenFile](https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-reopenfile),
verifies identical object identity, sets that narrower DACL and reads it back
to require the exact two ACEs and protected flag. This metadata handle shares
writes to coexist with the old write-open object, but has no data-write access.
After closing the old object, an attempted write open must fail while only the
metadata handle remains: this specifically tests the ACL across the transition.
The new read-only execution pin is reopened from that same object, denies write
sharing, permits delete sharing and is re-hashed. Each rename uses a separate
read/DELETE operation handle derived from the pin, with the existing checked
rename pre/postconditions. There is no close-and-reopen by pathname and no
interval where ordinary write opens are permitted. Owner ACL override, hostile
same-user code and privileged bypass remain outside this private fixture's
claim. If an owner nevertheless opens a write-capable handle in the ACL-only
interval, the subsequent execution pin excludes write sharing and must refuse
while that handle remains open; a write completed and closed before the pin
exists is detected by the digest check performed after acquiring the pin.
Thus content acceptance relies on the final pin and its re-hash, not merely
the failed write-open probe. The W1a copy API and static rename fixtures are
unchanged. This is not yet a reviewed ACL policy for existing installed files.
The production candidate needs the same separation from its old write-open
object so the Host can execute the new Controller while the supervisor retains
a no-write-sharing pin. Its ACL transition must follow the eventual installed
directory policy, which remains part of the open production-parent ACL decision.

The child connects to the existing current-user/SYSTEM-only local pipe; the
parent checks its kernel PID and requires a ready byte. It renames the retained
running image active-to-backup, gets a fresh child reply, reverses backup-to-active
and gets another reply. Both moves use the checked rename primitive, with
identity/path checks and write denial. The child remains alive after its last
reply until bounded cleanup of its actual process handle; its own ten-second
deadline also bounds a lost parent. The parent confirms process termination
before deleting only its exact test files. Failure leaves attempt evidence;
there is no installed image, Controller/Host lifecycle operation or production
entry. The child creates no descendants.

Hosted run 39214676528 on exact `07c3d30cd010e5f48f0596b01f68ded065abd2db`
passed on Server 2022 and 2025: rename tests two passed, full platform library
41 passed/zero failed/four ignored (including the 256 MiB+1 refusal), and both
separately invoked outside-Job tests `native_owner_round_trip` and
`native_running_image_rename` each one passed. All four step exit codes were
zero, with actual test `ok` lines in
`.treer/releases/boot-111-07c3d30c/run.log`. The latter test's assertions cover
child execution, live forward/reverse renames, write denial and confirmed
owned-child termination before exact cleanup. This verifies the corrected
fixture sequence, not the inferred internal reason for the earlier conflict.
It does not verify a running service, Host Job integration, production directory
ACLs, antivirus interactions, receipts or interrupted recovery. No installed
machine or `win_native` run occurred; W4 still needs explicit leader approval.

Before W2 chooses the production parent directory, specify its required ACL and
installer creation policy. The current verifier refuses Administrators mutation
grants as foreign writes, so the disposable private-parent tests do not establish
compatibility with a default Windows service-data directory (security event 5182).

## Historical bounded breakaway probe

First deliver a standalone native Rust diagnostic helper, separate from updater
dispatch. It does not parse update inputs, access enrollment credentials, acquire
transaction locks, replace files, change ACLs/Jobs/services or start an installer.
Its only child is the same verified helper image. No shell/CIM/PowerShell fallback.

Proposed fixed interface: `treer-breakaway-probe.exe --probe`. Its private child
mode is exactly `--probe-child`; other arguments fail before spawning. There are
no executable, command, path, PID, duration or creation-flag overrides. The runner
installs the frozen helper and records its SHA256 in the explicitly approved
`%LOCALAPPDATA%\Treer\self-update-probe-111` temporary directory. The helper
derives LocalAppData through the native Known Folder API, ignoring environment
overrides, and refuses images outside that fixed directory. It retains open
non-reparse ancestor directories and its own image without delete sharing,
rejects image hardlinks and compares final handle paths, size and SHA256. After
spawn it opens only the kernel-reported child image path matching its own path
and requires identical volume serial/file index to the pinned image. Same-account
replacement before the approved run remains a residual; the runner checks the
reviewed artifact hash. The helper itself writes no files. The runner
records the actual Host binary version separately before running it through the
real exec-origin Job; a terminal run cannot establish that Job's breakaway policy.

The parent queries its own Job membership, resolves its own image and starts that
exact absolute application with CreateProcessW, fixed child argv, no inherited
handles and an explicit empty Unicode environment block, with
CREATE_UNICODE_ENVIRONMENT. No workload environment is inherited. Request
CREATE_BREAKAWAY_FROM_JOB first, then in a second fixed attempt
CREATE_BREAKAWAY_FROM_JOB | DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP; no
CREATE_NEW_CONSOLE, shell launch or Job-policy changes. The flags are a request,
not proof. Stop at the first failure; this is a diagnostic sequence, never a
policy-changing retry. Exception required by security3182: if CreateProcess is
denied, run one fixed control with only CREATE_BREAKAWAY_FROM_JOB removed. This
control can distinguish a breakaway-specific refusal from general image launch
denial; it cannot make the probe succeed. Verify its image/liveness and clean it
up through its owned handle too. Report `breakaway_denied` only when this control
starts and is fully cleaned up; otherwise preserve `child_create` and the original
Win32 error, with a nested control report. Control cleanup uncertainty takes
precedence as `cleanup_unconfirmed`. Stop after this diagnostic; do not try the
next breakaway flag combination. The child only waits for ten seconds and exits; it spawns nothing,
prints nothing and never enters updater code. There is no child-ready assertion
from a message or PID file.

Retain the actual PROCESS_INFORMATION process handle, close the thread handle,
and use that process handle to query Job membership, PID and creation time while
checking the child is still alive. IsProcessInJob with a null Job handle checks
membership in any Job. Success requires the child is alive, every query succeeds,
the parent was actually in a Job, and the child is outside all Jobs. Parent not
in a Job is `not_exec_job_evidence`, never deployed-Host acceptance. A failed
create or child still in a Job cannot authorize activation.

Always terminate only the owned child process handle, then wait at most five
seconds for that same handle to signal. Never reopen a PID for cleanup. An
already-exited child is still waited on through its retained handle. Query or
cleanup failure produces a fixed failure, never a successful report. The child's
ten-second self-exit bounds its lifetime if the parent dies; this does not claim
the production supervisor can survive a real Controller restart. The launcher
also imposes a twenty-second overall probe deadline and reports an interrupted
probe as unknown, not success.

Emit exactly one bounded JSON report on parent stdout: schema version, result,
parent PID, nullable child PID/creation time, requested flags, nullable parent
and child Job membership, cleanup-confirmed boolean, fixed error stage and
nullable numeric Win32 error. No paths, argv, environment, credentials, raw child
output or exception strings. Only `breakaway_observed` with confirmed cleanup
exits zero; all other results exit nonzero. Freeze the precise field/result
whitelists with the implementation before live use.

Development implementation: standalone example
`crates/treer-platform/examples/treer-breakaway-probe.rs`, with Win32 FFI isolated
under its `support` module. This is not linked into a Controller command. The
runner records Host version using the existing read-only Host query and joins
that evidence with the helper report; the helper emits `host_version: null` and
`host_version_source: runner_existing_host_query`, not a fabricated version or
an ambient-environment value. The report also includes this helper's own SHA256
and size. Parent Job member enumeration is bounded to 1024 entries; an incomplete
query fails closed and no other process IDs are output. A failed parent limit
query has the fixed `parent_job_limits_unavailable` stage with null flags. Access
denied at CreateProcess is `breakaway_denied` only after the fixed control
starts and is cleaned up; otherwise it remains `child_create`. Termination
failure or an unconfirmed wait is
`cleanup_unconfirmed`, never retried through another API. The current Job may be
one level of a nested tree, so its limit flags are not claimed to enumerate all
ancestor limits; the independent child query still checks membership in any Job.

The implementation report keys are fixed: `schema`, `result`, `parent_pid`,
`host_version_source`, `host_version`, `windows_build`, `helper_sha256`,
`helper_size`, `parent_in_job`, `parent_job_limit_flags`, `attempts`,
`error_stage`, `win32_error`. Each attempt has `flags`, `child_pid`,
`child_creation_time`, `child_in_job`, `child_in_parent_job`, `cleanup_confirmed`,
`result`, `error_stage`, `win32_error`, `control`. The nullable `control` has the
same process fields without another `control`, with result `control_started` or
`failed`; it never reports `breakaway_observed`. Root/attempt results are `failed` or
`breakaway_observed`; diagnostic failures are distinguished by fixed error stage
(including `not_exec_job_evidence`). Output is below 8192 bytes; stdout failure
returns nonzero without a raw error message. Invalid argv fails before hashing
or spawning. A Windows build and native tests remain mandatory before live use.

Development verification commands (use the one agent-owned Cargo target and
DEBUG=0): `cargo test -p treer-platform --example treer-breakaway-probe` runs the
portable report whitelist tests on macOS/Linux and additionally exercises native
owned-child cleanup/create failure on Windows. On a native Windows worker
outside any ambient Job, explicitly run
`cargo test -p treer-platform --example treer-breakaway-probe windows::tests::native_job_matrix -- --ignored --exact`
for the isolated no-Job/allow/deny fixtures. These fixtures can change their own
test Job only; their code is absent from the production example. The production
build is `cargo build -p treer-platform --example treer-breakaway-probe --release`;
live argv is only `treer-breakaway-probe.exe --probe`. A native artifact hash,
full author gate, reviewed source SHA and exact #98 paths are still required
before any live run; cross-compilation checking alone is insufficient. Run the
synthetic Job fixtures only on a hosted/disposable Windows worker, never on the
user's win_native machine. That machine receives only the reviewed production
helper's fixed `--probe` command.

The runner must use the actual Host exec account's native Known Folder location,
not its own account's LocalAppData. Native fixtures intentionally differ only in
approved directory (their own executable directory) and child argv (the isolated
test-harness sleeper); those differences are compiled out of the production
example and must not be treated as coverage of the production placement/argv
checks. Keep the release helper small (image bound 32 MiB) and record its size;
the ten-second child budget includes hashing/inspection under load. The runner,
not this helper, enforces the twenty-second total deadline. Injected OS query/
termination failures and actual parent death still require acceptance evidence;
the existing native fixtures do not claim those cases covered.

Validate on native Windows with no Job, a synthetic Job allowing breakaway and
a synthetic Job refusing it; synthetic fixtures may create their own Jobs, but
the live probe never modifies the deployed Host Job. Test malformed argv, create
failure, early child exit, query failure, bounded cleanup and parent death. A
hosted synthetic success does not replace the authorized deployed-Host run.

API basis: [creation flags](https://learn.microsoft.com/en-us/windows/win32/procthread/process-creation-flags),
[CreateProcessW](https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-createprocessw),
[IsProcessInJob](https://learn.microsoft.com/en-us/windows/win32/api/jobapi/nf-jobapi-isprocessinjob),
[TerminateProcess](https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-terminateprocess).
Termination is asynchronous; waiting for the owned handle is mandatory.

## Accepted lifetime scope; first test increment

Leader3236 and security3237/3238 select a supervisor **inside the stable Host
Job**, launched by the Controller daemon rather than an Agent PTY process.
The bounded deployed probe3230 refused breakaway (Win32 error 5, root limit
0x2000); its same-Job control succeeded and was cleaned up. That diagnostic is
complete, not permission to repeat it or change Host policy. No BREAKAWAY_OK,
SILENT_BREAKAWAY_OK, WMI/CIM production fallback or Host upgrade is needed for
the selected scope: replacement of Controller/CLI only. Host updates remain
separate, user-approved operations. A transaction requiring survival of Host
exit is ineligible for this path.

Controller-only restart must leave the supervisor in the existing root Job.
Host exit intentionally kills the supervisor and installer writers. After Host
or OS loss, startup must classify the durable journal (no consumed permit,
synced decision, or partial publication), preserve evidence and never resume an
installer automatically. Death is not a rollback receipt. Publication still
requires individually atomic, staged and checked replacement steps and a
Host-death-at-each-phase acceptance matrix; none is implemented by this increment.

The test-only integration target
`crates/treer-platform/tests/windows_host_job_lifetime.rs` models a disposable
Host owning the root KILL_ON_JOB_CLOSE Job. It kills/waits only its Controller
child and starts a replacement, matching the current Host restart ordering.
It checks retained supervisor/installer process handles remain alive and the
installer continues writing. Separate cases then terminate the fake Host or
supervisor: root closure must kill both descendants; supervisor closure must
kill its nested installer while Host remains alive. Installer creation is
suspended, nested Job assignment precedes resume, and membership checks verify
root membership and exclusion from a sibling Agent PTY Job. Inheritable Job,
pipe and file sentinels exercise `bInheritHandles=false`; Controller death must
leave pipe EOF and release its exclusive file handle while supervisor lives.
The file sentinel represents a handle suitable for stdio; this is not a real
Console/PTY stdio integration test. All roles use CREATE_NO_WINDOW, no inherited
environment except three fixture inputs, and bounded lifetimes.

Only actual CreateProcess process handles are duplicated into the outer
observer; no descendant is reopened by PID for cleanup. The fixture opens its
observer PID solely for DuplicateHandle instrumentation, not production
authentication. The observer owns no Job handles, and short-lived membership
query handles close before the termination phase. Temporary files coordinate
only these owned test roles; they are not updater IPC or identity evidence.
This fixture models the Host ordering; it does not execute the real Host RPC
handler, and future Host tree-kill changes require renewed coverage.

Run explicitly on a disposable native Windows worker outside every ambient Job:
`cargo test --locked -p treer-platform --test windows_host_job_lifetime native_host_job_lifetime -- --ignored --exact --nocapture`.
Do not run it on win_native. A disposable Windows 11 build 22631 worker and the
oldest supported Windows must provide native evidence before acceptance;
cross-target checks and Server 2022 evidence alone cannot claim that coverage.
The operator guide currently names Windows 11 x64 as the initial native target.
This increment changes no production launch path, permit, lock or transaction
behavior. Security reviews its lifetime scope and native evidence before the
second increment wires launch/handoff.

## Increment 2a: owner-side launch and connection primitives

Leader3262 and security3259 accept the Server 2022/20348 and Server 2025/26100
passes for `351bbc02` as development evidence. Build 22631, the supported-version
floor, the real Host restart path and Console/PTY stdio remain final acceptance
items before enabling activation, installer authority, deployment or retirement
of the PowerShell updater. No new live-machine test is part of this slice.

`windows_update_bootstrap` is an unwired, private, test-build-only platform adapter.
Downstream crates cannot call it until directory trust and the daemon-owned
handoff have been reviewed and accepted. It
pins a non-reparse local fixed-drive NTFS image and every ancestor without delete sharing,
refuses hardlinks/alternate data streams, checks the expected SHA256 and bounds
the image to 256 MiB under leader5245 (the earlier hosted slices used 128 MiB).
The expected digest's authenticity,
selected installation/enrollment and provenance of the old Controller copy must
come from a future trusted preparation coordinator; this adapter cannot prove
them from a path/hash supplied by a caller. It does not use a downloaded image
or PATH implicitly.

The immediate job directory and image must both be owned by the current token's
user SID. `GetSecurityInfo` reads their owner and DACL through the retained,
non-reparse handles. A null DACL, invalid ACL, unsupported ACE type or any allow
ACE granting mutation rights to a SID other than that user or SYSTEM is refused.
Mutation includes generic write/all, file write/append/EA/attributes, child
deletion, deletion and owner/DACL changes. The conservative check also rejects
foreign inherit-only grants and grants denied elsewhere; it does not approximate
Windows effective-access evaluation. The directory must contain exactly the
pinned image, with no extra files or subdirectories. These checks run again
before launch and connection proof. They do not isolate the current account or
SYSTEM: either can change its own ACL/content between checks. The adapter never
repairs permissions on a supplied path.

The future preparation coordinator must create the job directory atomically with
`CreateDirectoryW` and explicit `SECURITY_ATTRIBUTES`: owner is the current token
user SID, with a protected DACL granting only that SID and SYSTEM. Creating it
first and repairing security later, as the disposable test fixture does, is not
a production preparation strategy. Set the owner explicitly even for elevated
tokens, whose default owner may be the Administrators group. Default inherited
ACLs under `%LOCALAPPDATA%` may grant other groups write access and will correctly
fail validation. Choose a location where this exact private descriptor can be
installed; do not loosen validation to accommodate inherited permissions.

The supervisor must not write scratch files into its image/working directory:
the one-image content invariant remains active at later verification points.
Any future scratch storage needs a separate job-private directory with the same
ownership and DACL validation and an explicitly reviewed content policy.

Native fixtures create a separate current-user-owned directory with protected
current-user/SYSTEM DACLs and one copied helper image. Cases reject world-write
grants on the directory or image and an extra DLL entry, including a change after
pinning. Fixture cleanup names only its two exact files and empty directory;
there is no recursive deletion. Native results are still required for these
checks; cross compilation is not execution evidence.
Additional caller-slice negative coverage remains required for Administrators
and Authenticated Users write grants, conditional/callback ACEs, a null DACL,
foreign directory ownership and a deny-then-allow grant (which must still refuse).

The owner endpoint generates a 128-bit selector using the Windows system CSPRNG
and derives one fixed private pipe name from its 32-character lowercase hex
encoding. Fixed-selector binding is private for adversarial fixtures. The selector
is visible in the child's command line and is not a secret or authorization token.
Existing pipe names fail (FIRST_PIPE_INSTANCE);
the existing native pipe adapter supplies the current-SID/SYSTEM-only DACL,
rejects remote clients and never impersonates them. The endpoint consumes
itself on one launch/connection attempt, including refusal or cancellation.
A same-account process that learns the selector can connect first and force an
abort. This is an accepted availability limit, not account isolation; never add
reconnect/retry logic that weakens the single-connection identity check.
The fixed production child argv is `--self-update-supervisor SELECTOR`; there
is **no such Controller entry point yet**. CreateProcess receives a fixed
absolute image, its pinned parent as cwd, an empty Unicode environment,
`bInheritHandles=false`, and CREATE_SUSPENDED/CREATE_NO_WINDOW. No breakaway,
shell, WMI/CIM, inherited observer handle or temp-file handle publication exists
in the adapter.

Before resume, and again on connection, the owner checks the actual retained
process handle's creation time, liveness, image volume/file identity and digest.
Bounded current-Job membership enumeration proves that the child is in the
caller's Job; KILL_ON_JOB_CLOSE is required and both breakaway flags refused.
This is **not proof that the caller is the Controller daemon rather than an
Agent PTY process**. Only the future daemon-owned coordinator may call it;
no CLI/request-handler call is wired. A five-second deadline is not renewed
after launch. Image opening, launch-time pin checks and post-connect image
verification use Tokio `block_in_place` on an explicitly required multithread
runtime; a single-thread runtime is refused before work starts. Other tasks and
timers can progress on a replacement worker. Native filesystem/hash calls cannot
be preempted, but expiry is rechecked before returning any connection proof.
Ownership stays on the calling task throughout the blocking section: cancellation
is observed at the next yield, with no detached worker that could spawn a child
after cancellation or outlive its cleanup guard. A bad client
PID, timeout or cancellation terminates/waits the actual owned child handle.
Drop cleanup is best effort, not a receipt; explicit abort confirms cleanup
or returns a fixed failure. No PID is reopened for cleanup.

The connection proves only that the owner's pipe client is its exact launched
image/process. It has no raw-stream, permit, ACK or dispatch accessor. Child-side
server/OS-parent attestation, mutual job/receipt binding, supervisor-owned locks
and synced once-only decision are still required before a complete handoff can
exist. These are the next reviewed slice, not implied by this type's name.

Windows native unit tests cover pipe-name squatting and image-digest conflict.
They also assert the exact UTF-16 production command-line bytes independently
of the minimal libtest helper argv.
The ignored `windows_update_bootstrap::tests::native_owner_round_trip` additionally
exercises verified connection/abort, a same-SID wrong-PID pipe client, and
cancellation while the actual owned child remains alive; retained duplicate
observation handles must signal after refusal/cancellation. It creates its own
root Job and must start outside ambient Jobs on a disposable worker, never
win_native. Test-only child argv invokes a libtest helper via `--skip SELECTOR`
with the same empty environment; that helper connects or waits, spawns no work,
and exits within ten seconds. The test root Job is intentionally held until
the isolated harness process exits. This models no production mutual handoff.

## Increment 2b1: inert child-side parent attestation

Security3281 accepts the exact `a2196422` hosted results for development-only
increment 2a. Leader3283 permits further inert, test-only increments on security
acceptance plus hosted native evidence. Authority-bearing callers, activation,
installer authority, a first live-machine run and deployment remain separate
decision points. No production caller is exposed by this increment.

The private child adapter queries `GetNamedPipeServerProcessId` and compares it
with the current process's OS parent from one process-only ToolHelp snapshot.
Enumeration is bounded to 65,536 entries and refuses failures without retry.
It opens that OS-derived parent PID exactly once with query/synchronize rights,
never a PID received in a frame. The opened parent's creation time must precede
the child's creation time: a reused PID after the real parent's exit cannot pass.
The parent handle and its non-reparse, read-pinned executable file remain owned
by the proof. There is no PID reopen, process termination or impersonation on
the child side.

The parent's **installed** image handle permits `FILE_SHARE_READ |
FILE_SHARE_DELETE`, while continuing to deny write sharing. It must not block
the later publication rename of the installed Controller. The immutable job-copy
handle still denies delete sharing. A native ordinary fixture renames a separate
disposable image while the parent-image handle remains open, checks the retained
file identity and digest through the renamed path, and verifies that writing to
the renamed file still fails. This is file-sharing evidence, not a real running
Controller publication test. A removed/replaced lookup path during later
registration re-verification still refuses; this primitive does not reconstruct
a session after publication or authorize retries.

Registration comparison requires exact equality of parent PID, creation time,
image volume/file identity and SHA256. It then rechecks the retained process's
liveness and image identity/digest, including liveness after hashing. This proves
only that registration fields match the kernel-attested parent at the check;
it is not an authenticated transaction, mutual receipt or execution permit.
ToolHelp parentage does not isolate other processes in the current account.
It records the creator-selected parent PID: a creator with the necessary process
rights can set a parent-process attribute. The trusted Controller creation path
and private image directory are therefore essential; parentage alone is not
stronger isolation from the current account.

The native fixture uses one fixed 56-byte registration observation and one-byte
success marker, with absolute five-second deadlines, solely to exercise this
adapter. It retains the proof during the inert child lifetime. Five rejection
cases independently alter PID, creation time, volume, file identity and digest;
each must disconnect without a success marker, after which the owner confirms
cleanup using its original process handle. An ordinary test rejects a pipe
server in the child's own process instead of its OS parent. These tests need
new hosted native execution; cross compilation alone is not runtime evidence.

This fixture transport is not a production protocol. The next sub-increment
still needs attempt/intent/registration digest binding, fresh one-shot challenge,
bounded typed frames and rejection of unknown/duplicate/out-of-order frames.
The production registration must match the exact durable registration record
and its digest, not just an owner's report over the pipe. Parent liveness and
identity must be verified again at permit acceptance and immediately before ACK;
the state-machine fixtures must test both points.
Supervisor-owned locks, durable once-only permit/decision and installer Jobs
remain subsequent reviewed work. The additional directory negatives, atomic
production preparation and daemon-only construction gate in security3281 remain
required before exposing any caller.

## Increment 2b2: test-only challenge and frame state

Security3291 accepts the sharing-mode correction subject to exact native evidence.
Release3293 and the checked logs show `9f683383` passing eight ordinary tests and
the outside-Job native round trip on Server 2022/20348 and Server 2025/26100.
This satisfies that development-only condition; final product gaps remain.

`update_binding` is private and compiled only for tests on all platforms. It
models a fixed 118-byte frame: magic `TRUB`, version byte `1`, one tag,
16-byte attempt, 32-byte intent digest,
32-byte registration digest, 16-byte owner challenge and 16-byte child challenge.
The ordered exchange is owner Hello, child Reply, owner Finish, child Ack.
Both sides reject empty attempt/intent/registration bindings and require exact
magic, version and attempt/intent/registration binding on every frame;
Reply echoes the owner's challenge, and Finish/Ack echo both. The first Hello
requires an empty child challenge. Zero challenges and reflection of one side's
challenge back as the other's are refused. Each modeled exchange receives an
absolute monotonic deadline once, never renewed by a valid frame. Wrong-length,
unknown, duplicate, out-of-order, mismatched or expired input permanently refuses
the exchange, including an extra frame after completion. Completion is only a
diagnostic state; it grants no permit or transaction authority.

The Windows fixture runs this exchange after the 56-byte parent observation and
success marker from 2b1. Each side obtains a fresh 128-bit nonce from
`BCryptGenRandom` using the system-preferred RNG; failure or all-zero output
refuses without retry. The same primitive generates private endpoint selectors.
The fixture uses fixed-size reads/writes and one absolute five-second budget for
the whole four-frame exchange. Parent/child handle and image verification runs
again on each received frame before responding. Registration observation and
launch retain their own existing fixture phase budgets; no production total
transaction deadline is claimed by this test helper.

Crucially, fixture attempt/intent/registration inputs are synthetic: the attempt
comes from the test selector, intent is a fixed fixture digest, and registration
is the digest of the checked 56-byte parent observation. They are not durable
preparation records, and nothing may promote the model's `complete` state into
authority. A subsequent slice must independently read and validate the exact
durable attempt/intent/registration before constructing a production exchange,
tie the authenticated process proofs to that record, and specify the final
parent/child rechecks at permit acceptance and before ACK. No locks, journal,
once-only permit, durable decision, installer or daemon caller exists here.

Pure state-machine tests run locally on macOS and in Windows test builds. They
cover the ordered exchange, every mutated byte of the initial frame (a mutated
first owner challenge is detected when echoed back), wrong lengths, duplicate
frames, reordering, cross-session replay and deadline expiry without recovery.
The Windows native fixture needs a fresh exact-SHA hosted run for the real pipe
exchange; the prior `9f683383` results do not cover this new protocol model.

## Proposed next slice: independent durable binding in disposable fixtures

The next inert substep, `update_registration`, models registration bytes only
under `cfg(test)`. Its version-2, 200-byte fixture layout is little-endian: distinct
`TURF` magic, version `u16`, total size `u16`, attempt (16 bytes), intent digest (32),
owner and child identity (56 bytes each: PID `u32`, creation `u64`, volume `u32`,
file identity `u64`, image digest (32)), boot identity (16), then start/expiry
ticks (`u64` each). The earlier version-1 fixture is rejected, not migrated.
Parsing rejects noncanonical headers/lengths, empty bindings and invalid process
pairs. A separate comparison requires all fields to match independently supplied
synthetic observations before the existing deadline model admits the window.
The owner still supplies its original expiry ceiling. No bytes enter the pipe
exchange, and no file, clock, process observation, real record, lock, permit or
production caller is added. Exact field equality is not proof of provenance,
process liveness or once-only use; copied bytes with identical observations are
not a replay fence. The nonzero boot identity must match the independent
observation and local clock sample at validation. The returned private deadline
wrapper checks the boot on every subsequent clock sample and permanently refuses
after a mismatch, even if ticks remain in range or a later sample returns to the
original identity. These synthetic tests reject otherwise matching prior-boot
records; they do not acquire or authenticate an OS boot identity. Native
registration must establish a trusted, cross-process Windows boot identity and
pair it with locally acquired clock samples before any use; a peer-supplied boot
value is insufficient. A private synthetic `ClockSource` now models acquisition
order: read boot, read tick, read boot again, and accept only matching nonzero
boot identities. Either boot read or the tick read failing refuses the sample,
without retry, fallback, or a separately sampled tick for the remaining budget.
Deadline checks through this source permanently refuse on acquisition failure,
just as they do on a changed boot; a later successful read cannot renew them.
Tests script the exact read order, every failure point, mixed/empty/different
boots, both deadline reader paths, and the original owner ceiling. This does not
select or call a Windows boot API: trusted OS acquisition, Fast Startup,
sleep/hibernate and VM snapshot semantics still require native review and
real-machine validation. Native publication and authenticated
process binding remain subsequent authority-reviewed work. This is not a stable
on-disk format.

### Native clock-source checkpoint (2026-10-19)

The accepted coherent-acquisition model (`5bfd9a69`, issue comments 4828,
4834 and 4839) does not settle the boot-source contract. The next native
experiment must distinguish an observed OS value from a supported security
guarantee. A successful hosted query cannot by itself establish freshness after
reboot, Fast Startup, hibernation or VM restore.

Microsoft's [Windows time documentation](https://learn.microsoft.com/en-us/windows/win32/sysinfo/windows-time)
states that Win32 `GetTickCount64` elapsed time includes sleep and hibernation.
Keep using the actual Win32 function for the proposed tick source; this statement
is about that API, not a similarly named managed-runtime property. A hosted
elapsed-time test still does not exercise a machine power transition.

The candidate `NtQuerySystemInformation` boot-environment query needs a separate
compatibility and trust review before implementation. Microsoft's
[function contract](https://learn.microsoft.com/en-us/windows/win32/api/winternl/nf-winternl-ntquerysysteminformation)
warns that the function and returned structures are internal and can change,
requires dynamic resolution, and does not document a boot-environment identifier
contract in its listed information classes. This is not evidence that no query
exists; it means the reviewed public contract does not yet justify treating a
candidate GUID as the model's trustworthy boot identity. Do not copy an inferred
class number or structure layout into the updater as a supported ABI. Likewise,
the [BCD identifiers](https://learn.microsoft.com/en-us/windows-hardware/drivers/devtest/boot-options-identifiers)
identify boot configuration entries; `{current}` identifies the running loader
entry, not a documented fresh identifier for every kernel boot. Neither a BCD
entry nor wall-clock boot-time arithmetic is an approved fallback.

For this proposed clock model, a new epoch means a full kernel-session restart,
not every user-visible startup. Hibernate resume and Fast Startup may continue
the existing kernel session; their elapsed time must still consume the original
five-second window. This is a proposed source contract, not validated native
behavior. Before accepting any candidate, a dedicated-machine experiment must
show different epoch values across two real full restarts, report equality only,
and separately establish the intended Fast Startup and hibernate behavior.
An identifier that remains constant across full restarts is rejected, even if
boot-before and boot-after always match. Agreement by two fixture processes
establishes consistency only, not authenticity or restart freshness.

A documented alternative to evaluate is a verifier-generated random epoch in a
volatile registry key. Microsoft's
[RegCreateKeyExW contract](https://learn.microsoft.com/en-us/windows/win32/api/winreg/nf-winreg-regcreatekeyexw)
ties volatile-key loss to hive unload, specifies full shutdown for HKLM, and
notes that user-selected shutdown normally uses Fast Startup. It also says the
volatile option is ignored for an existing key, and key creation initially
creates no values. Therefore this API alone does not establish a trustworthy,
initialized epoch. Do not treat opening an existing key as proof of volatility
or silently repair a missing/malformed value by generating a replacement.

Before selecting that alternative, review a fixed registry view and trusted
parent/key ownership and DACL, refusal of unexpected existing objects, the
single initializer and concurrent-reader protocol, and crash-before-value-write
behavior. A protected HKLM key would require an authority-bearing provisioning
step; no such write or permission change is authorized by this docs checkpoint.
An administrator able to replace that state is outside the proposed trust
boundary. VM snapshot restore can restore both registry state and the tick
counter: rejecting observed regression does not detect every replay, and no
snapshot-safe freshness claim follows. These gaps keep this an unselected
alternative, not an implemented fallback.

The proposed next deliverable is a private, test-only capability experiment,
after review of its exact API/ABI and failure behavior. It must acquire each
sample locally as boot-before, Win32 tick, boot-after; require matching nonzero
boot values; retain only the verifier's sample; and refuse unavailable APIs,
failed statuses, unexpected lengths, mixed identities and clock regression
without fallback or budget renewal. Two independently started fixture processes
must agree during the observed session. Output should report only fixed result
codes and equality/elapsed observations, not a persistent machine identifier.
No registration publication, process authority, installer, installed-state read
or production caller belongs in that experiment.
If the experiment uses the internal query, resolve it from the already-loaded
`ntdll` module without DLL search-path loading; refuse unsupported information
classes and any result length other than the reviewed exact ABI. Keep it behind
the test-only boundary and verify its absence from release binaries. API
availability alone cannot pass the restart-freshness experiment above.

Hosted Server 2022/2025 runs can establish availability and ordinary
cross-process agreement on their exact builds. They cannot approve reboot,
suspend/hibernate, Fast Startup or VM snapshot semantics. Before those tests,
post an `[action-leader]` request with the frozen helper/hash, exact dedicated
machine, supported transition and recovery procedure; no reboot, power-setting
change or snapshot operation is implied by the current task. If a trustworthy
boot source cannot be established, keep native registration unavailable rather
than weakening the boot binding. Live verifier-owned process/image observations,
exclusive registration publication and once-only consumption remain separate
uncompleted requirements.

The first implementation substep is the private `cfg(test)` model in
`crates/treer-platform/src/update_deadline.rs`: a fixed five-second window,
independent reader validation, original-owner expiry ceiling and permanently
refusing backward/expired samples. Its tests supply synthetic millisecond
readings, including a suspension-sized jump; they do not acquire or validate the
Windows clock, publish records, or wire the existing pipe exchange to this model.
Full fixture integration remains subsequent work; the native adapter substep is
described below.

The private Windows-only `windows_update_bootstrap/tick_deadline.rs` substep now
samples `GetTickCount64` into the validated model. It creates a bounded owner
window, independently accepts a supplied fixture window with an optional original
owner ceiling, and rechecks the native clock whenever returning remaining time.
The ordinary adapter test covers ordinary elapsed time and short-window expiry,
future-start refusal and an already expired original-owner ceiling. Thread sleep
does not suspend Windows: actual sleep, hibernation and modern standby remain
unverified, and no power transition is attempted on any machine. Hosted exact-SHA
evidence is required before accepting this native substep.

The subsequent transport substep reuses a `TickDeadline` throughout the positive
owner fixture, beginning before `PinnedImage::open`, bounding launch/connect and
registration exchange, then retaining the same deadline for all four frames.
The child starts its own local window before parent attestation and also retains
it through registration and frame exchange. Every awaited I/O operation checks
the native clock before and after the wait; identity verification is followed by
another clock check, and completion checks both the frame-model deadline and the
native clock. Tokio timers bound waits but cannot establish native-clock validity.
An I/O error or timer timeout permanently refuses that local deadline even if
the last native tick has not advanced to expiry yet; there is no retry budget.
This is partial integration: the two processes still have independent windows;
no window is sent over the pipe, and neither reads a durable record yet. The
existing negative-registration and rogue-client fixture paths retain their
earlier local timeout checks; they do not grant completion or authority.

A separate ordinary native test uses an actual private named pipe between peers
in the same test process, delaying Reply and ACK past the owner's short window
while the child's longer window remains valid. The owner must fail in both
cases. This tests transport/clock refusal, not cross-process authentication.
The child's actual window validity is recorded inside the delayed verification,
before writing. The owner must refuse with its first clock/timer failure in both
variants. The child may either observe pipe closure (an I/O refusal that poisons
its attempt), or finish a buffered ACK write before the owner future is polled
and closes the server. In the latter case its local clock remains valid and its
completion is diagnostic only; it does not establish owner ACK acceptance or
joint completion. Do not require a peer to observe closure synchronously with
the owner's deadline. Private diagnostics preserve the first refusal category;
outward errors remain fixed. The hosted 5ca87485 run demonstrated the overly
strict child-error assertion on both Server 2022 and 2025 (17 ordinary passes,
one failure; the actual-process fixture separately passed), so that SHA is not
accepted as a passing integration run.
`native_owner_round_trip` remains the hosted actual-process proof test. All such
changes require their own exact-SHA native evidence; the prior clock-adapter run
does not cover this integration.

The actual-process native fixture also keeps its original raw-cancellation case
and adds native-clock cancellation while connect is pending and after an owned
future overruns its short budget in `block_in_place`. On the next await, refusal
must drop the owned child and a retained SYNCHRONIZE duplicate of that child's
original process handle must be signaled. The blocking call itself is not
preempted; this fixture proves cleanup at the subsequent cancellation boundary,
not interruption of an arbitrary synchronous Windows API. These short deadlines
are cancellation-test inputs, not newly authorized transaction budgets.

Integration must not treat the child's local window as evidence of the owner's
remaining budget. A corrupted but internally valid record can leave the child
unexpired after the owner's original deadline. Every owner-side ACK/permit
acceptance must remain bounded by that original deadline; the stricter side wins.
Add an integration case where the owner is expired and the child is not: joint
completion must fail and no authority may result. The pure cross-model test now
feeds independent validated window ceilings into the frame model and checks both
a late Reply and late ACK: the owner refuses at its original expiry even while
the child's window remains valid, and replay cannot revive completion. This uses
synthetic clock mapping; native transport integration still needs the same case.
The models expose no authority constructor. Consider an authenticated owner
expiry echo that lets the child further shorten its window; it cannot replace
the owner's own checks or grant extra time. The first native clock-acquisition
slice must check the sleep/hibernation assumption on hosted images where feasible
and explicitly report any suspend mode or behavior that could not be tested.

This is a design checkpoint, not an implementation or authority grant. The
existing Unix registration writer in
`crates/treer-agent-server/src/self_update/launcher_invocation/start/registration.rs`
already binds intent bytes, actual child/owner process images and a fresh attempt,
then writes/syncs registration before constructing its in-memory binding. Its
Unix file/session identities cannot simply be reused on Windows. The proposed
Windows slice remains private/test-only and creates records only under a fresh
disposable fixture directory; it reads no installed configuration or transaction.

Proposed layout: a private fixture root contains `image/` and `records/`. The
`image/` directory contains only the pinned Controller copy and remains its cwd.
The `records/` directory contains only fixed intent and registration filenames;
neither paths nor filenames arrive over the pipe. The child derives the fixture
root from the retained final path of its OS-observed executable, pins all directory
handles and verifies that `image/` and `records/` are siblings under the same
private root. The entire chain must satisfy the same owner/protected-DACL and
non-reparse rules. No record path comes from pipe data, environment or argv beyond
the fixed selector. Directories are created atomically with
explicit owner/protected DACL descriptors, including in the new fixture. This
also makes the pending foreign-owner, BA/AU, conditional/callback, null-DACL and
deny-then-allow negative cases part of preparation acceptance, not a request to
weaken the one-image invariant.

The `records/` contents must equal the exact fixed filename set expected at each
publication phase, and exactly the two record names once published. Reject extra
entries and reparse points. Recheck at write time, before resume, during the
child's independent read, before Hello/Reply and before exchange completion.
Records use a separate fixed-size binary schema with distinct magic, version,
declared lengths and fixed fields; reject unknown versions, mismatched lengths
and trailing bytes. No free text or JSON parser handles these bytes, and record
bytes cannot be parsed as pipe frames (or vice versa).

The owner must retain a suspended child before publishing registration: split
the current spawn operation into a private suspended-process guard and a
consuming resume step. The guard owns the original process/thread handles and
cleans up on every failure. Capture the child creation identity and image from
those handles, derive the owner identity from the running owner process, and
record both. Create each record exclusively with its explicit private security
descriptor; bound the exact bytes, write fully, sync, close the writer, reopen
read-only without write/delete sharing, and read back identity, bytes and hash.
Any uncertainty preserves the fixture evidence and refuses resumption. No
production durability/rename claim follows from a successful file flush alone;
the native record-publication contract must be reviewed and tested separately.

Only after read-back verification may the owner resume the child. The child
opens the fixed records independently, checks size/version/allowed fields and
exact intent digest, and verifies the registration's child identity against
itself and owner identity against `ParentProof`. The owner checks the same
registration against its retained child and own identity. Both construct the
exchange binding from the exact locally read attempt/intent/registration bytes,
never a digest or record supplied by the peer. Copied records referring to a
different process, attempt or image must fail before Hello/Reply.

The fixture design should replace per-phase timing with one handoff deadline
chosen before image validation and child creation, covering record preparation,
resume, parent attestation and exchange. Cross-process deadline encoding needs an
explicit same-boot monotonic clock contract: encode `GetTickCount64` start/expiry
in milliseconds with checked arithmetic. Microsoft's
[Windows time contract](https://learn.microsoft.com/en-us/windows/win32/sysinfo/windows-time)
states that this clock includes sleep and hibernation; this is the required
behavior on supported builds, rather than a clock that excludes suspended time.
If that contract cannot be established for a build, refuse support for the
fixture there; never substitute a working-time-only clock. Refuse backward
readings, arithmetic overflow, a start later than local now, expiry before start,
or a span greater than a hard maximum budget fixed in code. Both child and owner
after read-back compute `effective_expiry = min(record_expiry, local_now + max_budget)`
with checked arithmetic and retain it without extension. A corrupted record may
shorten but must never enlarge the original owner deadline or the code budget.
Recheck this clock after every blocking phase and wake, even if an asynchronous
timer has not fired. Do not serialize Rust `Instant`, use wall-clock time or grant
a new budget on receipt. No deadline extension or restart reconstruction is
allowed. Process creation identity still binds this
record to the original live processes; clock data cannot recreate that identity.

Acceptance cases include missing/truncated/oversized/unknown-version records,
changed intent, changed registration, wrong owner/child creation or image,
copied/duplicate attempts, extra directory entries, sync/read-back failure,
parent death, timeout during each preparation/handshake phase, and cancellation
while the child remains suspended. Each refusal must leave no executing child,
use only its original process handle for cleanup, preserve uncertain evidence,
and produce no authority token. Native hosted runs are required for the file
security, sharing, process and pipe behavior. The current pure model completion
continues to grant nothing.

Implementation evidence must include ordinary tests and exact-SHA hosted native
runs on Server 2022 and 2025. Inject sync/read-back failures at the narrow I/O
boundary where possible, and distinguish injected refusal coverage from actual
OS failures. Report any OS failure that cannot be injected explicitly; successful
flush/read-back tests do not demonstrate power-loss durability.

Locks, real-installation record reads/writes, once-only permit, durable dispatch
decision and installer work remain outside this proposed slice. Their design
review and leader authority decision must precede any production wiring; no
fixture result may silently widen that boundary.

## Authority contract for the subsequent implementation review

- Owner/supervisor communication uses a local private named pipe, restrictive
  current-SID/SYSTEM DACL, remote clients refused and FILE_FLAG_FIRST_PIPE_INSTANCE.
  Verify kernel client/server PIDs against retained process handles and creation
  times, plus image file identity/hash. A pipe name or message PID is no proof.
  Keep bInheritHandles false; do not add anonymous inherited handles casually.
- The owner retains CreateProcess's process handle for lifetime identity. The
  supervisor starts inert inside the stable Host Job, outside every Agent PTY
  Job, before any transaction
  mutation. It acquires installation then enrollment locks itself before staging;
  the owner must not claim that duplicating a mutex transfers thread ownership.
  Moving Windows lock acquisition into the supervisor requires review before
  implementation; it is not an exception to existing Unix handoff rules.
- After verified permit and synced/read-back decision, spawn installer work
  suspended, assign it to a supervisor-owned Job with KILL_ON_JOB_CLOSE and no
  breakaway, then resume. Do not inherit that Job's handle into descendants.
  Confirm zero live Job processes plus closed writers before claiming noexec or
  quiescence; closing a handle or receiving one process exit is insufficient.
- Preserve the once-only permit, bound durable decision before ACK/dispatch,
  absolute deadlines, evidence on uncertainty and no gate reconstruction/resend.
  Native checked image rename, sharing-denial handling, DACLs, lock lifetime and
  restart/rollback need separate small diffs and real NTFS/Job tests. The probe
  grants none of these authorities and never supplies a reusable gate token.

No public Windows self-update caller, updater removal, signed-channel cutover or
Host restart is approved by this note. Breakaway refusal keeps the fixed
`activation_requires_host_terminal` pre-mutation outcome in the intended updater.
