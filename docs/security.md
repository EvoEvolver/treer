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

- Machines connect outward to the Proxy; operators do not need to publish Agent
  Server or local service ports.
- A short-lived, single-use enrollment key creates a long-lived credential
  bound to one server and one workspace. Machine-only control requests cannot
  target another machine.
- Browser users authenticate, and Proxy lookups are scoped through organization
  membership and workspace identity.
- The browser application and Proxy use separate origins. Credentialed CORS and
  browser WebSocket checks accept only the configured App origin; session
  cookies remain HttpOnly and scoped to the Proxy host.
- Passwords, enrollment secrets, and machine credentials are hashed at rest.
- Password reset links contain a short-lived single-use secret whose Argon2
  hash is stored in PostgreSQL. A successful reset revokes all user sessions.
- GitHub and Google use server-side OAuth authorization-code flows with
  PostgreSQL-backed, ten-minute, single-use state. Provider access tokens are
  discarded after fetching identity data.
- OAuth account merging accepts only provider-verified email addresses. Later
  logins use the provider's stable subject ID rather than a mutable email or
  username. Merging into an account with an unverified email rotates its
  password and revokes its sessions to prevent account pre-hijacking.
- The stable Host owns processes on the enrolled machine rather than moving the
  runtime into an opaque hosted control plane.
- Linux Agent network traffic crosses a namespace and Controller policy
  boundary, which can support stronger egress rules later.
- Linux Agent DNS resolution uses namespace-private resolver mounts and cannot
  reuse the host `nscd` socket, so virtual-host routing is not bypassed by host
  name-service plugins or cached answers.
- Registered machine services describe host-network endpoints. Registration,
  health probing, and routing do not cause Treer to own or sandbox the external
  service process.
- The Proxy is open source and can be self-hosted, inspected, and replaced.
- Managed Agents authenticate to both the Controller and Proxy with their
  private workload credential. The Proxy stores only its SHA-256 hash and binds
  it to the Agent, machine, and workspace. Managed Agents can exchange it for a
  60-second, Ed25519-signed token bound to one registered service. The Proxy
  resolves the stable Agent, machine, workspace, and service IDs before signing.
- An HTTP machine service can be published under a generated wildcard hostname.
  Public endpoints deliberately admit anonymous internet traffic; workspace
  endpoints require a current organization member session or a workload token
  whose audience is the target service.
- Organization owners and administrators can read an append-only PostgreSQL
  audit of organization creation and rename, workspace creation, invitations,
  member role changes, member removal, launch-profile configuration changes,
  and successful Agent and machine lifecycle changes. The invitation secret,
  launch command and arguments, and message, prompt, terminal, and network
  payloads are excluded from audit event payloads.

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
policy implementation preserves allow behavior when a workspace has no policy
document. Local Controller control routes require either an Agent workload
credential or a separate operator credential.

The Controller preserves a validated managed-Agent identity and workload
credential when proxying Agent control HTTP and terminal WebSocket requests.
The Proxy validates the credential again and applies the stored workspace
policy. Machine-only requests are restricted to their own machine; an
authenticated Agent credential is mandatory for cross-machine control.

## Credential and identity boundaries

| Credential or identity | Scope | Important limitation |
| --- | --- | --- |
| User session cookie | User plus organization memberships | Browser identity is not propagated to Host operations end to end |
| OAuth state | One provider login attempt, ten minutes, single use | Protects the callback against login CSRF; provider availability remains an external dependency |
| OAuth provider identity | One provider subject linked to one Treer user | A verified provider email may merge into an existing account, making provider email verification an account-linking trust boundary |
| Password reset token | One user, 30 minutes, single use | Delivered through the configured email account; email access becomes an account-recovery trust boundary |
| Admin session cookie | User invitation creation and aggregate platform resource counts | Separate high-impact trust boundary |
| Enrollment key | One workspace, ten minutes, single use | Must be delivered to the intended machine securely |
| Release signing key | All official binary manifests and channel pointers signed by one trusted publisher | The private key is an offline operator credential and must never enter the repository, Proxy, R2, or managed Agent environment |
| Release public key | Verification of objects attributed to the release signing key | Distribution is trustworthy only after installed updaters embed and enforce this key; that verification is not implemented yet |
| Machine Bearer credential | One machine record and workspace | Machine-only control is limited to resources on that machine; the credential remains long-lived |
| Controller instance ID | One Controller process lifetime | Fences and diagnoses duplicate connections; it is carried inside an already authenticated machine connection and is not an authentication credential |
| Agent ID | One Agent record in a workspace | Identifies a runtime, not the human who initiated every action |
| Agent workload credential | One managed Agent process; independently validated by Controller and Proxy for managed-Agent discovery, control, terminal, mail, service, inbox, and workload-token requests | Same-account host processes may inspect another process environment or Host metadata |
| Local operator credential | One installed Controller; used by the human CLI and never injected into managed-Agent environments | Stored under the same OS account, so it is not a sandbox boundary against a hostile same-account process |
| Workload identity token | One Agent and machine in one workspace, audience-bound to one service for 60 seconds | The target application must validate it and this does not isolate hostile Agents sharing an OS account |
| Operation ID | One mutating request | Provides retry idempotency, not a durable audit record |

Codex and Claude currently inherit the authenticated CLI state of the operating
system account running the Host. Treer has no provider credential vault,
credential-owner binding, per-user runtime environment, token-usage ledger,
quota, or invoice model. Placing personal subscription credentials on a shared
Host would therefore extend trust to that Host and its operators; workspace
scoping alone does not isolate those credentials.

The R2 release publisher writes detached Ed25519 signatures for immutable
manifests and mutable channel pointers, and records SHA-256 and byte length for
every binary. Cloudflare account access can publish or replace objects, but it
does not possess the release private key. This separation is not yet enforced
by running machines: the current updater consumes unsigned flat Proxy artifact
URLs. Do not claim supply-chain verification until the Controller embeds the
release public key and rejects unsigned, mismatched, downgraded, or incompatible
releases.

## Data and control-plane exposure

The Proxy can observe control messages, durable Agent mail bodies and metadata,
every requested network destination, and relayed terminal and workspace
virtual-host data. Agent mail is stored as plaintext in PostgreSQL;
Agent launch profiles, including their executable and argument arrays, are also
stored as plaintext and readable by workspace members and authorized managed
Agents. Launch profiles are configuration, not a secret store; credentials and
tokens must not be placed in their command, arguments, description, or working
directory.
context IDs may reference only same-workspace messages the sender previously
sent or received. Managed Agents may list the stable user ID, preferred name,
and organization role of humans in their workspace organization, but the Agent
directory does not expose member email addresses. Mail resolves Agent and human
IDs or unique display names through one workspace-scoped recipient namespace.
Human inbox reads require a current user session and current membership in the
workspace organization. Mailbox history contains only deliveries addressed to
that user; the browser does not dereference context IDs that are absent from
that history. Ordinary
outbound TCP payload stays between the source Controller and destination; the
Proxy authorizes its route but cannot observe its payload through Treer.
Browser-to-service tunneling strips cookies, authorization headers, proxy
authorization, and response `Set-Cookie` before forwarding, but this is not
end-to-end confidentiality from the Proxy.

Wildcard service ingress has different semantics from the authenticated
browser-to-virtual-host tunnel. It preserves application cookies,
`Authorization`, and response `Set-Cookie`. The Proxy consumes its host-only
ingress cookie and `Treer-Authorization`, removes client-supplied `X-Treer-*`
and forwarding headers, and then forwards the application request. Published
applications therefore remain responsible for their own authorization, input
validation, abuse controls, and data isolation. Treer reserves `/.treer/` on
published hosts for its authorization callback.

The Proxy retains hourly machine-to-machine traffic metadata: workspace, source
machine, destination machine, payload byte count, and data-frame count. It does
not retain network payloads. The traffic query uses the same workspace
membership middleware as other workspace APIs.

The workspace Audit page combines that traffic summary with the organization's
management-event ledger. Audit writes for covered organization mutations share
the same PostgreSQL transaction as the mutation. Runtime audit events are
written after a successful Controller result and are best effort so an audit
storage outage does not cause a client to retry an already-completed runtime
mutation. This is bounded management attribution, not end-to-end human
attribution for every Agent action.

The workload signing private key is stored in the Proxy PostgreSQL database. Its
Ed25519 public key is intentionally exposed through `/.well-known/jwks.json`;
the online verify endpoint exposes only claims already contained in a supplied
valid token. Tokens are not automatically attached to HTTP or generic TCP
traffic.

Durable identity data is stored in PostgreSQL. Live connections, pending
commands, terminal streams, and tunnels are owned by one Proxy
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
