# Security model

- Status: maintained
- Last source review: 2026-08-18 at `72921f1`

Treer's current security target is a personal or trusted-lab deployment. The
product offers scoped coordination and a clear upgrade path; it does not yet
provide strong isolation between mutually untrusted workspace members or
between an Agent and the owner of its execution machine.

## Supported trust tiers

| Tier | Runtime | Intended relationship | Status |
| --- | --- | --- | --- |
| Personal | Current local Host | One developer across owned machines | Supported by architecture |
| Lab | Current local Hosts in an organization workspace | Trusted or mostly trusted collaborators | Current primary fit |
| Managed | Ephemeral container or microVM backend | Untrusted customers and paid workflows | Future backend |

The local Host should remain available when a managed backend is added; the two
tiers optimize for different cost and trust requirements.

## Supported security story

The following statements are grounded in current behavior:

- Machines connect outward to the Proxy; operators do not need to publish SSH,
  Agent Server, or local service ports.
- A short-lived, single-use enrollment key creates a long-lived credential
  bound to one server and one workspace.
- Browser users authenticate, and Proxy lookups are scoped through organization
  membership and workspace identity.
- The browser application and Proxy use separate origins. Credentialed CORS and
  browser WebSocket checks accept only the configured App origin; session
  cookies remain HttpOnly and scoped to the Proxy host.
- Passwords, enrollment secrets, and machine credentials are hashed at rest.
- The stable Host owns processes on the enrolled machine rather than moving the
  runtime into an opaque hosted control plane.
- Remote working directories and transfer paths are constrained to the declared
  workspace root.
- Linux Agent network traffic crosses a namespace and Controller policy
  boundary, which can support stronger egress rules later.
- Linux Agent DNS resolution uses namespace-private resolver mounts and cannot
  reuse the host `nscd` socket, so virtual-host routing is not bypassed by host
  name-service plugins or cached answers.
- Registered machine services describe host-network endpoints. Registration,
  health probing, and routing do not cause Treer to own or sandbox the external
  service process.
- The Proxy is open source and can be self-hosted, inspected, and replaced.
- Managed Agents can exchange their private workload credential for a
  60-second, Ed25519-signed token bound to one registered service. The Proxy
  resolves the stable Agent, machine, workspace, and service IDs before signing.

These properties support the product phrase: **local custody, scoped
coordination, open control plane**.

## Claims not supported

Do not describe the current system as:

- zero trust or safe for mutually untrusted tenants;
- a guarantee that Agents cannot read files outside the workspace;
- per-user isolation of Codex, Claude, or other provider subscriptions;
- end-to-end encrypted from the central control plane;
- an enterprise sandbox or microVM runtime;
- fully attributable or auditable per human user.

The current Controller launches coding agents with permission-bypass flags. The
Linux wrapper isolates networking but not the host filesystem. The production
policy implementation currently permits all evaluated actions. Local
Controller access trusts the loopback boundary, and authorized workspace
members share a broad operational surface.

## Credential and identity boundaries

| Credential or identity | Scope | Important limitation |
| --- | --- | --- |
| User session cookie | User plus organization memberships | Browser identity is not propagated to Host operations end to end |
| Admin session cookie | User invitation creation and aggregate platform resource counts | Separate high-impact trust boundary |
| Enrollment key | One workspace, ten minutes, single use | Must be delivered to the intended machine securely |
| Machine Bearer credential | One machine record and workspace | Controller operations are attributed primarily to the machine |
| Agent ID | One Agent record in a workspace | Identifies a runtime, not the human who initiated every action |
| Agent workload credential | One managed Agent process | Same-account host processes may be able to inspect another process environment or Host metadata |
| Workload identity token | One Agent and machine in one workspace, audience-bound to one service for 60 seconds | The target application must validate it and this does not isolate hostile Agents sharing an OS account |
| Operation ID | One mutating request | Provides retry idempotency, not a durable audit record |

Codex and Claude currently inherit the authenticated CLI state of the operating
system account running the Host. Treer has no provider credential vault,
credential-owner binding, per-user runtime environment, token-usage ledger,
quota, or invoice model. Placing personal subscription credentials on a shared
Host would therefore extend trust to that Host and its operators; workspace
scoping alone does not isolate those credentials.

## Data and control-plane exposure

The Proxy can observe control messages, every requested network destination,
and relayed terminal, transfer, and workspace virtual-host data. Ordinary
outbound TCP payload stays between the source Controller and destination; the
Proxy authorizes its route but cannot observe its payload through Treer.
Browser-to-service tunneling strips cookies, authorization headers, proxy
authorization, and response `Set-Cookie` before forwarding, but this is not
end-to-end confidentiality from the Proxy.

The workload signing private key is stored in the Proxy PostgreSQL database. Its
Ed25519 public key is intentionally exposed through `/.well-known/jwks.json`;
the online verify endpoint exposes only claims already contained in a supplied
valid token. Tokens are not automatically attached to HTTP or generic TCP
traffic.

Durable identity data is stored in PostgreSQL. Live connections, pending
commands, terminal streams, transfers, and tunnels are owned by one Proxy
replica and routed across replicas through NATS when necessary.

## Hardening order

Strengthen the paths that real use makes important, while keeping the trust tier
explicit:

1. Emit append-only attribution and usage events before attempting billing.
2. Replace allow-all policy with reviewed defaults and auditable decisions.
3. Bind provider credentials and runtime actions to explicit owners.
4. Add filesystem/container or microVM isolation for managed workloads.
5. Add durable audit, revocation UX, quotas, and incident diagnostics.

Review this document with any change to authentication, credentials, policy,
networking, path handling, process launch flags, tenancy, or product security
language. Relevant source boundaries are
[`auth.rs`](../crates/treer-proxy/src/auth.rs),
[`policy.rs`](../crates/treer-proxy/src/policy.rs),
[`sandbox.rs`](../crates/treer-agent-server/src/sandbox.rs), and
[`network.rs`](../crates/treer-agent-server/src/network.rs).
