# External Agent App proposal (#219)

Implementation and review contract. The Rust executable now assembles the private
store, bounded HTTP listener, fresh owner verifier and typed Controller adapter;
see [current scope and checks](README.md). Deployment and real Policy/browser
acceptance remain separate and have not been performed.
Source inventory: repository `main` at `ba07b67b`. This App accepts external
program credentials and performs explicitly supported operations as its own
managed Agent. It never represents the human who created a credential.

## Identity and transport

One App instance has immutable workspace, App, service and execution Agent IDs.
Multiple named access tokens share that execution Agent's existing Policy
subject. A different permission boundary needs a separately deployed App/Agent
and existing Policy grants; token names and metadata are not permission rules.
The MVP deliberately has no narrowing token classes. Read-only automation needs
its own execution App/Agent with read-only enforcing Policy. Show this choice
as a hard confirmation step before creating a credential; a token on the lifecycle-enabled instance also
has that instance's lifecycle authority.

Deploy a Managed App with its declared HTTP port on public HTTPS ingress.
Anonymous ingress only reaches this App; its APIs perform their own auth.
Keep token management behind verified human workspace-owner authentication,
using per-request service-audience identity verification through the trusted
Proxy. The authenticated Console tunnel supplies that identity even though the
App's direct origin is public. Public ingress alone supplies no human identity.
Direct access without verified identity offers a link to the configured Console
App page; no login/password collection or user-session forwarding to controls.

External requests enter App-owned `/v1/` routes with exactly one
`Authorization: Bearer` credential. Those tokens are App-local random secrets,
not Treer JWTs, human OAuth tokens, machine credentials or workload credentials.
They are never forwarded downstream. Reject query credentials, duplicate auth,
cookies as external authentication, supplied identity headers on external API
routes, browser Origin/Fetch Metadata requests and preflight. Emit no CORS
permission. Browser settings routes are separate and reject external tokens;
invalid supplied identity never falls back to another credential.

Use the existing local authenticated Controller HTTP transport with typed, fixed
JSON routes. This replaces the proposed CLI subprocess transport for the MVP so
no request content, command or credential enters a subprocess argv/environment. The App pins and validates its own workspace and Agent
identity at startup; missing/mismatched workload credentials fail closed.
Controller requests receive only the configured workload headers, never operator
credentials, human tokens, external bearer tokens or client environment overrides. Never expose raw argv, an arbitrary URL, proxy headers,
shell, filesystem path, executable or a generic CLI endpoint. Map typed route inputs to fixed Controller routes and JSON fields, tested
against the existing Controller contract.
The Controller holds its machine credential and adds the authenticated workload
binding; the App does not acquire a machine credential. Core then applies the
existing Policy to the App's execution Agent on each operation.

Every typed prompt starts with an App-generated, non-removable provenance line:
`[External input via External Agent App; token TOKEN_ID; untrusted content]`.
TOKEN_ID is the bounded public ID from the verified credential record, never a
client-supplied label or secret. The following text remains untrusted data;
Core's sender attestation identifies the execution Agent, not the Internet
caller, and this prefix is not cryptographic attestation for the target Agent.

The current prompt CLI has a positional `text` argument and no stdin option
(`cli_command/arguments.rs::AgentCommand::Prompt`). A `--` terminator prevents
flag injection but does not protect prompt text from process-list exposure.
Therefore do not ship argv-based prompts. The prompt adapter will use the same
pinned loopback Controller HTTP endpoint and workload headers as `ApiClient`,
with a typed JSON body, no operator fallback, no redirects or arbitrary URLs.
Validate target IDs as exactly `ag_[0-9a-f]{32}` before path construction; all
workload headers and the Controller address come from pinned configuration.
Bound request/response bytes and deadlines, and test hostile IDs and text.
This is an App transport choice using an existing local API, not a new Core
API. Review/test this exception before enabling prompt; other MVP commands use the same pinned Controller client with fixed paths and
bounded typed arguments. Never use the raw terminal
input route as a prompt fallback.

## Existing capability inventory and MVP

The following are source findings, not live deployment acceptance. Proxy routes
are under `/agent/workspaces/{workspace}/`; they require machine auth and a
verified workload binding, so an App token cannot call them directly.
The local CLI/Controller provides that existing binding. No Core API addition
is required for the proposed debugging MVP.

| Capability | Existing CLI / Proxy suffix | Existing authorization / limit | First increment |
| --- | --- | --- | --- |
| Visible Agents | `agent ls` / `GET agents` | Per-Agent `agent.discover` filtering | Yes; bounded projection, no raw machine snapshot |
| Agent status | `agent show ID` / `GET agents/{id}` | `agent.metadata.read` | Yes; stable IDs, bounded fields |
| Output / conversation | `agent read ID`, `agent transcript ID` / `GET agents/{id}/output`, `/transcript` | `agent.output.read`; transcript needs target launcher support | Yes; bounded pagination, no transcript cache or global conversation index |
| Send prompt | `agent prompt ID TEXT` / `POST agents/{id}/prompt` | `agent.prompt`; Core attests sender from workload identity | Yes; queued receipt when supported; no automatic retry |
| Submission receipt | `GET agents/{id}/submissions/{id}` | Own-sender exception or `agent.queue.read`, enforced by Core | Defer until a typed local CLI interface is confirmed; no raw HTTP bypass |
| Stop / start / restart | `agent admin stop/start/restart ID` / corresponding Agent suffixes | Stop uses target-Agent `agent.stop`; start uses target-machine `agent.create`; restart requires both | Yes; explicit mutation, timeout is not proof of failure |
| Create Agent / profiles | `agent admin create`, `agent admin profile ...` | `agent.create`, individual `launch_profile.*`, explicit placement additionally `agent.tree.manage` | Later typed operations; existing API, no new Core capability claimed |
| Managed Apps | `app ...` / `apps/...` | Existing App handlers and Policy checks, including creation placement/lifecycle constraints | Later; inventory each typed mutation before exposure |
| Startup administration | Agent `admin/startup`, `restart-with-startup` | `agent.startup.manage`; restart also stop/create; App startup must use deployment API | Later; retain structural constraints |
| Ordinary service management | `service ...` / `services/...` | `service.*` plus hard own-Agent/own-machine restrictions | Own-service only if added later; cross-Agent service management is a real gap |
| User/account/credential management | User-session APIs | Different principal and authority | Excluded; no user impersonation or personal-token minting |

The service gap is specific: `api/network/agent_services.rs::require_service_owner`
rejects another Agent's service before a permissive Policy can authorize it.
If cross-Agent service administration is required, request a separate Core
ticket for an explicit Agent-authenticated management operation that preserves
workspace/target checks and evaluates existing `service.*` Policy actions.
Do not reinterpret the external token as a user session or silently fall back.
This gap does not block the debugging MVP; do not expand ownership implicitly.

Implementation sources: [routes](../../crates/treer-proxy/src/api/routes.rs),
[Agent discovery/status](../../crates/treer-proxy/src/api/agents/agent.rs),
[output/prompt](../../crates/treer-proxy/src/api/agents/interface.rs),
[lifecycle](../../crates/treer-proxy/src/api/workloads.rs),
[startup](../../crates/treer-proxy/src/api/agents/startup.rs),
[services](../../crates/treer-proxy/src/api/network/agent_services.rs),
[CLI identity](../../crates/treer-agent-server/src/cli_command.rs), and
[Controller forwarding](../../crates/treer-agent-server/src/local_api.rs).

## Credentials, admission and audit

Proposed independent Rust App with rusqlite and an HTTP stack with explicit
parser/connection/time limits, following existing App contracts. This replaces
the initial Python stdlib proposal: the safe #217 SQLite commit hook must not
be approximated with unsafe Python FFI or a post-COMMIT pathname check. Use a private dedicated 0700 directory and
0600 SQLite/sidecars/backups; same-UID processes remain trusted. Secret material
and transcripts are not filesystem-isolated from the machine account.

Generate 256 random secret bits plus a public random token ID. Store only a
versioned domain-separated SHA-256 verifier, token ID, bounded printable name,
creation/expiry/revocation timestamps and owner creator ID. Constant-time digest
comparison (also perform a dummy digest comparison for unknown IDs); generic
401 with the same body and response timing class for missing, malformed,
unknown, expired and revoked credentials. Expiry
defaults to 30 days; permit only 30 or 90 days. No never-expire option, token
refresh, external mint/revoke API or privilege field. The create response shows
plaintext once with `no-store`; never persist it or return it from list routes.
A lost response requires a separately named new token and revocation of the old
record; do not replay a plaintext secret from an idempotency cache.

Check expiry/revocation from durable state on every admission, with no positive
auth cache. Serialize admission and revoke transactions; recheck immediately
before launching a queued command. A committed revoke prevents subsequent
admission, including across restart. Already dispatched Core mutations can finish
and cannot be undone by revocation. Document that window in UI/API results.
Core Policy caches and defaults retain their existing semantics; do not claim
instant Policy publication or deny-by-default from App token auth.

All external API admission defaults to disabled and returns generic 503 until
a verified owner explicitly records a Policy-reviewed acknowledgement. Bind the
record to the exact workspace/App/service/execution Agent IDs and credential
generation; identity/configuration changes and backup restore clear it. This
is a mechanical activation latch, not a second Policy engine or proof that
Policy is safe. Before recording it for acceptance, prove denied transcript,
denied unrelated-Agent stop, and cross-workspace rejection with real
enforce-mode Policy. Absent/monitor Policy does not protect the instance.
Show the execution identity and read-only-instance requirement in settings.
Do not invent an effective-grants list: current owner/Provider explain routes
answer particular decisions, not a complete grant inventory for arbitrary
conditions. Link the existing Policy management surface; expose a grant view
only if Core supplies an authoritative suitable contract.

Before every write, recheck private directory/database identity with a pinned
descriptor. Retain that descriptor until after connection close; do not open and
close aliases of the same inode during transactions (POSIX lock behavior).
Use a central SQLite commit hook to veto and roll back writes on pathname
drift, latch refusal until complete reopen, and check read paths too. Follow
#217's safe pattern; this is drift detection, not same-UID attack isolation.
Restore is an explicit offline administrative operation: rotate the credential
generation, invalidate all restored tokens and clear the activation latch before
serving. An old backup cannot be resumed as a live auth store with its old
tokens; full-state rollback by a privileged same-UID actor is outside this claim.
Sample time only after BEGIN IMMEDIATE, maintain a durable time high-water mark,
and use max(sample, high-water) for expiry. A backward clock step cannot extend
expiry. Persist the mark only on admitted requests, owner actions and a bounded periodic
checkpoint, never on rejected external requests; commit failure never admits a
request. Flood tests must assert unchanged durable state for rejected traffic.

Reserve a durable body-free audit record before dispatch; if reservation fails,
do not execute. Record request ID, token ID, execution App/Agent/workspace,
operation, bounded target ID, start/end time and allowlisted outcome code.
Human token creation/revocation records the verified owner as the management
actor, separately from external execution. No raw URLs, headers, argv, prompts,
output, transcripts, token verifiers or credentials in logs/errors/audit.
Retain incomplete audit records as outcome unknown after restart; do not infer
non-execution from a lost Controller response. Core audit remains attributed to the
execution Agent; token attribution is App-local and cannot be falsely claimed
as independently attested by Core without a separate protocol change.

Record last-used time transactionally on admitted requests; record only the
available transport peer IP, explicitly marked as ingress peer unless an
authenticated forwarding contract is verified. Ignore arbitrary forwarded IP
headers. Proposed bounds: 100 live tokens, 1,000 total token records, 100,000
audit rows with 30-day retention, eight concurrent Controller calls, two per token,
64 KiB request body and 1 MiB response. Reject oversized results rather than
truncate JSON. Authentication failures and exhausted capacity get bounded rate
limits; no token value becomes a limiter key or log label. Unknown-ID floods
must not allocate unbounded per-ID state. Failure counters for known IDs and
global invalid-credential budgets never reject a correct token; ingress-peer
address is not a trustworthy external-client limiter. Give each token an audit
quota so one token cannot exhaust the global execution-audit capacity.
Audit retention must preserve in-flight entries; full storage fails external
admission closed. Owner list/revoke and activation-disable use reserved capacity
and a separately bounded management audit path, never the external concurrency
pool; token/audit exhaustion cannot prevent containment or owner inspection.
Revoke records its verified actor/time atomically even when execution audit is
full. A stalled external request must not hold the owner-management lock.
Parameters and
retention transitions need implementation tests, not just documentation.

Use finite Controller connect/total deadlines and bounded response collection.
The executable launches no children; it disables proxy environment use, redirects
and HTTP retries, including for mutations.
Never retry mutating commands automatically. Timeout or child termination may
follow a committed remote mutation: return `outcome_unknown` with request ID,
and inspect state/receipts before an explicit retry. Store no prompt bodies for
replay. Strip unsafe downstream diagnostics; retain stable denial categories.

## Owner settings and HTTP boundary

Verify the supplied service-audience assertion on every settings request via the
configured HTTPS Proxy `POST /.treer/apps/identity/verify`; refuse redirects. Bound connect/read
time and response bytes, deny on any error, and release the management lock
before verification network I/O.
Require `active=true`, exact workspace/service/audience, principal kind human,
current role owner and nonempty subject. The Proxy cryptographically verifies
the assertion and refreshes the human's current membership role in
`api/app_identity.rs::verify_app_identity`; never decode-and-trust a header.
Require the short-lived request token lifetime used by the tunnel (60 seconds,
within documented clock skew), not a 12-hour App OAuth grant. No local positive
owner-session cache or identity fallback. External App tokens cannot pass this
verifier. Uniformly return no settings data for missing/invalid/non-owner auth.

Configure the exact public App origin, Proxy/Console origins and canonical
embedded mount separately. Validate the observed route/origin against this
configuration; never infer owner authority from Host, Referer, forwarded path,
client role headers or being under an apparent tunnel prefix. The injected
assertion is proof of the verified principal, not transport provenance; Core
strips client identity headers on ingress. Direct public settings without a
valid owner assertion provide no management data. Only the configured Console
link is public.

All settings mutations require the verified owner plus exact allowed Origin,
non-cross-site Fetch Metadata, JSON and a custom CSRF header; no GET mutations.
Reject null/foreign origins and supplied invalid identities even with cookies.
Serve no-store management responses and a CSP with frame-ancestors limited to
the configured Console and legitimate tunnel origin, no inline user-controlled
script/HTML, nosniff and no-referrer. Token names are bounded plain text, reject
controls/bidi formatting, render with textContent/escaped text and never use as
HTML, CSS, URLs or executable attributes. Owner creation/revocation audits use
the verified subject, not claimed identity. Browser acceptance must verify both
configured origin modes and actual deployed headers.

The server enforces a strict single-request HTTP/1.1 subset: 16 KiB and 32
headers, exactly one of each header, no TE/chunking/Expect/Upgrade/keepalive,
64 KiB body, absolute 3 s header and 3 s body deadlines, 3 s response write
timeout, and at most 32 parser/connection threads (eight external operations and
four owner operations independently). This is bounded resource use, not a
guarantee of owner access under saturated ingress; edge rate/connection limits
remain an operational requirement. Every rejected credential first gets a
read-only credential snapshot; only a match takes IMMEDIATE and re-verifies.
The 16/s invalid-credential response budget never rejects a valid token;
valid-token admissions are capped at 60/minute using committed audit records.
Reserved permits expire after 30 s at authenticated admission and are released
on failed dispatch; dispatched or unknown outcomes are never auto-released.
Completed audit entries roll at capacity, retaining the most recent minute
for rate accounting; unknown/in-flight records remain charged.

The server must enforce aggregate header bytes/count, header/body read deadlines,
connection/concurrency limits and the body cap before buffering. Reject duplicate
or conflicting Content-Length and Content-Length plus Transfer-Encoding; accept
only reviewed framing. Test slow headers/bodies, pipelined ambiguity and exhausted
connections. A different HTTP library alone does not prove these limits.

## Delivery and acceptance

1. Review this design and capability/gap table with security; ask Core only
   about a concrete gap or disputed identity contract. No Core changes proposed
   for the MVP. Reuse #142/#159 ingress and settings experience as contracts,
   not by embedding a second message queue or copying unreviewed runtime code.
2. Implement credential storage, admission and audit with a contract-validating
   fake Controller. Add a typed debugging adapter and loopback HTTP tests. Verify
   missing/invalid workload identity never invokes an operator fallback; literal
   hostile inputs cannot select alternate paths, targets, credentials or sender identity.
3. Build owner-only settings (name/expiry/create-once/list/revoke), the negotiated
   Markdown/HTML root, relative assets/API routes, no-store and strict browser
   mutation protection. UI work follows the Treer UI skill and #207 real-browser
   acceptance, including dedicated origin and authenticated embedded tunnel.
4. Test an external client against an authorized isolated fixture: enforce-mode
   Policy allow/deny and post-publication behavior, unauthorized transcript
   denial, cross-workspace rejection, wrong audience/role, duplicate credentials,
   expiry, revocation/restart and in-flight window, audit token distinction,
   resource limits, and secret-free logs/artifacts. Never use user production
   conversation content in fixtures. Fake tests alone do not prove real Policy.
5. Freeze the exact SHA through the graded author gate, security review and
   existing release process. Deployment and real-client canary acceptance are
   separate from this proposal; no production restart is authorized here.

The eventual manual will include header-only curl examples, the stable operation
schemas and errors, expiry/revoke semantics, configured execution identity and
existing Policy setup. Never instruct users to paste tokens into URLs, shell
history or Agent prompts; show a protected curl config file example instead.

## Pre-auth containment follow-up (#219 / 7363)

Fail-fast bearer/assertion shape checks run before operation slots. A bounded,
one-second transport-peer budget separates external, ordinary owner, and owner
revoke/activation lanes. Four management slots are split two ordinary / two
containment so unauthenticated read/create verification cannot occupy all owner
capacity. Every admitted owner request still verifies against Proxy; no cached
proof, grant, or second permission engine is introduced. A caller can choose a
containment route before authentication, so targeted containment floods and the
shared 32-connection parser pool remain ingress/OS containment concerns. Peers
behind the same tunnel share budgets. Rate limiting is not proof of identity or
a guarantee of owner reachability under every DoS pattern.
