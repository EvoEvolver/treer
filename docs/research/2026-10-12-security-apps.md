# Apps security review — 2026-10-12

Scope: source at `3372b34`, Soul, Issues, Mail, email-endpoint, Policy,
Gits, Paper, Tower and Telegram. This is a focused source review and local
regression exercise, not a penetration-test certification or a claim that all
Apps are safe for hostile tenants. All probes use ephemeral loopback listeners,
temporary state, synthetic identities/credentials and small generated payloads.
No canary, deployed service, real mailbox, user machine or real credential was
queried. No SMTP delivery, deployment or restart is part of this review.

Severity uses the requested P0 (remote authority bypass/credential disclosure),
P1 (other significant boundary or availability failure), P2 (hardening/limits).
Reachability and operator configuration are explicit prerequisites below.

## Findings, ordered by severity

### A1 — P0: Policy edits had no caller authentication (fixed)

- Phenomenon: `POST /v1/policy/publish` and `/invalidate` accepted any reachable
  caller; Workspace ingress authentication is not policy-administrator authority.
  A self-reported role header was unnecessary. If this App is the active Policy
  Provider, an ordinary workspace caller can change the effective authorization
  rules. This does **not** establish unauthenticated Internet access: active
  providers cannot be made public through the normal settings API.
- Local reproduction: start `PolicyHandler` with a temporary `PolicyStore`, send
  a JSON publish body with `Content-Type: text/plain`, no credentials and a
  synthetic `X-Treer-Role: owner`. Baseline returned 200 and increased revision.
  The regression now asserts a failure and unchanged revision.
- Impact: loss of the active provider's policy integrity; scope depends on the
  policies/controllers consuming it and their cache/failure behavior.
- Recommendation/change: small containment shipped: editing is locked by
  default, explicit operator `POLICY_ADMIN_TOKEN` (minimum 32 non-whitespace
  characters), constant-time Bearer comparison, JSON-only writes and browser
  Fetch Metadata rejection. UI holds the manually entered token only in memory.
  Controller bundle reads remain compatible. This is a separate administration
  capability, **not** automatic authorization of every workspace user.
  A larger follow-up is human Treer OAuth + current administrator-role checks;
  do not enable editing for an untrusted shared-machine operator.

### A2 — P1: Mail/Issues OAuth state was not bound to a browser (fixed)

- Phenomenon: server-wide single-use state and PKCE existed, but a callback did
  not prove it belonged to the browser that initiated authorization. PKCE alone
  does not stop login CSRF when the App retains the verifier for the attacker.
- Local reproduction: generate an authorization state in one synthetic browser;
  present its callback in a second browser without the initiating cookie.
  The baseline lookup accepted the state. Regression tests require the
  initiating browser nonce, reject mismatches before token exchange, preserve
  the legitimate pending flow and reject callback replay.
- Impact: account/session confusion; a victim may act in the attacker's account
  or disclose content there. This is not proof of an OAuth code-verifier bypass.
- Recommendation/change: small fix: hashed, expiring browser nonce paired with
  state; HttpOnly SameSite=Lax cookie; transactional one-time state consumption.
  Existing unbound pending flows must restart login. Existing sessions survive.
  Soul/email-endpoint already use browser-bound state; their tests are rerun.

### A3 — P1: Mail ignored supplied identity headers and lacked write guards (fixed)

- Phenomenon: Mail authenticated only its App cookie. A supplied invalid
  `X-Treer-Identity-Token` was ignored when a valid cookie existed. Its mutation
  routes did not enforce Origin/Fetch Metadata or JSON content type.
- Local reproduction: synthetic valid Mail cookie + forged/empty identity
  header previously returned a session; now returns 401. Valid injected human
  identity is accepted. Null/foreign Origin and cross-site Fetch Metadata writes
  return 403. A same-site hostile origin is included: SameSite=Lax is not an
  authorization check for sibling sites. No claim that arbitrary cross-site POSTs
  always carry a Lax cookie is made.
- Impact: violation of fail-closed identity precedence; potential browser
  mutation attacks where ambient cookies are sent. Baseline rejection of an
  invalid header did not exist, but no ability to forge a signed token was found.
- Recommendation/change: small fix: injected token, explicit Bearer, then OAuth
  cookie; per-request Proxy verification for human kind, audience, expiry,
  workspace/current membership; no fallback after a supplied invalid token.
  Exact configured Origin allowlist, Fetch Metadata and JSON mutation guards.
  Mail is a human client; Agent tokens do not become human sessions.

### A4 — P1: authenticated upstream redirects forwarded credentials (fixed)

- Phenomenon: Python's default redirect handler retained Authorization when
  Mail/Issues followed a redirect from their configured Proxy. Policy's
  invalidation client also followed redirects with workload headers.
- Local reproduction: two ephemeral HTTP listeners, first returns 302 to the
  second, synthetic Bearer only. Baseline second listener received it; Mail and
  Issues now fail the request without contacting the sink. Policy uses the same
  no-redirect strategy. Soul already disables redirects.
- Impact: disclosure if an operator-configured trusted upstream is compromised
  or redirects unexpectedly. No user-selectable arbitrary SSRF destination was
  demonstrated. Configured upstream URL control is trusted operator authority.
- Recommendation/change: small fix: refuse redirects rather than transferring
  credentials or inventing an alternate trust origin.

### A5 — P1: hidden tar metadata escaped Soul's expansion budget (fixed)

- Phenomenon: regular member sizes/counts were bounded, but `tarfile` consumes
  PAX/GNU metadata before yielding members. A compressed archive could expand
  large hidden headers outside the payload budget.
- Local reproduction: a generated 32 KiB PAX comment and a 1 KiB patched budget;
  gzip/bzip2/xz and plain tar must fail preflight. This test never allocates a
  large bomb. Baseline parser could consume hidden metadata before rejecting.
- Impact: authenticated upload/client-import resource exhaustion, not a proven
  file overwrite or a claim of anonymous service access.
- Recommendation/change: small fix: count the complete decoded stream before
  tar parsing, including hidden metadata and padding; explicitly select the
  checked compression format. Existing traversal/link/device/member-count and
  payload checks remain. Stream cap includes tar overhead; gzip/bzip2/xz/plain
  are supported, other compression autodetection is disabled. This bounds
  expansion, not total CPU/concurrency/slow-client cost.

### A6 — P1: permissive local state and temporary-file modes (fixed in reviewed stores)

- Phenomenon: with ordinary umask 022, Mail/Telegram/Tower state could be created
  in 0755 directories with 0644 database files; Issues' directory was 0755 and
  sidecar creation relied on ambient umask. Policy wrote a predictable 0644
  temporary file before chmod. OAuth access tokens/transcripts may be present.
- Local reproduction: dedicated temporary state directories, umask 022;
  inspect mode bits only, never real data. Regressions check Mail, Issues,
  Telegram and Tower directories 0700, databases 0600 and available sidecars
  without group/other permissions.
- Impact: other OS users with path access could read sensitive local state.
  This does not protect against root or another process with the same UID.
- Recommendation/change: small hardening: private process umask/dedicated state
  directory and existing database modes; Gits/Paper existing state roots also
  tightened. Policy uses exclusive 0600 random temporary files + atomic rename.
  Soul/email-endpoint already establish private roots; Mail's store also
  tightens an existing endpoint root. Use **dedicated** directories: startup may
  chmod the configured state directory. Copied backups need equivalent controls.

### A7 — P1: Gits/Paper/Tower trust reachability rather than per-user ACLs (plan)

- Phenomenon/source reproduction: Gits dispatches Git HTTP/read/write operations
  without Treer identity verification; Paper's project/compile/WebSocket routes
  have no principal ACL; Tower leaves reads unauthenticated and makes its shared
  write token optional. Their documentation already describes trusted-workspace
  use, so this is an unsupported isolation property, not a bypass of a promised
  per-repository ACL. A reachable HTTP caller is the local reproduction model;
  no deployed endpoint was contacted.
- Impact: any reachable caller can access shared repositories/documents/Tower
  transcripts, and may mutate them according to the App's current routes.
  Workspace authentication gates entry, not per-folder/per-project access.
- Recommendation/change size: **large, not implemented**: define ACL and caller
  model first; verify App-audience tokens on HTTP and WebSocket handshakes;
  preserve Git client compatibility; require explicit capabilities for compiler
  operations and private reads. Until then restrict to mutually trusted members,
  bind privately, never expose publicly, and isolate compiler/filesystem access.
  Do not advertise these Apps as hostile-tenant services.

### A8 — P1: no comprehensive per-App resource/rate isolation (plan)

- Phenomenon/source reproduction: Python HTTP Apps use thread-per-connection
  servers; request/member/owner quota bounds do not provide a connection, CPU,
  OAuth-attempt or global memory budget. Paper compilation launches local tools.
  Tests exercise small configured caps, not destructive load or OOM probes.
- Impact: a reachable authenticated member or direct-port caller can exhaust
  shared resources; availability of unrelated critical services is not proven.
- Recommendation/change size: **large**: bounded connections/read deadlines,
  per-principal and global queues/rate limits, compiler workers and OS/container
  resource quotas. Proxy email hourly limits do not cover all App routes.

### A9 — P2: Soul secret scanning is heuristic, data is plaintext (documented)

- Phenomenon/local reproduction: synthetic `sk-syntheticToken12345` is detected;
  base64 and JSON unicode-escaped encodings of the same bytes evade raw regex.
  Embedded compressed files, custom token formats and semantic secrets are not
  generally detected. `allow_secrets` is an explicit owner acknowledgment, not
  encryption; false positives are also possible.
- Impact: private archives/backups can still contain secrets without a marker.
  Soul, Issues, Mail and other SQLite/blob stores have no App-level at-rest
  encryption. ACLs/modes do not protect against machine administrators or
  same-UID compromise.
- Recommendation/change size: documentation now forbids a secret-free guarantee.
  Do not upload live credentials; use disk/backup encryption and restrictive
  retention. Strong encryption/key lifecycle or recursive semantic DLP would
  be a large separate design; regex expansion cannot establish that guarantee.

### A10 — P2: Policy simulator rendered response text as HTML (fixed)

- Phenomenon/local reproduction: `matched_rule_id` was interpolated into
  `innerHTML`; policy authors could place markup there. A restrictive CSP limits
  script execution, so this review does not claim a demonstrated script bypass.
- Impact: content/DOM injection and misleading policy-test output.
- Recommendation/change: small fix: DOM nodes and `textContent` for all result
  fields; no interpolation of App/user strings into HTML.

## Other inspected boundaries and limits

- Soul rejects absolute/traversing paths, links and special files before storing
  blobs; extraction revalidates paths. Zip is not accepted. These checks do not
  make arbitrary restored harness content trustworthy: it is instructions/data
  for the newly created Agent. Claims of sandboxed restored content are invalid.
- SQL operations inspected use parameter binding; no concrete SQL injection
  was demonstrated. Subprocess usage inspected uses argv, but Paper's LaTeX
  toolchain and restored harness sessions are intentionally executable/trusted
  workloads, not a safe rendering sandbox. No guarantee about all dependencies
  or native compiler vulnerabilities is inferred from this review.
- App OAuth callback URLs come from operator configuration. Mail return paths
  are relative and reject `//`/external URLs; Issues/Soul use their configured
  App root. No local open redirect was demonstrated. Core OAuth/PKCE code is
  owned/audited separately, not certified by these App tests.
- Soul/Issues/email-endpoint validate supplied identities before falling back
  to sessions. Email-endpoint enforces member/contact binding and same-folder
  sender rules; outbox-only tests do not establish production SMTP deliverability.
- Main browser views use text/DOM rendering or framework escaping. No general
  XSS-free claim is made; the confirmed Policy interpolation was removed.
- Telegram's external Bot API and configured Proxy endpoints are operator trust
  inputs. This review did not perform Internet SSRF scans, dependency advisories
  or real Bot-token tests.

## Operating guidance and validation

Managed App execution is **machine-account execution**, not an App sandbox.
On a critical-services machine use a dedicated unprivileged OS user/private
HOME or a container/VM; mount only App data, constrain network/resources, avoid
Docker/Host sockets and unrelated service credentials, encrypt disks/backups,
and keep ingress workspace-only. A separate state directory alone does not
isolate two processes sharing a UID. Public access is never a substitute for
App authentication. See [App guidelines](../../apps/GUIDELINES.md) and
[security model](../security.md); Unsupported Claims must retain these limits.

Run the cross-App local regressions with:

```sh
python3 apps/security/tests/test_boundaries.py
```

Focused component suites and documentation check results are recorded in the
final validation section before merge. Browser tests use only fake local Proxy
identity/OAuth behavior, not deployed Treer login. Security-boundary changes
require core-dev/release-eng review before merging.
