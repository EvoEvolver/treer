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
