# Security model

- Status: maintained
- Last source review: 2026-08-21 at `239f9c6`

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
  cookies remain HttpOnly and scoped to the Proxy host. A login `return_to` is
  accepted only for the configured Proxy origin and the two exact ingress/App
  OAuth authorization paths.
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
- Core Messages are workspace-scoped and authorize send, read, receive,
  acknowledgement, and operator-only import independently. Context edges do not
  grant access to a parent body, stable deliveries remain repeatable until
  explicit acknowledgement, and body-free outbox events recover after restart.
  Multi-recipient sends and acknowledgement batches use one pinned policy
  revision, and denied/hidden recipient resolution returns one non-disclosing
  unavailable result.
- The official plugin runner clears the inherited environment, keeps raw Agent,
  machine, and operator credentials in the parent process, and exposes only a
  private manifest-limited broker to nested `treer` commands. Policy evaluates
  every allowed semantic command again at the Proxy.
- Plugin browser OAuth produces a revocable capability bound to one plugin,
  workspace, service, bridge Agent, and current human membership. Mail stores
  only an opaque local cookie-to-capability mapping; logout revokes the Core
  capability before deleting that mapping. Membership or service removal
  invalidates later use, and operator plugin uninstall revokes every matching
  Core session before removing local package versions.
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
- protected from a malicious plugin running as the same operating-system user;
- a claim that Telegram users are authenticated Treer human principals;
- an enterprise sandbox or microVM runtime;
- fully attributable or auditable per human user.

The current Controller launches coding agents with permission-bypass flags. The
Linux wrapper isolates networking but not the host filesystem. The production
policy implementation preserves allow behavior when a workspace has no policy
document. Local Controller control routes require either an Agent workload
credential or a separate operator credential.

The three Message/plugin rollout switches default off, but they are deployment
sequencing controls rather than security controls. Once enabled, ordinary
identity, immutable scope, and Policy checks remain authoritative. A plugin can
be launched by any same-UID process that can invoke the CLI with the execution
environment switch, so that switch must never be described as code isolation.

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
| Agent workload credential | One managed Agent process; independently validated by Controller and Proxy for managed-Agent discovery, control, terminal, service, and workload-token requests | Same-account host processes may inspect another process environment or Host metadata |
| Local operator credential | One installed Controller; used by the human CLI and never injected into managed-Agent environments | Stored under the same OS account, so it is not a sandbox boundary against a hostile same-account process |
| Workload identity token | One Agent and machine in one workspace, audience-bound to one service for 60 seconds | The target application must validate it and this does not isolate hostile Agents sharing an OS account |
| Human App identity token | One user and workspace, audience-bound to one enabled service for 12 hours | Apps own their sessions and authorization; Proxy verification rechecks current membership and service existence |
| Plugin broker token | One `treer plugin run` process and private Unix socket | Limits access to the broker but is not usable as a Proxy/Controller credential and is not a same-UID sandbox |
| Plugin-human capability | One user, workspace, plugin ID, service, and bridge Agent until expiry or revocation | Rechecks membership/service binding; logout, explicit revocation, or plugin uninstall revokes Core state, while the local cookie mapping and host process remain trusted |
| Telegram bot token | One Telegram bot, supplied only to the Telegram plugin as a declared secret | Telegram and any same-UID process able to inspect it can act as the bot; it is not a Treer identity |
| Telegram external identity | Numeric user, chat, topic, update, and Message IDs asserted by the bridge plugin | Admission metadata only; inbound Core Messages are authored by the authenticated bridge Agent |
| Operation ID | One mutating request | Provides retry idempotency, not a durable audit record |

Codex and Claude currently inherit the authenticated CLI state of the operating
system account running the Host. Treer has no provider credential vault,
credential-owner binding, per-user runtime environment, token-usage ledger,
quota, or invoice model. Placing personal subscription credentials on a shared
Host would therefore extend trust to that Host and its operators; workspace
scoping alone does not isolate those credentials.

The R2 release publisher writes detached Ed25519 signatures for immutable
manifests and mutable channel pointers, and records SHA-256 and byte length for
every binary plus the commit, version, platform, and Rust compiler for every
build. Cloudflare account access can publish or replace objects, but it does not
possess the release private key. This separation is not yet enforced by running
machines: the current updater consumes unsigned flat Proxy artifact URLs. Its
optional `--proxy` value directly selects that unverified binary source; without
the option, the source comes from the first locally installed service after
stable server-ID ordering. Do not claim supply-chain verification until the
Controller embeds the release public key and rejects unsigned, mismatched,
downgraded, or incompatible releases.

## Data and control-plane exposure

Canary version and branch-alias Preview URLs are public internet endpoints.
Version IDs and branch aliases are not authentication. Each preview exposes the
frontend bundle plus `/health` and `/config.json`, and its runtime configuration
points the browser at the Canary Proxy. The base Worker's `workers.dev` route
remains disabled. Deployments that require private review must protect previews
with Cloudflare Access or disable them; an unshared URL is not an access-control
boundary.

The Proxy can observe control messages, every requested network destination,
and relayed terminal and workspace virtual-host data. Canonical Message bodies,
recipients, context edges, and acknowledgement state are plaintext in Core
PostgreSQL and are visible to Proxy and database operators. They are not
end-to-end encrypted. Message bodies are deliberately excluded from ordinary
logs, structured errors, audit payloads, domain events, and the transactional
Message outbox; this does not protect database rows or backups. Core does not
yet provide an operator-facing retention/export/deletion policy or attachment
store, so deployments must manage PostgreSQL retention and backups explicitly.

Mail no longer owns a second Message database. Its plugin-owned SQLite state
maps opaque browser cookies to Core plugin-human capabilities; the Mail process
can observe bodies that it renders or sends. The Telegram plugin can observe
bridged bodies and stores external offsets, delivery hashes, errors, and
Telegram/Core ID mappings in its own SQLite database, but canonical bodies stay
in Core. Plugin uninstall does not delete these state databases. Its Bot API
token remains plugin-owned. A configured numeric Telegram
allowlist is channel admission, not Treer authentication, and Telegram account
compromise is outside Treer's trust boundary.

Agent launch profiles, including executable and argument arrays, are plaintext
and readable by workspace members and authorized managed Agents. Profile
commands run after the enrolled machine user's interactive shell startup files,
so those files remain part of the trusted execution environment. Launch
profiles are configuration, not a secret store; credentials and tokens must not
be placed in their command, arguments, description, or working directory.

Managed Agents may list stable user IDs, preferred names, and organization roles
for humans in their workspace organization, but the directory does not expose
member email addresses. Message recipient resolution uses the same
workspace-scoped Agent/human namespace. A sender may reference only an existing
same-workspace context it can already read; the edge never expands recipient
visibility. When no workspace policy document exists, policy currently defaults
to allow, including Message and plugin OAuth actions.

Ordinary outbound TCP payload stays between the source Controller and
destination; the Proxy authorizes its route but cannot observe its payload
through Treer.
Browser-to-service tunneling strips cookies, authorization headers, proxy
authorization, and response `Set-Cookie` before forwarding, but this is not
end-to-end confidentiality from the Proxy.

Custom Agent interfaces are active content supplied by a workspace Agent and
are currently served from the Proxy origin inside a sandboxed iframe. The
iframe blocks top-level navigation and direct parent access, and gateway
credentials are not forwarded to its machine service. This is not a hostile
content boundary: code in that page shares the Proxy origin and may attempt
same-origin control API requests using the viewer's session. Enable custom
interfaces only for trusted Agents and workspaces until Treer gives tunnel
content a separate cookie-free origin and capability-scoped browser session.

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

Message mutations are the first domain operations with a transactional outbox:
their body-free event row is committed with Message or delivery state and
retried by a restartable dispatcher. Other runtime domain events and runtime
audit writes remain best effort and are not covered by that Message-specific
guarantee.

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
4. Run untrusted plugins and managed workloads under separate users, containers,
   or microVMs with scoped secret delivery.
5. Add durable audit, revocation UX, quotas, and incident diagnostics.

Review this document with any change to authentication, credentials, policy,
networking, path handling, process launch flags, tenancy, or product security
language. Relevant source boundaries are
[`auth.rs`](../crates/treer-proxy/src/auth.rs),
[`policy.rs`](../crates/treer-proxy/src/policy.rs),
[`message_store.rs`](../crates/treer-proxy/src/message_store.rs),
[`plugin_store.rs`](../crates/treer-proxy/src/plugin_store.rs),
[`sandbox.rs`](../crates/treer-agent-server/src/sandbox.rs), and
[`network.rs`](../crates/treer-agent-server/src/network.rs).
