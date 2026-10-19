# #158 second-agent candidate: launcher-dev — preparation only

Leader decision 5648 authorizes preparation, NOT execution. Leader alone executes
ONE switch after an explicit target decision, security review of these exact bytes,
apps-dev's itemized browser answers, and its longer sample. No other rollout.
Read security 5647 and the eventual review before running any lifecycle command.
This is a supplement to frozen afd8f310, not a new Rust build.

## Target, saved startup, and measurement meaning

Target /treer-dev-team/launcher-dev = ag_cfc9d39a455d48d4995faf3d314adf08;
machine srv_9452d9fd5db447d3b242dd66c7689594; Controller cwd treer;
provider cwd /Users/dooms/treer; saved thread 01a12e09-93e9-79a1-9e66-45a25bac49d8.
Prepared against generation 15 after the leader's 18:06Z Node restart. This is
the author agent: the leader, not this process, performs the switch and sampling.
The two failed submissions during the old hang must not be blindly resent;
leader and launcher-dev already reconciled the work in 5644.

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
had only agent-ui.json (52 B), queue.json (1,102,591 B).
Private root below had only empty images/. Revalidate; never assume idle based
on a preparation snapshot. All apps-dev and core-dev backups, roots, locks and
audit directories are untouched and never passed to these commands.

## Prerequisites, manifests, quiet window and process identity

Binary tree /Users/dooms/.local/opt/treer-rust-launcher/afd8f310 is unchanged:
SHA256SUMS 8a8bb492aaaf3ce4845089d9ae64071c6ff983144c93d725bf6a48b5eb247b98;
native 4,150,496 B, SHA256 e6c0cc244bd7055abebcac350443eaf674ee0973c3ff8bc325c1409d0ca471fc.
Supplement /Users/dooms/.local/opt/treer-rust-launcher/launcher-dev-afd8f310-runbook-v1
has a separately pinned SHA256SUMS in the freeze comment. Directories 0555/files
0444 (binary 0555) prevent accidents, not same-user replacement.

```sh
cd /Users/dooms/.local/opt/treer-rust-launcher/afd8f310
shasum -a 256 SHA256SUMS
shasum -a 256 -c SHA256SUMS
cd /Users/dooms/.local/opt/treer-rust-launcher/launcher-dev-afd8f310-runbook-v1
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
audit=/Users/dooms/.local/state/treer/switch-evidence/launcher-dev-afd8f310-attempt-1
test ! -e "$audit"
test ! -L "$audit"
mkdir -m 700 "$audit"
treer agent admin startup show ag_cfc9d39a455d48d4995faf3d314adf08 > "$audit/node-startup.json"
treer agent queue ls ag_cfc9d39a455d48d4995faf3d314adf08 > "$audit/queue-before-stop.json"
```

Record launcher PID+lstart and all provider/tool descendants PID+lstart, including
detached processes. Then stop; verify Controller stopped AND every exact recorded
identity exited. PID reuse/unclear ownership means halt, not kill by name.

```sh
treer agent stop ag_cfc9d39a455d48d4995faf3d314adf08
```

## POST-STOP durable eligibility and stability

Only after verified exit of launcher AND provider/descendants:

```sh
set -eu
umask 077
audit=/Users/dooms/.local/state/treer/switch-evidence/launcher-dev-afd8f310-attempt-1
python3 /Users/dooms/.local/opt/treer-rust-launcher/launcher-dev-afd8f310-runbook-v1/check-node-idle.py \
  /Users/dooms/treer/.treer/agents/ag_cfc9d39a455d48d4995faf3d314adf08 \
  --thread 01a12e09-93e9-79a1-9e66-45a25bac49d8 \
  --private-root /Users/dooms/.local/state/treer/agent-private/srv_9452d9fd5db447d3b242dd66c7689594/ag_cfc9d39a455d48d4995faf3d314adf08 \
  > "$audit/post-stop.json"
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
audit=/Users/dooms/.local/state/treer/switch-evidence/launcher-dev-afd8f310-attempt-1
set +e
/usr/sbin/lsof -nP -- /Users/dooms/treer/.treer/agents/ag_cfc9d39a455d48d4995faf3d314adf08/agent-ui.json /Users/dooms/treer/.treer/agents/ag_cfc9d39a455d48d4995faf3d314adf08/queue.json /Users/dooms/.local/state/treer/agent-private/srv_9452d9fd5db447d3b242dd66c7689594/ag_cfc9d39a455d48d4995faf3d314adf08 > "$audit/lsof.txt" 2> "$audit/lsof-error.txt"
lsof_status=$?
set -e
test "$lsof_status" = 1
test ! -s "$audit/lsof.txt"
test ! -s "$audit/lsof-error.txt"
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

After stop, immediately before first start, leader must list held Core Message
deliveries/submissions for this exact target using its authorized Controller
view, retaining counts and IDs only in queue-before.json. Prompt queue listing
alone is not proof that no Core Messages await delivery. Do not impersonate the
target to read/ack them. If the supported view is unavailable, halt and ask the
Controller owner rather than asserting zero. After start reconcile every held ID
as delivered exactly once; uncertain delivery must never be resent blindly.

No archive/prior-run/sidecar move applies: this is a first import. Any unexpected
Rust file/backup/marker means halt; do not apply the old core-dev helper.

## Import once and remove flag WITHOUT restart

After all checks and manifest reverification, use the target's own Controller
injected credential/private root. Stderr capture OFF, message watcher ON.

```sh
treer agent admin startup set ag_cfc9d39a455d48d4995faf3d314adf08 --cwd treer -- /usr/bin/env AGENT_UI_CWD=/Users/dooms/treer AGENT_UI_PUBLIC_DIR=/Users/dooms/.local/opt/treer-rust-launcher/afd8f310/public AGENT_UI_SESSION_ID=01a12e09-93e9-79a1-9e66-45a25bac49d8 AGENT_UI_PORT=0 RUST_SPIKE_REGISTER=1 /Users/dooms/.local/opt/treer-rust-launcher/afd8f310/treer-agent-rust-spike --import-node-state
treer agent start ag_cfc9d39a455d48d4995faf3d314adf08
```

Require node_state_imported; byte_backup_retained, same saved thread, new
generation/registered interface, ready/open transport, no fence/error. Record
historyScan counts, NEW backup manifest hash and entry hashes matching BOTH
post-stop manifests. No fresh-thread fallback. Then remove the flag, no restart:

```sh
treer agent admin startup set ag_cfc9d39a455d48d4995faf3d314adf08 --cwd treer -- /usr/bin/env AGENT_UI_CWD=/Users/dooms/treer AGENT_UI_PUBLIC_DIR=/Users/dooms/.local/opt/treer-rust-launcher/afd8f310/public AGENT_UI_SESSION_ID=01a12e09-93e9-79a1-9e66-45a25bac49d8 AGENT_UI_PORT=0 RUST_SPIKE_REGISTER=1 /Users/dooms/.local/opt/treer-rust-launcher/afd8f310/treer-agent-rust-spike
```

## Acceptance, per-PID samples and rollback

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

On failure stop Rust and verify exact launcher AND provider exit. Before import,
or failed import with NO published rust-spike.json: original Node files remain
authoritative; preserve failed backup/history/lock/audit, reinstall original Node
startup below and start. If publication/source integrity is unclear, stay stopped.
Do NOT restore or undo a failed unpublished import backup.

After published import, independently validate the NEW backup's identity,
complete manifest and all file size/SHA256 entries. Stop/verify exit first, then:

```sh
treer agent admin startup set ag_cfc9d39a455d48d4995faf3d314adf08 --cwd treer -- /usr/bin/env AGENT_UI_CWD=/Users/dooms/treer /Users/dooms/.local/opt/treer-rust-launcher/afd8f310/treer-agent-rust-spike --restore-node-backup
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
