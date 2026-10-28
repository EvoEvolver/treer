# External Agent App (#219)

A runnable Unix Rust App that accepts external bearer tokens as its own fixed
execution Agent and delegates each operation to the existing Controller/Core
Policy boundary. It never executes as a human owner and has no secondary grant
engine. [DESIGN.md](DESIGN.md) records the reviewed identity and trust contract.

The App provides a fixed-native-CLI gateway, typed operations, registered external
agent inboxes and device-code onboarding. Release evidence is source/artifact and
runtime specific; local TCP/browser fixtures are not deployed human-session or
Policy acceptance. [ACCEPTANCE.md](ACCEPTANCE.md) records the supported integration
handoff. Preserve the exact release receipts when distinguishing accepted runtime
features from new source increments.

## Run and configuration

Build `cargo build --locked --manifest-path apps/external-agent/Cargo.toml`.
Run `target/debug/treer-external-agent /private/path/config.json` from this App's
Cargo directory, as a managed App process. The config must be a private 0600
regular file owned by the process account, not a symlink. Configuration fields:

```json
{
  "workspace": "ws_example",
  "app": "app_example",
  "service": "svc_example",
  "agent": "ag_0123456789abcdef0123456789abcdef",
  "machine": "machine_example",
  "controller": "http://127.0.0.1:8790/",
  "proxy": "https://proxy.example/",
  "public_origin": "https://external.example/",
  "owner_origin": "https://settings.example/",
  "console_origin": "https://console.example/",
  "tunnel_origin": "https://proxy.example/",
  "console_url": "https://console.example/workspaces/ws_example/",
  "listen": "127.0.0.1:8080",
  "owner_listen": "127.0.0.1:8081",
  "state": "/private/external-agent/state",
  "policy_acceptance_sha256": null,
  "prompt_review_sha256": null
}
```

Use actual registered identities/origins; no service or App is created by this
command. New App runtime IDs may use an `app_` identity; retain the actual
managed `TREER_AGENT_ID`. The debugging target MVP accepts stable `ag_` IDs only.
The Controller must be an exact loopback HTTP IP origin, not a hostname or
forwarded URL. Production Proxy and browser origins require HTTPS; loopback
HTTP is accepted for isolated fixtures. No URL can contain credentials, queries
or fragments. The full config and a workload-credential digest bind the store.
A security-config/credential change fails startup against an old store; perform
an explicit offline rotation/new private state and reissue credentials, never
silently resume old credentials under a different identity or configuration.

Startup requires matching managed `TREER_WORKSPACE_ID`, `TREER_AGENT_ID`,
`TREER_SERVER_ID`, `TREER_AGENT_SERVER_URL` and nonempty
`TREER_WORKLOAD_CREDENTIAL`. It checks the Controller health workspace/machine,
prompt-delivery capability and authenticated own-Agent metadata before opening
the store or listener. Controller health is unauthenticated binding/capability
metadata only; the subsequent own-Agent GET performs the identity/Policy check. It never reads an operator credential/config or invokes a shell or provider.
The explicitly configured CLI gateway invokes only its pinned native `treer` binary. Missing/mismatched identity fails closed.
All downstream HTTP disables environment proxies, redirects and retries.

External execution starts disabled. Freshly verified owner activation confirms the
current generation/identity and use of this App's existing workspace Policy.
Policy refusal remains enforced on every downstream operation. Activation and
prompt no longer require separately configured acceptance/adapter hash pins.
The two legacy optional fields remain serialized and must not be edited on an
initialized store: preserving their bytes preserves existing token bindings.
No denial against an otherwise authorized same-workspace target is required.

## HTTP and owner entry points

The HTML root is a bilingual settings page; curl receives the Markdown manual.
Assets and fetches are relative, so the authenticated Console mount is retained.
Direct public access supplies no owner authority; use the configured Console
link. There is no App session cookie or OAuth fallback.

| Route | Contract |
| --- | --- |
| `GET /health` | Small public liveness response |
| `GET /v1/agents` | Core-filtered, bounded metadata projection |
| `GET /v1/agents/ID` | Agent status metadata projection |
| `GET /v1/agents/ID/output` | 100 lines by default |
| `GET /v1/agents/ID/transcript` | One turn by default |
| `POST /v1/agents/ID/prompt` | JSON `text`, queued receipt; provenance marker always prepended |
| `POST /v1/agents/ID/stop`, `/start`, `/restart` | Empty JSON object; execute once |
| `GET /owner/state` | Fresh owner proof; identity, enabled state and token metadata |
| `GET /owner/audit` | Fresh owner proof; last 100 body-free execution records |
| `POST /owner/tokens` | `name`, `days` (30/90), `authority_confirmed`; create-once plaintext |
| `POST /owner/revoke` | Token `id`; records verified human actor atomically |
| `POST /owner/activation` | `enabled`, `generation`, `agent`, `policy_reviewed` |

Token-protected `/v1` operations require exactly one Authorization Bearer header;
`/v1/device/request` and `/v1/device/token` use the device flow described below. Browser Origin,
Fetch Metadata, cookies, client Treer identity headers, duplicate headers and unknown operation fields are refused; only scoped inbox
pagination accepts its bounded known query fields; no CORS permission is emitted. Input
cannot select a workspace, executable, URL, workload headers or sender identity.
The Controller sends its real machine/workload binding to Core for every call;
no App Policy result cache exists. Metadata excludes cwd/commands. Output and
transcripts remain sensitive, authorized Core data; there is no conversation
index/cache. No raw terminal input fallback exists.

Core's narrow source review compared App `d3a58fd2` with exact Core
`c7cf3b051120fe0f3a4f19cc927dd3a3a22f1546`; `runtime/controller.rs` remains
unchanged in subsequent App commits. This is a source contract verdict, not
proof about a shorthand binary version or a deployed fixture:

| Operation | Existing Core Policy check/resource |
| --- | --- |
| List | `agent.discover` for each target; results filtered |
| Status | `agent.metadata.read` on target Agent |
| Output / transcript | `agent.output.read` on target Agent |
| Prompt | `agent.prompt` on target Agent |
| Stop | `agent.stop` on target Agent |
| Start | `agent.create` on target machine |
| Restart | Both target `agent.stop` and machine `agent.create` |

There are no `agent.start` or `agent.restart` Policy actions. Each call uses
existing PolicyEngine/provider/cache semantics, without a new promise to
refetch the provider for every request. Fixed local Controller routing binds the
workspace; a foreign Agent ID cannot select a foreign workspace. A real enforce
bundle must explicitly establish the intended allows/denials.

Queued prompts use `delivery=queue`, `notify=null`, `require_receipt=true` and
require the Controller's `prompt.queue` / `prompt.queue.read` capabilities.
The Proxy attests the actual execution Agent sender. A submission receipt
proves queue/admission, not completed execution; invalid/lost receipts return
unknown and are not retried.

Generic/public ingress and authenticated Agent Interface UI forwarding are
distinct transports. Core's UI forwarding sanitizes incoming credentials and
rewrites Host to `127.0.0.1:<registered-interface-port>`; Controller AIS uses
Host `agent-interface`. Do not require the original Console/public Host or
interpret injected UI source/Agent/interface headers as human-owner authority.
Management still requires its service-audience fresh human-owner verification.
Actual deployed owner embedding/header behavior remains fixture evidence; no
new Host allowlist is inferred from this source review.

Owner calls verify `X-Treer-Identity-Token` through the configured Proxy on every
request, outside the Store mutex: active, exact audience/service/workspace,
human, current owner and maximum 60-second assertion lifetime. Verify deadline
is three seconds, response cap 16 KiB; errors fail closed. Mutations additionally
require configured exact Origin, same-origin/same-site Fetch Metadata, JSON and
`X-External-Agent-CSRF: 1`. External tokens cannot manage tokens. All responses
are no-store with strict CSP/nosniff/no-referrer; names render as text, not HTML.
Token timestamps are RFC3339 UTC; browser display uses the local timezone.

## State, containment and limits

**First response if revocation is unreachable:** stop this App's execution Agent
through the existing Console or Controller stop operation, using an authorized
operator identity. This Policy-protected control path is independent of the
App listener and disables all external access through this App at once. Do not
use an external App token to perform containment through the saturated listener.
Already dispatched downstream operations may still complete; retain the private
audit state and reconcile unknown outcomes before any restart. Keep the App
stopped until ingress protection and the affected credentials are reviewed.

The executable binds two **distinct loopback sockets before accepting traffic**.
`listen` is the external data plane; it returns 404 for every `/owner/` route,
even with a valid owner assertion. `owner_listen` is the workspace-only settings
plane; it returns 404 for `/v1/`. Selection is by listening socket, never a header.
The two origins must differ. Owner mutations accept only the management, Console
or authenticated tunnel origin, not the anonymous data origin.
Public parser admission (24 connections) cannot consume private admission
(8 connections), including while public clients stall incomplete headers.
Management listener failure terminates the process; managed restart does not
reset durable activation/revocation state. Independent Console/Controller stop
remains required for shared CPU, FD, tunnel or process resource exhaustion.

Keep the Managed App's primary UI port on `owner_listen`, with workspace
authentication and its usual embedded Console entry. `service` is **that private
management service's audience**, not the public data service. Release maps a
separate service/ingress to `listen` only within its concrete fixture scope.
Managed App creation currently declares one HTTP port (`apps/README.md`);
it cannot atomically provision this second mapping. Existing operator machine
service/ingress APIs represent separate target ports
(`CreateMachineServiceRequest` in `crates/treer-protocol/src/network.rs`).
An authorized operator must provision/verify the second mapping; this App never
registers services or changes ingress, grants, or the primary App's access mode.
Never publish the management socket. Both listeners share the same fixed App
workload identity and state; there is no second execution identity.

Current Core already provides public ingress limits (`ServiceIngressLimits`
in `crates/treer-protocol/src/network.rs`): per-service and per-IP token buckets,
concurrency, body bounds and deadlines. `crates/treer-proxy/src/ingress_guard.rs`
uses the TCP client or an explicitly configured trusted front; limits are per
Proxy process, not distributed across replicas. Release uses these existing
controls, not a new edge framework. Retain actual effective values and trusted
front configuration in the fixture inventory without changing them here.

The ordinary tunnel strips `Forwarded` and `X-Forwarded-For`; the current public
ingress subsequently inserts `X-Treer-Client-IP` after removing caller headers
(`crates/treer-proxy/src/api/network/ingress.rs`). This App **accepts but ignores**
that one informational header, including forged values on direct loopback
calls. It never grants identity/authority or changes rate buckets. Other
`X-Treer-*` headers still fail external execution. App budgets continue to use
the actual TCP peer and token; verified per-client limiting belongs to Proxy.
Release must verify installed Core's behavior, both port mappings and independent
stop, and record the residual shared-resource risk. An offline single-token revoke tool
remains a follow-up; independent first response stops the App.

Use a dedicated 0700 state directory **outside version control**, on a private
account or container. SQLite and sidecars are 0600. Tokens have random 128-bit
public IDs and 256-bit secrets; only a domain-separated verifier is persisted.
Plaintext is returned once and never logged or audited. Named tokens share the
App's existing authority; read-only automation needs its own enforcing read-only
App/Agent. Ingress source metadata is the actual transport peer IP, explicitly
not an authenticated external-client IP; forwarded headers are ignored.

Admission reserves a body-free durable audit before dispatch. The opaque permit
binds operation, target, token and generation. Dispatch rechecks expiry,
revocation, activation and scope. No Store lock spans network I/O. Actual results
may finish after revoke/disable. Timeouts, lost/oversized responses and failed
result commits return outcome_unknown with a request ID; never retry mutations
automatically. Restart marks unfinished records unknown, without replay.
Owner listing/revoke/disable have their own bounded management audit ring and
four operation slots, separate from the eight external execution slots. Two
owner slots and an independent pre-auth budget are reserved for revoke/activation;
read/create verification cannot consume that containment capacity.

Headers are bounded to 16 KiB/32 fields, bodies 64 KiB, downstream results 1 MiB,
with absolute 3 s header/body deadlines, 3 s writes, 2 s downstream connect/10 s
total deadline. At most 24 public and 8 private parser connections exist. HTTP/1.1 is single-request,
Connection: close only; duplicate headers, transfer encoding, Expect and Upgrade
are refused. Public parser saturation cannot occupy private parser slots;
a hostile authenticated management client can still saturate its own plane.

Missing/malformed bearer credentials and absent/empty/oversized/non-printable
owner assertions fail before operation slots or network verification. The
pre-auth budget uses the actual TCP peer, never forwarded headers: 64 external
and 16 ordinary owner requests per one-second window, with an independent
16-request revoke/activation window. Each lane holds at most 128 peer entries (384 total), expiring after one second;
that lane's table exhaustion fails its admission closed. Ordinary traffic
cannot consume containment's peer-table capacity. This is
transport rate limiting, not authorization: every owner call admitted for
verification still checks current Proxy identity without an auth-result cache.
Peers sharing a tunnel address share that lane's budget. A deliberate flood of
plausible assertions on the containment routes can still deny the same peer's
legitimate containment requests if it can reach the workspace-only management
plane; retain independent Console/Controller containment. The reserved lane
protects against read/create floods, not all DoS.

Rejected credentials use a read-only snapshot, never IMMEDIATE or durable
writes; a match is re-verified inside the write transaction. A bounded invalid
response counter never locks out a correct token. Per-token admitted rate is
60/minute; concurrent limits are eight globally and two per token. Live tokens
are bounded to 100, total token records 1000. Execution audits are bounded to
100,000 globally/1000 per token; oldest completed records roll at capacity after
a minute (rate window), unknown/in-flight evidence is never automatically
pruned. The independent management ring keeps 1000 events. Abandoned reservations
are explicitly releasable and expire after 30 seconds on the next valid
admission; failed dispatch releases its reservation. Reserved counts as in-flight.
Dispatched/unknown rows are never guessed to be safe to resend.

The #217 pinned-descriptor/commit-hook protection rejects database pathname
drift and stays latched until full reopen, including rollback of a concurrent
write. This is not hostile same-UID/VFS isolation or rollback detection.
Offline backup restore requires generation rotation, revoked restored tokens
and cleared activation. A forward clock jump permanently advances the expiry
high-water mark: correct the clock and reissue credentials via explicit offline
recovery, rather than lowering the mark and resurrecting expired tokens. Unknown
outcome reconciliation and token-record retirement remain follow-ups; full
unknown evidence capacity fails execution closed, while owner inspection and
containment remain available.

Audit is a bounded operational window, not an immutable forensic archive: a
valid token can age out its own completed records with roughly 1000 further
admitted calls (subject to the rate limit). Owners needing longer retention must
export body-free records to separately controlled storage before they roll;
this App has no automatic durable audit exporter. Unknown evidence never rolls.

### Workload credential/configuration rotation recovery

Before deployment, retain the configured identity and private state location in
the operator runbook. If the execution Agent is recreated, its workload credential
is rotated, or any bound configuration changes, startup against the old state
must fail closed. Recovery is an explicit offline operation:

1. Stop only this App process and remove its public exposure. Do not try to
   lower the expiry high-water mark or edit the stored binding to bypass refusal.
2. Preserve the old complete state privately for audit/reconciliation. Never
   copy old token rows into the replacement state or restore old activation.
3. Verify the replacement managed identity/credential and exact configuration.
   Configure a new, empty private state directory; activation remains off.
4. Confirm the replacement identity and its current workspace Policy, then
   explicitly enable with fresh owner acknowledgement. Create replacement tokens through the
   authenticated owner page; distribute them privately and retire old credentials.

The old token generation is not imported. Uncertain dispatched outcomes in the
old audit still require reconciliation, never automatic replay. This procedure
is separate from restoring an unchanged binding, which requires the documented
restore-generation rotation.

### Open activation gates

Security accepted the default-off MVP at #219 event 7363. Its pre-auth
follow-up now has fail-fast shape checks, bounded transport-peer budgets and
separate containment capacity; TCP tests cover missing/invalid `/owner` floods
while an authenticated owner revokes. Security event 7725 found no blocker in
the `b1bf25d1` socket/origin/header/client delta (static review only). Public and
private parser slots are isolated; shared process/tunnel resource exhaustion and
authorized management-plane floods still need independent containment.
Historical acceptance below included narrower read-only/denial scenarios. They
are retained evidence, not current activation requirements. Current validation
uses authorized operations and platform workspace isolation. Legacy acceptance
pins are metadata and do not gate execution. Assertions can be
replayed during their valid window; no single-use nonce/jti is supplied by this
App. Host-origin validation is also a pending loopback hardening follow-up.

## Verification

From the repository root, with a private Cargo target directory:

```sh
cargo test --locked --manifest-path apps/external-agent/Cargo.toml
cargo clippy --locked --manifest-path apps/external-agent/Cargo.toml --all-targets -- -D warnings
cargo build --locked --manifest-path apps/external-agent/Cargo.toml
python3 apps/external-agent/tests/smoke.py --binary "$CARGO_TARGET_DIR/debug/treer-external-agent" --browser
```

The optional browser smoke needs Node and Playwright installed; set
`EXTERNAL_PLAYWRIGHT_MODULE` to its `index.mjs`. It uses real Chromium and the
actual executable/headers through an isolated gateway that emulates assertion
injection; it is **not** the required deployed canary/browser acceptance.
No credentials are printed, traced or recorded in screenshots. All fixture
identities, tokens, content and Policy responses are synthetic. Rust TCP tests
cover strict framing, slow headers, hostile targets/text, owner role/audience/
lifetime/CSRF, rate/revoke/expiry, owner access during downstream waits and
rejected floods, changing fake Policy, lost results, and secret-free metadata.
Storage tests cover descriptor locks, drift rollback, bounded audit rolling,
reservation containment and no writer lock for rejected credentials.

Before release: exact-SHA aggregate boundary review, graded author/release gate,
real enforce-mode Policy allow/deny/change tests, real external-client canary
acceptance and actual dedicated/embedded browser/header acceptance under #207.
No #203/#204 production activation is implied by this App or its tests.

### Owner execution audit

The existing owner settings page includes a manual **Refresh audit** action backed
by `GET owner/audit` on the same fixed owner UI prefix. It shows at most the last
100 body-free execution records, request/operation/target/outcome/timestamps and a
masked token identifier. It does not display request bodies or token secrets and
has no replay/resend/clear-unknown control. `outcome_unknown` is not completion.
The response is bounded to128KiB/10seconds and must match the loaded App identity;
empty, denied and unavailable states are explicit. Failed/denied responses clear
previous records. Language change and page exit discard the displayed audit.
This does not change owner proof, CSRF, data-plane isolation or retention.

Focused UI-only HTTP fixture (no real owner credentials/Core Policy claims):

```sh
EXTERNAL_PLAYWRIGHT_MODULE=/absolute/playwright/index.mjs \
  node apps/external-agent/tests/owner-audit-browser.mjs
```

It covers desktop/light and390px/dark, valid/empty/denied/unavailable/foreign/
over-cap/oversized responses, masked token IDs, text-only rendering, language
reset and absence of mutation requests. Actual Core/API/owner-chain acceptance
remains separately owned by the integration fixture.

The Console iframe root may carry bounded `theme`, `look`, `lang` and
`embed_origin` appearance hints. Only GET `/` accepts these known, nonduplicate
keys; API requests still reject query strings. Hints never grant owner access,
change CSRF origins, or select CSP frame ancestors (which remain configured
Console and Proxy origins). Assets and owner API paths remain App-relative under
the authenticated embedded `/ui/` prefix.

### Copy an integration skill for an external Agent

The authenticated owner page offers a secret-free Agent handoff. Fresh-owner
`GET /owner/integration` returns the exact configured data base URL, identity,
enabled/activation/prompt state, explicit readiness blockers and `skill_markdown`.
A loopback URL is labelled local-only; a configured HTTPS URL is explicitly
unverified, never evidence that a public data mapping exists. Neither this read
nor copying/downloading the guide creates a token or enables the App.

`GET /SKILL.md` is a readable/downloadable, nonsecret manual under either root
socket and the normal embedded App prefix. It has configured URLs and setup/API
instructions, not token material or an assertion of current enabled state. The
owner handoff includes an enabled-state snapshot labelled as such. All responses
retain no-store/CSP; the owner API retains fresh human-owner verification and
query refusal. Token acquisition uses the existing create-once owner flow, with
explicit private file handoff. The runnable read-only discovery example reads a
0600 token file, sends Bearer only in an HTTP header, disables redirects/proxies,
and never prints the token or automatically retries. Tokens share the App's
Policy authority; a read-only example is not a read-only grant.

For a data-only service published after initialization, keep the original Config
and state unchanged. Optional startup `treer-external-agent CONFIG
--data-advertisement PRIVATE_JSON` supplies **display metadata only**, preserving
existing token verification. The same-user private regular file must have no group/
other access, one hard link, no symlink and at most 4096 bytes. Its exact schema is:

```json
{"schema_version":1,"config_sha256":"<SHA256 of exact CONFIG file bytes>","workspace":"<Config.workspace>","app":"<Config.app>","service":"<Config.service>","agent":"<Config.agent>","machine":"<Config.machine>","listen":"<Config.listen>","data_service_id":"svc_<32 lowercase hex>","data_base_url":"https://<actual data ingress>/"}
```

All identity/socket fields must match Config; the secondary data service differs
from the owner service. The HTTPS root must differ from owner/Console/Proxy
origins. A mismatch fails startup. This operator-supplied declaration changes only
owner integration instructions and SKILL.md; it does not change socket routing,
Host/auth/CSRF/CSP, credential binding, tokens, activation or Policy acceptance.
It remains `configured_https_unverified`; a declared URL is not connectivity or
current-identity acceptance evidence. Changing security Config/pins still requires
a separately supported transition; this display feature cannot adopt such changes.

The copied short guide supports environment secrets or private files and exact
API schemas. The optional Python example in the download checks unauthenticated
health before Bearer discovery. Bounded read backoff is allowed; uncertain
mutations must not be automatically repeated.

### Explicit authorized local operator activation

When the workspace owner explicitly authorizes deployment-side activation but no
human browser session is available, the same managed App entry may run:

```
treer-external-agent /private/original-config.json --operator /private/one-shot.json
```

This command validates the ordinary inherited managed workspace/Agent/machine/
Controller identity and workload credential. It performs authenticated Controller
preflight and requires exclusive access to the stopped App's existing private
store. It never fabricates a human assertion or exposes an HTTP admin bypass.
The request must be same-user private (no group/other access), regular, single-link,
no-follow, <=4096 bytes, inside a private 0700 directory:

```json
{"config_sha256":"<SHA256 of exact unchanged Config bytes>","expected_generation":"<current store generation>","confirmed":true,"operation":{"action":"enable"}}
```

The exact Config digest and current generation are checked. A sibling `.consumed`
file is created exclusively and fsynced before mutation; preserve it on success
or failure and never replay an ambiguous command. Inspect state before issuing a
new command. Ordinary startup never consumes operator requests automatically.
Enabling records `policy_reviewed` with actor `deployment_operator`, preserves
all existing tokens/generation/audit and leaves downstream Policy unchanged.

For an authorized integration check only, use a new request with
`"operation":{"action":"create_test_token","output":"/private/NEW-token-file"}`.
It creates one 15-minute token in an exclusively created 0600 file; stdout has
only its ID and TTL, never plaintext. The parent must be private 0700. Revoke it
with another fresh request and
`"operation":{"action":"revoke_test_token","id":"<returned test_token_id>"}`.
This command refuses any token not named and created by this operator test flow;
existing user tokens are never read or revoked. All commands retain explicit
operator attribution. Start the unchanged normal runtime after the bounded
operator phase; keep test token delivery private and remove it after revocation.


### Authenticated native CLI gateway

`POST /v1/cli` accepts `{"argv":["agent","ls","-R","/"],"stdin":"","timeout_ms":30000}`
with the existing App Bearer token. It runs a pinned native `treer` executable
with direct argv, this App's managed identity, and an otherwise cleared environment.
No shell, caller-selected executable/env/cwd, operator fallback, or per-command
read-only whitelist is involved. The original typed `/v1/agents` API remains available.
Normal finite agent/message/status/App/tree operations use Controller semantics,
including current workspace authorization and Policy. Tree traversal is `agent ls -R`,
not an invented `tree` command. PTY `agent attach`, `service forward`,
`network connect`, client-local `machine put/get`, message `--body-file`/`--external-source-file`
values other than `-` (stdin), and `--follow` return
`unsupported_cli_mode` before execution. Remote `exec` remains ordinary CLI argv;
`--` preserves literal command arguments.

Response: `{request_id,result:{exit_code,stdout,stderr,timed_out,truncated,outcome_uncertain}}`.
An actual CLI nonzero exit is returned with HTTP 200 and its original exit code.
Timeout/output cap returns exit 124/125 and uncertainty: a killed CLI transport does
not undo an operation already admitted by Controller. No command is retried.
Requests are <=64 KiB JSON, <=256 argv items/32 KiB argv, timeout 1..60000ms
(default 30000), and stdout+stderr <=2 MiB. Streams are UTF-8 text with replacement
for invalid bytes. Public ingress can impose a shorter deadline; choose a timeout
below its effective request limit. Caller disconnect does not authorize replay.

Deployment adds `--cli-gateway /absolute/private/gateway.json` alongside the existing
`--data-advertisement` option. Gateway JSON has exactly `config_sha256`, `executable`
(absolute trusted native treer path), and `executable_sha256`. Use a same-user private
regular file (0600, one link, no final symlink, <=4096 bytes). The executable must be
same-user, executable, non-group/world-writable, one-link, regular, no final symlink,
<=128 MiB; its SHA is checked at startup and before each execution. Keep this fixed
trusted file and its parent directories under deployment custody. No request changes
this declaration. Child cwd is exactly `Config.state`, not the App process cwd;
Controller-side remote exec cwd retains the normal Controller contract. App Config serialization, token verifiers, generation and activation
are unchanged. SQLite upgrades execution audit v2->v3 transactionally, preserving all
rows/tokens; `cli` records contain no argv/stdin/stdout/stderr. The App rechecks token
admission/disable/revoke before dispatch and holds no Store lock during subprocess I/O.
To disable only the gateway, retain this successor binary and omit `--cli-gateway`;
an old v2 binary cannot reopen the v3 audit schema. Do not restore an old DB over new
execution history. Source-only process fixtures are distinct from actual deployed
native CLI/Bearer acceptance.

### Addressable external agents and durable inboxes

The owner page registers an external agent with an immutable local key (1–64
ASCII letters/digits/`_`/`-`, starting with a letter/digit) and a display name.
It returns a stable `AppID/key` address and an expiring key once. Registration is
bounded to 100 identities, including retained registrations. A full existing App
key can also `GET /v1/external-agents` or `POST /v1/external-agents` with
`{"key":"assistant1","name":"Assistant","days":30,"authority_confirmed":true}`.
Public responses use the existing `{request_id,result}` envelope; registration
returns `result.address`, `result.key`, `result.token` metadata and
`result.plaintext`. Store the plaintext privately. Creation is not automatically
retried; if the response is lost, list identities and issue a replacement key
through the owner page instead of recreating/reusing the identity.

With the registered agent's key:

- `GET /v1/inbox?limit=50[&cursor=URL_ENCODED_CURSOR]` returns Core
  `deliveries`, `remaining_unacknowledged` and optional `next_cursor` in `result`.
  Each delivery has `delivery_id`, recipient and message; `message.kind` is
  `prompt` or `message` (legacy missing kind is normalized to `message`).
- `POST /v1/inbox/ack` with
  `{"delivery_ids":["dlv_..."],"operation_id":"unique-stable-operation-id"}`
  acknowledges only that agent's deliveries atomically. Repeating the same
  operation ID and exact batch is idempotent; changing the batch/scope conflicts.
  ACK means handled/accepted delivery, not attested task completion.

GET never acknowledges. A dropped response, restart or repeated GET can return
an unacknowledged delivery again: deduplicate durably by delivery ID and ACK only
after recording the handling result. Reset the cursor periodically (or after
restart) to rescan unacknowledged work; a cursor is pagination, not confirmation.
Core's existing durable delivery/retention rules apply, including explicit
message expiry; there is no second App payload queue. ACK errors/uncertainty do
not trigger automatic retries by this App. An explicitly repeated identical ACK
uses the same operation ID.

Internal teammates use ordinary `treer agent prompt AppID/key "task"` and
`treer message send --to AppID/key --body "message"`. App paths resolve through
the same owner App. Endpoint prompts do not initially support wait/notify/steer.
This requires the matching Core endpoint/prompt/cursor implementation; installed
c7cf alone is insufficient. `/api/tree?path=` on the authenticated owner socket
exposes registered children using the shared App-owned tree contract, without
Host processes or separate credentials for those children.

Agent keys retain ordinary `/v1/agents` and `/v1/cli` team-operation authority.
They scope only inbox access; they are not independent Core Policy principals.
Aggregate gateway `message` reads/ACK/reply are refused with `use_scoped_inbox`;
use the scoped inbox API, and use normal `message send` to the received sender's
stable address for a reply. Existing full App keys retain their aggregate
mailbox/gateway behavior. Logical inbox separation is not a sandbox against
otherwise authorized workspace operations.

Owner routes are `GET/POST /owner/external-agents` and
`POST /owner/external-agents/tokens`. Both POST bodies use the registration shape
above; replacement uses the existing immutable key and treats `name` as a token
label, without renaming the agent or automatically revoking the old key. Owner
responses return `{agent,token,plaintext}`. Existing `/owner/revoke` works for
agent keys. Owner proof/CSRF rules are unchanged. No plaintext key is stored.

Schema v4 adds only registration/token bindings and inbox audit operation codes.
The v1–v3 upgrade preserves Config binding, generation, all token verifiers,
management history and uncertain execution evidence. The original Config bytes
must remain unchanged. Do not run an old binary against the upgraded database
or restore an old database over new deliveries/audit; retain the new binary for
containment and revoke keys through its supported interfaces.

A full key can `POST /v1/external-agents/revoke-token` with `{"id":"TOKEN_ID"}`
to revoke only a registered agent key created by that exact full key in the
current generation. This is idempotent and retains the registered identity and
history. It cannot revoke another creator's keys, ordinary full keys or existing
user credentials. Scoped keys cannot administer this route. This also lets a
short-lived full test key clean up its own child keys before its operator-issued
parent key is revoked; no owner impersonation or database edits are needed.

### Device authorization (preferred agent onboarding)

An external agent no longer needs a human to copy a token. On the data origin,
POST `/v1/device/request` without credentials with
`{"key":"assistant1","name":"Assistant","expires_in":600}`. The optional
expiry is 60–1800 seconds, default 600. Response201 is standalone JSON containing
`device_code`, `user_code`, `verification_uri`, `expires_in` and `interval:5`.
Only the agent retains the high-entropy device code. Show the human the short
`ABCD-1234` user code and actual workspace-only verification URL. Never put the
device code in a URL. Public device endpoints retain the external browser/header
boundary and do not accept an existing Bearer header.

In the owner page the human pastes the short code. Fresh owner verification and
normal CSRF checks apply to both `POST /owner/device/lookup {user_code}` and
`POST /owner/device/decision` with
`{user_code,approve:true|false,days:30|90,authority_confirmed:true|false}`.
Approval requires explicit authority confirmation; denial does not. Lookup
returns `{request:{key,name,expires_at,status,scope,existing_agent,existing_name}}`.
Expiry is epoch seconds. Scope is `workspace_operations_and_own_inbox`.
An existing identity's actual name is displayed separately: approval issues a
key for that identity and never silently renames it. A request can be decided
only while pending and unexpired. The owner never receives the resulting token.

The agent polls `POST /v1/device/token {device_code}` at most once per returned
interval. Pending is200 `status:authorization_pending`; early polling429
`slow_down`; denied403 `access_denied`; expired410 `expired_token`; already
claimed409 `already_claimed`. Approved200 returns exactly once:
`{status:"approved",access_token,token_type:"Bearer",expires_in,agent}`.
All status bodies include a `status` field; slow/pending bodies include interval.
Unknown/invalid device codes are authentication failures. Device endpoints do
not use the normal token-operation `{request_id,result}` envelope.

Request state and code hashes persist across restarts. Approval stores the human
actor/decision time and chosen lifetime. First approved poll atomically claims
the request and creates the token/registration in one SQLite transaction. A
concurrent/repeated poll cannot mint another token. A lost successful response
requires a fresh human authorization and revocation of the lost key through the
normal owner token list; there is no plaintext recovery. The token uses the same
normal revocation/generation/workspace rules and gateway+own-inbox scope as other
registered agent keys. Original full keys and configuration binding remain valid.

Pending/approved device requests are bounded to100; total retained requests1000.
Expired records older than a day retire at admission. Code lookup/decision
attempts share a durable30/minute budget in addition to existing HTTP peer and
slot limits. Codes accept eight hex characters, optional middle hyphen,
case-insensitive, with outer whitespace trimmed. Malformed/unknown/expired owner
lookups return400; exhausted budget429; nonpending decision401 with no new grant.
Schema v5 adds only device grant/budget records; v4 inbox rollout is independent.
Keep the upgraded binary for containment; no old-binary/database downgrade.
