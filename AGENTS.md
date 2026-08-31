# Treer repository guide

Treer is a self-hostable control plane and distributed runtime for coordinating
coding agents across enrolled machines. This file is the short map for agents
changing the repository; follow links rather than expanding this file into a
second manual.

## Instruction boundary

- Repository development starts with [the documentation index](docs/README.md).
- Managed Treer operations use [the embedded Treer skill](skills/treer/SKILL.md).
  That skill is the runtime CLI contract printed by `treer --skill` and
  `treer --skills`; do not move, rename, or duplicate it here.
- Recipe installs use [the bundled installer skill](skills/treer-install/SKILL.md),
  printed by `treer --skill install`. Creating an Agent with `--recipe`
  sends that skill as the first prompt.
- Apple container Host setup on a Mac uses
  [the macOS container skill](skills/treer-macos-container/SKILL.md), printed
  by `treer --skill macos-container`.
- The root [README](README.md) owns setup and operator examples.
- [PLAN.md](PLAN.md) and dated [research](docs/research/) preserve design
  history. Use source, tests, and maintained docs for current behavior.

## Read by task

| Task | Start here |
| --- | --- |
| Understand product scope | [docs/product.md](docs/product.md) |
| Classify or sequence future capabilities | [docs/roadmap.md](docs/roadmap.md) |
| Change components, Core Message, or protocols | [docs/architecture.md](docs/architecture.md) |
| Change auth, isolation, policy, or trust claims | [docs/security.md](docs/security.md) |
| Verify a change or assess known gaps | [docs/quality.md](docs/quality.md) |
| Build or operate a workspace App | [apps/README.md](apps/README.md) |
| Operate Treer from a managed agent | [skills/treer/SKILL.md](skills/treer/SKILL.md) |
| Install a git recipe with an installer Agent | [skills/treer-install/SKILL.md](skills/treer-install/SKILL.md) |
| Put a Treer Host in an Apple container machine | [skills/treer-macos-container/SKILL.md](skills/treer-macos-container/SKILL.md) |

## Source map

| Path | Responsibility |
| --- | --- |
| `crates/treer-proxy` | Public API, auth, policy, App identity, Core Message/DAG/outbox, workspace routing |
| `crates/treer-agent-server` | Machine Controller, local API, Proxy link, networking |
| `crates/treer-agent-host` | Stable local process ownership and idempotent mutations |
| `crates/treer-agent-runtime` | PTY lifecycle, output replay, working-directory boundary |
| `crates/treer-cli` | Human/Agent commands and the Core Message surface |
| `crates/treer-protocol` | Shared Proxy, Controller, browser, and CLI models |
| `crates/treer-host-protocol` | Controller-to-Host socket contract |
| `apps` | Ordinary workspace services, channel presentation, configuration, state, and docs |
| `web` | Standalone React control plane with runtime Proxy URL configuration |

## Change discipline

1. Read the closest maintained document and the owning source boundary.
2. Keep wire models in the shared protocol crates; keep process mechanics below
   product-aware Controller logic.
3. Update the closest documentation in the same change when behavior, trust
   assumptions, commands, or component ownership changes.
4. Run `just check` before handing off. It checks documentation, App tests,
   frontend type/build health, Rust
   formatting, tests, and Clippy.
5. Keep generated artifacts, dependencies, and local research checkouts out of
   commits. Reference repositories belong under the ignored `.references/`.
