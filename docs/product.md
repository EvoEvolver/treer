# Product direction

## Purpose

Treer is a self-hostable control plane for interactive coding agents. It
connects machines into organization-scoped workspaces, keeps Agent processes
alive on those machines, and gives humans and Agents one place to observe,
prompt, organize and connect them through the browser and CLI.

It is built first for an individual with several machines and for trusted
teams or research groups that share long-running Agent sessions without
publishing machine ports or handing out direct machine access.

## What Treer provides today

> Local custody, scoped coordination, open control plane.

1. A small persistent Host and Controller on each machine you control, enrolled
   into one workspace with a single-use key. Machines connect outward; local
   ports stay private.
2. Agents launched from editable command profiles (any CLI agent or a plain
   terminal), with persistent PTYs, browser and native terminal attach,
   queued prompts, structured transcripts, and an embedded Agent UI when an
   Agent Interface Server is registered.
3. A workspace Tree of folders holding Agents and Apps, with paths usable as
   CLI targets and Policy selectors.
4. Durable Core Messages between humans and Agents with explicit
   acknowledgement and an ordered context DAG. Mail, Telegram and other
   channels are replaceable Apps over public contracts.
5. Managed Apps: supervised HTTP services with stable identity, platform-
   assigned origins, workspace login by default and human identity forwarded to
   the App.
6. Explicit service forwarding, per-workload HTTP proxy entries with automatic
   identity, public HTTP ingress and optional raw TCP publication.
7. Recovery of intended-running workloads after Controller replacement, Host
   restart or reboot, with explicit stop intent preserved.
8. An open-source Proxy deployable as SaaS, a Compose stack or a single
   container, scaling to several replicas with NATS.

This is a convenience-first promise with an understandable security reason. It
is not a claim that the runtime safely hosts mutually untrusted users; the
boundary is in the [security model](security.md).

## Principles

**Runtime before surfaces.** The durable product is the runtime and control
contract. Console, CLI and Apps are clients of the same API; every feature is
operable by Agents through the API the UI uses.

**Core semantics, replaceable channels.** Message data, recipients, context,
Policy, acknowledgement and persistence belong to Core. Channels own
presentation, transport, secrets and delivery mappings.

**User-owned execution environments.** Treer runs exact argv from editable
profiles. Users own Agent installation, provider credentials, containers, VMs
and filesystem or network restrictions. Treer does not build a sandbox.

**Minimal core.** New behavior goes into launchers, Apps or profiles unless it
needs a shared identity, routing or durability contract.

**Accurate security wording.** Make observable protections (local custody,
scoped credentials, outbound-only machines, inspectable code) easy to
understand, and never claim isolation the implementation lacks.

## Fit

| Scenario | Fit |
| --- | --- |
| One developer on several owned machines | Good |
| Trusted lab or team sharing sessions | Primary |
| Self-hosted internal Agent coordination | Good, if operators accept the gaps below |
| Public multi-tenant paid Agent service | Needs independently provided isolation |
| Protection from the enrolled machine's owner | Not provided |

## Known limits and direction

- Policy defaults to monitor/allow; restrictive defaults and decision audit
  are opt-in through a Policy Provider.
- Work is terminal- and Message-oriented. Durable work objects (issues, tasks,
  runs, artifacts) live in Apps such as [Issues](../apps/issues/README.md), not
  in Core.
- Core Messages are text-only, without operator retention, export or deletion
  policy.
- Provider subscription credentials are not isolated per user; Treer does not
  implement billing.
- Domain events outside the Message subsystem have no transactional outbox.
- App state has no automatic cross-version migration.

New capabilities should reuse Core identity, Policy, Tree and Message
references rather than invent parallel channels, task buses or identity
schemes.
