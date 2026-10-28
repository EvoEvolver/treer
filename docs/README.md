# Treer documentation

Start with the document matching your question and follow its links into the
source. When a document and the code disagree, the code, shared protocol types
and tests win; fix the document.

| Document | Use it for |
| --- | --- |
| [Root README](../README.md) | What Treer is and the shortest way to try it |
| [Product direction](product.md) | Audience, promise, principles, known limits |
| [Architecture](architecture.md) | Components, process model, Proxy cluster, Messages, Apps, Policy, routing, data stores |
| [Operator guide](operator-guide.md) | Deploying, enrolling machines, launchers, Apps, networking, updates, backup, troubleshooting |
| [Self-hosted deployment](../deploy/README.md) | Compose and single-container details, images, tags, updater |
| [Security model](security.md) | Trust model, defended boundaries, unsupported claims |
| [Policy Provider protocol](policy-provider.md) | App-supplied Policy bundles, caching, failure behavior, management APIs |
| [App-owned trees](app-tree.md) | App-implemented child directories and Message sub-addresses |
| [Release process](releases.md) | Hosted deploys, Canary, machine artifacts, images, signed releases |
| [Quality and maintenance](quality.md) | Verification commands, CI, known gaps, documentation rules |
| [Workspace Apps](../apps/README.md) | App contract and the in-repo Apps |
| [App guidelines](../apps/GUIDELINES.md) | Agent-readable indexes, JSON routes, `/_human/` browser surfaces |
| [Gits App](../apps/gits/README.md) | Workspace Git hosting |
| [Agent launchers](../launchers/README.md) | Optional launcher boundaries and the [Agent launcher](../launchers/agent/README.md) |
| [Console](../web/README.md) | Web Console development and hosting |

## Skills

Skills are instructions for Agents. The first three are embedded in the
Controller and printed by `treer --skill [install|macos-container]`, so keep
their paths stable.

| Skill | Purpose |
| --- | --- |
| [treer](../skills/treer/SKILL.md) | CLI contract for managed Agents |
| [treer-install](../skills/treer-install/SKILL.md) | Installing a git recipe as a launch profile |
| [treer-macos-container](../skills/treer-macos-container/SKILL.md) | Running a Host in an Apple container machine (`deploy/apple-container-machine/Dockerfile`) |
| [treer-team](../skills/treer-team/SKILL.md) | Integrating Agents and Apps into a workspace, presets, new-machine setup |
| [treer-ui](../skills/treer-ui/SKILL.md) | UI conventions for the Console, launcher UI and App UIs |

## Keeping docs current

| Change | Update |
| --- | --- |
| Commands or setup | Operator guide, `skills/treer/SKILL.md` |
| Components, routes, protocols, state | Architecture |
| Auth, credentials, Policy, isolation | Security model |
| Self-host images or updater | `deploy/README.md`, release process |
| Verification or CI | Quality |

Run `node scripts/check-docs.mjs` after documentation changes.
