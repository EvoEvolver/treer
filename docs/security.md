# Security Model

Treer is a control plane for coding agents, not a sandbox. Claims not listed
here are not made.

## Trust model

- **One OS account on one machine is one trust domain.** Every process running
  as the same OS user (Host, Controller, Agents, Apps, launchers, tools, the
  user's shell) is trusted with everything that user can read or do, including
  each other's credentials and state files. Treer does not defend its own
  binaries, state, sockets or processes against a same-user process.
- **Workspace members and enrolled machines are trusted administrators of their
  workspace** unless an enforcing Policy restricts them. The default Policy is
  monitor/allow, not deny-by-default.
- **The Proxy is the multi-tenant boundary.** Accounts, organizations,
  workspaces, sessions, tokens, Policy and App identity are enforced there, for
  SaaS and self-hosted clusters alike.
- **PostgreSQL, NATS and the Proxy host are trusted backends.** Use separate OS
  users, containers or VMs to isolate code or people on one machine.

## What is defended

### Tenant isolation and authentication at the Proxy

- Every Core route authenticates first and scopes the subject to one workspace
  before Policy runs. Restricted-workspace membership (direct/group grant,
  creator, organization manager) is recomputed from PostgreSQL.
- Human sessions are opaque HttpOnly cookies (`treer_session`); the platform
  administrator has a separate cookie scoped to `/api/admin` that exposes only
  aggregate statistics, never tenant inventories.
- Password sign-in uses constant-time comparison and a decoy hash for unknown
  accounts, and is limited to 10 attempts per account per 15 minutes across all
  replicas (counted in PostgreSQL; `treer-proxy --reset-login-attempts` clears
  a lockout).
- Organization invitations are accepted only by a human cookie session from
  the exact Console origin; OAuth invitation state is hashed and single-use.
- Credentials are scoped and short-lived where possible:

| Credential | Scope and lifetime |
| --- | --- |
| Enrollment key | One workspace, ten minutes, single use |
| Machine credential | One enrolled machine and workspace, until rotation/removal |
| Agent workload credential | One managed workload across process generations |
| Local operator credential | One Controller install; never injected into Agents |
| Workload identity token | One Agent/machine/service audience, 60 seconds |
| Human App token | One user/workspace/service audience; 12 h via OAuth, 60 s via browser forwarding |
| Release signing key | Offline; signs official release manifests |

### Authorization and Policy

- Policy evaluates authenticated subjects against workspace resources:
  `machine.exec`, `machine.file.read`/`machine.file.write`, `agent.create`,
  `agent.stop`, `agent.prompt`, `agent.startup.read`/`manage`, `agent.tree.*`,
  `agent.discover`, `workspace.audit.read` and the service actions. Monitor mode
  computes denials without enforcing them.
- Agent-authenticated mutations are bound to the Agent's own machine and ID;
  cross-owner requests fail before Policy can grant them.
- Only a current owner selects a Policy Provider (trusted code) or its
  publication authority; delegated Policy cannot deny owner recovery.
- Workspace audit is a sensitive read (owners, or an explicit enforce-mode
  allow), projected through per-action allowlists and served `no-store`.

### Machine enrollment and the Controller connection

- A machine enrolls once with a single-use key and then authenticates with its
  workspace-bound credential. Enrollment binds the machine ID to one
  installation identity; a deleted machine's ID is never inherited by another
  installation.
- Machine WebSocket ownership in the cluster is leased atomically; responses
  must match their pending target machine and current connection. Snapshots and
  Agent events are checked against durable Agent credential bindings.
- Machine exec and file transfer stay beneath the enrolled root, refuse the
  Treer state directory (credentials, private state) under any Policy, and run
  exec with a minimal environment that carries no Treer credentials.
- Chunked uploads are re-authorized per request, bound to the subject that
  began them and quota-limited in PostgreSQL.
- The Controller refuses link-local service targets (cloud metadata) unless
  `TREER_ALLOW_LINK_LOCAL_SERVICES` is set on that machine.

### Network ingress

- Public service ingress is anonymous by design and rate-, size- and
  time-limited per service and per verified client address. Client addresses
  come from the TCP peer unless a trusted front is configured
  (`--ingress-trusted-proxies` plus one `--ingress-client-ip-header`); the Proxy
  strips client `x-treer-*` headers and forwards `X-Treer-Client-IP`.
- Workspace ingress requires a current member session or service-audience token
  and a browser-bound login code (`__Host-` verifier cookie, PKCE-style
  challenge, single use, five minutes).
- Managed App OAuth requires S256 PKCE, exact redirect matching and single-use
  codes. An App selected as the active Policy Provider cannot be made public.

### Browser security

<a id="control-plane-browser-csrf"></a>
**Control-plane CSRF.** With authentication enabled, cookie-authenticated
POST/PUT/PATCH/DELETE requests and WebSocket upgrades require exactly one
`Origin` equal to the configured Console origin (the App UI, Agent interface
and virtual-host tunnel routes also accept the Proxy origin). Missing, `null`,
duplicate and foreign origins get `403 csrf_denied`; contradicting Fetch
Metadata is refused. Classification uses the matched route template. Bearer
clients do not need an Origin; an invalid Bearer never falls back to a cookie.
Login, registration, reset and admin login are checked before their handlers;
OAuth start is a same-origin JSON POST.

**CSP, framing and COOP.** Tunnelled App and Agent UIs keep their CSP; the Proxy
replaces only `frame-ancestors` with the Console origin and `'self'`. The Console
HTML is served with `Cross-Origin-Opener-Policy: same-origin` and a CSP that
forbids framing (Proxy `app_dist`, the Worker and the nginx image). When the
Proxy serves the Console itself, App interfaces on the Console origin are
refused.

<a id="embed-sessions"></a>
**Agent interface origins and embed sessions.** With wildcard ingress, each
Agent UI is served on its own `ag-<id>` origin, so its JavaScript does not share
the Console or Proxy origin. The Console mints a single-use code
(`agent.output.read` and `agent.prompt` checked for the human); the Agent origin
redeems it into a 12-hour `__Host-treer_ingress_embed` cookie
(`Secure; HttpOnly; SameSite=None; Partitioned`). Every request requires
`Sec-Fetch-Site: same-origin` (or a WebSocket), rechecks membership and Policy,
and sockets close on logout or membership removal. Browsers that block the
partitioned cookie fall back to the shared-origin Proxy tunnel unless the
operator sets `--agent-embed-fallback refuse`. Embedded Apps and Agent UIs
remain trusted workspace code.

### Cross-user isolation on a shared machine

- Credential and state files are written atomically with mode 0600 and
  directories 0700 (Windows: a protected DACL for the current user and SYSTEM),
  so other OS users cannot read them.
- The Host socket is mode 0600 inside a per-user runtime directory; Windows
  named pipes reject remote clients and grant only the current user and SYSTEM.
- The local Controller API listens on loopback and requires the operator or
  workload credential, because other OS users can reach loopback.
- Per-Agent private state lives in 0700 directories (cooperative quota).

### Release signing

- Controller updates verify the signed release manifest against a preconfigured
  Ed25519 public key, then each binary's size and SHA-256, and fail closed.
  The key never comes from the documents being verified; only the official
  distributor may use the bundled key.
- The Unix bootstrap on the official origins verifies the signed channel and
  manifest. Self-hosted bootstrap installers check same-origin SHA-256 only, and
  the Proxy withholds copyable install commands on cleartext non-loopback URLs.

## Unsupported claims

Treer explicitly does **not** defend against or promise:

- Anything a process running as the same OS user can do: read or replace
  Treer binaries, credentials, private state, startup stores or sockets; spoof
  local provenance headers; impersonate another Agent through its local proxy
  entry. Treer does not pin digests of its own files, recheck ancestry or
  ownership on every operation, or refuse to start on local integrity mismatch.
- A sandbox, zero-trust or mutually untrusted multi-tenancy on one machine.
  Agents, Apps, launchers and machine exec run with the machine account's full
  authority; Folders and Deny rules are not containment.
- Deny-by-default permissions without an explicit enforcing Policy.
- Immediate revocation: bearer tokens live until expiry, Policy caching and
  TCP reauthorization polling are not instantaneous.
- Provider-secret isolation, redaction of secrets in transcripts, exports,
  argv, startup specifications or logs, or resistance to prompt injection.
- Complete, tamper-proof audit; protection from database, host or NATS
  operators; per-tenant fairness of shared capacity.
- Release freshness, downgrade protection, SBOMs or reproducible builds;
  Compose image updates do not verify signatures.
- Isolation of embedded App or Agent JavaScript from the origin it runs on, or
  complete native WebView bridge security.
- Restriction or auditing of ordinary outbound network traffic.
- Safety of mixed-version Proxy fleets during security-relevant migrations.

## Where controls live

| Control | Source |
| --- | --- |
| Authentication, sessions, login limits, CSRF | `crates/treer-proxy/src/auth.rs`, `crates/treer-proxy/src/auth/handlers/{session,csrf,oauth,admin,middleware}.rs` |
| Accounts, organizations, invitations | `crates/treer-proxy/src/auth/{accounts,organizations}.rs` |
| Workload and service tokens | `crates/treer-proxy/src/identity.rs`, `crates/treer-proxy/src/api/app_identity.rs` |
| Policy and Providers | `crates/treer-proxy/src/policy.rs`, `policy_store.rs`, `policy_provider_store.rs`, `policy_tree.rs`, `crates/treer-proxy/src/api/policy_*.rs` |
| Machine enrollment and connection | `crates/treer-proxy/src/auth/machines.rs`, `crates/treer-proxy/src/agent_socket.rs`, `crates/treer-proxy/src/cluster.rs` |
| Machine exec, files, uploads | `crates/treer-proxy/src/api/agents/machine*.rs`, `crates/treer-proxy/src/upload_gate.rs`, `crates/treer-agent-server/src/{machine_file_transfer,machine_upload}.rs`, `crates/treer-platform/src/machine_file.rs` |
| Ingress, tunnels, embed sessions | `crates/treer-proxy/src/ingress_guard.rs`, `crates/treer-proxy/src/api/network/*.rs` |
| Console headers (CSP, COOP) | `crates/treer-proxy/src/api/app_dist.rs`, `web/worker/index.ts`, `web/default.conf.template` |
| Workspace audit | `crates/treer-proxy/src/audit.rs`, `crates/treer-proxy/src/api/workspace_audit.rs` |
| Platform admin | `crates/treer-proxy/src/admin.rs`, `crates/treer-proxy/src/updater.rs` |
| Local credentials, file modes, pipes | `crates/treer-platform/src/{lib,windows}.rs`, `crates/treer-agent-server/src/service.rs`, `crates/treer-agent-host/src/main.rs` |
| Local Controller API | `crates/treer-agent-server/src/{local_api,local_api_handlers}.rs` |
| Process environment filtering | `crates/treer-platform/src/environment.rs` |
| Per-Agent private state | `crates/treer-agent-server/src/private_state.rs` |
| Release signature verification | `crates/treer-agent-server/src/{release_update,controller_update}.rs` |

## Reporting issues

Report suspected vulnerabilities privately through a GitHub security advisory
on the repository, not a public issue. Include the version, the boundary
crossed (from the list above) and reproduction steps. Attacks that require a
same-user local process are out of scope under the trust model.
