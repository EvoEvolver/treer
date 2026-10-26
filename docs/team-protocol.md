# Team collaboration protocol

How the Treer development team coordinates work. [Team roles](team-roles.md)
define who owns what; this protocol defines where state lives and how work is
handed off. Source: Issues #20 (protocol v1, 2026-10-15), #29 (tiered
releases, v1.1) and #137 (comment titles, v1.2). The Issues App is the source of truth; if this file and a
newer ticket comment disagree, the ticket wins and this file should be updated.

## Protocol v1

1. **Tickets are the single source of truth.** Every unit of work has one ticket.
   The assignee is the owner. Status, frozen SHA and review verdicts live in
   the ticket, as comments and as the ticket status. Chat messages only wake
   someone up; they never carry state.
2. **Reviews are written on the ticket**, not forwarded through the leader:
   `[review] <SHA> pass|changes needed — key points`. Authors and the releaser
   read the ticket. Request a review by `@`-mentioning the reviewer, otherwise
   the notification has no recipient. A bare `@name` fails to resolve when the
   directory holds more than one agent with that name (for example two
   `leader`s); use the path `@/treer-dev-team/<name>` or the stable agent ID.
3. **Releasers comment every step:** `[release] <step> <SHA> <result>`. Order is
   expressed with `blocked_by`.
4. **Commands are idempotent.** Check real state first (git, gh, manifest,
   docker, `df`) before acting. Re-read the ticket's latest comments and status
   before every piece of work on it, not only after a restart: acting on a plan
   remembered from earlier uses stale SHAs. After a restart, follow the latest
   ticket state and do not execute old instructions found in chat.
5. **Messages are short and structured:** object or SHA, verdict, blocker, next
   step. Put background in the ticket number instead of repeating it.
6. **Small increments fix themselves.** P3 suggestions with no behaviour change
   are fixed by the author, who comments on the ticket. Only behaviour or
   security-boundary changes need a second review.
7. **Check shared resources before starting:** `df -h`, ports and containers.
   Write conflicts on the ticket. Use random or exclusive ports, record the PIDs
   you start and stop only those; never kill by process name or command line.
8. **No completion notices by default.** Use `--notify` or `--wait` only when
   you actually need the result to continue.

## Practices learned in use

- **Do not wait to be woken when idle.** After every finished item, and every
  time you become idle, immediately query your queue (assigned to you and
  unresolved, for example `GET /v1/issues?assignee=<your id>&status=open`) and
  start the next item by priority. Stand by only when nothing in the queue is
  actionable. Once every agent sat idle while workable tickets remained.
- Change the ticket **status** when state changes, not only the comments, so
  list views and "assigned to me" queries stay accurate.
- Hand a ticket over by changing the assignee and commenting why. The new
  assignee verifies the real state before redoing anything; work may already
  be finished and frozen.
- Do not put text containing backticks inside double-quoted shell arguments:
  the shell runs it as a command substitution. Use single quotes or a heredoc
  when posting comments or messages.
- Say what was not run or not verified. A check that could not start (for
  example a missing build output) is a finding to report, not a baseline.

## Agent self-service and minimal human input

New work defaults to Agent self-service through supported APIs and commands.
Owners gather normally accessible facts, choose routine implementation details,
prepare inputs and verify results themselves. Missing packaging, test fixtures,
internal coordination or a discoverable ID is not a user decision. Do not ask
users to perform technical steps that an authorized Agent can complete.

Before setting `waiting_on=user`, separate work that can proceed independently
and complete it. Record the exact remaining authorization or decision, its
effect, the prepared scope and the responsible executor. An unavailable external
resource may instead require an explicit deferral and a condition for resuming;
do not manufacture a pending user decision for it.

The leader collects genuine decisions into one short set of yes/no questions.
Each question names the concrete target and impact; "no" has an explicit defer,
skip or alternative outcome. Do not repeatedly request an authorization already
given. Facts that cannot yet be verified remain unknown: do not turn a yes/no
answer into fabricated identity, access or acceptance evidence.

Human choices remain human choices, including actual external email delivery,
data deletion and access expansion. Prepare a specific recipient, content,
quantity or target scope before requesting authorization. Agent self-service
does not authorize inventing approval, bypassing access controls, copying
credentials or automatically replaying an uncertain operation.

An individual external dependency must not block unrelated acceptance or source
work on the same ticket. Update `waiting_on` when that dependency changes, and
preserve what was actually skipped rather than claiming it passed.

## Comment titles (v1.2)

Tickets grow to many comments of several KB each, and reading all of them costs
context. Since #137 an Issues comment can carry a one-line title, and readers can
skim titles before opening any text.

- **Write a title on every comment longer than about 5 lines.** Start with the
  role tag, then the ticket, the SHA and the result, for example
  `[freeze] #123 Core 1deda33: gate EXIT 0`, `[review] 0950a75 pass`,
  `[release] ci 95ab3c1 passed`, `[decision]`, `[handoff]`. A title is one trimmed
  line of at most 200 bytes (the web form allows 64 characters), without control
  characters, and it never replaces the body. Short comments need no title.
- **Read titles first.** `issues show N --titles` returns the activity with each
  comment as its title (or, for untitled and older comments, a one-line
  `excerpt` of the first 160 characters) plus `body_bytes`. Then read only the
  comments you need in full with `issues activity N EVENT`. The default
  `issues show N` still returns full text. Write a comment with
  `issues comment N 'body' --title 'title'`.
- Re-reading "the latest comments" before work (rule 4) means reading the latest
  titles and then the full text of those that matter, not only the title: a
  title is a pointer, and state still lives in the comment and the ticket status.

## Formal source and feature preservation (#20:9716)

Formal Console, App and native publication must use an exact commit already in
`origin/main`. A release tag references that exact commit. An isolated preview
may use a task branch, but is not a formal delivery. Fetch main immediately
before publication; a task branch or configurable preview ref cannot replace
this check.

Compare the candidate against the **previous actually deployed source**, not
only its task base. Record its deployment receipt, the complete component path
diff, and named delivered features with retained/added/explicitly authorized
removal outcomes and regression evidence. Historical deployed branch-only
features must be reconciled individually into main; never overwrite main with
an entire historical branch. Feature removal or narrowing needs a concrete
user authorization. Git deletion checks alone cannot establish feature parity.

Run `python3 scripts/release-source-preflight.py --candidate FULL_SHA --review
/absolute/review.json` before mutation. The JSON schema is
`treer.release-source-review/v1`, with `candidate`, `previous_live_source`,
`previous_live_receipt`, `reviewed_by`, `component_paths`, exact ordered
`changed_paths` rows (`status`, `path`, from `git diff --no-renames --name-status`),
and `feature_comparison` rows (`feature`, `outcome`, `evidence`; removals also
`user_authorization`). Deleted paths require a matching
`deleted_path_authorizations` entry explaining authorized removal or relocation.
The maintained Canary entry requires this absolute file through
`TREER_RELEASE_SOURCE_REVIEW`. Narrow component runbooks must invoke the same
preflight and bind its receipt to their frozen plan. Recheck the actual live
version before applying: a changed predecessor invalidates the comparison.

Publication receipts record candidate/source SHA, main SHA observed at preflight,
previous live source and functional differences. This source check is not deploy
authorization, artifact provenance, or browser acceptance; record those separately.

## Tiered releases (v1.1)

Developers may test several versions locally before a batch deploy. Three
channels, increasing in gate strength:

| Channel | Purpose | Gate |
| --- | --- | --- |
| **A. Development preview** | An agent runs its own branch as a preview profile or App. | None. Needs authorization, no release ticket. |
| **B. Own-machine staging** | Launcher switches to a local bundle; an App switches a versioned directory behind a `current` symlink. Does not touch Proxy, Controller or canary. | The component's own unit and browser tests. The leader authorizes. |
| **C. Release cut (canary)** | Batch switch of versions, daily or when enough has accumulated. | Full gate and five platforms, chosen by changed paths: launcher, web or Apps only changes skip the full Rust suite and five-platform run. |

Channel B rules:

- Roll out gradually: a preview agent, then two or three agents, then all.
- It must be reversible in one step. Keep the previous version installed.
- Back up an App's database before switching its version.

Release tickets carry the `type:release` label. Each push, CI run, canary result,
manifest and hash is a `[release]` comment. Only the release role pushes or
deploys.

## Author gate before freeze

Release re-runs everything, but first-attempt surprises (a test the author never
ran, a platform path nobody exercised) cost a full gate cycle each. So before a
`[freeze]` comment the author runs the graded gate for their change class, the
same command release runs, and pastes the exit code and the log path into the
freeze comment. `python3 scripts/release-grade.py <base> <head>` prints the
class, the local gate commands and whether native, canary and Policy checks are
required; see [Graded gates](releases.md#graded-gates-and-staging-batches).

| Change touches | Author runs before freezing |
| --- | --- |
| `docs/`, root `*.md` | `node scripts/check-docs.mjs` |
| one App under `apps/` | the printed gate: docs check, that App's tests, and `npm run check` when its `package.json` defines it |
| `web/` | `cd web && pnpm typecheck && pnpm build && pnpm test:e2e` (build first, then test; use random or exclusive ports) |
| `launchers/agent` | `just agent-launcher-test`, plus the Windows launcher smoke (`launchers/agent/tests/windows-smoke.mjs`) when the change touches Windows or installer paths: run it on the `win_native` runner or state that it was not run |
| `crates/`, `skills/`, build files, unknown paths | `just check` against an isolated PostgreSQL/NATS and a private build target (never the shared database or another agent's target), and for auth, identity or policy changes the full Proxy suite, not a focused subset; `just check` already runs the workspace tests, so no extra `cargo test -p treer-proxy` is needed when it ran with the PostgreSQL URL set |
| `scripts/`, `.github/`, `justfile` | `just check` |

The freeze comment says what was not run (for example no JDK for the Android
tests, no Windows runner) so release can decide, instead of assuming a green
local gate. A focused test subset is a legitimate development check but is not
the author gate: the defaults-profile change (#84) passed its focused PostgreSQL
tests and still broke a workspace-deletion test only the full suite runs. Release
keeps the final say and still runs the exact-SHA gate, CI, native, canary and
the Policy check.

### Cross-target check for platform code

The author gate runs on one macOS machine, so it cannot see code that only
compiles or lints on Linux or Windows. The chunked upload change (#166, main
`41f1a32a`) passed `just check` twice on macOS and still broke CI and the
Windows build in three ways: a `matches!` guard that clippy rejects on Linux
(`Errno::NOTSUP` and `OPNOTSUPP` are the same value there), an import removed
from a parent module that `cfg(windows)` code reached through `use super::*`,
and a method that existed only in the Unix module.

From now on a change under `crates/` that touches platform code (`cfg(unix)`,
`cfg(windows)`, `cfg(target_os = ..)`, file, process or socket APIs, or imports
that such code receives through `use super::*`) needs a cross-target check on
top of the macOS gate, before the `[freeze]` comment:

1. **Crates whose dependency graph has no `ring`** (check with
   `cargo tree -p <crate> -i ring`; `treer-platform` is one): run, with a
   private `CARGO_TARGET_DIR`,
   `cargo clippy --locked -p <crate> --all-targets --target x86_64-unknown-linux-gnu -- -D warnings`
   and the same with `--target x86_64-pc-windows-gnu`. Both targets are
   installed on the shared Mac (`rustup target list --installed`). On `41f1a32a`
   the Linux command for `treer-platform` reproduces the CI failure exactly.
2. **Crates that depend on `ring`** (at least `treer-agent-server`, which is
   where the Windows errors were, and `treer-cli`, `treer-proxy`): the local cross-check cannot run on the shared Mac, because
   `ring`'s build script needs a cross C compiler (`x86_64-linux-gnu-gcc`,
   `x86_64-w64-mingw32-gcc`) and fails with `ToolNotFound`. Do not install
   toolchains on the shared machine for this. Ask release for a **hosted dry
   run** on the exact SHA instead: a throwaway branch with a push-triggered
   workflow that runs clippy with `-D warnings` on `ubuntu-latest` (the same as
   CI) and `cargo check --locked --all-targets` on `windows-2022` and
   `windows-2025`, with the checkout pinned to the SHA. It takes about ten
   minutes and does not use the machine gate slot.
3. The freeze comment lists the targets and commands that ran with their log
   paths, or says "cross-target not run". Release runs the hosted check on the
   exact combined candidate whenever the union touches platform files, before
   pushing to `main`; a red hosted check blocks the push.

Windows warnings that already exist on `main` (for example unused test imports)
are not failures; new compile errors and new warnings in the changed code are.
This rule is about catching platform breakage early. It does not replace the
exact-SHA gate, CI, the five-platform native build or the Windows runner jobs.

## Real-browser acceptance for Console, browser and embedding paths (v1.3)

Three trains in two days passed every gate, reached `canary_passed` and were
still broken in a real browser: the upload dialog (#166), CORS for the
cross-origin Console (#173) and embedded Agent interface isolation (#156). In
#156 the Console's own Content-Security-Policy `frame-src` allowed only the
Proxy origin, so Chromium silently blocked the new `ag-` iframe (core-dev-2's
diagnosis); the three-engine matrix passed because Playwright served the
Console without the deployed Worker's headers, and the live check had been
left to "the user's Console session". That is no longer an acceptable way to
ship.

**Scope.** Any change to a Console, browser or embedding path: iframe, sandbox,
CSP or COOP, cookies, CORS, auth handoff, upload or download UI, or anything
else a browser renders or enforces.

**Rules.**

1. Such a change passes a **real-browser acceptance on canary against a real
   launcher-backed agent before it is pushed to `main` for the user**. "Live
   browser acceptance unverified" is a release blocker, not a closure note, and
   a ticket is not closed with checks left for the user's own Console session.
   We verify; the user does not.
2. Release keeps a **permanent acceptance fixture on canary-a**: the launcher
   installed plus one disposable launcher agent with a fake harness, no real
   credentials and no real provider account. On every Console train it
   exercises the embedded view and the standalone view. Until the fixture
   passes, no Console, browser or embedding change ships.
3. The acceptance runs in real Chromium and, for embedding, also WebKit and
   Firefox, against the **deployed headers** (the real Worker and Proxy
   responses), never against routes that Playwright serves itself. It records
   response header values, status codes and a pass or fail per step: mint,
   bootstrap, cookie, the Agent UI inside the frame, a popup from the frame,
   logout ending the embed session, the standalone view, and the **negative
   checks** of the security rules (a cross-origin request with the embed cookie
   refused, strict refusal and the compatibility choice), not only the happy
   path.
4. The fixture session comes from the canary-tester environment and is never
   printed, stored in logs, traces, screenshots or HAR files. The fixture agent
   lives on canary-a only and is disposable.
5. A canary deploy that fails the acceptance, or a report from the leader,
   security or the user that a Console path is broken, is **rolled back by
   release at once, without waiting for a diagnosis**. A fix-forward that
   touches a security rule (Fetch Metadata, `frame-ancestors`, CSP on embed
   responses, origin or nonce checks, the sandbox) goes back to security before
   it ships; nobody loosens a rule to make a frame load.

**Rollback procedure (release).**

1. Before pushing any Console, browser or embedding change, prepare a revert
   branch (`git revert -m 1 <merge>` of that change on top of the candidate) and
   keep the previous `canary_passed` checkout, with its `dist/` and checkpoint,
   which hygiene never removes.
2. To roll back, redeploy the previous `canary_passed` SHA with
   `scripts/canary-release.py <sha> --resume`, **started from the repository
   root**, not from inside the SHA's checkout (started inside it, the script
   builds a nested worktree, finds no checkpoint and re-runs the whole gate;
   this cost ten minutes of a rollback on 2026-10-19). It reuses the green
   gate, CI and native evidence for that SHA, redeploys the Console Worker and
   the Proxy together, runs health, artifact byte checks and the two-machine
   E2E, and the Policy default-install check follows. Keep the first run's logs
   under another name.
3. Verify: the Proxy `/api/auth/config` capability list no longer contains the
   new capability, the header probe and a real Chromium login and logout pass,
   and the fixture's standalone and embedded views work.
4. Post the rollback on the ticket and on #98 with the SHA, Proxy deployment id
   and what was and was not verified. `main` keeps the bad merge until a
   fix-forward or the prepared revert is gated and shipped through the normal
   train.

**Acceptance fixture procedure (release).** The fixture is code in the release
support directory, not a standing service; each train brings it up from the
SHA under test and takes it down.

- *Agents.* A disposable `fixture-launcher` agent (launcher from the SHA, fake
  Codex harness) and, for launcher UI that only the Claude harness shows, a
  `fixture-claude` agent (fake Claude). Both live on canary-a only, run the
  launcher archived from `git archive <sha> launchers/agent` with
  `npm ci --omit=dev --ignore-scripts`, hold no credentials and are removed
  when the run ends. A launcher-side change is accepted only on a fixture built
  from the SHA being shipped, never from an older copy; the SHA is recorded in
  the fixture state.
- *Per Console train.* One command brings the second agent up, runs the
  diagnostics, the header probes, the pinned three-engine script and the popup
  probe, takes the agent down and writes the record under
  `.treer/releases/<sha>/`. Add the train's own step (for example the audit
  charts or the tasks panel) and, when a socket path changed, a hold of at
  least 90 seconds with a logout close check.
- *Pinned scripts.* Every acceptance script is run by a wrapper that refuses
  any other sha256. A script change is a reviewed new hash: the record names the
  hash, the old one is retired, and the script lives in the repository under
  `launchers/agent/tests/acceptance` so its self-test is versioned with it.
- *Session.* The canary-tester login is made fresh for each run, written to a
  mode-0600 temporary file and deleted afterwards; logs hold statuses, header
  names, counts and close codes only.
- *Limits to state in every record.* What was not exercised live (for example a
  non-owner identity, an older Proxy without the capability, phone widths, a
  browser's default Tab order), and any failed earlier run, kept under its own
  name.

## One full gate at a time on the shared machine

Up to five agents used to run full gates (Rust with PostgreSQL/NATS, Playwright
with four workers, launcher browser tests) on the same Mac at once. The load
average reached 11-27 and timing-sensitive tests then failed for reasons that
had nothing to do with the change (#112, #125, #128, #129). `just check`,
`just agent-launcher-test`, `just test` and `just web-test` therefore take a
machine-wide slot through `scripts/gate-lock.py` (an advisory `flock` under
`~/.cache/treer-gate/`, released by the OS if the holder dies):

- One holder at a time, and waiters are served in arrival order. Every waiting
  gate takes a numbered ticket under `~/.cache/treer-gate/queue/` and only the
  head of the queue contends for the slot, so a gate that arrived later can no
  longer win it from an older one (launcher-dev's Rust M2 gate waited more than
  twenty minutes behind newer gates before this, #174). A waiting gate prints
  `gate-lock: waiting for gate slot held by <name> (pid ..., since ...)`, plus how
  many waiters are ahead of it when it is not the head.
- Release gates take priority: the release scripts set `TREER_GATE_ROLE=release`
  (`scripts/release-canary.sh` does it for `just check`). Release tickets sort
  before author tickets, so a release gate that arrives goes before queued
  authors, and release gates keep their own arrival order.
- A waiting gate refreshes its ticket every poll. A ticket whose process died,
  or that was not refreshed for 30 seconds (a hung waiter or a reused pid), is
  removed by the next waiter that sees it; a crashed waiter never blocks the
  queue. Waiters started before this change take no ticket and can still cut
  in until they have run.
- `python3 scripts/gate-lock.py --status` (add `--json` for scripts) shows the
  holder and the queue in order; use it instead of guessing who is next.
- Nested use is free (the child gets `TREER_GATE_HELD=1`), so `just check`
  can call `just agent-launcher-test`.
- Focused runs (one spec, `cargo test -p <crate> <filter>`, a single script
  test) do not take the slot; they stay cheap. Do not run
  `pnpm test:e2e` or `cargo test --workspace` directly as an author gate: use
  the recipes so the slot applies.
- Hosted CI (`CI` set) and `TREER_GATE_LOCK=0` skip the slot; the latter is for
  emergencies and must be stated in the freeze comment.
- Author gates run with private databases/ports as before; the slot only stops
  them from competing with each other and with the release gate.

