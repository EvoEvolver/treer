# Security Model

Treer's current supported tier is trusted or mostly trusted machines and
workspace members. It is not a safe multi-tenant execution sandbox.

## Supported Claims

- Machines enroll once, then authenticate with a workspace-bound credential.
- Managed Agents have separate workload credentials, verified by both the
  Controller and Proxy and bound to one Agent, machine, and workspace.
- Local operator requests use a private Controller credential that is not
  injected into managed Agent environments.
- Service tokens are short-lived and audience-bound. Human App token
  verification rechecks current workspace membership and service existence.
- Core Message authorizes send, read, receive, ack, and operator import
  separately. Context edges do not expand visibility.
- Message bodies stay out of ordinary logs, audit payloads, domain events, and
  outbox envelopes. They remain plaintext in Core PostgreSQL.
- Organization management and successful lifecycle mutations produce
  append-only audit events without prompts, terminal data, commands, or secrets.

## Unsupported Claims

Do not describe Treer as zero trust, mutually untrusted multi-tenancy,
end-to-end encrypted from the Proxy, per-user provider credential isolation, or
a filesystem sandbox. The current coding-agent launch modes can execute with
the machine account's authority. Same-account processes may inspect files,
process metadata, local configuration, or credentials available to each other.

Apps do not create a security boundary. They are ordinary processes with the
same operating-system and network authority supplied by their supervisor.
Treer Policy limits what authenticated requests Core accepts; it cannot stop an
App from using other same-machine credentials or services it can reach. Run
untrusted code under a separate user, container, VM, microVM, or stronger
sandbox.

## Credentials

| Credential | Scope and limit |
| --- | --- |
| Enrollment key | One workspace, ten minutes, single use |
| Machine bearer credential | One enrolled machine and workspace; long-lived until rotation/removal |
| Agent workload credential | One managed Agent process; same-account inspection remains possible |
| Local operator credential | One Controller install; protects the local API but is not a same-account sandbox |
| Workload identity token | One Agent/machine/service audience for 60 seconds |
| Human App token | One user/workspace/service audience; verification rechecks membership and service |
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
`identity.rs`, `policy.rs`, `message_store.rs`, and the Controller sandbox and
network modules.
