# Team collaboration protocol

How the Treer development team coordinates work. [Team roles](team-roles.md)
define who owns what; this protocol defines where state lives and how work is
handed off. Source: Issues #20 (protocol v1, 2026-10-15) and #29 (tiered
releases, v1.1). The Issues App is the source of truth; if this file and a
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
| `mobile/` | the touched platform's own tests (Android JVM `./gradlew :app:testDebugUnitTest`, iOS `just mobile-ios-ci`); `release-grade.py` classifies `mobile/` as web, so a green Web gate says nothing about Android or iOS: state them as unverified when the toolchain is missing (the Android CI job is #97) |
| `scripts/`, `.github/`, `justfile` | `just check` |

The freeze comment says what was not run (for example no JDK for the Android
tests, no Windows runner) so release can decide, instead of assuming a green
local gate. A focused test subset is a legitimate development check but is not
the author gate: the defaults-profile change (#84) passed its focused PostgreSQL
tests and still broke a workspace-deletion test only the full suite runs. Release
keeps the final say and still runs the exact-SHA gate, CI, native, canary and
the Policy check.
