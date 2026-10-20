# #158 launcher-dev retry after historyAuthority — leader only

Prepared after incident7167 and verified Node rollback. This successor requires
Core/security review of the exact commit, gate, artifact and this runbook before
ONE leader-run retry. It does not execute anything or authorize a third agent.
Do not retry afd8f310 or f5c393df, or overwrite prior manifests/audits. The user authorization
to finish migration persists; this review is technical, not another user prompt.
History trust statement: full-history API array order is interpreted as
chronological, as in Node. Supplied numeric startedAt must be nondecreasing;
null/missing timestamps cannot independently prove order. The wire has no
continuous sequence/declared count, so silent unmarked omissions cannot be
detected. Normalization still needs an explicitly recognized idle/active status
and a matched unpaginated request. Resumed/imported histories missing that status
refuse readiness, including zero or one active candidate. Non-null startedAt
must be an integer when a candidate exists; a float/string intentionally refuses
then (actual incident timestamp values were not available in the sanitized audit).
Zero candidates passes bounded projection without rewriting any status; the
identity/order/pagination rules remain unchanged for every candidate frame.
The real failing predicate is unknown: record the successful historyAuthority
counters below; an invalid predicate is a latent refusal on any later candidate
frame, not proof of repaired real-producer normalization. Every inProgress
candidate reserves one conservative extra byte, even when not normalized.
A silent default page limit without any marker is also an unprovable omission.
Record these residuals, not a gap-free claim.

## Target, saved startup, and measurement meaning

Target /treer-dev-team/launcher-dev = ag_cfc9d39a455d48d4995faf3d314adf08;
machine srv_9452d9fd5db447d3b242dd66c7689594; Controller cwd treer;
provider cwd /Users/dooms/treer; saved thread 01a12e09-93e9-79a1-9e66-45a25bac49d8.
Prepared against Node generation21 / PID44096 after incident7167 rollback.
PID is historical: record the current PID plus birth immediately before stopping. This is
the author agent: the leader, not this process, performs the switch and sampling.
The attempt-2 acceptance submission5cfbce65-09b9-4e4f-ac26-9f8b6210e6ee /
turn01a15a95-ff47-7951-942a-9dd87b305f7a was interrupted and visible once after
rollback. Do not resend it or either prior acceptance prompt. Preserve
attempt-1, attempt-2 AND attempt-3 unchanged. Attempt-3 failed before readiness
at historyAuthority with 785 scanned turns, zero inProgress candidates; no
acceptance was submitted. Its original audit and manifests remain evidence.
Use sanitized rust-acceptance-failure.json and node-rollback-verification.json
from attempt-2 for prior diagnosis; do not read provider-native history/config.

Original Node command (retain its explicit --thread-id on rollback):
/Users/dooms/.local/opt/node-v22.23.3-darwin-arm64/bin/node
with argv /Users/dooms/.treer/launchers/agent/run.mjs --thread-id
01a12e09-93e9-79a1-9e66-45a25bac49d8 --cwd /Users/dooms/treer.
Save the current complete startup response before stopping; differences from this
snapshot require reconciliation, not overwriting a newly changed specification.

Read-only comparison found 491 turns / 2,728,777 bytes of projected AIS entries
for launcher-dev (25 pages of 20 turns). These are compact JSON bytes of the
concatenated entries array, excluding tool detail/native frames/provider JSONL;
not an app-server restore-frame measurement or an import eligibility promise.
Web-dev measured 467 turns / 3,703,734 projected bytes (5 pages of 100 turns).
Both measurements kept the reported turn count stable within their scan;
web-dev's earlier scan was discarded when its turn count changed. One-call
1000-turn responses contained exactly 10,000 entries; those results were not
assumed complete and were replaced with the paged measurements above.
Launcher-dev is selected as the smaller projected byte load, despite web-dev
having fewer turns. Leader must accept that candidate choice before execution;
this is not a claim that the native provider frame is smaller.
No provider was started and no user Codex-home/config/auth was read to measure.

Source /Users/dooms/treer/.treer/agents/ag_cfc9d39a455d48d4995faf3d314adf08
is authoritative current Node state. Do not reuse old source hashes: revalidate
only content-free metadata at stop. Prior Rust state/backup/fence is retained
under the same target private root and must be preserved using the stopped-only
helper below before fresh import. Apps-dev is out of scope.

## Prerequisites, manifests, quiet window and process identity

Binary/supplement tree is separately pinned by the successor freeze comment:
/Users/dooms/.local/opt/treer-rust-launcher/history-authority-v1.
Compare binary bytes/SHA and SHA256SUMS SHA with that exact reviewed post.
Directories0555/files0444/binary0555 prevent accidents, not same-user replacement.
This tree contains the new binary, unchanged browser assets, checker, this
runbook and target-specific archive helper; old frozen trees remain untouched.

```sh
cd /Users/dooms/.local/opt/treer-rust-launcher/history-authority-v1
shasum -a 256 SHA256SUMS
shasum -a 256 -c SHA256SUMS
treer agent show ag_cfc9d39a455d48d4995faf3d314adf08
treer agent admin startup show ag_cfc9d39a455d48d4995faf3d314adf08
```

Compare hashes with the exact reviewed freeze before stop AND immediately before
start. Announce a short quiet window to all senders: no messages/prompts to this
agent until accepted. Do not prompt the target to test idleness. Require its last
turn complete, idle, empty prompt queue, no pending ack/notification/goal edit,
attachment or draft. Inspect goal edits locally without logging content.

Use a NEW absent audit directory (never reuse it for a retry):

```sh
set -eu
umask 077
audit=/Users/dooms/.local/state/treer/switch-evidence/launcher-dev-active-history-attempt-4
test ! -e "$audit"
test ! -L "$audit"
mkdir -m 700 "$audit"
treer agent admin startup show ag_cfc9d39a455d48d4995faf3d314adf08 > "$audit/node-startup.json"
# Do not save raw queue output (it can include prompt bodies).
# Through the supported operator view, save counts and only submission id/state,
# sourceMessage/sourceMessages messageId/deliveryId/ackPending plus pending
# notification/attachment counts into queue-before-stop.json (0600).
# Require queued/running/ackPending/notification/attachment counts all zero.
python3 /Users/dooms/.local/opt/treer-rust-launcher/history-authority-v1/check-node-idle.py \
  /Users/dooms/treer/.treer/agents/ag_cfc9d39a455d48d4995faf3d314adf08 \
  --thread 01a12e09-93e9-79a1-9e66-45a25bac49d8 \
  --private-root /Users/dooms/.local/state/treer/agent-private/srv_9452d9fd5db447d3b242dd66c7689594/ag_cfc9d39a455d48d4995faf3d314adf08 \
  --source-only --delivery-audit > "$audit/queue-before-stop.json"
```

Record launcher PID+lstart and all provider/tool descendants PID+lstart, including
detached processes. Then stop; verify Controller stopped AND every exact recorded
identity exited. PID reuse/unclear ownership means halt, not kill by name.

```sh
treer agent stop ag_cfc9d39a455d48d4995faf3d314adf08
```

## POST-STOP durable eligibility and stability

Only after verified exit of launcher AND provider/descendants, take lsof of the
two source files and recursive private tree (see checks below) and require no
holders before the preservation helper. Keep the agent stopped throughout.

### Preserve attempt-3 tuple before a fresh import

Leader executes the reviewed target-specific helper, replacing RECORDED_PID
with the just-stopped Node launcher PID, never the historical PID44096:

```sh
python3 /Users/dooms/.local/opt/treer-rust-launcher/history-authority-v1/archive-launcher-dev-prior-run.py --stopped-pid RECORDED_PID
```

It takes the existing lease inode; moves rust-spike.json, node-backup-v1 and
.node-backup-restored, plus any existing rust-journal-history, empty rust-images
and stderr log, into the new attempt-4/prior-run directory; before/after numeric
size+hash manifests must match. Nothing is deleted. Images must be empty. Unknown
entries, links, shared files, an existing archive or unsafe modes refuse. The
helper is a preservation step, not proof that descendants exited. Keep backups
private and report only manifest hashes/counts. Ordinary interrupted moves roll
back; power loss between renames needs inspection while stopped, not automatic
retry. Never sweep UUID temporaries or another agent's paths.

Then run the durable checker twice:

```sh
set -eu
umask 077
audit=/Users/dooms/.local/state/treer/switch-evidence/launcher-dev-active-history-attempt-4
python3 /Users/dooms/.local/opt/treer-rust-launcher/history-authority-v1/check-node-idle.py \
  /Users/dooms/treer/.treer/agents/ag_cfc9d39a455d48d4995faf3d314adf08 \
  --thread 01a12e09-93e9-79a1-9e66-45a25bac49d8 \
  --private-root /Users/dooms/.local/state/treer/agent-private/srv_9452d9fd5db447d3b242dd66c7689594/ag_cfc9d39a455d48d4995faf3d314adf08 \
  --delivery-audit > "$audit/post-stop.json"
```

Require exit0/code=node_idle and queued/running/pendingAck/pendingNotification/
attachmentReferences ALL zero. Script refuses draft images, source sidecars,
existing backup/journal/marker, unsafe files and thread mismatch; it writes
nothing and prints only codes, counts, names, sizes and SHA256s. This is not full
importer validation or process-exit proof. Any failure aborts migration: reinstall
the original Node startup and start unchanged, reconcile. Never delete/change a
receipt, invent completion or blindly resend. No further attempt without a new decision.

Record UTC times; after at least 10 seconds repeat the EXACT command with output
post-stop-2.json. Require both exit0, and identical queueSha256 AND
threadFileSha256 (the manifest entries must match). Any change means halt.
Run lsof on the two source files and private root, record output to lsof.txt
(0600), require no holders. Example, after verifying /usr/sbin/lsof exists:

```sh
set -eu
umask 077
audit=/Users/dooms/.local/state/treer/switch-evidence/launcher-dev-active-history-attempt-4
set +e
/usr/sbin/lsof -nP +D /Users/dooms/.local/state/treer/agent-private/srv_9452d9fd5db447d3b242dd66c7689594/ag_cfc9d39a455d48d4995faf3d314adf08 > "$audit/lsof.txt" 2> "$audit/lsof-error.txt"
lsof_status=$?
set -e
test "$lsof_status" = 1
test ! -s "$audit/lsof.txt"
test ! -s "$audit/lsof-error.txt"
set +e
/usr/sbin/lsof -nP -- /Users/dooms/treer/.treer/agents/ag_cfc9d39a455d48d4995faf3d314adf08/agent-ui.json /Users/dooms/treer/.treer/agents/ag_cfc9d39a455d48d4995faf3d314adf08/queue.json > "$audit/lsof-source.txt" 2> "$audit/lsof-source-error.txt"
lsof_status=$?
set -e
test "$lsof_status" = 1
test ! -s "$audit/lsof-source.txt"
test ! -s "$audit/lsof-source-error.txt"
```

Missing lsof, permission/error output or any holder means STOP; do not suppress it
or treat failure as proof. Listing is not killing or containment of same-user races.

## Copy rehearsal and held messages

Before live import record rehearsal.txt (0600): done/skipped and fixed reason.
The frozen production artifact requires Controller-injected private-state
binding; apps-dev rehearsal was skipped for that reason (security 5564 accepted).
Do not copy real credentials, run under another Agent identity, change default
Codex home, spawn a real provider or improvise env overrides. If a future supported
offline import-only path exists, it must use disposable 0700 source/private roots,
no Controller/provider, verify source hashes and retain only numeric outcome;
otherwise explicitly SKIP. Existing mock fixture tests do not count as rehearsal
of this real queue. A copy refusal halts live import.

Core6590/security6592 establish that a cross-agent held-delivery view is
unavailable: record that fact and source-boundary reason, NOT zero. Public
prompt queue is not a held-message mailbox. The mandatory supported durable
queue audit and importer both refuse queued/running/ackPending; do not waive
those. Optional SELF-only IDs/counts receive is allowed only in a separate
leader-coordinated window, then repeat idle/stability checks; no impersonation.
Reconcile known delivered IDs across restart once, never blindly resend.

## Import once and remove flag WITHOUT restart

After all checks and manifest reverification, use the target's own Controller
injected credential/private root. Stderr capture OFF, message watcher ON.

```sh
treer agent admin startup set ag_cfc9d39a455d48d4995faf3d314adf08 --cwd treer -- /usr/bin/env AGENT_UI_CWD=/Users/dooms/treer AGENT_UI_PUBLIC_DIR=/Users/dooms/.local/opt/treer-rust-launcher/history-authority-v1/public AGENT_UI_SESSION_ID=01a12e09-93e9-79a1-9e66-45a25bac49d8 AGENT_UI_PORT=0 RUST_SPIKE_REGISTER=1 /Users/dooms/.local/opt/treer-rust-launcher/history-authority-v1/treer-agent-rust-spike --import-node-state
treer agent start ag_cfc9d39a455d48d4995faf3d314adf08
```

Require node_state_imported; byte_backup_retained, same saved thread, new
generation/registered interface, ready/open transport, no fence/error. Record
historyScan counts INCLUDING staleInProgressCount/threadStatus and historyAuthority
(invalidIds, duplicateIds, decreasingTimestamps, nonIntegerTimestamps,
identityValid, orderValid, paginationMarkers, fullHistoryRequest,
normalizationRequired, normalizationApplied, threadStatus), NEW backup manifest hash and entry hashes matching BOTH
post-stop manifests. No fresh-thread fallback.

Require explicit threadStatus idle, same thread, no fence, and recorded counters.
staleInProgressCount may be ZERO: zero candidates must report
normalizationRequired false and normalizationApplied false; this is projection-only startup, not proof of
stale-turn normalization. Before ANY acceptance prompt, require all four numeric
predicate counters zero, identityValid and orderValid true, fullHistoryRequest
true and paginationMarkers empty. Otherwise HARD STOP: do not send acceptance;
stop the exact new Rust process and verify its exits/holders, then perform the
NEW-backup rollback below. Report the counters to Core/security. This is a
latent candidate-frame refusal that would fence after a real turn started.
Do not manufacture a candidate or retry to hide it. Candidate normalization
still needs every original authority predicate. Only after all checks are clean,
remove the import flag below without restarting.
`totalTurns`/hasOlderItems describe observed projection, not proof of complete
history when invalid predicates pass with zero candidates.

```sh
treer agent admin startup set ag_cfc9d39a455d48d4995faf3d314adf08 --cwd treer -- /usr/bin/env AGENT_UI_CWD=/Users/dooms/treer AGENT_UI_PUBLIC_DIR=/Users/dooms/.local/opt/treer-rust-launcher/history-authority-v1/public AGENT_UI_SESSION_ID=01a12e09-93e9-79a1-9e66-45a25bac49d8 AGENT_UI_PORT=0 RUST_SPIKE_REGISTER=1 /Users/dooms/.local/opt/treer-rust-launcher/history-authority-v1/treer-agent-rust-spike
```

## Acceptance, per-PID samples and rollback

Use ONE newly identified read-only acceptance turn after import, not a resend
of the interrupted attempt-2 prompt. Record its new submission/turn ID and verify
exactly one provider dispatch plus one user message/answer. If interrupted,
stop for diagnosis/rollback; do not submit the acceptance again.

Leader verifies real Console embedded AND standalone: same thread, latest50,
older page and return-to-latest, ONE read-only tool question with exactly one
user message and completed answer, drained queue/no duplicate turn dispatch.
Reconcile held message IDs once. Mock/browser happy-path reports do not replace
the itemized checks. Security reviews before any third agent.

At start, 10 minutes, and after normal message-path use over a few hours record
UTC, each PID+lstart+elapsed+RSS separately (never sum into launcher RSS):
`ps -o pid,ppid,lstart,etime,rss -p RECORDED_PID`. Include provider/MCP/tool
children, no process-name matching. Record Rust journal/history/stderr/temp sizes
and totals at each sample. 64 MiB per journal/log, 128 MiB combined Rust journal,
rotated history, stderr and temps. Error/fence/unexpected growth or nonsettling
RSS halts further rollout. Retained JSON is not total RSS; no steady-use/soak claim.

### Retention interpretation (operator addendum; #158 security 5832)

Old v1 remains unchanged; this successor uses the separately reviewed successor
artifact. The retention explanation does not widen its execution scope.
History is disk-only, with at most 64 files and a 67,092,480-byte history budget
(128 MiB minus two 32 MiB live/publication budgets and two 8 KiB stderr budgets).
Before copying a checkpoint, validated oldest sequence files are removed until
both the new file count and byte total fit. Thus an initial MB/hour slope must
not be extrapolated to unbounded growth or a time-to-128-MiB failure.

Apps-dev's 3h41m observation was 21 snapshots / 8,868,647 bytes, not a plateau
test. At the observed 5–6 snapshots/hour and similar sizes, the count bound would
be reached around 11–13 hours; larger snapshots can hit the byte bound first.
For the next leader-run sample around 12–14 hours, record count, total bytes,
oldest/newest sequence names and the same PID identity; compare with earlier
names to verify oldest-first removal. Do not claim a plateau before observing
it. Validated publication temporaries also count toward retention limits.

History is never recovery input or loaded as a full payload into memory. Live
receipts/checkpoints, Node migration backups and images are not pruned by this
rotation. Copies contain private plaintext: the bounds limit retention but do
not eliminate the cost of retaining many full copies. Follow-up #215 evaluates
fewer snapshots/write amplification; no retention setting or runtime change is
part of this addendum. See the README's disk-history operator note for limits
and existing rotation/crash test evidence.

Before NEW import begins, an abort may restore the exact prior evidence tuple:

```sh
python3 /Users/dooms/.local/opt/treer-rust-launcher/history-authority-v1/archive-launcher-dev-prior-run.py --stopped-pid RECORDED_PID --undo
```

Only use --undo before any fresh import writes and if ALL destinations remain
absent. Keep the old failed fence intact; never start Rust from that tuple.
Once a new import begins do NOT undo the old archive or restore its old backup.

On failure stop Rust and verify exact launcher AND provider exit. Before import,
or failed import with NO published rust-spike.json: original Node files remain
authoritative; preserve failed backup/history/lock/audit, reinstall original Node
startup below and start. If publication/source integrity is unclear, stay stopped.
Do NOT restore or undo a failed unpublished import backup.

After published import, independently validate the NEW backup's identity,
complete manifest and all file size/SHA256 entries. Stop/verify exit first, then:

```sh
treer agent admin startup set ag_cfc9d39a455d48d4995faf3d314adf08 --cwd treer -- /usr/bin/env AGENT_UI_CWD=/Users/dooms/treer /Users/dooms/.local/opt/treer-rust-launcher/history-authority-v1/treer-agent-rust-spike --restore-node-backup
treer agent start ag_cfc9d39a455d48d4995faf3d314adf08
```

Require node_state_backup_restored AND exit0 (no provider). Failure means STOP,
not Node start. On success explicitly stop and verify restore-only exit, then
reinstall the exact original startup (last two commands also serve pre-import abort):

```sh
treer agent stop ag_cfc9d39a455d48d4995faf3d314adf08
treer agent admin startup set ag_cfc9d39a455d48d4995faf3d314adf08 --cwd treer -- /Users/dooms/.local/opt/node-v22.23.3-darwin-arm64/bin/node /Users/dooms/.treer/launchers/agent/run.mjs --thread-id 01a12e09-93e9-79a1-9e66-45a25bac49d8 --cwd /Users/dooms/treer
treer agent start ag_cfc9d39a455d48d4995faf3d314adf08
```

Confirm original thread/ready interface; reconcile Rust-period real provider
delivery (not merged into restored Node journal), no blind resend. Keep backup,
marker, journal/history, lock and audit; never clear fences or reuse this audit.
