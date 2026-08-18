# Product direction

- Status: maintained
- Last source review: 2026-08-18 at `72921f1`

## Purpose

Treer is a self-hostable collaboration layer for interactive coding agents. It
connects machines into organization-scoped workspaces, keeps agent processes
alive on those machines, and provides browser, CLI, terminal, transfer, and
private-service paths through one open control plane.

The first useful product is for an individual or a trusted research group that
wants to share long-running Agent sessions without publishing SSH ports or
handing every collaborator unrestricted direct machine access.

## Current product promise

> Local custody, scoped coordination, open control plane.

In concrete terms:

1. A user installs a small persistent service on a machine they control.
2. A short-lived, single-use link enrolls that machine into one workspace.
3. Workspace members can create, observe, prompt, and stop Codex, Claude, or
   shell agents through the web application or CLI.
4. Managed agents can discover and coordinate with peers through a stable CLI
   contract rather than knowing machine addresses.
5. Agents can register and maintain long-running machine services, then expose
   stable workspace aliases without making service ports public.
6. Machines connect outward to the Proxy; local Agent Server, service, and SSH
   ports do not need to be publicly exposed.
7. The central Proxy is open source and can be privately deployed.

This is a convenience-first promise with a comprehensible security reason. It
is not a claim that the current runtime safely hosts mutually untrusted users.
The exact trust boundary is documented in the [security model](security.md).

## Product principles

### Runtime before surfaces

The durable product is the runtime and control contract. The browser, CLI,
voice input, richer collaboration, and future applications are clients of that
contract. Product features should not bypass shared identities and protocols to
become one-off UI behavior.

### Local first, managed later

The current local Host is the fastest path to personal and lab adoption. A
future container or microVM backend can implement the same control-plane
contract for public, paid, or mutually untrusted workloads without forcing that
operational cost into the prototype.

### Persuasive security, accurate wording

The early product should make its observable protections easy to understand:
local process custody, workspace-scoped machine credentials, outbound-only
machine connections, and inspectable control-plane code. Security messaging
must not claim isolation or attribution that the implementation lacks.

### Open coordination, protectable execution

Open sourcing the control plane builds trust and allows private deployment. A
commercial workflow may still keep proprietary prompts, orchestration, data, or
managed execution on infrastructure controlled by its operator. Treer supplies
the coordination boundary; it does not yet prevent a machine owner from
inspecting an Agent that runs on that machine.

## Current fit and non-fit

| Scenario | Current fit |
| --- | --- |
| One developer using several owned machines | Good architectural fit |
| Trusted or mostly trusted lab sharing sessions | Primary product fit |
| Self-hosted internal Agent coordination | Good fit if operators accept current gaps |
| Public multi-tenant paid Agent service | Future runtime and accounting work required |
| Strong protection from the enrolled machine owner | Not provided by the local Host |

## Near-term sequence

1. Make enrollment, session sharing, peer coordination, and updates feel
   reliable enough to drive real use.
2. Emit append-only attribution events carrying user, workspace, machine,
   agent, provider, model, time, and provider-reported usage when available.
3. Add policy, quota, audit, and credential ownership only as actual workflows
   demand them.
4. Add a production isolation backend for the managed trust tier while keeping
   the current Host for personal and lab use.

Subscription credentials are not currently isolated per platform user, and
Treer does not implement billing. See [Security model](security.md) for the
credential risk and [Architecture](architecture.md) for the runtime boundary.
