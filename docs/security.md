# Security Model

Treer's current supported tier is trusted or mostly trusted machines and
workspace members. It is not a safe multi-tenant execution sandbox.

## Supported Claims

- Machines enroll once, then authenticate with a workspace-bound credential.
- Managed Agents have separate workload credentials, verified by both the
  Controller and Proxy and bound to one Agent, machine, and workspace.
- An Agent can register or clear only its own Agent Interface Server. The
  Controller verifies the private-loopback manifest before advertising it and
  never forwards the workload credential to the interface. Its local recovery
  cache contains only a process-bound descriptor and is revalidated before use.
- Agent-authenticated routes cannot create, update, or delete machine services,
  virtual hosts, or service ingresses. That denial is enforced before workspace
  Policy, so an old CLI cannot restore the capability. Agents may create Managed
  Apps; Core atomically owns those Apps' service and virtual-host records.
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

## Unsupported Claims

Do not describe Treer as zero trust, mutually untrusted multi-tenancy,
end-to-end encrypted from the Proxy, per-user provider credential isolation, or
a filesystem sandbox. The current coding-agent launch modes can execute with
the machine account's authority. Same-account processes may inspect files,
process metadata, local configuration, or credentials available to each other.

Apps do not create a security boundary. Managed Apps currently run through the
same Host and sandbox backend as command Agents, under the installing machine
account. Externally managed Apps inherit their supervisor's authority.
Treer Policy limits what authenticated requests Core accepts; it cannot stop an
App from using other same-machine credentials or services it can reach. Run
untrusted code under a separate user, container, VM, microVM, or stronger
sandbox.

Agent Interface Servers likewise do not create a security boundary. They run
with the Agent's operating-system authority and may expose transcript and prompt
operations to callers already authorized by Proxy Policy. Interface ports stay
on Agent-private loopback; the Controller is the external routing and policy
boundary.

`proxy-env` is not a full traffic intercept. The injected HTTP CONNECT and
SOCKS listeners classify destinations locally: workspace virtual-host names
and the reserved local-API address stay on the Treer path; ordinary internet
destinations are dialed on the machine and never wait on the Proxy. Linux
`transparent` mode still captures all Agent TCP through the TUN. Do not describe
macOS `proxy-env` as a forced proxy for GitHub or other public sites.

## Credentials

| Credential | Scope and limit |
| --- | --- |
| Enrollment key | One workspace, ten minutes, single use |
| Machine bearer credential | One enrolled machine and workspace; long-lived until rotation/removal |
| Agent workload credential | One managed Agent process; same-account inspection remains possible |
| Local operator credential | One Controller install; protects the local API but is not a same-account sandbox |
| Workload identity token | One Agent/machine/service audience for 60 seconds |
| Human App token | One user/workspace/service audience; verification rechecks membership and service |
| Platform admin session | Cookie scoped to `/api/admin`; separate from user accounts; can list emails and issue password-reset links |
| Updater token | Shared Bearer secret between Proxy and the Compose updater sidecar; never exposed to browsers |
| Mail cookie | Local opaque handle to an App token; compromise of Mail state grants that token until expiry |
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
The Proxy strips gateway credentials and Treer headers before forwarding, but
it remains in the browser-to-service data path.

Self-hosted Compose gives the updater sidecar the host Docker socket and a
read-only bind of `compose.yaml`. Compromise of that sidecar is host Docker
compromise. Proxy, App, PostgreSQL, and NATS do not mount the socket. Hosted
Railway does not run the sidecar. `/api/admin/update*` require a current
platform admin session; workspace members cannot start an image apply.

Linux `publish_ports` maps a namespace TCP port onto the machine loopback. It
is not an internet listener. Any process on that machine that can reach
`127.0.0.1` can reach the published service. Agent-scoped services use a
separate Unix bridge into the same namespace and do not open a host TCP port.
Managed Apps use `publish_ports` for their declared HTTP UI port, then route the
stable service and virtual hostname to that loopback listener. Their command,
arguments, working directory, and hostname are plaintext Proxy metadata; the
Managed App API deliberately has no secret field.

Only a logged-in workspace user or operator API may directly mutate service,
virtual-host, and ingress records. Managed Agents can list or probe existing
records for compatibility but cannot publish their own sandbox listeners.

## Policy And Rollout

Policy is authoritative only after authentication establishes an immutable
subject and resource scope. A missing workspace Policy currently defaults to
allow. Monitor mode records decisions without denying; enforce mode applies the
decision. The Core Message feature flag is deployment sequencing, not a
security control.

## Hardening Order

1. Replace allow-all defaults with reviewed policies and auditable decisions.
2. Bind provider credentials and runtime actions to explicit owners.
3. Add a real isolation backend for untrusted workloads and scoped secret
   delivery.
4. Add credential rotation, retention/deletion workflows, quotas, and incident
   diagnostics.
5. Enforce signed release manifests and downgrade protection in installed
   updaters.

Relevant source boundaries are `crates/treer-proxy/src/auth.rs`,
`identity.rs`, `policy.rs`, `message_store.rs`, `updater.rs`,
`deploy/updater/updater.py`, and the Controller sandbox and
network modules.
