# Security Model

Treer's current supported tier is trusted or mostly trusted machines and
workspace members. It is not a safe multi-tenant execution sandbox.

On Windows, local Named Pipes reject remote clients and use explicit current-user
and SYSTEM DACLs. Credential-bearing configuration and recovery files use the
same user boundary, with atomic file replacement. SID-scoped startup mutexes and
PID creation-time checks avoid duplicate Host starts and terminating a reused
PID. Job Objects provide process-tree cleanup, not resource isolation or an
untrusted-code sandbox; processes under the same user remain mutually trusted.
The Windows installer checks hashes supplied by the same artifact origin and
matching binary build identities; this is not independent signature validation.
Use a trusted HTTPS Proxy/artifact origin for public installation.

## Supported Claims

- Controller hot updates authenticate the signed release channel and manifest
  against a preconfigured local Ed25519 public key, then verify both binary
  lengths and SHA256 hashes before executing either binary, including `--help`.
  Missing signatures, keys, invalid metadata, or corrupt binaries fail closed.
  The trust key is never obtained from the download server. This does not cover
  bootstrap installers, Windows in-place updates, or replay/downgrade protection.

- Machines enroll once, then authenticate with a workspace-bound credential.
- Managed Agents have separate workload credentials, verified by both the
  Controller and Proxy and bound to one Agent, machine, and workspace.
- Controller snapshots and incremental Agent events are checked against durable
  Agent credential bindings before changing routing state. Nested inventory must
  match the authenticated machine's workspace and server ID. Unknown legacy
  Agents without durable credentials remain supported; they are not proof of a
  durable machine binding. Command responses must match their pending target
  machine and arrive through a current machine connection.
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
- Organization and workspace management plus lifecycle mutations record
  body-free audit events. Several lifecycle routes log and continue when audit
  persistence fails; audit completeness and tamper-proof storage are not promised.
- Machine exec and file upload require current workspace access and distinct
  `machine.exec` / `machine.file.write` Policy actions. Their paths are confined
  beneath the enrolled Host root, and audit records omit command arguments,
  output, and file content.
- The original Agent startup register/read/clear interface remains self-only.
  The Proxy applies distinct `agent.startup.read` and `agent.startup.manage`
  actions, and a restored process is launched only while its durable Agent
  credential remains active and bound to the same machine.
- The separate admin startup read/set routes evaluate `agent.startup.manage`
  on the target Agent, including cross-Agent callers. Browser users require
  workspace access and are evaluated as humans; local operators use their
  authenticated machine identity. Replace-and-restart additionally requires
  the existing `agent.stop` on the Agent and `agent.create` on its machine.
  App runtime edits are rejected in favor of the App deployment configuration.
  Startup management bodies and responses contain no credential field; known
  Treer workload/operator credential literals in new specifications are rejected. Audit events
  record actor/target IDs, never the command, arguments, or credentials.

## Machine connection ownership

Machine WebSocket registration is authenticated against the enrolled machine's
workspace and server ID before it may claim distributed ownership. A current
connection can recover an expired lease only through atomic create-if-absent;
renewal cannot overwrite a different connection's owner record. Lease revision
checks also prevent cleanup of an old socket from releasing its replacement's
ownership. A missing lease alone is not evidence of another Controller.

## Process environment

Host/Controller do not inherit a caller's Treer workload identity or Agent UI
session. A shared platform environment filter removes Treer identity and
credential variables, `TREER_BIN`, `AGENT_UI_*`, `CODEX_UI_*`, and proxies whose
host is loopback (including localhost, IPv4 loopback and IPv6 ::1). Service
launchers filter before spawning; daemon entry points filter before creating
threads, including direct service-manager launches. On Unix a contaminated
daemon re-execs with the same PID so its initial OS-visible environment is
clean too. Controller hot replacement applies the same filter.

The PTY runtime filters the Host environment before applying each workload's
explicit environment, so restored Agents and Apps cannot inherit another
Agent's UI identity through the Host. Fresh workload credentials and per-Agent
proxy entries are injected afterwards. This is inheritance hygiene, not
provider-secret isolation: other user environment variables are retained and
same-account access remains possible. The Controller's control-plane HTTP
clients ignore environment proxies; external proxy variables are preserved for
user tools, but are not a control-plane transport configuration.

Machine `exec` uses a separate minimal OS environment allowlist. It carries no
`TREER_*`, `AGENT_UI_*` or `CODEX_UI_*` identity/configuration, environment proxy,
provider credential or loader-hook variables; it does not silently delegate the
calling Agent's workload identity. Windows keeps the native system/profile
baseline (including `SystemRoot`, `windir`, `USERPROFILE`, AppData, ProgramFiles,
ComSpec and PATH) with case-insensitive name matching. Agent/App launches instead
inherit the filtered Host baseline and receive their own fresh workload identity;
they never receive machine/enrollment/operator credential placeholders from the
Controller. Previously those names were inserted with empty values: names alone
were not evidence that the actual service secrets leaked.

This reduces ambient environment exposure, not machine-account authority. Exec
still runs as the machine's OS user and can read that user's accessible files,
including local CLI configuration; omitting a credential from its environment
does not isolate it from that account. Restrict `machine.exec` using Policy and
separate OS accounts/containers where that authority is not appropriate.

- The administrator password is compared as fixed-length SHA-256 digests with a
  constant-time comparison, and sign-in for an unknown email verifies a decoy
  password hash. This reduces timing differences that reveal password length or
  account existence; it does not guarantee equal response time for arbitrary
  input (hash parameters, database latency and CPU queuing still vary).

- Password sign-in for humans and the administrator is limited to 10 attempts per
  account (or the single administrator) per 15 minutes across all Proxies. Attempts
  are counted atomically in PostgreSQL before password hashing, using the database
  clock and PostgreSQL `lower()` (the same normalization as the account lookup, so
  Unicode case variants share one budget); success clears the count, and expired
  rows are removed in bounded batches. Once the budget is spent a correct password
  is also refused, so guesses cannot be confirmed past the limit.
  **Known limitation:** the budget is per account, not per client. Without a
  trusted client-address model (the peer address behind a reverse proxy or load
  balancer is only the edge node), any anonymous client can make new sign-ins for
  a known account or the administrator fail for the rest of the window, and can
  create short-lived counter rows. Mitigations: existing user and administrator
  sessions are unaffected; operators recover with `treer-proxy
  --reset-login-attempts` (same `DATABASE_URL`; clears all counters and exits;
  not an HTTP route); restrict administrator login to trusted networks (a risk-reduction precondition, not a
  guarantee that the software prevents lockout; under a sustained flood first limit
  the failing traffic at a trusted ingress, then reset, then sign in). A trusted
  proxy-header model and per-source limits are follow-up work.
  Counting fails closed: a database error during reservation or clearing rejects
  the sign-in even with the right password (recovery of the database restores
  sign-in; no automated error-injection test exists). The limit applies once every
  Proxy replica runs this version (older replicas neither count nor enforce it),
  and the `login_attempts` table must be kept on rollback (older means the released
  Proxy without that table; unreleased intermediate table layouts are not migrated). It does not cover
  machine or other token endpoints, global CPU budgets, or OAuth/SSO. The
  administrator comparison and decoy hash reduce timing differences (see above)
  without guaranteeing equal response time for arbitrary input.

- The Controller refuses to open a service stream or probe to a link-local address
  (169.254.0.0/16, fe80::/10, including cloud instance-metadata endpoints) unless
  `TREER_ALLOW_LINK_LOCAL_SERVICES` is set on that machine, and connects to the
  address it checked. Human-registered services may still target any other host
  the machine can reach (private networks included); publishing such a service,
  especially with public access, exposes that host, so treat service creation as
  a network-reach permission and restrict it with Policy.

- The Proxy withholds the copyable Unix and Windows install commands when its
  configured public URL is cleartext HTTP on a non-loopback host (the commands
  download and run a script whose checksums come from the same origin, so a
  network attacker could replace both). Configure an HTTPS public URL. The
  `/install.sh` and `/install.ps1` endpoints and the connect command (which
  carries the enrollment key) are separate capabilities and are not blocked;
  `install.ps1` itself refuses cleartext non-loopback artifact origins.

## Unsupported Claims

Public Unix and Windows bootstrap installers check SHA256 hashes from the same
artifact origin before installation; Windows also checks matching build identities.
These same-origin checks do not independently authenticate the publisher or the
downloaded bootstrap script. Bootstrap signature verification and rollback/freshness
protection are not supported.
Compose image updates do not verify release signatures or enforce an anti-rollback
floor. Signed Controller updates do not yet enforce release freshness or a
persistent highest accepted version. Treer makes no end-to-end supply-chain proof,
SBOM completeness, or reproducible-build guarantee. Actual hosted platform-token
permissions and rotation were not verified by this source review.

Workspace Apps do not uniformly enforce per-principal data ACLs, resource/rate
isolation, or encryption at rest. Secret scanning is heuristic and cannot promise
to detect encoded or unknown secrets. Deploy critical Apps under separate accounts
or externally managed containers with minimal mounts and resource limits; the
shared machine account is not a sandbox.

Treer does not currently provide deny-by-default workspace permissions. Absent
an enforcing Policy, the default permits authenticated workspace actions; monitor
mode does not enforce denials. Use explicit enforce-mode rules and treat workspace
members and enrolled machines as trusted administrators until these are reviewed.

Folders and path Deny rules are not a sandbox or a containment boundary. An
authenticated caller allowed `agent.create` can create other workloads, and a
caller allowed `agent.tree.manage` can reorganize identities into paths with
different permissions. Grant both actions cautiously: a Deny on `/untrusted/**`
alone does not prevent self-moves, explicit creation elsewhere, machine exec,
same-user files or direct networking. Monitor mode does not block any Deny.

New Agent/App creation inherits the authenticated workload caller's current
direct folder unless an authorized explicit destination is requested. Identity,
name and placement are reserved under the workspace transaction lock, not from
environment variables or client-reported paths. Humans/operators default to
root; an unplaced workload caller must request an authorized destination or be
placed by the owner. This removes accidental root placement, not the need for
Policy. Existing nodes are unchanged: legacy unplaced identities do not match
path/relationship selectors, even if the UI displays them at root. Inventory
them and use kind/ID rules or explicitly place them after reviewing the effect
on current rules. Tree move/rename source/destination authorization and secure
deny defaults remain separate migration work; they are not promised here.

Bearer service tokens can be replayed until expiry. Agent tokens and per-request
human forwarding tokens last 60 seconds; human App OAuth tokens last 12 hours.
Membership and service checks are not session revocation: logging out or resetting
a password does not invalidate an already issued App JWT. Workload signature
verification does not uniformly recheck Agent/machine revocation or current Policy.
Do not claim immediate token revocation or single-use bearer tokens.

Policy schema v2 Tree facts are fetched from the current workspace in the
primary database, independently of Provider cache age. A concurrent Tree change
during evaluation rejects the request for retry; a move after acceptance does
not revoke an in-flight operation. Human/machine subjects without Tree placement
do not implicitly belong to root. Links do not grant rights. Defaults remain
monitor/allow; schema v2 does not make an unconfigured workspace deny-by-default.

Owner Tree read/manage and Provider configuration recovery cannot be denied by
delegated Policy; current membership, workspace scoping and structural invariants
still apply. Monitor logs require current owner membership. The original
configured Provider's App diagnostics bridge additionally checks its current
runtime credential, the human token's exact service audience and current service
binding. Its explain/validate/log endpoints retain that owner requirement.
Malicious Provider code can misuse a forwarded Owner token during its lifetime,
so selecting a Provider remains a trust decision. No token or caller attributes
are stored in decision diagnostics. These logs are per-Proxy, bounded and
volatile, and are not tamper-proof or complete durable audit records.

Publication authority defaults to `owner_session`. Only a current owner can
explicitly choose `provider_operator` on the selected Provider binding. This
requires a Core owner session: service-audience human tokens cannot change the
choice, and the App's configuration endpoint is read-only. This prevents a
Provider from persisting authority using an owner's short-lived UI token. It
trusts that App and its management-token holders to edit arbitrary rules,
including their own access. The App checks its private management token; Core
does not receive that token and cannot independently prove that the App checked
it. The separate operator validate/explain endpoints require fresh current
Provider runtime credentials and the database switch on every request; they do
not expose decision logs. Operator explain permits continuous workspace Tree/permission
simulation and enumeration; no dedicated diagnostic rate budget is enforced.
Provider selection and owner recovery remain owner-only. The switch and its
audit are atomic, and changing the bound App or
service resets authority to the default, including old Proxy SQL writes.
Dedicated capability-gated APIs prevent old Proxies from silently ignoring the
choice. This is not bounded delegation or protection against a compromised
Provider. Previously validated in-flight App writes are not instantly cancelled
when an owner switches back. A 0600 management-token file does not isolate
Agents sharing one OS user; use separate OS users/containers and enforced
network/Policy restrictions for that boundary. See the
[publication contract](research/2026-10-13-policy-publish-authority.md).

Schema v2 deployment is not safe with a mixed fleet of old/new Proxy binaries:
old Proxies can convert unsupported bundle errors into configured stale/fail-open
decisions. Upgrade and drain all Proxy replicas before publishing v2; one
replica's capability reply does not certify cluster compatibility. Reverting
Proxy software requires a compatible v1 Provider bundle first. Disable operator
publication and verify App compatibility before rolling back to binaries without
`policy.publish-authority.v1`; 404/missing capability must not trigger legacy
fallback or an unvalidated App write.

Service-audience directory and recipient resolution apply Core Agent discovery/
metadata and human-list rules too; a denied Agent cannot be discovered through
an App directory instead. The currently verified human's own identity remains
available without enumerating others, allowing Provider authentication even
when delegated human-list access is denied. Monitor still exposes authenticated
workspace metadata; use enforce with explicit defaults to hide resources.

PostgreSQL and NATS are trusted control-plane backends, not tenant isolation
boundaries. Opaque login/admin/ingress sessions and signing-key metadata require
confidential database access and backups. The Compose NATS service has no broker
authentication; its published ports are loopback, but its private Docker network
must also be trusted. Use network isolation, least-privilege backend credentials
and transport protection for external backends. Treer does not guarantee global
login throttling, workload quotas, bounded WebSocket backlogs, complete audit
delivery, or protection against backend administrators modifying audit records.

Proxy help hides configured administrator-password and NATS-URL environment
values, alongside existing secret arguments. This is diagnostic hygiene, not a
general guarantee of log redaction: arbitrary command/environment values and
debug HTTP URLs may still contain user-provided secrets. Avoid dumping process
environments, configuration files, request queries, or startup snapshots in support
logs; protect log and backup access with the same account boundary as live secrets.

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

Browser sandbox flags do not isolate trusted App or Agent Interface JavaScript
from the user's Proxy authority. Embedded pages share the Proxy origin and can
make authenticated requests to that origin; if the control plane is also hosted
there, `allow-scripts` plus `allow-same-origin` permits access to the parent page.
Native WebView bridges currently lack a complete frame-origin and navigation
boundary. HttpOnly cookies prevent direct script reads, not authenticated API
calls or bridge abuse. Do not open untrusted interfaces with a privileged user
session. See the [2026-10-12 Web security review](research/2026-10-12-security-web.md)
for local reproductions, remaining work, and verification limits. CSP and
anti-framing headers are defense in depth, not evidence of a complete XSS,
CSRF, or mobile WebView security audit.

Apps do not create a security boundary. Managed Apps currently run through the
same Host process runtime as command Agents, under the installing machine
account. Externally managed Apps inherit their supervisor's authority.
Both managed kinds share a stable workload identity and lifecycle. Stop keeps
that identity and its credential while disabling recovery; deletion revokes it.
Start requires `agent.create` on the owning machine, stop requires `agent.stop`
on the workload, and restart requires both. Legacy App routes use these same
checks. Reconnect adopts active legacy identities without rotating live secrets;
only a missing local credential permits rotation of an active identity. An old
App identity already revoked by the previous stop implementation migrates to
the App's canonical identity rather than reviving the revoked credential.
Treer Policy limits what authenticated requests Core accepts; it cannot stop an
App from using other same-machine credentials or services it can reach. Run
untrusted code under a separate user, container, VM, microVM, or stronger
sandbox.

Agent Interface Servers likewise do not create a security boundary. They run
with the Agent's operating-system authority and may expose transcript and prompt
operations to callers already authorized by Proxy Policy. Interface ports use
machine loopback, which is shared without external isolation; the Controller is the external routing and policy
boundary.

For registered Agent Interface UI tunnels, Core strips client `x-treer-*` headers
and injects `X-Treer-Interface-Source: proxy-ui` with the resolved Agent and current
interface instance. Controller semantic AIS requests use `controller-api` with
the same target binding. Strict launchers check these markers, loopback Host and
browser Origin/Fetch Metadata to reject direct cross-origin HTTP/WebSocket and
DNS-rebinding requests. The values are public, not secrets: local non-browser
clients can forge them and remain within the trusted machine-account model.
UI provenance is not sender attestation or per-command Policy authorization;
only Controller semantic calls may attest queue senders. Old Core lacks provenance
and must be updated before strict launchers are enabled. See the
[forwarding contract](architecture.md#browser-defense-forwarding-provenance).
The launcher checks the actual listener port on every HTTP request and WebSocket
upgrade. Do not claim mutually untrusted local-process isolation. Export's
no-Origin heuristic is not independent archive authentication. See the
[local launcher audit](research/2026-10-12-security-launcher.md).

Native Codex/Claude and ACP launch modes default to unattended full authority
and inherit the Agent's own Treer credential for tools. Session export excludes
known credential/configuration files, but it does not redact secrets embedded
in allowed conversation or memory files. Transcript, provider stderr, tool
output and completion notifications likewise have no general content-redaction
guarantee. Treat exported archives and histories as sensitive data. Message
source attribution and non-system-instruction warnings do not guarantee that
the model will resist prompt injection or decline malicious instructions.

Repository launchers are optional code executed with the Agent's operating-
system authority. Treer's binary installer, updater, and release artifacts do
not fetch or install them. The optional `launchers/agent` launcher is obtained
separately; its explicitly invoked installer owns harness dependencies and UI
setup. Ordinary Agent startup does not fetch or update software. Its browser
UI is served through the private Agent Interface tunnel. Selecting a launcher
trusts that launcher and its selected harness with the Agent's authority; a
private UI tunnel does not create an operating-system sandbox.

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
| Agent workload credential | One stable managed workload across process generations; same-account inspection remains possible |
| Local operator credential | One Controller install; protects the local API but is not a same-account sandbox |
| Workload identity token | One Agent/machine/service audience for 60 seconds |
| Human App token | One user/workspace/service audience; 12 hours via OAuth, 60 seconds via authenticated browser forwarding; verification rechecks membership and service |
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

Workspace folders and placements are presentation metadata, available to
current workspace members and authenticated workspace-bound Agents/operators.
Agent/operator tree reads require `agent.tree.read`; mutations require
`agent.tree.manage`, both on the workspace's `agent.tree` resource. These
permissions grant shared workspace organization, including peer placement,
without process-control authority. Browser membership authorization is
unchanged. Folder moves cannot change a workload's machine,
credential, filesystem path, or Policy scope. They do not create an isolation
boundary. The Proxy validates workspace ownership and rejects folder cycles.

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

The authenticated Managed App UI tunnel requires current workspace access and
resolves the App within that workspace before forwarding. It replaces only CSP
`frame-ancestors` with the configured control-plane origin and `'self'`, and
removes conflicting `X-Frame-Options`, so the control plane can embed the human
interface. Other CSP directives remain enforced; direct App origins and ordinary
virtual-host tunnels retain their embedding policies. Embedded App content is
trusted workspace code, not an isolation boundary.

For Workspace ingress authenticated with an ingress session, and for the
workspace-authenticated Managed App UI tunnel, the Proxy issues a fresh
60-second human App token on each request. It uses the App's registered
`service_id` as audience and the current `workspace_member_role`, and stops
issuing when membership is revoked. The App receives it in
`X-Treer-Identity-Token`, so **App code can act as the current user through that
service's App APIs until this token expires**. This is intentional delegation
to trusted workspace code, not merely a login hint. Exposure of this forwarded token grants that short-lived, service-scoped
authority; the token itself does not reveal the platform login session or grant
another App's audience. This token scope does not isolate browser-embedded App
JavaScript from the Proxy-origin session authority described above. Apps must verify tokens and current
membership on every request, avoid logging/persisting forwarded tokens, and
apply their own data permissions. The 12-hour OAuth token is never used for
per-request forwarding. Public ingress issues no human identity, even if the
browser presents Treer cookies. Existing Agent service-token forwarding is
unchanged. The authenticated UI tunnel remains a workspace route even for an
App whose separate public ingress is anonymous.

Client `x-treer-*` and `Treer-Authorization` headers are removed before trusted
identity injection, including WebSocket upgrade requests. The embedded tunnel
strips all cookies and gateway authorization; ingress strips its session cookie
and Treer login/admin cookies while retaining App-owned cookies and application
Authorization. Authentication-disabled local mode does not mint human tokens.

Automatic human identity does **not** replace CSRF defenses. Ingress session
cookies are `SameSite=Lax`; cross-site navigations can therefore carry the
session. Apps must check `Origin` and `Sec-Fetch-Site` on state-changing browser
requests, including requests authenticated with an injected human token. Reject
untrusted/null origins and cross-site Fetch Metadata, use non-GET mutations,
require JSON/custom request headers where appropriate, and do not allow broad
credentialed CORS. Origin checks must account for the App ingress origin and
the configured Proxy origin of the embedded tunnel, rather than the rewritten
upstream Host. Non-browser service-token clients may lack these browser headers;
that is not a reason to exempt human browser requests from validation.


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
Policy Provider cannot be switched to public HTTP access or published as raw
public TCP: selection, publication and every accepted public-TCP connection
check the active Provider service. PostgreSQL triggers serialize selection and
publication under one advisory lock, so concurrent requests on any Proxy cannot
both succeed. Open public-TCP streams periodically recheck the active Provider
and fail closed on a failed or timed-out check. Revocation is not instantaneous:
bytes already in flight and the interval/query-timeout window remain possible.

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
machine. New default Policy Apps initialize an administration token locally via
a flag in their startup command. The Proxy uploads only bundled source/assets;
it neither generates nor reads the token, and no token is returned by the
installer or included in startup arguments or provisioning audit records. The
App owns its private token file, rotation and recovery. This does not isolate the
token from other processes with the same OS account or grant workspace owners
automatic OS access. Existing App startup commands are not silently migrated.
Monitor mode computes denials without enforcing them; decision audit
is still absent. The Core Message feature flag is deployment sequencing, not a
security control.

## Hardening Order

1. Replace allow-all defaults with reviewed policies and auditable decisions.
2. Bind provider credentials and runtime actions to explicit owners.
3. Keep external isolation and secret-delivery integration explicit; do not
   grow an owned sandbox implementation.
4. Add credential rotation, retention/deletion workflows, quotas, and incident
   diagnostics.
5. Add signed-channel freshness and downgrade protection to installed updaters;
   signature and binary digest checks are already enforced for Controller updates.

Relevant source boundaries are `crates/treer-proxy/src/auth.rs`,
`identity.rs`, `policy.rs`, `message_store.rs`, `updater.rs`,
`deploy/updater/updater.py`, and the Controller CLI, startup-store, and explicit network modules.

### Managed App email notifications

`POST /api/apps/{service_id}/email` lets a Managed App use the Proxy's fixed
`TREER_EMAIL_FROM` sender (default `service@treer.ai`). It accepts only the
runtime identity owning that App and service, with a service-audience token;
human tokens and unrelated Agents cannot use it. Workspace Policy evaluates
`email.send` against that App's Agent subject and `machine.service` resource.
Deployment Policy must explicitly govern this action; permissive/audit modes
retain their ordinary semantics.

Recipients must have verified email in the workspace's organization or match
the operator's `TREER_EMAIL_ALLOWLIST`. This is permission to send email, not
permission to read Core Messages. Fixed hourly limits are 20 attempts per App
and 5 per recipient per workspace, shared across Proxy replicas and restarts.
Subject is at most 200 bytes and plain text at most 4096 bytes. Caller HTML,
From, and Reply-To are rejected; HTML is generated from escaped text. Credentials
stay in the Proxy. Body-free PostgreSQL audit rows retain App ID, time,
recipient SHA-256 and sending/sent/failed outcome; hashes are pseudonymous, not
anonymous. Policy denials also use the ordinary Policy audit path.

The email-endpoint App defaults to workspace access and local outbox. It admits
only administrator-bound, current workspace human recipients and rechecks thread
ownership for every read and reply. It forwards Agent/human service tokens to
Core Message so the actual caller authors the canonical message. Emails contain
only explicit summaries and links; operators and callers must keep secrets out
of summaries and subjects. Optional Agent wake requires both administrator
opt-in and `agent.prompt` Policy. Same-UID App isolation is still not promised.

Queued prompt delivery uses the existing `agent.prompt` authorization. Public
request bodies cannot supply sender identity: the Proxy resolves the verified
caller to an Agent ID and canonical tree path, and the Controller supplies it
only over the registered private AIS channel. Human/operator prompts have no
Agent sender. Queue listing requires `agent.queue.read`; reading or cancelling
one's own submission is allowed to its authenticated Agent sender. Reading
another sender's receipt requires `agent.queue.read`, and cancelling another
sender's pending item requires `agent.queue.cancel`. Ownership comes from the
saved launcher receipt, never from request fields. Machine-scoped callers remain
restricted to their machine. Running turns require abort instead of queue cancel.

Completion notifications are authored by the target launcher's identity and
remain subject to Core Message Policy and workspace membership. They carry
bounded prompt/assistant previews; callers must avoid placing secrets in tasks
intended for other Agents. Durable queue and outbox files are launcher-owned
private state, not Core session storage. Missing queue/notification capabilities
fail explicitly instead of falling back to disruptive terminal insertion.
