# #158 ONE apps-dev switch — leader executes after security review

Decision 5559 selects apps-dev instead of another core-dev retry. This is a
runbook supplement to the frozen afd8f310 binary, not a new launcher build or
permission to switch other agents. Security must review these exact bytes first.
The leader executes all lifecycle commands. No prior-run archive or sidecar move
applies to this target; do not run the core-dev helper.

## Identity and quiet window

Target ag_d90bfc482e364cdca643487c3ee362da, /treer-dev-team/apps-dev;
machine srv_9452d9fd5db447d3b242dd66c7689594; saved thread
01a12e92-c3fc-7742-b773-4a9818d249f2; provider cwd /Users/dooms/treer.
Snapshot at preparation: Node generation 6, private root has only empty images/,
source has agent-ui.json and queue.json, no sidecars. Revalidate before action.
Exact target private root:
/Users/dooms/.local/state/treer/agent-private/srv_9452d9fd5db447d3b242dd66c7689594/ag_d90bfc482e364cdca643487c3ee362da.
All core-dev retry-2 leftovers, its backup/history/lock and earlier archives stay
untouched. This procedure uses no core-dev source, private root or audit path.
The startup API's name field was stale (issue-tracker); agent show confirmed the
stable ID and current path. Use the stable ID in every mutation below.

Announce a short quiet window to the team: no prompts/messages to apps-dev until
the switch and first-turn acceptance finish. Do not prompt apps-dev to check if it
is idle. Require its last turn finished, AIS idle, queue empty, no pending
ack/notification/goal edit, attachment or image draft. These are necessary but
not sufficient: the POST-STOP durable check below is mandatory (incident 5216).
An uncertain previous delivery requires reconciliation, never blind re-send.

## Verify immutable artifacts and save original startup

Original binary tree: /Users/dooms/.local/opt/treer-rust-launcher/afd8f310.
Expected original SHA256SUMS hash:
8a8bb492aaaf3ce4845089d9ae64071c6ff983144c93d725bf6a48b5eb247b98.
Expected binary SHA256 (4,150,496 bytes):
e6c0cc244bd7055abebcac350443eaf674ee0973c3ff8bc325c1409d0ca471fc.
Supplement: /Users/dooms/.local/opt/treer-rust-launcher/apps-dev-afd8f310-runbook-v1.
Compare its SHA256SUMS hash with the review/freeze comment, then verify its entries.
Existing original manifest/tree is unchanged. Modes prevent accidents, not
replacement by the same OS user. Repeat both manifest checks immediately before start.

```sh
cd /Users/dooms/.local/opt/treer-rust-launcher/afd8f310
shasum -a 256 SHA256SUMS
shasum -a 256 -c SHA256SUMS
cd /Users/dooms/.local/opt/treer-rust-launcher/apps-dev-afd8f310-runbook-v1
shasum -a 256 SHA256SUMS
shasum -a 256 -c SHA256SUMS
treer agent show ag_d90bfc482e364cdca643487c3ee362da
treer agent admin startup show ag_d90bfc482e364cdca643487c3ee362da
```

Compare the complete startup response to the original below (including absence
of --thread-id). Save it in a NEW private audit directory; abort if the path exists.
Do not reuse this directory for another attempt.

```sh
set -eu
umask 077
audit=/Users/dooms/.local/state/treer/switch-evidence/apps-dev-afd8f310-attempt-1
test ! -e "$audit"
test ! -L "$audit"
mkdir -m 700 "$audit"
treer agent admin startup show ag_d90bfc482e364cdca643487c3ee362da > "$audit/node-startup.json"
```

Original command /Users/dooms/.local/opt/node-v22.23.3-darwin-arm64/bin/node;
argv /Users/dooms/.treer/launchers/agent/run.mjs --cwd /Users/dooms/treer;
Controller cwd treer. The saved thread is loaded from agent-ui.json by Node.

Record launcher PID + lstart and every owned provider/descendant PID + lstart.
Then stop, verify Controller stopped AND all those exact process identities have
exited (including detached descendants). Stop returning is not exit proof;
PID reuse or unclear ownership means halt, not kill by name.

```sh
treer agent stop ag_d90bfc482e364cdca643487c3ee362da
```

## POST-STOP frozen source check — mandatory before import

After confirmed launcher/provider exit, recheck source contains ONLY the two
regular private files, and images/ is empty. Require no Rust journal, backup or
restore marker at the private root; any unexpected state means STOP and inspect,
not archive/delete. Do not remove or replace any existing lock inode.

```sh
set -eu
umask 077
python3 /Users/dooms/.local/opt/treer-rust-launcher/apps-dev-afd8f310-runbook-v1/check-node-idle.py \
  /Users/dooms/treer/.treer/agents/ag_d90bfc482e364cdca643487c3ee362da \
  --thread 01a12e92-c3fc-7742-b773-4a9818d249f2 \
  --private-root /Users/dooms/.local/state/treer/agent-private/srv_9452d9fd5db447d3b242dd66c7689594/ag_d90bfc482e364cdca643487c3ee362da \
  > /Users/dooms/.local/state/treer/switch-evidence/apps-dev-afd8f310-attempt-1/post-stop.json
```

Require exit 0, code=node_idle, queued=running=pendingAck=pendingNotification=0.
Also require attachmentReferences=0; the checker refuses nonempty images/,
sidecars, prior backup/journal/marker and unsafe roots. The output includes a
names/bytes/SHA256 manifest for both source files, retained in post-stop.json
under umask 077 (0600). Match these entries against the NEW backup after import.
Invalid input, missing thread, malformed/duplicate JSON keys or unsafe file
placement also fail. The script prints only fixed codes, counts, sizes and hashes;
it edits nothing and does not check process exit itself. This is a pending-work
check, not a replacement for the importer's full compatibility validation.
Any failure: abort migration, preserve evidence, reinstall/keep original Node
startup and start Node unchanged, then reconcile. Do not delete a running record,
set it done/interrupted manually, or resend. A subsequent attempt needs a new
decision and audit path. Keep the quiet window until this check and import finish.

## Explicit import, same thread, remove one-time flag without restart

Reverify both manifests immediately before these commands. Controller injects
the target's own identity/credential/private root; do not copy environments.
Stderr capture remains OFF; the message watcher remains ON.

```sh
treer agent admin startup set ag_d90bfc482e364cdca643487c3ee362da --cwd treer -- /usr/bin/env AGENT_UI_CWD=/Users/dooms/treer AGENT_UI_PUBLIC_DIR=/Users/dooms/.local/opt/treer-rust-launcher/afd8f310/public AGENT_UI_SESSION_ID=01a12e92-c3fc-7742-b773-4a9818d249f2 AGENT_UI_PORT=0 RUST_SPIKE_REGISTER=1 /Users/dooms/.local/opt/treer-rust-launcher/afd8f310/treer-agent-rust-spike --import-node-state
treer agent start ag_d90bfc482e364cdca643487c3ee362da
```

Require node_state_imported; byte_backup_retained, same thread, new generation,
registered interface, ready/open transport, no fence/error. No fresh thread fallback.
Record import marker, NEW backup manifest hash, generation and historyScan counts.
Then remove the flag without restarting:

```sh
treer agent admin startup set ag_d90bfc482e364cdca643487c3ee362da --cwd treer -- /usr/bin/env AGENT_UI_CWD=/Users/dooms/treer AGENT_UI_PUBLIC_DIR=/Users/dooms/.local/opt/treer-rust-launcher/afd8f310/public AGENT_UI_SESSION_ID=01a12e92-c3fc-7742-b773-4a9818d249f2 AGENT_UI_PORT=0 RUST_SPIKE_REGISTER=1 /Users/dooms/.local/opt/treer-rust-launcher/afd8f310/treer-agent-rust-spike
```

## Acceptance and limits

Leader checks real Console embedded UI AND standalone page: actual frame origin,
same thread, recent window, older page, return to latest. Send ONE read-only
question requiring a tool, verify one user message, one turn/start, streamed and
completed answer, drained queue. Do not retry an uncertain submission.
Record launcher PID/lstart/RSS and journal/history/stderr/temp byte totals at
start, 10 minutes and a few hours. Individual journal/log limit 64 MiB; combined
Rust journal + rotated history + stderr + temp limit 128 MiB per agent. Abort
further rollout on error/fence, unexpected growth or non-settling RSS. Retained
JSON limits are not an RSS ceiling; no 24-hour/whole-team claim. Security reads
import + 10-minute evidence before any second agent; separate decision required.

## Rollback — leader only, preserve all evidence

Before import, or import failed without any published rust-spike.json: stop Rust
and verify exact launcher/provider exit. Node sources were not changed by import;
keep them authoritative, reinstall original Node startup below and start. Preserve
any new backup/history/lock and audit. Do NOT restore/undo a failed import backup;
if publication or source integrity is uncertain, stay stopped for diagnosis.

After import published: stop Rust, verify launcher AND provider exit, independently
validate the NEW backup manifest identity, sizes and hashes. Then restore ONLY
that backup via the target's injected private root:

```sh
treer agent admin startup set ag_d90bfc482e364cdca643487c3ee362da --cwd treer -- /usr/bin/env AGENT_UI_CWD=/Users/dooms/treer /Users/dooms/.local/opt/treer-rust-launcher/afd8f310/treer-agent-rust-spike --restore-node-backup
treer agent start ag_d90bfc482e364cdca643487c3ee362da
```

Require node_state_backup_restored and exit 0 (no provider starts). If either is
missing, STOP and inspect; do not start Node. After success explicitly stop again
and verify restore-only exited before installing the original startup:

```sh
treer agent stop ag_d90bfc482e364cdca643487c3ee362da
treer agent admin startup set ag_d90bfc482e364cdca643487c3ee362da --cwd treer -- /Users/dooms/.local/opt/node-v22.23.3-darwin-arm64/bin/node /Users/dooms/.treer/launchers/agent/run.mjs --cwd /Users/dooms/treer
treer agent start ag_d90bfc482e364cdca643487c3ee362da
```

The last two commands also apply to the pre-import abort branch. Confirm original
thread and ready Node interface. Rust-period accepted work is NOT merged into the
restored Node journal; reconcile real provider delivery without blind resend.
Keep backup, marker, journal, history and lock; no second attempt or another
agent is authorized by this runbook.
