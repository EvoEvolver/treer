# Security Model

Treer's current supported tier is trusted or mostly trusted machines and
workspace members. It is not a safe multi-tenant execution sandbox.

## Supported Claims

- Machines enroll once, then authenticate with a workspace-bound credential.
- Managed Agents have separate workload credentials, verified by both the
  Controller and Proxy and bound to one Agent, machine, and workspace.
- An Agent can register or clear only its own Agent Interface Server. The
  Controller verifies the registered loopback manifest before advertising it and
  never forwards the workload credential to the interface. Its local recovery
  cache contains only a process-bound descriptor and is revalidated before use.
- Agent-authenticated service mutations are bound to the Agent's own machine
  and Agent ID; cross-owner requests fail before Policy can grant them.
- Local operator requests use a private Controller credential that is not
  injected into managed Agent environments.
- Service tokens are short-lived and audience-bound. Human App token
  verification rechecks current workspace membership and service existence.
- Core Message authorizes send, read, receive, ack, and operator import
  separately. Context edges do not expand visibility.
- Message bodies stay out of ordinary logs, audit payloads, domain events, and
  outbox envelopes. They remain plaintext in Core PostgreSQL.
- Organization and workspace management plus successful lifecycle mutations
  produce append-only audit events without prompts, terminal data, commands, or
  secrets.
- Machine exec and file upload require current workspace access and distinct
  `machine.exec` / `machine.file.write` Policy actions. Their paths are confined
  beneath the enrolled Host root, and audit records omit command arguments,
  output, and file content.
- An Agent may register, read, or clear only its own Host-restart startup spec.
  The Proxy applies distinct `agent.startup.read` and `agent.startup.manage`
  actions, and a restored process is launched only while its durable Agent
  credential remains active and bound to the same machine.

## Unsupported Claims

Do not describe Treer as zero trust, mutually untrusted multi-tenancy,
end-to-end encrypted from the Proxy, per-user provider credential isolation, or
a filesystem sandbox. The current coding-agent launch modes can execute with
the machine account's authority. Same-account processes may inspect files,
process metadata, local configuration, or credentials available to each other.
Machine exec and file upload are likewise not restricted administration APIs:
they run or write with the Controller account's filesystem permissions. Host
root containment prevents accidental path targeting outside the enrolled tree,
not access that the same account can obtain through commands, symlinks, or other
same-account processes.

Agent startup specs are an explicit persistence capability, not a sandbox or a
reliable operating-system boot service. The machine-local private state file
contains the startup argv and plaintext workload credential. Do not place
secrets directly in argv; use files or a secret provider already protected for
the machine account. Anyone with that account's authority can modify the same
state or execute an equivalent command.

Apps do not create a security boundary. Managed Apps currently run through the
same Host process runtime as command Agents, under the installing machine
account. Externally managed Apps inherit their supervisor's authority.
Treer Policy limits what authenticated requests Core accepts; it cannot stop an
App from using other same-machine credentials or services it can reach. Run
untrusted code under a separate user, container, VM, microVM, or stronger
sandbox.

Agent Interface Servers likewise do not create a security boundary. They run
with the Agent's operating-system authority and may expose transcript and prompt
operations to callers already authorized by Proxy Policy. Interface ports use
machine loopback, which is shared without external isolation; the Controller is the external routing and policy
boundary.

Repository launchers are optional code executed with the Agent's operating-
system authority. Treer's binary installer, updater, and release artifacts do
not fetch or install them. The ACP launcher is obtained separately and performs
builds only during an explicit `apply.sh` operation; ordinary Agent startup
does not fetch or update software. Its headless profiles expose no browser
path. Selecting the Remote Codex UI profile explicitly trusts and builds the
immutable upstream commit recorded in the launcher lock file, then serves it
only through the existing private Agent Interface tunnel.

Treer does not install provider CLIs, capture network traffic, manage OS DNS,
create namespaces, or configure a sandbox. Profiles and external launchers own
those choices. Ordinary internet traffic is outside Treer's authorization and
metering boundary.

The thin CLI carries an Agent credential separately from the Controller's
operator and machine credentials. Agent mode requires both Agent ID and its
credential; missing credentials must not fall back to an operator identity.
This enables a caller-provided isolation boundary to expose only the Agent's
identity. It does not protect credentials from another process under the same
unisolated account, or from the machine owner.

Each managed Agent/App HTTP proxy entry attributes requests to its fixed runtime
identity, independent of PID or caller-supplied headers. The proxy uses existing
Agent credentials and service Policy; it strips local Treer credentials and
untrusted identity headers, then issues audience-bound HTTP tokens for registered
service addresses only. Service names resolve inside the authenticated workspace.
HTTP pins the stream to the canonical token audience; CONNECT resolves its
selector before checking Policy against the stable ID. Renaming changes a
convenience address, not the identity or permission key. HTTP CONNECT carries opaque TLS/native protocol bytes,
without injected target credentials. Ordinary external traffic is forwarded
without Treer identity and is outside Core authorization/metering. A local
process able to reach another workload's entry can use that identity. Entry ports
are not secrets, and Treer does not prevent bypassing the proxy. Use external
network/account isolation when reachability itself must separate identities.

Service TCP connections are reauthorized periodically and closed when their
identity, service, membership, mapping, or Policy no longer permits them.
Polling and Policy caching mean revocation is not instantaneous. New anonymous
public TCP ports require an explicit public action; the application owns its
protocol authentication. There is no claim that arbitrary outbound networking
is restricted or audited.

## Credentials

| Credential | Scope and limit |
| --- | --- |
| Enrollment key | One workspace, ten minutes, single use |
| Machine bearer credential | One enrolled machine and workspace; long-lived until rotation/removal |
| Agent workload credential | One managed Agent process; same-account inspection remains possible |
| Local operator credential | One Controller install; protects the local API but is not a same-account sandbox |
| Workload identity token | One Agent/machine/service audience for 60 seconds |
| Human App token | One user/workspace/service audience; verification rechecks membership and service |
| Human user session | Cookie `treer_session` for browsers; native iOS/Android also receive the same token in JSON when `X-Treer-Client` is exactly `mobile`, `mobile_ios`, or `mobile_android`. `Authorization: Bearer` is accepted on every user route. The header is not CORS-allowed. Do not put the token in query strings |
| Platform admin session | Cookie scoped to `/api/admin`; separate from user accounts; can list emails and issue password-reset links |
| Updater token | Shared Bearer secret between Proxy and the Compose updater sidecar; never exposed to browsers |
| Mail cookie | Local opaque handle to an App token; compromise of Mail state grants that token until expiry |
| Voice ASR vendor key | Proxy process only (`TREER_VOICE_ASR_API_KEY` / `DASHSCOPE_API_KEY`); never the iOS/Android app |
| Voice LLM vendor key | Proxy process only (`TREER_VOICE_LLM_API_KEY`); used for utterance-to-command; never the phone |
| Telegram bot token | One Telegram bot; Telegram and any process that can inspect it can act as the bot |
| Release signing key | All official release manifests; must remain offline and outside runtime systems |

Telegram numeric user/chat/topic allowlists are channel admission, not Treer
authentication. Inbound Telegram Messages are authored by the authenticated
bridge Agent and retain sender-asserted external metadata.

## Data Exposure

Proxy and database operators can read Message bodies, recipients, context
edges, and acknowledgement state. Deployments must manage PostgreSQL backups,
retention, export, and deletion because Core does not yet expose those operator
workflows.

Mail can read any body it renders or sends and stores pending PKCE state plus a
cookie-to-token mapping in SQLite. Telegram can read bridged bodies and stores
Bot API offsets, delivery hashes, errors, and external/Core ID mappings. App
state and WAL files require normal credential-store protection and backup.

Public service ingress deliberately accepts anonymous internet traffic.
Workspace ingress requires a current member session or service-audience token.
For restricted workspaces, current membership means an effective direct or
group workspace grant, workspace creation ownership, or organization manager
access. Authorization is recalculated from PostgreSQL, and removing a user from
the organization invalidates all workspace access.
The Proxy strips gateway credentials and Treer headers before forwarding, but
it remains in the browser-to-service data path. The Proxy records directional
payload byte and frame totals for these tunnels under the synthetic `browser`
client endpoint. Each aggregate carries a traffic class and meter version so
future billing rules remain explainable; workspace members can read those
aggregates. It does not store payload content, request paths, headers, or NATS
control-plane traffic in the usage ledger.

Self-hosted Compose gives the updater sidecar the host Docker socket and a
read-only bind of `compose.yaml`. Compromise of that sidecar is host Docker
compromise. Proxy, App, PostgreSQL, and NATS do not mount the socket. Hosted
Railway does not run the sidecar. `/api/admin/update*` require a current
platform admin session; workspace members cannot start an image apply.

Registered endpoints are ordinary sockets reachable from the Controller.
Same-machine processes may reach them outside the Proxy. Explicit forwarding
provides workspace authentication at the Treer entry point, not host firewall
isolation. User-provided containers or separate accounts supply isolation when
required. Agent-owned service mutations cannot select another Agent's identity.
Managed Apps and standalone HTTP services receive platform-assigned domains;
custom hosts are rejected. Workspace access is the default, while explicit
public HTTP/TCP exposure accepts anonymous internet connections. Commands,
arguments, target addresses, and launch snapshots are plaintext configuration;
keep secrets out of argv and profiles.

Workspace members may change an existing Managed App's owned ingress between
`workspace` and `public` access from the App settings UI. This endpoint resolves
the ingress from the App record and does not accept a service, hostname, or
arbitrary ingress identifier from the caller. An App selected as the active
Policy Provider cannot be switched to public access.

## Policy And Rollout

Policy is authoritative only after authentication establishes an immutable
subject and resource scope. A workspace owner may delegate rules to a selected
Managed App, but the Proxy retains authentication, scoping, runtime binding,
Provider selection, and the typed cache-invalidation recovery path. Provider
fetches are bounded loopback requests through the Controller, not arbitrary
Proxy egress. A valid stale bundle is used only for the configured window;
`fail_closed` is the default and `fail_open` is an explicit owner choice. A
missing Provider and missing legacy workspace Policy currently defaults to
the visible `Treer Default` monitor/allow baseline. The owner-only default App
installer is a recovery path: it uses bounded machine uploads, private Managed
App creation, protocol validation, and automatic Provider binding without
consulting delegated Policy. It never downloads executable content on the
machine. Monitor mode computes denials without enforcing them; decision audit
is still absent. The Core Message feature flag is deployment sequencing, not a
security control.

## Hardening Order

1. Replace allow-all defaults with reviewed policies and auditable decisions.
2. Bind provider credentials and runtime actions to explicit owners.
3. Keep external isolation and secret-delivery integration explicit; do not
   grow an owned sandbox implementation.
4. Add credential rotation, retention/deletion workflows, quotas, and incident
   diagnostics.
5. Enforce signed release manifests and downgrade protection in installed
   updaters.

Relevant source boundaries are `crates/treer-proxy/src/auth.rs`,
`identity.rs`, `policy.rs`, `message_store.rs`, `updater.rs`,
`deploy/updater/updater.py`, and the Controller CLI, startup-store, and explicit network modules.
