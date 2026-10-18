# #158 real run inventory — 2026-10-17

Leader created both Agents (3530). Before any prompt, `treer agent admin startup
show` for each exact ID confirmed `/bin/sh -c` and the exact command in
P2-CREATE.md at f1bf2330, including unset CODEX_HOME/resume variables and watcher
off. Rust uses the fixed snapshot, **not** the Cargo target; this resolves 3533's
saved-command consistency question. No startup edit or restart was needed.

| Field | Node | Rust |
| --- | --- | --- |
| Agent ID | ag_3a854453eb4740fb990189f8840a5523 | ag_c129f914467e48b6b6e412c95e91eb65 |
| Tree path | /treer-dev-team/experiments/158-node | /treer-dev-team/experiments/158-rust |
| Controller start UTC | 2026-10-17T20:54:37.046Z | 2026-10-17T20:54:38.620Z |
| Generation | 1 | 1 |
| AIS loopback port | 58752 | 58858 |
| Instance | agent-ui-codex-ag_3a854453eb4740fb990189f8840a5523-34065 | rust-spike-9dbcebfa-d17b-42ec-8b79-b66631c48511 |
| Native thread ID | 01a14ba5-280b-7661-9a75-b50a08a64bb6 | 01a14ba5-2e61-77a2-88c3-4b62dcdb56ea |
| Actual model / effort | gpt-6.1-sol / medium | gpt-6.1-sol / medium |
| Cwd (0700) | /Users/dooms/treer-exp/158-node | /Users/dooms/treer-exp/158-rust |
| Own private-root suffix | ag_3a854453eb4740fb990189f8840a5523 | ag_c129f914467e48b6b6e412c95e91eb65 |

Both private roots are below
`/Users/dooms/.local/state/treer/agent-private/srv_9452d9fd5db447d3b242dd66c7689594/`,
0700 owned by dooms (UID501). Both CODEX_HOME variables are unset; default
`/Users/dooms/.codex` exists, mode0755. Only directory existence/mode inspected;
no home contents read/listed/copied. Provider-created native thread IDs above
remain in that shared history for the user to archive after teardown.

Pre-prompt process tree (PID / parent / process group / RSS KiB / CPU time):

| Role | PID | Parent | PGID | RSS KiB | CPU |
| --- | --- | --- | --- | --- | --- |
| Node wrapper | 34065 | 49727 | 34065 | 34288 | 0:00.10 |
| Node runtime | 34069 | 34065 | 34065 | 53184 | 0:00.60 |
| Node Codex | 34079 | 34069 | 34065 | 72640 | 0:00.30 |
| Rust launcher | 34134 | 49727 | 34134 | 5664 | 0:00.06 |
| Rust Codex | 34149 | 34134 | 34149 | 71728 | 0:00.22 |

This is a startup sample, not a long-run result. Report Node wrapper and runtime
separately and together; Codex costs are separate. RSS sums double-count shared
pages. Live Node heap/native/buffer attribution is not instrumented; do not
derive heap usage from RSS or silently modify the frozen baseline to obtain it.

All six executable/script hashes in P2-CREATE/PREFLIGHT rechecked before prompts
and match. Shared installed UI directory is the pinned version's `public/`:
4 files; aggregate SHA256
`c3474cf098b5d6ba964ce990983d55f8c5f998bd8e66d467d1837ec88f0ca242`.
Algorithm: sorted root-relative file path (leading slash), NUL, file bytes, NUL,
concatenated into SHA256. Both serve those exact assets unchanged.

Workload is the 24-round text-only sequence in P2-CREATE.md (one round/minute,
after both previous completions, followed by idle observation). First round is
manual and its transcript checked before cadence. No retry after uncertainty;
stop both on unexpected tool/file/network activity, failure or generation change.
Actual creation time makes the earlier +3h end **2026-10-17T23:54:37Z**; use
that instead of leader's approximate 23:58Z or the later 00:00Z hard end.
Leader owns final stop/delete and rehash; request it at the earlier end or fault.
No real prompt had been sent when this inventory was written.

## First attempt stopped on receipt lookup fault

First pair submitted 20:58:40Z. Both completed with exactly
`experiment 158 round 1 acknowledged.`; both transcripts inspected, no tool
items. Node's Controller wait returned `done`; Rust's wait failed with 404
`submission_not_found` although the native turn completed. No prompt retried.
Stopped both exact IDs through Controller at 20:58:52.870Z (Node) and
20:58:53.011Z (Rust), generation1. Post-stop `ps` found none of the five
recorded PIDs. No unattended cadence, long-run samples or savings claim.

Rust own journal was 1226 bytes: one durable `done` submission,
`1c641e5c-c628-4e0a-97d5-e35962a98982`, operation
`cmd_ff7d0e26b769446e96057db7a71ee455`, turn
`01a14ba8-dcc8-77a3-80c8-d221e4910ad4`. Its immutable admission receipt had
the same ID and disposition `sent`; assistant preview matched the expected
reply. Node root had only an `images` directory, no top-level journal. These
observations concern only the two disposable private roots, not Codex home.

Diagnosis: Controller's existing percent-encoder escapes UUID hyphens (`%2D`)
in submission GET and queue DELETE paths. Rust compared the raw URI segment
against the unescaped stored UUID; direct fixture tests used unescaped paths.
Fix must decode only the opaque ID segment once, with a bounded valid character
set; add common tests using Controller-style encoded lookup and cancellation.
This was an integration gap, not failed native inference or lost admission.
Snapshot remains unchanged; any corrected build needs a new reviewed snapshot
and leader-run experiment, never hot replacement in a running experiment.

Leader 3542 deleted both exited generation1 IDs through Controller and rehashed
the original Rust snapshot unchanged. Exact-root lstat independently confirms
both Controller private roots absent. Rust cwd removed with plain rmdir.

Node cwd rmdir failed: its launcher stores its own state under
`/Users/dooms/treer-exp/158-node/.treer/agents/ag_3a854453eb4740fb990189f8840a5523/`,
outside the Controller private root. Preserved as planned, then inspected only
these two exact regular files with O_NOFOLLOW (no Codex-home access):

| File | Bytes | Mode | SHA256 | Structure |
| --- | --- | --- | --- | --- |
| agent-ui.json | 52 | 0600 | 74ce140c5ffbcb1ddcabd42ea54ec0f5e708d49ec85a0b4f495c6a938751a9c2 | threadId only |
| queue.json | 908 | 0600 | abee7242ed69861f7ce74c2f833aa38386d966a1ea73192d1b8bbe220b5c6bb6 | version, one records entry |

Thus the Node comparator's own journal is in cwd, not its otherwise empty
Controller private root; include that journal's bytes in later matched samples.
This is a placement/teardown difference for the port plan, not a claim that
Controller should delete user cwd content. Evidence is saved; leader may remove
only these two self-created files, then rmdir the exact empty Agent directory,
agents directory, .treer directory, Node cwd and experiment parent. No recursive
deletion; preserve any unexpected files. Native experiment threads remain in
the user's shared Codex history and are not cleanup targets. Final residue
cleanup confirmation is still pending leader.

Follow-up #160 tracks moving Node queue/AIS state into Controller private-state
placement with no managed cwd fallback and explicit existing-state migration.
Observed residue modes were already0600; the finding concerns placement and
retention outside deletion purge, not demonstrated world readability. Keep the
frozen Node comparator unchanged during #158; schedule the fix separately.

## Second attempt pre-prompt inventory

Leader3556 confirms exact first-pair residue cleanup, then new empty0700 cwds
and NEW IDs. Controller admin startup views independently read for both new IDs:
commands exactly match the corrected P2-CREATE.md, Rust uses fixed
`treer-agent-rust-spike-3bfa138b` (rehash23cfdc129a671f28b5dfdd4be256791c1e8b2e2284483cf35347a796824699e3).
Both unset CODEX_HOME/resume variables, watcher0, native Codex pinned as before.

| Field | Node | Rust |
| --- | --- | --- |
| ID | ag_f995f9210636400086fbf24848fe003b | ag_b8548689ef144e75b6828534f8ba9f48 |
| Started UTC | 21:07:03.903Z | 21:07:05.306Z |
| Generation | 1 | 1 |
| AIS port | 59243 | 59281 |
| Instance | agent-ui-codex-ag_f995f9210636400086fbf24848fe003b-40230 | rust-spike-1a94eb50-6509-43f8-adfb-e76f95708da8 |
| Native thread | 01a14bb0-8dc5-75b2-ad2f-932ae05f451f | 01a14bb0-9376-7fb0-b1f2-da39c9a782f9 |
| Model/effort | gpt-6.1-sol / medium | gpt-6.1-sol / medium |
| Launcher PID / PGID | 40230 / 40230 (wrapper) | 40278 / 40278 |
| Node runtime PID | 40234 (parent40230) | — |
| Codex PID / parent / PGID | 40245 / 40234 / 40230 | 40280 / 40278 / 40280 |

Own private roots are0700 under the same Controller/server prefix plus these
new IDs; cwds are the same empty paths as the first attempt. Shared default
Codex home used in place; no home contents inspected. Same installed four UI
files, tool paths and six hashes as original inventory, rechecked before prompt.
Script remains 24 identical text-only rounds with N1–24, one/minute after both
previous completions, followed by matched idle polling. First pair manual,
check both transcripts and leader acknowledgement before unattended cadence.
Record only process metrics, state counts/kinds and own journal sizes; no Codex
home contents. End **2026-10-18T00:00:00Z** (earlier than creation+3h).

### Second attempt stopped before cadence

Round1 at21:08:39Z completed on both sides. Node Controller wait returned done
submission `sub_da4845a0-d8df-41d2-b358-d5d89a58d421`, turn
`01a14bb2-0342-70b0-b13e-8ae2f55ee44b`; Rust returned done submission
`3a79b14e-f605-4ee1-b101-14739e91d75d`, turn
`01a14bb2-0366-79b2-b8b5-0c75df9cde52`. Both exact expected response, no tool
items (Node userMessage+agentMessage, Rust agentMessage only). Receipt fix works
through real Controller. No retry, rounds2–24 never started.

Observer preflight initially used Node fetch, which synthesized browser
Sec-Fetch metadata and correctly received403 from the native loopback route;
changed observer to bounded native HTTP GET, not launcher authentication.
Deep descendant sampling then found unexpected `railway` processes in BOTH
trees: Node40255(parent40245,PGID40255), Rust40295(parent40280,PGID40295).
Initial inventory missed those grandchildren; do not claim those initial trees
were complete. No home/config contents inspected to establish their purpose.
Likely default-config MCP startup is an inference, not a verified tool call.

Stopped both exact Agents on unexpected-process preflight at21:10:29.094Z(Node)
and21:10:28.875Z(Rust), both generation1. Rust child40295 briefly survived as
parent1/ownPGID40295, then exited itself before explicit PID revalidation.
Prepared SIGTERM command did **not** execute because revalidation ps returned
no rows (shell && short-circuited). Follow-up ps found none of the seven PIDs.
Corrected the initial ticket note that incorrectly said a manual signal was sent.
No broad/group/name-pattern kill or user system change occurred.

Own evidence recorded as metadata+SHA256 only (no queue contents in report):

| File | Bytes | Mode | SHA256 |
| --- | --- | --- | --- |
| Node cwd agent-ui.json | 52 | 0600 | ffd434b247baa84e6eb08894a61a15af5ea56a9789baf1de321056ef5918d0d9 |
| Node cwd queue.json | 908 | 0600 | e8bbe04a0b63bc204377401d50e528bf6df5bc561adb72a007ff05c91e7c183f |
| Rust private-root rust-spike.json | 1226 | 0600 | 0581788baf9b358624bcc5d82532fb6443aaf46e59c3a8d51e11b7def1fafcd2 |

Corrected Rust snapshot23cfdc12 and Node entryc8b73478 hashes still match after
stop. Leader may now delete these two stopped IDs, confirm private-root purge,
remove only this pair's two exact Node cwd state files, then plain-rmdir empty
directories as before. No shared Codex-home cleanup.

`p2-observe.mjs` is an external observer draft bound to these now-stopped IDs:
syntax checked; --sample-only exposed the unexpected process tree. Continuous
mode never started and no second-run.jsonl was created. A new attempt requires
new IDs and a reviewed process/MCP boundary (either explicit matched configuration
or explicit acceptance of the existing children), plus first-turn human check.
No long-run or many-turn evidence yet; do not infer memory savings from startup.

### Third attempt preparation: matched MCP exposure accepted

Leader3564 accepts Railway as part of the matched default-home workload.
Security3566 supersedes3565's mandatory config override with explicit controls:
pin full process inventories including MCP children, repeated deep checks,
human transcript checks at cadence boundaries, separate MCP RSS and exact
PID+start-time revalidation of detached survivors at teardown. This is recorded
acceptance of full-account tool exposure, not evidence of isolation. The broader
per-role MCP provisioning finding is tracked in #161; Node cwd retention in #160.

`p2-observe.mjs` now requires a private `third-inventory.json` in the evidence
directory and refuses to reuse second-pair IDs. It continuously samples every
10seconds, checks exact process identity/parent/PGID/name/start time, and separates
launcher/provider/MCP RSS. A human first-pair check permits rounds2–6. It then
pauses submissions after rounds6,12,18,24, sends a checkpoint message, and keeps
sampling/deadline enforcement active. Only after the leader's transcript check
on #158 may the operator enter `approve-through 12`, then18, then24 on its stdin.
It does not signal detached processes itself: it records exact survivors for
individual identity revalidation and reports stop results. No observation can
prove absence of all filesystem/network activity; transcript/process checks are
the measured controls, not OS containment. No third inventory/run file exists
yet; the leader still owns previous-pair cleanup and new-pair creation.

Leader3573 confirmed second-pair deletion/private purge and exact Node residue
cleanup, then created the third pair at21:17Z. Pre-prompt sample-only PASS at
21:18:34Z; both gen1, ready idle, fresh zero-turn threads, gpt-6.1-sol/medium.
Saved startup commands match the fixed reviewed commands; Rust snapshot and
Node entry/runtime hashes match the pinned values.

| Field | Node | Rust |
| --- | --- | --- |
| Agent | ag_0440f32614d947d3b2e7d3d5aebbf466 | ag_af23ab4256bf437f987b4d0b1e0cae11 |
| AIS port | 57046 | 57087 |
| Instance | agent-ui-codex-ag_0440f32614d947d3b2e7d3d5aebbf466-46008 | rust-spike-8e55485a-d665-4831-8573-c75783a2a1f0 |
| Native thread | 01a14bb9-be70-7ad1-8ed8-5d7c555459b1 | 01a14bb9-c347-7160-a1ea-ad0db840b065 |
| Launcher PID | 46008(wrapper),46011(runtime) | 46056 |
| Provider PID | 46022 | 46057 |
| MCP PID / PGID | 46032 / 46032 | 46075 / 46075 |
| All processes start (machine local) | Sat Oct17 17:17:06 2026 | Sat Oct17 17:17:07 2026 |

Exact parent/PGID/comm/start-time data is in evidence-directory
`third-inventory.json` (0600). Launcher RSS startup snapshot was83856KiB Node
(34288+49568) and5056KiB Rust; provider68416/67616KiB, MCP9424/9424KiB.
These are startup snapshots only, not a long-run conclusion. No home inspected.

Security3574 passes observer47aea21a with three small changes, now addressed:
deep sample immediately before each submit pair; approval output records actor
launcher-dev, who types approval only after leader reads the completed block's
transcript/tree and records it on #158; rejected stop sends a loud operator-retry
message. Operator must retry a failed exact-ID stop and verify both Agents and
recorded descendants exited before teardown, regardless of returned stop result.

Third round1 completed21:18:53Z: both exact expected reply and Controller done.
Node submission`sub_b95c3490-d856-417a-96d1-7edee7402259`,
turn`01a14bbb-60b7-7650-a80e-64e99f6837b0`; Rust
submission`22fdf935-2e6b-4960-bfdd-d119b864e5f4`,
turn`01a14bbb-6055-7380-8dab-ca326f0b38dc`. Leader3581 personally read both
transcripts and approved rounds2–6. Node userMessage+agentMessage versus Rust
agentMessage only: **missing userMessage is a transcript port gap**, not parity.

Observer started21:22Z with explicit first-turn approval, private evidence
`/Users/dooms/.cache/treer-158-tUql8b8E/third-run.jsonl` and interactive exec
session26393. Keep it alive; do not start another observer or reuse the output.
At each checkpoint, read the latest #158 titles/full leader acknowledgement,
then type the next exact `approve-through N` into this session only. A gate
notification alone is not approval. Evidence records the actor launcher-dev.
Hard stop23:59:30Z remains; sampling continues during pauses and final idle.
No long-run result is available at this checkpoint.

#### Scripted workload complete; idle sampling in progress

Leader approvals: round1 in3581; block6 in3603; block12 in3621; block18
in3631. Launcher-dev relayed each approval to the existing observer's stdin;
the evidence file records through12/18/24 and relay actor. No approval means
no next block. The last scripted round24 completed at21:50:49Z, checkpoint
at21:50:57Z. All24 matched pairs returned done with the exact scripted replies;
the observer recorded rounds2–24, while round1 was manual (IDs above).

At the24 checkpoint both remained gen1 idle,24turns each, with no extra or
changed recorded process. Node had24 userMessage and24 agentMessage items;
Rust had24 agentMessage items and the known missing-user audit gap. No tool
items observed. Snapshot RSSKiB: launcher96960/7952, provider79664/76656,
MCP5776/5808 (Node/Rust). These are preliminary snapshots, not final memory
savings, CPU or leak conclusions. The last block's human review was requested
on #158. There is no round25: matched idle sampling continues to23:59:30Z,
then exact-ID stop, descendant verification and leader-owned purge per plan.
The planned continuation was superseded by user intervention at22:31Z and
leader3697's immediate stop request. Both exact IDs stopped22:33:04Z; seven
recorded PIDs subsequently absent, no manual descendant signals. Leader3700
completed deletion/private purge/exact cwd cleanup. Evidence is retained and
hashed. See [the P3 report](P3-REPORT.md) for the clean-window boundary, results,
intervention, teardown, limitations and port decision. The original multi-hour
idle target was not completed; no further run starts under this authorization.
