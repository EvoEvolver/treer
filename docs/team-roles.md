# Team roles and ownership

This charter assigns durable repository responsibilities to roles, independently
of the people or Agent sessions currently filling them. Address team Agents by
`/treer-dev-team/<role>` or stable ID. The coordinator is addressed with
`treer message send --to leader`.

| Role | Owned directories and responsibilities |
| --- | --- |
| `core-dev` | `crates/`; Proxy, Controller, Host, runtime, protocol and CLI; database migrations; authentication, authorization, Policy and security boundaries; `docs/architecture.md`, `docs/security.md` |
| `launcher-dev` | `launchers/agent/`, the single launcher; harness adapters, Agent UI, installation, switching and profiles; harness session export/import interfaces |
| `apps-dev` | `apps/`; workspace Apps including issues, Soul, mail, telegram, paper, gits, policy, tower and email-endpoint; `apps/GUIDELINES.md`; App identity usage |
| `web-dev` | `web/`, React console and Playwright; `mobile/`; console presentation of Core features; consistent path/folder semantics |
| `release-eng` | `.github/`, `scripts/`, release and Canary deployment; installers, artifacts and signatures, machine updates; CI, test infrastructure and unstable tests; cross-review before merge; worktree hygiene and documentation indexes |
| `leader` | Coordination, verification, user reports, machine updates and coordinated Agent restarts; no implementation code |

## Collaboration rules

1. Work within the owning role's directories. Request cross-owner changes with
   `treer message send --to /treer-dev-team/<role>`, wait for their merge, then
   rebase. A one- or two-line cross-directory reference adjustment may be included
   locally; identify it in the commit description and notify the owner. Report
   bugs in another owner's area instead of implementing an unsolicited fix.
2. Before merging changes to authentication, permissions, credentials, Proxy
   routing, or Host/Controller startup environments, request `core-dev` review
   with the commit and specific risks. Core changes to these boundaries require
   `release-eng` review.
3. Use one worktree and branch per task. Merge completed, checked work into local
   `main`. Do not push. Deployment belongs to `release-eng` and requires a leader
   instruction. Host, Controller and Agent restarts are coordinated by `leader`;
   implementation tasks do not authorize runtime restarts.
4. Use closed test environments; follow the launcher tests' `hermeticEnv()`
   pattern to remove inherited workload identity and harness settings. Give each
   worktree its own Cargo target directory; never use `/Users/dooms/treer/target`
   as a shared build directory across Agents.
5. Report each completed task to `leader` with its commit and check results.
   Communicate uncertainty and pending review or operational checks explicitly.
6. List cleanup candidates before removal. Remove worktrees or branches only
   after explicit cleanup approval; preserve uncommitted or unmerged work.

Role names and paths are the maintained addressing convention. Saved commands,
scripts and prompts must use these paths, sibling references, or stable IDs,
instead of historical Agent names.
