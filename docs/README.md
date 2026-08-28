# Treer documentation

This is the maintained map for repository knowledge. Start with the document
matching the question; follow its source links for implementation detail.

## Maintained documents

| Document | Use it for |
| --- | --- |
| [Product direction](product.md) | Purpose, current audience, product promise, and delivery boundaries |
| [Capability roadmap](roadmap.md) | Real scenarios, capability planes, dependencies, and sequencing |
| [Architecture](architecture.md) | Components, ownership rules, protocols, state, and information flows |
| [Security model](security.md) | Trust tier, supported claims, credentials, isolation, and known gaps |
| [Quality and maintenance](quality.md) | Verification, documentation rules, current gaps, and review triggers |
| [Canary environment](canary.md) | Canary deployment, two-machine tests, and environment operations |
| [Release process](releases.md) | Immutable release manifests, Cloudflare App deployment, and Production promotion |
| [Workspace Apps](../apps/README.md) | App trust boundary, ownership, runtime model, and in-tree AIS adapters |
| [App guidelines](../apps/GUIDELINES.md) | Agent Markdown indexes, JSON data routes, and `/_human/` browser surfaces |
| [Gits App](../apps/gits/README.md) | Workspace-local Git hosting, deployment, storage, and trust boundary |
| [Treer Mail App](../apps/mail/README.md) | Mail setup, browser OAuth, legacy migration, backup, and limits |
| [Treer Telegram App](../apps/telegram/README.md) | Telegram setup, bindings, reply mapping, recovery, and limits |
| [Root README](../README.md) | Installation, deployment, and operator command examples |
| [Self-hosted Compose](../deploy/README.md) | GHCR images, updater sidecar, and `/admin` control-plane updates |
| [Treer agent skill](../skills/treer/SKILL.md) | Runtime CLI contract exposed to managed coding agents |
| [Treer install skill](../skills/treer-install/SKILL.md) | Recipe-install contract prompted when creating an installer with a git URL |

## Active execution plans

Active plans record approved target behavior and delivery gates. Source and
maintained current-state documents remain authoritative until each phase ships;
the plan identifies every maintained document that must change at completion.

- [Machine connection UX](research/2026-08-28-machine-connection-ux-plan.md) —
  truthful Online/local/fenced/stopped status, sleep/wake and duplicate
  reconnect, one Host per hostname+workspace, `proxy-env` internet bypass, and
  copyable recovery commands. Implement on `feat/machine-connection-ux`.

## Historical material

- The proposed [Agent communication policy design](research/2026-08-19-agent-policy-design.md)
  defines the identity propagation, policy document, cache, and rollout needed
  to govern Agent discovery, mail, prompt injection, and terminal control.
- The completed [machine services execution plan](research/2026-08-18-machine-services-plan.md)
  records the service-registry migration and verification scope.
- The [source-level project review](research/2026-08-18-project-review.md) is a
  snapshot of Treer at commit `72921f1`. It includes the technology survey,
  detailed flows, and comparisons with Herdr and AgentENV.
- The [prototype plan](../PLAN.md) records the original design and delivery
  rationale. Some shipped behavior has moved beyond its prototype non-goals.

Historical documents explain why decisions were made; they do not override
maintained documents, source, or tests.

## Authority and scope

Use this order when documents disagree:

1. Executable behavior, shared protocol types, and tests define what ships.
2. Maintained documents describe current intent and cross-component contracts.
3. The root README defines supported operator workflows.
4. Plans and dated research preserve context at a named revision.

The root [AGENTS.md](../AGENTS.md) is a development index. It deliberately links
to, but does not duplicate, the embedded Treer skill. Keep
`skills/treer/SKILL.md` at its current path because the CLI embeds it at build
time and prints it through `treer --skill` and `treer --skills`. Keep
`skills/treer-install/SKILL.md` next to it; `treer --skill install` and
Agent create `--recipe` embed that file.

## Update map

| Change | Documentation to review |
| --- | --- |
| User-visible setup or commands | `README.md`, embedded Treer skill |
| Self-hosted images or `/admin` update | `deploy/README.md`, release process, security model |
| Product scope or promise | Product direction |
| Capability classification or sequencing | Capability roadmap |
| Component ownership, route, protocol, or state | Architecture |
| Auth, credentials, policy, isolation, or security wording | Security model |
| Verification command, CI, known gap, or doc convention | Quality and maintenance |

Run `node scripts/check-docs.mjs` after documentation changes. The check verifies
required entry points, the embedded-skill contract, and repository-relative
Markdown links. Create an execution plan only when substantial work needs a
durable decision log; do not create empty document trees in anticipation of it.

This layout follows the progressive-disclosure model described in OpenAI's
[Harness engineering](https://openai.com/index/harness-engineering/): keep the
root map small and move durable detail into indexed, versioned repository files.
