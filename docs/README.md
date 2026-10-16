# Treer documentation

This is the maintained map for repository knowledge. Start with the document
matching the question; follow its source links for implementation detail.

## Maintained documents

| Document | Use it for |
| --- | --- |
| [Team roles and ownership](team-roles.md) | Directory owners, review boundaries and collaboration rules |
| [Team collaboration protocol](team-protocol.md) | Tickets as source of truth, review and release comments, tiered releases |
| [Product direction](product.md) | Purpose, current audience, product promise, and delivery boundaries |
| [Capability roadmap](roadmap.md) | Real scenarios, capability planes, dependencies, and sequencing |
| [Architecture](architecture.md) | Components, ownership rules, protocols, state, and information flows |
| [Security model](security.md) | Trust tier, supported claims, credentials, isolation, and known gaps |
| [Policy Provider protocol](policy-provider.md) | App-supplied policy bundle contract, caching, failure behavior, and management APIs |
| [Quality and maintenance](quality.md) | Verification, documentation rules, current gaps, and review triggers |
| [Canary environment](canary.md) | Canary deployment, two-machine tests, and environment operations |
| [Release process](releases.md) | Immutable release manifests, Cloudflare App deployment, and Production promotion |
| [Workspace Apps](../apps/README.md) | App trust boundary, ownership, runtime model, and in-tree AIS adapters |
| [Agent launchers](../launchers/README.md) | Optional profile-driven Agent runtimes and presentation boundaries |
| [App guidelines](../apps/GUIDELINES.md) | Agent Markdown indexes, JSON data routes, and `/_human/` browser surfaces |
| [Issues App](../apps/issues/README.md) | Shared human/Agent issue tracker deployment, API, permissions and backup |
| [Gits App](../apps/gits/README.md) | Workspace-local Git hosting, deployment, storage, and trust boundary |
| [Treer Mail App](../apps/mail/README.md) | Mail setup, browser OAuth, legacy migration, backup, and limits |
| [Treer Telegram App](../apps/telegram/README.md) | Telegram setup, bindings, reply mapping, recovery, and limits |
| [Root README](../README.md) | Product overview, core philosophy, capabilities, and the shortest hosted start |
| [Operator guide](operator-guide.md) | Local development, enrollment, auth, networking, messaging, and command examples |
| [Self-hosted Compose](../deploy/README.md) | GHCR images, updater sidecar, and `/admin` control-plane updates |
| [Treer team setup skill](../skills/treer-team/SKILL.md) | Existing Agent/App integration, presets and new-machine setup |
| [Treer agent skill](../skills/treer/SKILL.md) | Runtime CLI contract exposed to managed coding agents |
| [Treer install skill](../skills/treer-install/SKILL.md) | Recipe-install contract prompted when creating an installer with a git URL |
| [macOS container skill](../skills/treer-macos-container/SKILL.md) | Apple container machine Host setup on a Mac |
| [Treer voice skill](../skills/treer-voice/SKILL.md) | Spoken assistant context used by the Proxy Voice LLM; not printed by `treer --skill` |
| [Voice APIs](operator-guide.md#voice-apis-opt-in) | Opt-in Proxy ASR and spoken command; native clients retired |
| [Treer UI skill](../skills/treer-ui/SKILL.md) | UI conventions and review checklist for the Console, launcher UI and App browser UIs; not printed by `treer --skill` |

## Active execution plans

- [Supply-chain security review (2026-10-12)](research/2026-10-12-security-supply-chain.md):
  local findings, release-workflow fixes, dependency audit snapshot and minimum
  baseline for critical-service deployment.

Active plans record approved target behavior and delivery gates. Source and
maintained current-state documents remain authoritative until each phase ships;
the plan identifies every maintained document that must change at completion.

- [Unified App and Agent workloads](research/2026-10-11-unified-workloads.md):
  stable identity and generation, one Controller lifecycle owner, shared CLI/UI
  controls, persistent workspace folders, legacy migration, and Canary gates.

- [Core simplification, explicit service forwarding, and recovery](research/2026-09-30-core-simplification-plan.md):
  target boundaries for a thin CLI, user-owned runtime environments, private/public
  port forwarding, platform-assigned service domains, and restart recovery. This
  replaces the native transparent-network expansion direction. The working-tree
  implementation and its validation/remaining deployment gates are tracked there.

- [Native Windows support](research/2026-10-11-native-windows.md): source audit,
  Windows target compilation evidence, local IPC and process adaptations,
  user-mode installation, manual recovery, and proposed native acceptance gates.
  Initial implementation and native CI gates are in the working tree; Windows
  runtime acceptance and artifact publication remain pending.

## Design proposals

- [Policy path rules](research/2026-10-13-policy-path-rules-design.md) and
  [schema v2 contract](research/2026-10-13-policy-schema-v2-contract.md): current
  Tree facts, component paths and folder relations, owner diagnostics, staged App
  action checks, consistency, migration and recovery boundaries.
- [Tree links](research/2026-10-13-tree-links-design.md): discovery/addressing
  aliases without automatic grants; permissions remain separate Policy rules.

- [Policy enforce migration](research/2026-10-12-policy-enforce-migration.md):
  planning only; versioned deny defaults, administrator recovery, existing-workspace
  previews, UI authority, token windows, rollout and rollback risks.

- [Web security review, 2026-10-12](research/2026-10-12-security-web.md):
  local browser/header and native cookie checks, remaining interface trust
  boundaries, dependency advisories and explicit verification limits.

- [Core security review, 2026-10-12](research/2026-10-12-security-core.md):
  severity-ranked authentication/authorization audit, local identity-routing and
  diagnostic-secret fixes, and explicit limits on defaults, revocation, backends,
  resource budgets, audit, and filesystem isolation.

- [Architecture review, 2026-10-12](research/2026-10-12-architecture-review.md):
  severity-ranked source audit of ownership, delivery, environment inheritance,
  recovery, release compatibility, App deployment, and tree semantics; records
  the focused virtual-host revision fix and larger proposals separately.

- [External Agent identity and remote CLI access](research/2026-09-30-external-agent-cli-plan.md):
  proposed separation of identity from Host-managed processes, a public CLI
  gateway backed by the existing Agent Server parser, and optional explicit
  reverse service forwarding. Planning only; implementation is not authorized
  by this document.

## Historical material

- [Per-Agent/App HTTP proxy entries](research/2026-10-08-workload-http-proxy.md)
  records explicit identity-bound listeners, inherited client configuration,
  automatic service tokens, recovery behavior, and local Docker validation.
- The [local Docker regression report](research/2026-10-08-local-docker-regression-report.md)
  records current-source installation, two-Proxy communication, App deployment,
  browser access, and cold recovery checks, identified install/CLI gaps, their
  fixes, and the unattended Docker regression including recent-App cold recovery.
- The [native macOS networking execution log](research/2026-09-05-macos-network-execution.md)
  preserves the previous parity plan and its evidence. Further network-capture
  development is superseded by the core simplification plan.
- The [macOS transparent network investigation](research/2026-09-05-macos-transparent-network.md)
  records successful Apple Linux machine and native macOS capture probes, real
  Policy/relay-ledger experiments, the native half-close failure, and Tailscale references.
- The completed [machine connection UX plan](research/2026-08-28-machine-connection-ux-plan.md)
  records truthful Online/local/fenced/stopped status, sleep/wake reconnect,
  one Host per hostname+workspace, and `proxy-env` internet bypass.
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
3. The operator guide defines supported operator workflows; the root README is
   the concise product entry point.
4. Plans and dated research preserve context at a named revision.

The root [AGENTS.md](../AGENTS.md) is a development index. It deliberately links
to, but does not duplicate, the embedded Treer skill. Keep
`skills/treer/SKILL.md` at its current path because the Controller embeds it at build
time and serves it through `treer --skill` and `treer --skills`. Keep
`skills/treer-install/SKILL.md` next to it; `treer --skill install` serves it as
documentation for user-owned installers. Automatic `--recipe` execution is retired. Keep
`skills/treer-macos-container/SKILL.md` for Mac container-machine Host
setup; `treer --skill macos-container` prints it. The image Dockerfile is
`deploy/apple-container-machine/Dockerfile`.

## Update map

| Change | Documentation to review |
| --- | --- |
| User-visible setup or commands | Operator guide, embedded Treer skill |
| Apple container machine Host | `skills/treer-macos-container/SKILL.md`, `deploy/apple-container-machine/Dockerfile` |
| Self-hosted images or `/admin` update | `deploy/README.md`, release process, security model |
| Product scope or promise | Product direction |
| Capability classification or sequencing | Capability roadmap |
| Component ownership, route, protocol, or state | Architecture |
| Auth, credentials, policy, isolation, or security wording | Security model |
| Verification command, CI, known gap, or doc convention | Quality and maintenance |
| Voice ASR or command LLM | Operator guide Voice APIs, architecture, security model |

Run `node scripts/check-docs.mjs` after documentation changes. The check verifies
required entry points, the embedded-skill contract, and repository-relative
Markdown links. Create an execution plan only when substantial work needs a
durable decision log; do not create empty document trees in anticipation of it.

This layout follows the progressive-disclosure model described in OpenAI's
[Harness engineering](https://openai.com/index/harness-engineering/): keep the
root map small and move durable detail into indexed, versioned repository files.
