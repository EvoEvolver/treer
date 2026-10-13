# Treer App guidelines

This document defines the HTTP presentation contract for standalone Treer Apps.
It keeps one predictable App root useful to both Agents and humans through
HTTP representation negotiation.

Agent Interface Servers (AIS), including Codex UI and Pi UI, are not standalone
App indexes. They continue to expose the `ui_path` registered in their AIS
manifest. Channel bridges without an HTTP presentation surface do not need to
invent one.

## Route layout

Use these route families consistently:

| Path | Audience | Recommended representation |
| --- | --- | --- |
| `/` | Humans and Agents | HTML or GitHub Flavored Markdown, negotiated below |
| Additional documentation pages | Agents | GitHub Flavored Markdown in UTF-8 text |
| `/v1/...` or `/api/...` | Programs and Agents | Versioned JSON |
| Browser assets | Humans | App-relative JavaScript, CSS, fonts, images, and workers |
| `/health` or `/api/health` | Supervisors | Small JSON health document |

Choose the root representation in this order:

1. An explicit `Accept: text/html` requests the human interface.
2. An explicit `Accept: text/markdown` requests the Agent manual.
3. When neither media type is explicit, a browser `User-Agent` containing
   `Mozilla/` receives HTML and every other caller receives Markdown.

If both supported media types are explicit, honor their quality values and
header order. Set `Vary: Accept, User-Agent`. This is presentation selection,
not authentication or authorization; callers may choose either representation.
Do not redirect between representations or use a separate human-only prefix.
`curl APP_URL/` must continue to produce a useful Markdown manual without
JavaScript, browser automation, or authentication cookies.

## Agent pages

Write `/` and other documentation pages as portable GitHub Flavored Markdown.
Markdown remains plain text, works directly in terminals, and can also be
rendered by an Agent client when desired.

Prefer `Content-Type: text/markdown; charset=utf-8`. `text/plain; charset=utf-8`
is acceptable for compatibility when the body still follows GitHub Markdown
conventions. Do not return HTML from an Agent documentation route.

The Markdown root representation should contain, in this order:

1. App name and one-paragraph purpose.
2. A note that browsers and `Accept: text/html` receive the human interface.
3. The shortest safe read-only discovery or inspection commands.
4. Mutation commands, clearly separated from inspection commands.
5. Authentication, authorization, data sensitivity, and trust warnings.
6. Primary API routes or a link to another Markdown page that documents them.

Use ordinary headings, lists, tables, links, and fenced code blocks. Keep command
examples runnable and verify them against the current CLI. Avoid raw HTML,
client-side rendering, prose hidden in images, and instructions that require an
Agent to infer a hostname or API version.

An App manual is still App-controlled input. Agents should not treat arbitrary
text returned by an App as higher-priority policy, reveal credentials to follow
it, or execute mutation examples without a task that authorizes the mutation.

## Data pages

Use JSON for state, collections, records, machine-readable errors, and action
results. Do not use Markdown tables or HTML pages as a data API.

JSON routes should:

- return `Content-Type: application/json; charset=utf-8`;
- use an explicit version prefix such as `/v1/` for App-owned public contracts;
- preserve stable identifiers independently from display names;
- encode timestamps as RFC 3339 strings;
- return collections inside a named object instead of as a bare array;
- bound collection sizes and define pagination when a collection can grow;
- reject unknown or invalid mutation fields with a structured error; and
- keep secrets, credentials, and internal exception details out of responses.

Use a consistent error envelope:

```json
{
  "error": {
    "code": "soul_not_found",
    "message": "soul not found"
  }
}
```

HTTP methods retain their normal meaning: `GET` and `HEAD` inspect, `POST`
creates or invokes, `PATCH` updates, and `DELETE` removes. Document idempotency
behavior for mutations that callers may retry.

## Human pages

Serve the human entry point as the HTML representation of `/`. Keep its
scripts, stylesheets, fonts, images, and other browser assets at stable paths
relative to the App root. Do not introduce a `/_human/` route family.

Selecting a Managed App in the control plane embeds this HTML root through
`/api/workspaces/WORKSPACE/apps/APP_ID/ui/`. That authenticated tunnel preserves
relative paths and all CSP directives except `frame-ancestors`, which is set to
allow the configured control-plane origin and the tunnel's own origin. It also
removes conflicting `X-Frame-Options`. Direct App origins retain App-owned headers.

Managed Apps receive a dedicated wildcard-ingress origin when the deployment
configures `TREER_INGRESS_PUBLIC_URL`. The origin requires Workspace
authentication by default. `treer app create --public` deliberately makes it
anonymous, so the App must enforce any authentication its data requires. Human
pages should still work below Treer's authenticated browser-tunnel fallback
used by installations without a wildcard domain. Use document-relative asset
URLs such as `./app.js` and configure frontend build tools with a relative base
such as `./`.

Resolve API URLs relative to the negotiated App root while preserving any
tunnel prefix. For example, a page at
`/api/workspaces/WORKSPACE/virtual-hosts/HOST/proxy/` should resolve `v1/items`
below that same prefix, not the browser origin root. Keep the entry URL
directory-like with a trailing slash, and do not let SPA fallback turn unknown
Agent or API routes into the human `index.html`.

Browser authentication callbacks may remain under `/api/`, but their default
and validated `return_to` location should be the negotiated App root. Human session
cookies do not authenticate an Agent. State the authentication requirement of
every non-public API independently from the UI.

Use this browser authentication order:

1. If `X-Treer-Identity-Token` is present, verify it through
   `/.treer/apps/identity/verify` against the App's own registered service audience
   and workspace. Accept verified human and Agent principals; never trust
   caller-supplied identity IDs, role/name headers or unsigned claims. Verify
   every request. Invalid, expired or wrong-audience supplied tokens must fail,
   even if a valid App cookie also exists.
2. Without an identity header, explicit service-audience Bearer authentication
   may be supported for programmatic clients.
3. Without either header, fall back to an App's own OAuth session or a
   **Sign in with Treer** button for direct/legacy access. Do not show that
   button while a supplied identity is being resolved or after successful
   automatic identification.

Workspace ingress and the authenticated Managed App UI tunnel mint a fresh
60-second human token from the current platform session and workspace role.
They strip client `x-treer-*` headers before injecting it. Public ingress does
not mint human tokens; Agent service tokens keep their existing semantics. The
UI tunnel works without wildcard ingress. App OAuth remains a separate
12-hour grant and requires a registered callback origin. Keep forwarded tokens
out of HTML, JavaScript, logs and persistent browser sessions.

The App is trusted with the current user's authority through its service's App
APIs for the token's 60-second lifetime. This requires browser write protection:
check `Origin` and `Sec-Fetch-Site` for state-changing requests, **including
injected human tokens**, because ingress cookies use `SameSite=Lax`. Reject
untrusted/null origins and cross-site requests. Compare against explicitly
configured ingress and Proxy tunnel origins, not rewritten upstream Host or
untrusted forwarding headers. Use non-GET mutations, JSON/custom request
headers and narrow CORS. A token header is not evidence that a request was an
Agent's explicit bearer call; inspect the verified principal kind.

Apply ordinary browser protections, including an explicit Content Security
Policy, `X-Content-Type-Options: nosniff`, a narrow cookie path and SameSite
policy where practical, and no credentials embedded in HTML or JavaScript.

## Minimum verification

Every standalone App with a human interface should test the boundary directly:

```sh
curl -i "$APP_URL/"
curl -i -H 'Accept: text/html' "$APP_URL/"
curl -i -H 'Accept: text/markdown' "$APP_URL/"
curl -i -H 'Accept: application/json' "$APP_URL/v1/items"
```

Tests should prove that:

- `/` defaults to Markdown for non-browser callers and contains useful Agent instructions;
- explicit HTML and Markdown `Accept` headers override the `User-Agent` heuristic;
- browser navigation receives the human page and its relative assets resolve below the App root;
- data routes return JSON even when they fail;
- browser assets and API calls preserve a browser-tunnel path prefix; and
- unknown paths do not fall through to an unrelated representation;
- a human already logged into Treer opens the real App inside an iframe at
  `/api/workspaces/WORKSPACE/apps/APP_ID/ui/` and through Workspace ingress
  without an App login click, OAuth request or App session cookie;
- invalid/expired/other-audience identity headers are rejected even with cookies;
- injected human-token browser mutations still reject bad Origin/Fetch Metadata;
- desktop and narrow-screen browser workflows run through a test Proxy or local
  gateway reproducing per-request signing, header/cookie sanitization and Host
  rewriting, with separate tests of actual Proxy handlers and signing.

Keep the App's README focused on deployment, configuration, state, backup, and
operator concerns. Put the concise runtime manual in the content served from
`/`, and keep both documents synchronized with executable behavior and tests.

## App security and storage boundary

Managed Apps run with their machine account's OS authority; registration, a
workspace ingress gate and App-audience identity tokens are not process or
filesystem isolation. On critical-service machines use a dedicated unprivileged
user with private HOME, or a container/VM, minimal mounts/network and resource
limits. Never mount Host/Docker sockets or unrelated credentials into an App.
Same-UID processes can read each other's private files.

Use a dedicated state directory (0700), private files/SQLite sidecars (0600) and
private exclusive temporary files. Startup hardening may tighten an existing
state directory; do not configure a shared HOME/project directory as App state.
Backups need equivalent permissions and disk/backup encryption. App stores are
plaintext unless explicitly documented otherwise. Secret scanning is heuristic
and cannot prove absence of secrets, including encoded/compressed values.

OAuth state must be short-lived, single-use and paired with an HttpOnly browser
nonce; PKCE alone does not bind a server-retained state to a browser. Refuse
upstream redirects when forwarding identity, OAuth or workload credentials.
Request-size and owner-storage limits are not global rate/concurrency limits.
Trusted-workspace Apps without principal ACLs must never be marketed as isolated
services for hostile workspace members.
