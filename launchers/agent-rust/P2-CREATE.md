# #158 proposed commands for leader — review before execution

Third-attempt plan: second pair stopped on unexpected descendants (P2-RUN.md).
Leader 3564 accepts the matched Railway MCP workload; security 3566 withdraws
3565's mandatory override requirement with the controls below. Leader must
delete/purge the second pair and its exact cwd residue before creating new IDs.
Do not restart either previous pair. Source and snapshot remain unchanged.

For the third pair, pin each full tree before prompting: Node wrapper, runtime,
native Codex and Railway child; Rust launcher, native Codex and Railway child.
Record PID, parent, PGID, executable name, start time and separate RSS for every
process. Recheck deeply at startup, before every cadence block, throughout idle
sampling and teardown. Any additional descendant, changed process identity,
generation, tool/MCP-call item or unexpected file/network activity stops both.
This is matched full-account exposure accepted by leader, not a sandbox claim.

Human transcript checks gate round1 and each six-round cadence block (1–6,
7–12, 13–18, 19–24); no next block without the leader's explicit check recorded
on #158. The observer must pause submissions at block boundaries while keeping
process/state sampling and the hard stop deadline active. MCP RSS is a separate
line, excluded from launcher-only RSS. Combined totals must label all parts.

Detached descendant containment remains a production blocker. For this run,
record exact descendant PID plus start time before stop, then verify all exited.
If any remain, revalidate that exact PID/start-time pair immediately before an
individual SIGTERM; never signal by name or an inherited group. Record each
signal and result, including when a process exits before revalidation. Escalate
an identity mismatch instead of signaling a potentially reused PID. Leader
retains Controller deletion and exact cwd cleanup ownership.

Second-attempt proposal: first pair stopped on the Controller-encoded ID lookup
fault (P2-RUN.md). Security 3544 and Core 3547 pass source fix 3bfa138b;
Core independently ran the two encoded-path regressions. Do not create
another pair until leader completes first-pair deletion/purge and verifies the
names/cwds are absent again. Never restart the old first-pair IDs. Original
snapshot remains unchanged for evidence; commands below select the corrected
snapshot. Both Controller-encoded GET/cancel tests pass on Rust and pinned Node.

Source and command inventory are reviewed; earlier pairs used these commands.
Third-pair IDs and actual process inventory are still pending leader creation.
Only two disposable Agents under `/treer-dev-team/experiments/`, mac_m2,
effective UID 501. No startup/session change to any existing Agent.
See [preflight](P2-PREFLIGHT.md) for Rust, Codex and Treer binary hashes.

The Node command selects the installed official version directly, bypassing its
mutable current pointer. Node native executable SHA-256 is
`68f4d07ca49e0500cc135c7e0a445093e228e42e126ac22306d045f0a8c2636b`.
The installed entry script hash is
`c8b7347838dff416345293f7e27d27514c88efae8f3acf2791b03df6d4d5cf5f`,
runtime.mjs hash `d48b904ecd73833260bc3c5da1624bbdf96575162c056143846cc6b96c12f2e2`.
Both use this installed version's identical public assets.

`CODEX_BIN` below is the **Node launcher's documented production configuration**
for a trusted absolute native executable (launcher README, Windows/portable
provider path section), not a Rust fixture override. Rust explicitly unsets it.
Security 3522 acknowledged this Node-only distinction;
using an npm wrapper on only one side would distort the matched process tree.

Security 3522 closed security source items 1–5; 3524 accepts leader 3521's
default-home decision and replaces the fresh-login condition. Core 3525 closes
all three 3487 blockers on source 0c53b246 after four independent fault tests.
The first-attempt approvals and subsequent delta approvals are recorded above.
Actual inventory and first-turn human observation below still precede the
unattended workload.

The Rust executable was copied from the verified release build to
`/Users/dooms/.cache/treer-158-tUql8b8E/treer-agent-rust-spike-3bfa138b`: directory mode
0700, executable mode 0555, SHA-256
`23cfdc129a671f28b5dfdd4be256791c1e8b2e2284483cf35347a796824699e3`.
The command below uses this fixed snapshot outside the Cargo target. Do not
rebuild into the target, replace the snapshot, or update pinned Node/tools
during the run. Rehash the snapshot, Node entry/runtime, tools and UI assets
immediately before creation and at teardown. Owner permissions are not OS
immutability; any changed hash invalidates the experiment. Retain this snapshot
as evidence until review; it is not a managed-root cleanup target.

Leader decision 3521 accepts full-account exposure and the Node-only production
`CODEX_BIN` setting. Both commands now unset `CODEX_HOME` to use the user's
existing default Codex home in place, with new threads only and no session
import/resume or authentication copying. Security acknowledged this arrangement
in 3524, including the existing home's model/MCP/instructions/tool settings.
Codex itself reads its existing configuration/authentication and writes two new
experiment threads into shared history; we do not inspect or copy those files.
This shared home is outside disposable cleanup: never delete or modify it as
part of teardown. Controller-owned launcher journals remain separate/private.
Nobody on our side reads, lists, copies or prints Codex home contents. Evidence
uses process metrics and only our own journal counts/sizes. Record native thread
IDs from the app-server for the user to archive: both threads remain in shared
Codex history after Controller teardown; we do not remove them ourselves.

## Preparation and creation

The following three paths were absent at 2026-10-17 20:46 UTC. Recheck absence,
physical parents, and no VCS markers before creation. Commands deliberately lack
`-p`: an existing target fails instead of silently adopting data. Preparation is
limited to these new, empty directories; Controller places private state.

```sh
/bin/mkdir -m 700 /Users/dooms/treer-exp
/bin/mkdir -m 700 /Users/dooms/treer-exp/158-node /Users/dooms/treer-exp/158-rust
```

Node baseline (the official portable launcher, native Codex, watcher disabled):

```sh
/Users/dooms/.local/bin/treer agent admin create --machine mac_m2 --kind command --name 158-node --folder /treer-dev-team/experiments/ --cwd treer-exp/158-node -- /bin/sh -c 'set -eu; umask 077; unset CODEX_HOME AGENT_UI_SESSION_ID CODEX_UI_THREAD_ID CODEX_UI_SESSION_ID RUST_SPIKE_CODEX_ARGS; exec /usr/bin/env AGENT_UI_MESSAGE_WATCH=0 AGENT_UI_PORT=0 AGENT_UI_TREER_BIN=/Users/dooms/.local/bin/treer CODEX_BIN=/Users/dooms/.local/lib/node_modules/@openai/codex/node_modules/@openai/codex-darwin-arm64/vendor/aarch64-apple-darwin/bin/codex /Users/dooms/.local/opt/node-v22.23.3-darwin-arm64/bin/node /Users/dooms/.treer/launchers/agent/sha256-56f0ef5e8cc2679282044a86b35ea3b36ff67b5a258dda210b46db1219d4b6a3/scripts/treer-agent.mjs --harness codex --cwd /Users/dooms/treer-exp/158-node'
```

Rust (default release build, no test-only knobs, disposable-only registration):

```sh
/Users/dooms/.local/bin/treer agent admin create --machine mac_m2 --kind command --name 158-rust --folder /treer-dev-team/experiments/ --cwd treer-exp/158-rust -- /bin/sh -c 'set -eu; umask 077; unset CODEX_HOME AGENT_UI_SESSION_ID CODEX_UI_THREAD_ID CODEX_UI_SESSION_ID CODEX_BIN RUST_SPIKE_CODEX_ARGS; exec /usr/bin/env AGENT_UI_MESSAGE_WATCH=0 AGENT_UI_PORT=0 RUST_SPIKE_REGISTER=1 AGENT_UI_CWD=/Users/dooms/treer-exp/158-rust AGENT_UI_PUBLIC_DIR=/Users/dooms/.treer/launchers/agent/sha256-56f0ef5e8cc2679282044a86b35ea3b36ff67b5a258dda210b46db1219d4b6a3/public /Users/dooms/.cache/treer-158-tUql8b8E/treer-agent-rust-spike-3bfa138b'
```

The Controller injects each Agent's own identity and explicit private-state root;
no credential or token value is in argv or the saved command. Before the first
prompt, post returned IDs, roots, actual ports/instances, generation and process
trees. Verify both use the same default Codex home, launcher roots are
private/outside VCS, identical actual model/effort, fresh native thread IDs and
successful authentication without importing/resuming old sessions.

## Time box, stop and teardown

Maximum three hours after creation; hard end **2026-10-18 00:00:00 UTC**, whichever
comes first. Do not begin if review/auth delays leave less than two hours; post a
revised window instead. Use only identical benign text-only/no-tool prompts;
the script sequence and cadence must be posted before the first prompt. Model
and user have full account authority (approval never, danger-full-access); an
empty cwd is not isolation. Leader confirmed this exposure accepted in 3521.
Stop immediately on unexpected activity, uncertainty or any resource bound.
Record run generations before prompts and monitor them throughout. Any
unexpected exit/restart or generation change stops both experimental Agents;
do not let a replacement silently continue the workload. A human must watch
the process tree and transcript for tool items during the first prompts.

Proposed fixed text-only workload: 24 rounds, one per minute after both preceding
submissions complete, identical prompt on both sides: "Without using any tools,
reading files, or making network requests, reply with exactly: experiment 158
round N acknowledged." Replace N with the same round number 1–24. No retries
after uncertain delivery. Then leave both idle until the approved end time,
sampling process RSS/CPU and bounded transcript counts every minute with equal
UI polling. Record actual completion times and stop on any tool item. Model and
effort must be matched and posted before starting; do not assume defaults match.

Exact stop commands (names reserved only for these two IDs; verify IDs first):

```sh
/Users/dooms/.local/bin/treer agent stop /treer-dev-team/experiments/158-node
/Users/dooms/.local/bin/treer agent stop /treer-dev-team/experiments/158-rust
```

Record process trees before and after; verify launcher/provider/CLI descendants
are gone. Do not kill by a name pattern or signal an inherited process group.
Preserve only the experimental measurement evidence outside the managed roots,
then delete these two IDs through Controller lifecycle (which purges their
private state after Proxy-confirmed Agent deletion):

```sh
/Users/dooms/.local/bin/treer agent admin delete /treer-dev-team/experiments/158-node
/Users/dooms/.local/bin/treer agent admin delete /treer-dev-team/experiments/158-rust
/bin/rmdir /Users/dooms/treer-exp/158-node /Users/dooms/treer-exp/158-rust
/bin/rmdir /Users/dooms/treer-exp
```

`rmdir` must fail on unexpected content: preserve and report it, never substitute
recursive deletion. Verify private-root purge (or report deferred purge; do not
manually delete it), save hashes/ports/IDs and teardown observations, and report
any deviation from the inventory. Do not claim nothing else was touched unless
the measured evidence supports it.
