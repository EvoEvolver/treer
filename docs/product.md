# Product direction

- Status: maintained
- Last source review: 2026-09-30 core simplification (working tree)

## Purpose

Treer is a self-hostable collaboration layer for interactive coding agents. It
connects machines into organization-scoped workspaces, keeps agent processes
alive on those machines, and provides browser, CLI, terminal, and private-service
paths through one open control plane.

The first useful product is for an individual or a trusted research group that
wants to share long-running Agent sessions without publishing machine ports or
handing every collaborator unrestricted direct machine access.

## Current product promise

> Local custody, scoped coordination, open control plane.

In concrete terms:

1. A user installs a small persistent service on a machine they control.
2. A short-lived, single-use link enrolls that machine into one workspace.
3. Workspace members can upload a bounded file into an enrolled machine root,
   run a short non-interactive command, save reusable command-based launch profiles, and use
   them to create, observe, prompt, and stop Agents through the web application,
   CLI, or native iOS/Android clients. The browser still includes a full-screen
   mobile terminal with touch controls and a full-screen Agent UI iframe. The
   native apps use a fleet Home, an AIS WebView for thread UI, and an optional
   native TUI overlay.
4. Managed Agents and humans exchange durable Core Messages with explicit
   acknowledgement and an ordered context DAG. Mail and Telegram are
   replaceable workspace Apps over public contracts rather than privileged
   Proxy features.
5. Agents can register their own ordinary TCP/HTTP endpoints, forward services
   across the workspace, and deploy supervised HTTP Apps. Public HTTP origins use
   platform-assigned domains and workspace authentication by default. Explicit
   anonymous publication is available for HTTP and configured raw TCP ports.
   Managed Agents and Apps inherit one HTTP proxy entry each for automatic
   identity on registered service requests; subprocesses share that identity.
6. An Agent Interface Server can declare a browser UI path alongside its
   semantic capabilities. The control plane embeds that page in place of the
   Agent terminal, and transparently tunnels relative HTTP and WebSocket
   traffic to the registered Interface port.
7. Machines connect outward to the Proxy; local Agent Server and service ports
   do not need to be publicly exposed. A workspace member can tell whether a
   machine is Online, local-only, fenced as a duplicate, or stopped, and can
   copy a recovery command that includes the real workspace ID. Lid-close
   sleep and Proxy bounces reconnect without re-enrollment. One hostname in
   one workspace is one supervised Host.
8. The central Proxy is open source and can be privately deployed.
9. Organization owners and administrators can review management activity.
   Every workspace member can inspect recent billable logical payload usage,
   classified by machine network, service ingress, virtual host, and Agent UI.
   Broker overhead and deployment topology do not affect that usage.

This is a convenience-first promise with a comprehensible security reason. It
is not a claim that the current runtime safely hosts mutually untrusted users.
The exact trust boundary is documented in the [security model](security.md).

## Product principles

### Runtime before surfaces

The durable product is the runtime and control contract. The browser, CLI,
voice input, richer collaboration, and future applications are clients of that
contract. Product features should not bypass shared identities and protocols to
become one-off UI behavior.

### Core semantics, replaceable channels

Canonical Message data, recipients, context edges, policy, acknowledgement,
idempotency, and persistence belong to Core and are available through
`treer message`. External channels own presentation, transport, secrets, and
delivery mappings. Browser channels use service-audience App tokens; Agent-run
channels use the managed Agent's CLI identity. This keeps Mail, Telegram, and
future adapters independently replaceable without creating parallel
collaboration databases or private Core integrations.

### User-owned execution environments

Treer launches exact argv from editable profiles. Users own Agent installation,
provider credentials, external proxy configuration, containers, VMs, and filesystem/network
restrictions. Treer does not build or promise a sandbox backend. Its CLI uses a
separate Agent credential so a user-provided isolation boundary can expose only
that identity. Without such a boundary, processes under the same account are
trusted and can reach each other.

### Persuasive security, accurate wording

The early product should make its observable protections easy to understand:
local process custody, workspace-scoped machine credentials, outbound-only
machine connections, and inspectable control-plane code. Security messaging
must not claim isolation or attribution that the implementation lacks.

### Open coordination, protectable execution

Open sourcing the control plane builds trust and allows private deployment. A
commercial workflow may still keep proprietary prompts, orchestration, data, or
managed execution on infrastructure controlled by its operator. Treer supplies
the coordination boundary; it does not prevent a machine owner from
inspecting an Agent that runs on that machine.

## Current fit and non-fit

| Scenario | Current fit |
| --- | --- |
| One developer using several owned machines | Good architectural fit |
| Trusted or mostly trusted lab sharing sessions | Primary product fit |
| Self-hosted internal Agent coordination | Good fit if operators accept current gaps |
| Public multi-tenant paid Agent service | Requires independently provided isolation; outside Core scope |
| Strong protection from the enrolled machine owner | Not provided by the local Host |

## Near-term sequence

The maintained [capability roadmap](roadmap.md) classifies the larger backlog
preserves the broader backlog. The active sequence is the
[core simplification plan](research/2026-09-30-core-simplification-plan.md):

1. Keep authentication, scoped identity, explicit connections, and durable process intent.
2. Move command parsing and help into the Controller; keep `treer` a transport client.
3. Remove transparent capture, virtual DNS, provider installation, and custom-host provisioning.
4. Make restoration repeatable: attach surviving processes, restart intended-running
   processes from saved launch snapshots, and report stopped/revoked/failed items.

Subscription credentials are not currently isolated per platform user, and
Treer does not implement billing. Core Messages are text-only and do not yet
have operator-facing retention, export, or deletion policy. App state has no
automatic cross-version migration. See [Security model](security.md) for the
credential risk and [Architecture](architecture.md) for the runtime boundary.
