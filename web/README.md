# Treer control plane

Agent operations use stable IDs. Display names are unique within a Tree folder,
so the same label can appear in different folders. Hover a workload row to see
its complete path; the selected Agent's header shows folder breadcrumbs. The
full-screen mobile header shows the complete path. Machine and audit inventories
show the canonical path, falling back to a stable ID when unavailable.

Use **Copy path** in the workload menu to copy an absolute Treer Tree path, such
as `/research/reviewer`. Pass it as a single CLI argument, for example
`treer agent read /research/reviewer`; quote paths containing spaces. These paths
are workspace Tree addresses, independent of a machine working directory.
Rename, create and move conflicts keep the editor open and display the backend's
error. Renaming or moving retains the selected Agent's stable identity.

Folder expansion is cached in browser localStorage under
`treer:folder-expansion:v1:<workspace_id>`, keyed by stable folder IDs. Uncached
folders, including newly created folders, start collapsed. Toggles save
immediately; reload and workspace switching preserve both expanded and collapsed
choices. Renaming/moving a folder retains its preference. Successful tree reads
prune deleted IDs; failed reads never erase the cache. If browser storage is
unavailable, the tree still toggles in memory and starts collapsed on reload.

Selecting a different Agent reveals and saves only its ancestor folders, leaving
other preferences intact. The initial/default Agent selection does not override
collapsed preferences, so a user can collapse the selected Agent's folder and
reload without reopening it. The current console has no Agent deep-link route;
a future explicit deep-link selection should use the same reveal behavior.

**Edit startup command** reads the current argv and saves it through one restart
operation. The editor scrolls on short mobile screens and preserves input after
a denied operation. Managed Apps have separate settings; their startup editing
requires the forthcoming App update API.

## Local verification

```sh
pnpm install --frozen-lockfile
pnpm build
pnpm test:e2e
```

Browser tests mock the API and workspace events. The test runner creates a
throwaway HOME and allows only basic process environment variables, excluding
Agent identities, workload credentials, provider settings and HTTP proxies. It
starts its own loopback Vite process and never reuses an existing server. Set
`PW_PORT` to select a free port. Playwright browser binaries must be installed;
`PLAYWRIGHT_BROWSERS_PATH` may point to an existing installation.

The same-name Agent and startup editor cases cover desktop and 390px viewports;
the long-command editor test writes desktop and mobile screenshots under the
ignored `test-results/` directory for visual inspection.

## Browser security boundary

The Cloudflare Worker runs before every static/SPA response. It adds a fresh
script nonce and CSP, denies framing, disables MIME sniffing and referrer
transmission, and marks HTML/config/health as `no-store`. Do not cache rewritten
HTML at an upstream edge: its script nonce must match its CSP. Nginx adds the
anti-framing, MIME and referrer headers, but its static shell does not have the
Worker's nonce-based script restriction. These changes require a normal release;
source inspection and local Wrangler tests do not verify deployed Canary headers.

App/Agent Interface iframes remain trusted code with Proxy-origin user authority.
Their sandbox is not a tenant or user-session security boundary. See the
[security review](../docs/research/2026-10-12-security-web.md). The security suite
starts its own local-only Wrangler and uses synthetic identities; build the
bundle before running `pnpm test:e2e security.spec.ts --workers=1`.
