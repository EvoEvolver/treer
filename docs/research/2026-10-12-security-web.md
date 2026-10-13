# Web and mobile security review — 2026-10-12

Scope: `web/`, native WebView integration in `mobile/`, and the checked-in
Cloudflare Worker configuration for `app.canary.treer.ai`. Baseline:
`3372b34`. This is a source review plus isolated local regressions, not a
penetration-test certification. No Canary/user-machine requests, real accounts,
real credentials, SMTP, deployments or runtime restarts were used. The package
registry advisory lookup received dependency metadata only, under an empty HOME
and an environment allowlist.

## Severity and result

No unauthenticated remote P0 exploit was established. P1 findings below require
execution of untrusted/compromised interface JavaScript, a malicious embedding
site, or another stated prerequisite. If interface code becomes an attacker
entry point, the session/API exposure can become P0 impact. Registry advisory
severity is recorded separately from demonstrated Treer exploitability.

Small fixes cover native cookie construction, control-plane response headers and
cache policy. Larger App-origin/native-bridge isolation work remains open. Do
not advertise safe execution of untrusted Apps or Agent Interfaces.

## P1 — Native session cookies were readable by interface JavaScript

**Phenomenon.** Android manually installed `treer_session` without HttpOnly or
SameSite; iOS constructed a cookie without HttpOnly, SameSite or Secure. Android
set Secure for HTTPS but also made a redundant HTTPS-host cookie. Cookies are
for the Proxy host and shared with interface WebViews.

**Local reproduction.** `node mobile/tests/session-cookie.mjs` reconstructs the
old Foundation property dictionary with a synthetic session and confirms
`isHTTPOnly == false` and `isSecure == false`. It then checks the replacement's
HttpOnly/Secure flags, exact host, path, SameSite header and malformed-input
rejection. Android `SessionCookieTest` exercises its pure header builder.
Neither test reads an existing cookie store. Native runtime script extraction
was not attempted on a device; this distinguishes confirmed construction
properties from platform enforcement.

**Impact.** JavaScript executing at the Proxy origin could read the original
session cookie; iOS could also send it over HTTP to the same host. This is
conditional session-credential exposure, not evidence of an unauthenticated
remote exploit. HttpOnly still allows authenticated fetches.

**Recommendation/status.** Fixed header construction on both platforms:
host-only, Path=/, HttpOnly, SameSite=Strict, Secure on HTTPS, reject malformed
session values and credential-bearing/non-HTTP URLs. No transport-security claim
for explicitly configured HTTP development endpoints. Existing deployed cookies
are not repaired until the updated client injects them. Test actual WKWebView
and Android CookieManager enforcement in a dedicated emulator/device fixture.

**Change size.** Small: two helpers, call sites, Foundation/JUnit regressions and
iOS build-file registration. Full Android/iOS app builds remain unverified here.

## P1 — Embedded interfaces retain the user's Proxy-origin authority

**Phenomenon.** Web App/Agent Interface iframes allow both scripts and same-origin
content. They are served from the Proxy origin, where browser session cookies
also authenticate Core APIs. When the control plane shares that origin, child
JavaScript can access the parent and remove the iframe sandbox. A separate
control-plane Worker origin protects that parent DOM, but does not make the
Proxy-hosted child lose its Proxy API authority.

**Local reproduction.** `security.spec.ts` serves synthetic parent/child/API
responses on loopback, uses the production sandbox flags, and asserts that the
child reads a simulated user API, changes its parent's DOM and removes its
sandbox. This fixture establishes the browser-origin property; it does not
claim to exercise real Proxy authorization or bypass workspace membership.

**Impact.** A compromised interface can act with the visiting user's Proxy
session, beyond that App's service-audience APIs. HttpOnly protects direct cookie
reads, not request authority. CSP on the outer control plane does not constrain
the separately loaded interface document.

**Recommendation/status.** Open architectural work. Serve each untrusted
interface on a distinct origin with no platform login cookie; use narrowly
scoped App/service credentials, exact origin checks, and an explicit navigation
policy. Preserve a clearly documented trusted-code model until then. Do not
remove `allow-same-origin` alone and claim the API authority has been isolated.
Updated `docs/security.md` Unsupported Claims.

**Change size.** Large: Proxy routing/cookie scopes, OAuth/identity delegation,
web embedding and native clients; needs core-dev/launcher-dev/apps-dev design.

## P1 — Native bridge origin, navigation and path confinement are incomplete

**Phenomenon.** iOS injects its interception bridge into all frames, accepts
script messages without checking `frameInfo` origin/main-frame status, enables
universal access from file URLs, and allows programmatic navigation. Android's
`addJavascriptInterface` is available to child frames, permits mixed content and
third-party cookies, and enables WebView debugging even in release builds.
Forwarders combine caller-controlled relative paths with an authenticated UI
route; dot segments and redirect behavior are not explicitly confined. iOS also
returns upstream response headers to JavaScript without a credential-header
filter.

**Local reproduction.** Source trace: `AgentAISWebView.swift` message handler,
`relativePath`, navigation delegate and injection; `APIClient.interfaceURL`;
Android `AgentWebViewScreen.kt`, `AgentUiBridge.tunnelPath` and
`TreerApi.forwardUiRequest`. No native emulator was available. We have not
proven a device exploit, exact dot-segment normalization behavior across all
clients, or a credential-bearing redirect/response in production.

**Impact.** Foreign/child-frame JavaScript may inherit privileged native request
capabilities. HTTP/mixed content broadens injection opportunities; bridge
requests can bypass browser CORS. Exact escape and credential-disclosure impact
requires native runtime reproduction. Debug access also requires local debugging
access; it is not a remote compromise on its own.

**Recommendation/status.** Open coordinated redesign, not a partial claim of
isolation: trusted bundled top-level origin only; deny remote/subframe bridge
calls; cancel unauthorized programmatic navigation; remove interfaces before
leaving trusted content; reject normalized paths escaping the UI prefix; forbid
credential-bearing cross-origin redirects; filter sensitive response headers;
separate ephemeral website data; HTTPS only outside explicit development mode.
Disable release debugging and unnecessary mixed/third-party content with native
compatibility tests. Cookie construction is fixed separately above.

**Change size.** Large for the complete authority boundary; individual settings
are small but do not solve bridge confinement. Requires native harness tests and
core/launcher contract review before claiming safe untrusted content.

## P1 — Control-plane clickjacking and missing script CSP

**Phenomenon.** Original Worker and Nginx shell responses lacked anti-framing,
MIME and referrer headers. The Worker originally ran only for config/health;
static/SPA responses bypassed it. No control-plane script CSP was installed.

**Local reproduction.** Local Wrangler serves the built bundle. Tests fetch
root, SPA/deep paths, asset, config, health and missing-file responses; inspect
headers and fresh matching nonce attributes; verify the shell actually boots;
block an injected non-nonce script and an event-triggered eval; and refuse
framing from a second loopback origin. Eval is triggered by a real browser click
because Playwright evaluation itself bypasses CSP.

**Impact.** An attacker could frame an otherwise frameable authenticated UI and
attempt clickjacking (success also depends on cookie/browser/origin context).
Missing CSP makes a future injection flaw easier to exploit; no React XSS was
found by this review.

**Recommendation/status.** Fixed Worker routing to run before all assets. Added
per-response cryptographic script nonce CSP, exact configured Proxy HTTP/WS
connection and frame origins, no objects/base/form external targets,
`frame-ancestors 'none'`, X-Frame-Options DENY, nosniff and no-referrer. HTML,
health and config use no-store; HTML validators are removed to avoid nonce/CSP
mismatch. Nginx receives anti-framing/object/base, referrer and MIME headers,
but **not** the Worker's nonce script restriction. Inline styles remain allowed
for React/UI libraries. No HSTS deployment claim is made.

**Change size.** Small/moderate: Worker helper/routing/HTML rewriting, Nginx
headers and local browser regressions. Nginx syntax/runtime was not exercised;
its location-level header overrides were removed to preserve inheritance.

## P2 — Capability URLs and ingress logging need lifecycle controls

**Phenomenon.** Password-reset and invitation values are URL query capabilities.
AuthScreen retains them until completion/navigation. The Worker enables
observability at 100% sampling. Actual provider log fields and retention are not
verified locally. Referrer policy was absent before this patch.

**Local reproduction.** Source trace of AuthScreen query parsing and
`wrangler.jsonc` observability. No real reset/invitation URLs or production logs
were inspected. Browser localStorage searches found theme/tour preferences only,
not platform session tokens. Browser auth uses credentials-included cookies;
JSON session tokens are requested only by native clients in the backend helper.

**Impact.** URLs can remain in history, screenshots or ingress logs. Query
capabilities can leak through referrers without an adequate policy. This review
does not establish that Cloudflare currently records or exposes those secrets.

**Recommendation/status.** Added no-referrer header plus an early HTML meta
policy. Follow up by capturing one-use values into component state and clearing
only sensitive parameters immediately (preserve legitimate `return_to`), and by
redacting/disabling query logging at every ingress. Disable sensitive URL capture
in telemetry. Frontend error strings are escaped but not universally redacted;
backend errors must not contain credentials. No routine frontend console logging
of credentials was found.

**Change size.** Small frontend lifecycle work plus operator/provider logging
configuration and end-to-end verification; not completed here.

## P2 — Dependency advisories remain (not a clean audit)

**Phenomenon/local reproduction.** Isolated `pnpm audit --json` against the
committed lockfile returned 16 advisory entries: critical 1, high 6, moderate 6,
low 3. Counts describe advisories, not sixteen independent remotely exploitable
Treer bugs. `pnpm why` located these chains:

| Dependency | Installed / chain | Advisory evidence |
| --- | --- | --- |
| shell-quote | 1.10.0; browser runtime | GHSA-pqg4-j6r4-53mv (critical upstream): quote comment-object followed by line terminator |
| sharp | 0.35.2; Wrangler → Miniflare (dev) | GHSA-rgj7-g3m4-5g8c, GHSA-wq5f-xc86-pv6w |
| undici | 7.29.0; Wrangler → Miniflare (dev) | 10 entries: GHSA-3wwx-pv8p-q78v, GHSA-pmjh-fq2x-6v4x, GHSA-r53p-7pc4-xj5r, GHSA-rfgv-xxqx-mfg5, GHSA-3xpg-4rpp-hhhm, GHSA-2jfj-6hjv-fm6j, GHSA-2gqq-gqf2-x968, GHSA-w293-vg96-wgc3, GHSA-8436-99hf-9mmv, GHSA-rx4f-c7p8-82vq |
| braces | 3.0.3; Tailwind/chokidar/micromatch (dev) | GHSA-vfj7-8cjw-p6xm |
| source-map-js | 1.2.1; PostCSS/Vite (dev) | GHSA-68fv-2mgg-jv7q |
| postcss-selector-parser | 6.1.4; Tailwind (dev) | GHSA-rj75-hqrm-r3gf |

**Impact.** Most findings concern developer/build tools and untrusted parser,
image, network or cache inputs. They do not run in the static browser bundle.
The runtime `formatCommandLine` calls `quote([command, ...args])` with strings;
`parseCommandLine` rejects comment/operator objects. The upstream critical
comment-object condition is not demonstrated reachable through Treer's typed
startup editor. This is not grounds to ignore the advisory or call it Treer P0.

**Recommendation/status.** Open: upgrade to upstream fixed releases, preferably
Wrangler/Tailwind/PostCSS parents, verify lockfile and rerun build/browser tests
and audit. Review dependency provenance as well as registry severity; do not
silently force incompatible transitive versions. Keep developer servers loopback
and builds isolated from privileged credentials. No dependency upgrade was
bundled into this security-boundary change.

**Change size.** Small for direct shell-quote upgrade; moderate for toolchain
updates/compatibility. A future clean audit is not asserted.

## Checks without a demonstrated finding

- React renders reviewed API names/errors as text. A malicious synthetic Agent
  name is visible literally and does not create an event-handler image. No
  `dangerouslySetInnerHTML` was found; first-run tour HTML is static source.
- `postEmbedTheme` sends appearance only; it now fails closed for invalid,
  empty or non-HTTP(S) frame URLs instead of broadcasting to `*`. No privileged
  incoming control-plane postMessage handler was found.
- Browser `return_to` permits only configured Proxy origin and two exact
  authorization paths. Added rejection of URL credentials/fragments; browser
  regression also rejects foreign origin and unrelated path. Backend OAuth
  consumes provider-specific state and redirects to configured app URL; this
  is source evidence, not a complete OAuth penetration test.
- Backend login/admin cookies are HttpOnly, SameSite=Strict and conditionally
  Secure; ingress cookies use Lax. Production must enable Secure and HTTPS.
  JSON mutation requests and exact credentialed CORS are useful defenses but
  not universal CSRF proof. Missing-Origin and same-site sibling/App-origin
  cases require the core-dev API audit; no blanket CSRF-safe claim is made.
- Native errors may include upstream/localized error details; frontend text
  escaping is not secret redaction. No real error payloads were sampled.

## Verification and deployment limits

Run from this worktree, with synthetic identities and isolated environment:

- `pnpm --dir web build`; Worker-generated types plus `tsc -p tsconfig.worker.json`.
- `PW_PORT=<free-port> pnpm --dir web test:e2e --workers=1` (runner creates
  private HOME and strips Agent/provider/Proxy environment).
- `node mobile/tests/session-cookie.mjs`; `plutil -lint` iOS project.
- `node scripts/check-docs.mjs`.

Android JUnit and full native builds were not run: configured Android Studio JBR
is absent; no device/emulator verification was performed. Nginx runtime and live
Cloudflare edge/cache rules are not verified. No PostgreSQL tests are applicable
to these client/header-only changes. Headers were verified on **local Wrangler**,
not on deployed `app.canary.treer.ai`. Release-eng must verify deployed headers,
CDN cache behavior and nonce matching during a separately authorized release.

Reference contracts: [Cloudflare static asset headers](https://developers.cloudflare.com/workers/static-assets/headers/),
[Worker-first routing](https://developers.cloudflare.com/workers/static-assets/routing/worker-script/),
[Android native bridge risks](https://developer.android.com/privacy-and-security/risks/insecure-webview-native-bridges),
[iframe sandbox behavior](https://developer.mozilla.org/en-US/docs/Web/HTML/Reference/Elements/iframe).
