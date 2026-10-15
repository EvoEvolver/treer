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

The desktop Tree sidebar has a focusable vertical separator with a 24px mouse/touch
target. Drag to resize immediately; Left/Right change 16px (Shift: 64px), Home/End
select the limits, and double-click restores 272px. The minimum is 240px; the
maximum is the smallest of 480px, 45% of the viewport and the space left after
reserving 400px for main content plus the 8px separator. Window resizing clamps
the displayed width while retaining the preferred width for larger windows.
Preferences use `treer:tree-width:v1:<user_id>` in localStorage across workspaces.
Unavailable storage falls back to the default on load and allows in-memory
resizing. Pointer capture and a temporary overlay protect drags across terminals
and embedded iframes; release/cancel/focus loss removes protection and fits the
terminal after layout commits. Separator colors use theme tokens.

Below 768px the separator is hidden and the existing mobile layout stays intact.
Managed App/Agent Interface embeds retain the same surrounding console sidebar.
Native iOS/Android screens and their separate Agent WebViews do not render this
sidebar; no native source or embedded Agent-owned interface is changed.

**Edit startup command** reads the current argv and saves it through one restart
operation. The editor scrolls on short mobile screens and preserves input after
a denied operation. Managed Apps have separate settings; their startup editing
requires the forthcoming App update API.

## Terminal replay

The shared desktop/mobile browser terminal and Managed App logs swallow historical
terminal queries using xterm parser handlers (DA1/DA2, CPR/DSR, DECRQM/DECRQSS,
OSC color queries and option-gated window reports).
Handlers remain active until the last replay write has parsed, then are disposed
before queued live output parses. Live queries still respond; user input is never
filtered by this replay guard. See the [reproduction and impact report](terminal-replay-report.md).

## Live Managed App discovery

The console refreshes its authoritative Core App list after App-like runtime
creation/update, known App rename/delete, workspace snapshots and machine events.
`kind: app` and the legacy `app:` name prefix are refresh hints, never an App
identity or URL source. Requests are debounced for 200ms with a maximum wait of 2 seconds during
continuous events; both timers are cleared on a flush or workspace switch. There
is no polling.
Returning to a visible/focused page also refreshes the list, including hostname,
public URL and status. Pure metadata changes without runtime events become visible
on this foreground refresh. Stale requests cannot overwrite newer workspace data
or local rename/delete events.

A selected runtime first discovered as an App switches from its terminal to the
App interface automatically. An explicit **Show logs** choice made after App
recognition stays with that workspace/runtime/App identity across metadata and
status refreshes. Mobile uses the same component and its existing full-screen
interface and workload **Logs** menu.

## Tree creation menu and map

The Tree header has one **+** menu for **Add folder** and **Add agent**. Each item
keeps its original disabled conditions; without machines, Add agent explains the
required setup and opens machine settings, with a visible explanation and a
**Set up a machine** button that opens enrollment instructions. The tour still targets `create-agent`.
Folder context menus retain their existing creation actions and destinations.

**Expand tree map** opens a read-only treemap in a large dialog.
Folders contain equally weighted Agent blocks; empty folders keep a placeholder.
Both Tree rows and map blocks display only **agent** or **app**, including their
accessible labels; runtime shell/command/harness kinds remain internal. Green dots indicate running/idle/working/starting Agents on online machines;
other states use muted dots and always have a text label. Names, workload category,
machine and state are text, with full paths on hover/focus. Opening an Agent uses
its stable ID and closes the dialog. Folder headers drill down; breadcrumbs and
the back button return to ancestors. Drill-down does not change sidebar expansion
preferences or send mutations. Narrow screens stack proportional blocks and
scroll vertically; deeper/tight containers offer drill-down instead of tiny labels.
The dialog traps focus, supports native button keyboard actions and Escape, and
returns focus to its trigger on dismissal. It has no new library dependency.

The map adds no dependencies and keeps the existing single-file build and release
contract. The local Worker security test verifies the map under the unchanged
nonce CSP. See [the size/review notes](tree-map-report.md).

## Trusted Policy publication settings

Workspace settings → Policy Provider shows the Core-confirmed `publish_authority`.
Owners can switch between `owner_session` and `provider_operator`. Enabling the
latter requires a separate warning/confirmation: Policy App administration token
holders gain full v2 publication/enforce authority without per-request owner
approval. The warning links the operator guide and recommends isolating the
Policy administration Agent, including Tree-management and machine-account risks.
Non-owners see read-only status; Core still checks owner authority on every write.

Writes go only to Core's browser-session
`PUT /api/workspaces/<workspace_id>/policy-provider/publish-authority`, with
`expected_app_id` guarding against a changed binding. No App/service identity
token is collected, stored, or sent. Success displays the confirmed mode and
Core audit action `policy_provider.publish_authority.set`; failures keep the
confirmed state and display the server error. Missing capability
`policy.publish-authority.v1`, an unknown/missing enum, an unsupported route or an
unconfirmed response is unavailable, with no legacy/App fallback.

The trusted landing URL is
`/orgs/<organization_id>/workspaces/<workspace_id>#policy-provider`. It selects
the workspace, opens settings and scrolls to the Policy Provider block on desktop
and mobile. An inaccessible/unknown deep-link target shows an error instead of
falling back to another workspace. SPA history navigation to a different settings
target unmounts the previous workspace and its pending confirmation before
loading the new target, including browser back/forward. Policy App links must use the Core-supplied `treer_url` for this exact
workspace; the console never derives its origin from an App-owned URL or token.

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
