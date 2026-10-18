# #158 P3: measured prototype benefit, no production switch

Decision: **GO for a staged Rust port; NO-GO for replacing team launchers**.
The reduced launcher RSS merits further implementation, but the prototype lacks
production behavior and failed the user's real-browser send acceptance. Keep
Node in production, preserve the existing startup command for rollback, and
review each functional boundary before any disposable or non-critical switch.

## Evidence and comparison window

Third pair, source `3bfa138bdd24219222caa99c506e901918097ce6`, default release
build, unchanged installed Node comparator, native Codex, gpt-6.1-sol/medium.
Exact commands, Agent IDs, thread IDs, binary hashes and process inventory are
in [P2-RUN](P2-RUN.md) and [P2-CREATE](P2-CREATE.md). Both used the explicitly
accepted default Codex configuration in place, with one Railway MCP child each.
No user Codex-home contents were inspected or changed.

Evidence retained outside the repository, mode 0600:

| File under `/Users/dooms/.cache/treer-158-tUql8b8E/` | Bytes | SHA-256 |
| --- | ---: | --- |
| `third-run.jsonl` | 912588 | `472a21521a0899cd46c58e77d03aca33273068e01e65163574db4ccadbb11c12` |
| `third-inventory.json` | 2002 | `321ad6bd5d4023c65d17f5788b698d25cbce4b76b4706fd76386dd83e5c8b7be` |

The JSONL remains unmodified, including the intervened suffix and stop records.
Offline RSS/CPU curves are retained as `third-run-curves.svg` in that directory
(SHA-256 `0770ab2d050312e18d54942a4f0dbb3524719ce0cb02d839496621900bfe9ec9`).
Regenerate from the unmodified evidence with
`node launchers/agent-rust/p2-curves.mjs INPUT.jsonl NEW-OUTPUT.svg`.
The plot independently scales launcher, provider, MCP RSS and launcher CPU;
it applies the same first-extra-turn cutoff as the tables below.
There are 433 samples; only the 422 before the first extra turn are compared:
21:22:28.397–22:31:04.442 UTC, 68m36s. Typical sample interval was 10.294s,
maximum 11.243s; extra pre-submit samples explain the minimum 0.209s interval.
Round 1 was manual at 21:18:53; rounds 2–24 have paired submission/completion
records. Leader checked every six-round block, including final approval 3638.
All 24 scripted replies matched exactly, without tool items. Both remained
generation 1 with the original processes and threads throughout this window.

The matched final idle subset is 234 samples, 21:50:57.590–22:31:04.442 UTC,
40m07s, after both reached 24 turns. This is **not** the originally planned
multi-hour idle test. No inference about multi-hour leaks follows from it.

## RSS and CPU results

RSS is summed per process role from `ps`, in KiB. Node launcher includes its
wrapper and runtime; Rust has one launcher process. Provider and MCP are kept
separate. These are resident-page observations, not private memory/PSS, heap,
allocation counts or container totals; shared pages may be counted twice.

| Clean comparison window, min / median / peak KiB | Node | Rust |
| --- | ---: | ---: |
| Launcher | 82848 / 88992 / 101840 | 3616 / 6752 / 8352 |
| Native Codex | 44432 / 66192 / 87920 | 47088 / 64752 / 85136 |
| Railway MCP | 4544 / 5088 / 6304 | 4544 / 5120 / 6288 |
| Final idle launcher | 85920 / 88768 / 101840 | 5056 / 6640 / 7952 |
| Final idle Codex | 56768 / 65488 / 79664 | 55392 / 64176 / 76656 |

The launcher median differs by 82240 KiB (80.31 MiB), or 92.4% relative to
this Node launcher. That is a **prototype launcher-only observation**, not
92.4% savings for a feature-complete Agent or the team. Rust retains fewer
transcript items and implements fewer features; no production memory forecast
is warranted. Provider/MCP memory is similar and remains a substantial cost.

| CPU seconds between clean first/last samples | Node | Rust |
| --- | ---: | ---: |
| Launcher | 12.34 | 1.47 |
| Native Codex | 6.53 | 5.63 |
| Railway MCP | 0.02 | 0.02 |
| Launcher, final idle subset only | 6.58 | 0.77 |

These are differences of cumulative `ps time`, rounded to 0.01s, including
observer/UI polling and event handling; they exclude startup before the first
sample. They do not measure energy or prove a throughput difference. Paired
rounds 2–24 admission-to-done latency min/median/max was 1615/1985/3725ms
Node and 1842/2081/3681ms Rust, dominated by provider/network timing; no model
speed improvement is claimed.

At the last clean sample launcher RSS was 88224/7600 KiB (Node/Rust), provider
58672/58576 and MCP 5072/5104. Final idle RSS fell from 96960/7952 launcher
and 79664/76656 provider at the first idle sample; it fluctuated, rather than
growing monotonically. This short run neither establishes nor rules out leaks.
Node state/journal bytes at 24 turns were 23264/22029; Rust 19180/27230.
Rust's smaller state is not parity: Node has 24 user + 24 assistant items,
Rust only 24 assistant items. Node heap/native/buffer instrumentation was not
enabled, to keep the comparator frozen; no attribution beyond process RSS.

## Human intervention and early stop

Leader 3697 reports the user opened Rust's existing React UI, sent a message,
saw their message missing and a UI crash. The backend stayed alive, generation
1, and produced a greeting; **do not describe this as a process crash**.
The first sampled extra turn is Rust working at 22:31:14.766 (Node still 24).
Node first reaches 25 at 22:31:25.026. Who submitted Node's extra turn is not
established by the observer; do not infer matched prompts from matching counts.
No extra prompt was sent by the observer. Both later have only message kinds,
no tool kinds. Rust briefly reports one userMessage during its extra turn,
then loses it when the completed turn replaces the transient turn. Exact
human prompt contents were not retained by this numeric observer.

Independent browser reproduction after stopping/deleting the pair used a local
fixture process, fake Codex/private Controller, fresh temporary HOME/CODEX_HOME,
and the exact pinned public UI assets. Two Chromium runs reproduced a blank
page immediately after send: `Cannot read properties of undefined (reading
'limit')`. The HTTP snapshot reducer reads `action.snapshot.history.limit`,
but Rust `/api/prompt` returns the submission receipt from `call("submit")`;
Node returns `{...statePayload(), submission}`. This response-shape mismatch
explains the reproduced browser exception without a backend crash. The fake
provider emits a sealed relay failure, so this is not a live-model success or
full UI parity test. The fixture children and browser were stopped; no managed
Agent was created. Reproducer and result remain in the evidence directory as
`ui-repro.mjs` and `ui-repro-result.json` (result SHA-256
`172494385debf434c6e6ee3f2cfd5a9d69f12696e07ec4889779d40f75bfb00c`).
Separately, Rust `turn/completed` replaces the turn and only preserves previous
items if the completion has no items; a completion containing assistant items
can therefore drop an earlier user item, matching the live numeric evidence.
No prototype source was changed to conceal either failure.

Exclude the **entire suffix starting 22:31:14.766** from matched results.
Leader ordered immediate stop at 22:32:41; operator SIGINT asked the observer
to stop both exact IDs. Both Controller stop requests fulfilled at 22:33:04.517.
Its post-stop inventory at 22:33:04.614 found Railway PIDs 46032 and 46075
reparented to PID 1 in their own process groups. A subsequent exact-PID check
found all seven recorded PIDs absent; no manual signal was sent to descendants.
This is measured evidence of a containment/reaping gap on **both** backends.

Leader 3700 confirms both Agent IDs deleted, private state purged, exact Node
cwd residue recorded then removed and empty experiment directories rmdir'd;
`/Users/dooms/treer-exp` is gone. No user Codex-home cleanup. Snapshots, hashes
and numeric evidence remain. Post-stop hashes of Rust, native Codex, Treer,
Node, installed Node entry/runtime and the public asset aggregate match the
preflight hashes. The observer exited 0 and is no longer running.

## Required port work and acceptance

1. Browser/API parity first: run the **same existing React UI browser suite**
   against Node and Rust. Cover send, visible user prompt, streaming, completion,
   failure, queue, abort, reconnect and reload, with no page exceptions. Reproduce
   the reported crash on a disposable hermetic fixture, capture the actual
   browser error, and fix its cause before claiming UI compatibility.
2. Preserve userMessage items after completion/recovery, with exact text and
   Controller-attested sender/trust data. Cover direct UI, Controller submit,
   queue, watchers, paging and restart. An assistant-only log is not auditable.
3. Contain and reap detached descendants, including independent PGIDs, with a
   regression and post-stop inventory that fails closed. Do not rely only on
   killing the provider's process group or on MCP children eventually exiting.
4. Implement private durable state placement/recovery (#160), before provider
   startup; preserve thread/queue/idempotency as a unit and fail closed on
   legacy/unsafe/uncertain state. No silent replay, cwd residue or broad deletion.
5. Complete Codex error/recovery/notify parity, then Claude and remaining ACP
   adapters; port goals, attachments, file browser/task output, session transfer
   and the full production regression suite. Keep unsupported capabilities off.
6. Carry external_source channel/account/conversation/message/endpoint/generation
   into visible untrusted source markers (#159), without claiming authenticated
   identity from sender-asserted metadata. Apply role-scoped MCP policy (#161).
7. File downloads must include #163 and #165: fixed **1 GiB per file**, fstat
   check and 413 before bytes, visible en/zh failure, bounded concurrency/429,
   write-idle timeout, cross-site navigation checks and documented/hardened
   ancestor-swap behavior. Validate real Console iframe -> Proxy -> launcher
   and standalone non-ASCII downloads. 128 MiB concerns uploads (#166), not
   downloads. Rust currently has no file-download implementation.

No production gate, five-platform run, adversarial browser acceptance or
dependency audit was performed for this experiment. The existing focused
contract/fault and encoded-ID tests remain the verified implementation scope.
Resource wins do not waive these blockers. Continue the Rust port under new
reviewed increments; retain Node for current work and resolve #160 separately
while it remains in production. Any eventual rollout needs explicit approved
startup changes, identity/session preservation and a tested one-step Node rollback.
