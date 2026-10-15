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

Explicitly selecting an Agent reveals and saves only its ancestor folders,
leaving other preferences intact. Reload starts at Overview with no selected
Agent, preserving collapsed preferences. The current console has no Agent deep-link route;
a future explicit deep-link selection should use the same reveal behavior.

The desktop Tree sidebar has a focusable vertical separator with a 24px mouse/touch
target. Drag to resize immediately; Left/Right change 16px (Shift: 64px), Home/End
select the limits, and double-click restores 272px. The minimum is 240px; the
maximum is the smallest of 480px, 45% of the viewport and the space left after
reserving 400px for main content plus the 8px separator. Window resizing clamps
the displayed width while retaining the preferred width for larger windows.
The compact arrow sits at the vertical center of the separator, with a 44px
click/touch target around its 16×28px visual surface. It collapses/expands the sidebar,
including via Enter/Space, and exposes `aria-expanded` and `aria-controls`.
A narrow rail keeps the expand button available while collapsed. The original
width is restored on expansion, and the terminal is fitted after either toggle.
Preferences use `treer:tree-width:v1:<user_id>` and
`treer:tree-collapsed:v1:<user_id>` in localStorage across workspaces.
Unavailable storage falls back to the default on load and allows in-memory
resizing and toggling. Collapse defaults to off when not cached. Pointer capture and a temporary overlay protect drags across terminals
and embedded iframes; release/cancel/focus loss removes protection and fits the
terminal after layout commits. Separator colors use theme tokens.

Below 768px the separator and arrow are hidden, desktop collapse is ignored,
and the existing mobile layout stays intact.
Managed App/Agent Interface embeds retain the same surrounding console sidebar.
Native iOS/Android screens and their separate Agent WebViews do not render this
sidebar; no native source or embedded Agent-owned interface is changed.

**Edit startup command** reads the current argv and saves it through one restart
operation. The editor scrolls on short mobile screens and preserves input after
a denied operation. Managed Apps have separate settings; their startup editing
requires the forthcoming App update API.

## Console composition

Workspaces open at **Overview** without attaching a PTY. The main view preserves
folder hierarchy and persisted sibling order; it never regroups Agents into flat
attention lists. Status badges and subtree counts overlay the Tree. Attention
rules follow iOS: blocked/failed Agents and nonterminal Agents on offline/missing
machines need attention; online starting/working Agents are working; online
idle/unknown Agents are idle. Apps remain visible but do not count as Agents in
these filters. Legacy unknown statuses are neutral; `stopped` is terminal.

Folders with attention/working descendants expand by default, while idle/empty
folders collapse. Manual toggles take priority while the overview is mounted;
they are separate from the sidebar's persisted expansion preferences. Filter
chips retain matching Agents and their ancestor folders, automatically expanding
those ancestors. Clear a filter to restore the unfiltered manual toggles. Filtered
subtree counts include only matches. Machines online is informational. Folder
and Agent controls are native keyboard buttons; complete paths are in accessible
labels and hover titles. Repeated labels remain addressable by stable IDs.
Small attention/working text uses separate foreground tokens, checked at 4.5:1
against default and hover backgrounds in both themes (`pnpm check:contrast` and
the overview browser tests); button fill colors remain independent.

Selecting an Agent opens its existing Interface UI when available, otherwise its
terminal; the Tree menu's Logs action remains the PTY fallback. Live snapshots
update status/counts without selecting an Agent; workspace switching clears the
selection. Use desktop Overview or the workspace breadcrumb to return. Phones
start at Overview and expose **Browse tree**; closing a full-screen Agent retains
return-to-Tree behavior. Trusted Policy settings links and tour previews retain
their direct destinations. Treemap layout and launch operations are unchanged.

`workspace-app.tsx` coordinates selection, views and user actions. Its dialog
state and rendering live in `workspace-dialog-state.ts` / `workspace-dialogs.tsx`;
App metadata and snapshot/event synchronization in `use-workspace-live-feed.ts`;
organization members/groups in `use-organization-members.ts`; machine/audit
queries in `use-workspace-metrics.ts`; route refs in `use-workspace-route.ts`;
and the mobile terminal key rows in `mobile-terminal-keys.tsx`.
These boundaries retain the existing request paths, refresh timings, mutation
fencing, dialog defaults and focus/selection behavior. Authentication and trusted
Policy settings remain in the existing control-plane flow. The single-file
build and dependency list are unchanged.

## Audit traffic

Audit shows workspace traffic for the last 6 hours, 24 hours (default), 7 days,
or 30 days. Range changes query both machine usage and Agent detail with the same
`hours` value; they do not filter a cached 24-hour response. The existing Core API
supports up to 720 hours. Machine overview keeps its independent 24-hour summary.

The SVG trend switches between billable bytes and data frames. Short ranges use
hourly buckets; longer ranges group those records into UTC days. The current hour
and edge days may be incomplete. Unrecorded windows display zero, which does not
prove there was no traffic or guarantee historical coverage. Direct traffic is
machine-reported and its frames are excluded. Agent detail is included in machine
usage, never added to the billable total; route rows display payload bytes.

Bars expose values on hover or keyboard focus. The expandable data table provides
the same values as text; theme colors and layout adapt to dark mode and narrow
screens. No chart dependency or build change is required. The existing ten-second
refresh cadence remains; requests never overlap within a range, and leaving the
view or changing workspace/range cancels them. The Refresh audit button reloads
the selected range. Failures show a retry button instead of stale totals; an older
Proxy without Agent detail is explicitly identified while machine usage remains
available. Selecting a different workspace starts at the default range.

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

## Tree creation menu and Overview map

The Tree header has one **+** menu for **Add folder** and **Add agent**. Each item
keeps its original disabled conditions; without machines, Add agent explains the
required setup and opens machine settings, with a visible explanation and a
**Set up a machine** button that opens enrollment instructions. The tour still targets `create-agent`.
Folder context menus retain their existing creation actions and destinations.

**Workspace map** in the Overview header opens a read-only treemap in a large
dialog. The sidebar Tree header contains only the creation menu; open Overview
to explore the map on desktop or phones.
The map is a fixed canvas inside the viewport-sized dialog, with no canvas or
breadcrumb scrollbars. A local squarified layout recomputes on resize and assigns
each Agent or App weight 1; a folder weighs the sum of its descendants. Siblings
are ordered by full path (stable ID breaks ties), so status and API response order
do not move blocks. Padding and folder title bars reserve a little drawing space.
Empty folders have zero area and remain accessible through **Go to**.
Tree rows and map blocks share a Bot icon for Agents and a window icon for Apps.
Both display only **agent** or **app**, including accessible labels; runtime
shell/command/harness kinds remain internal. Status dots use the existing theme
tokens (working, online or muted), with the full state always in the hover/focus
hint and accessible name. Labels progressively reduce from full path to name,
initials and status dot as space decreases. No button is smaller than 44×44px.
Small blocks remain visual markers: drill into their parent folder to enlarge
its children, or use the native **Go to** picker to open any immediate child
without targeting tiny blocks. This picker also keeps very dense, flat folders
fully keyboard accessible. Opening an Agent uses its stable ID and closes the
dialog. Folder headers drill down; breadcrumbs, an ancestor picker and the back
button return to ancestors. Navigation never changes sidebar expansion
preferences or sends mutations. Nested previews are bounded to 12 folder levels
and approximately 700 nodes; branches fold earlier when their child folder
headers cannot fit a touch target. Explicit drill-down resets that budget.
The dialog traps focus, supports native button keyboard actions and Escape, and
returns focus to its trigger on dismissal. It has no new library dependency.

The map adds no dependencies and keeps the existing single-file build and release
contract. The local Worker security test verifies the map under the unchanged
nonce CSP. See [the size/review notes](tree-map-report.md).

## Retained workspace tabs

Overview is a pinned page; its desktop tab stays visible when the tab strip scrolls. Opening an Agent or App from the Tree or Overview
opens or selects its stable-ID tab; a tab’s full Tree path distinguishes repeated
names. Desktop tabs support Left/Right, Home/End, Delete to close, and
Ctrl/Cmd+1–9 and Ctrl/Cmd+W. Shortcuts apply while focus is in the Console
chrome; terminal, text-input and embedded-page keyboard events stay with their
input surface. The overflow menu lists all tabs. Phones use a tab-count button and an
accessible two-column switcher with individual close controls.

Switching pages hides retained panels instead of unmounting their iframe/xterm,
keeping drafts, scroll and terminal connections. Overview stays mounted; the
most recently used **6 total pages on desktop / 4 on phones**, including Overview,
stay live. Excess workload pages unmount; selecting an unloaded tab reconnects
it and announces the reload. These limits bound retained browser instances, not
connections an embedded application chooses to open. Closing a tab disposes its
browser instance, never stops its machine workload. Closing the final workload
tab returns to Overview. Switching workspace/user disposes the old scope’s live
surfaces; registration/generation changes and explicit reload still rebuild the
relevant surface. Hidden terminals continue receiving output but never resize
the PTY to a zero-size container or take focus when they reconnect.

`treer:tabs:v1:<user_id>:<workspace_id>` stores only up to 64 stable IDs in tab
order. It contains no titles, URLs, tokens, draft text, terminal output or session
state. Refresh restores the list with Overview selected and all workload pages
cold, connecting only on explicit selection. Successful inventory reads prune
deleted IDs; invalid/oversized cache data is ignored. Storage access is guarded;
when unavailable, tabs operate in memory. App/Agent Interface sandbox and Proxy
permissions are unchanged; hidden iframes retain their existing trusted authority.
App settings’ explicit external URL controls still open a separate browser tab.

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
starts its own loopback Vite and local-only Wrangler processes under Playwright
management and never reuses an existing server. Set `PW_PORT` (1–65534) to select
a free consecutive port pair: Vite uses `PW_PORT`, the security Worker uses
`PW_PORT + 1`. An occupied port fails startup explicitly, even if another service
answers `/health` with HTTP 200. Both child processes are stopped by Playwright;
Worker state stays inside the throwaway HOME. Playwright browser binaries must be installed;
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

## Accessibility

- The workload tree is one Tab stop (roving tabindex). Arrow Up/Down, Home and
  End move between rows; Right expands a folder, enters it, or moves to a row's
  actions menu button; Left collapses, returns to the row, or goes to the parent
  folder. Actions buttons are reached with Right, not Tab.
- The error toast is a `role="alert"` live region portalled to `<body>` so it is
  announced and can be dismissed while a modal dialog is open.
- `prefers-reduced-motion: reduce` disables animation and shortens transitions.
- `pnpm run check:contrast` checks the HSL tokens in both themes against WCAG
  2.1 (4.5:1 text, 3:1 non-text). Documented exceptions: dark-theme blue and red
  as plain text reach 3:1 only, and input borders keep a 1.4:1 hairline floor.

## Internationalization

The console is available in English and Simplified Chinese with no new
dependency. Everything lives in `src/i18n/`:

- `en.ts` is the source dictionary. Keys are `screen.name`; values may contain
  `{name}` placeholders. `zh.ts` is typed `Record<keyof en, string>`, so a missing
  key fails `typecheck`; `e2e/i18n.spec.ts` also checks placeholders and HTML
  tags (tour texts) match.
- Call `t("key", { name })` anywhere in render code. `App` subscribes to the
  language once, so a switch re-renders the whole tree. Use `tnodes` to put
  React elements inside a sentence, `statusLabel`/`roleLabel` for server enums,
  and `intlLocale()` for `Intl`/`toLocale*` calls.
- Do not evaluate `t()` at module load (it would freeze the language). Use keys in
  constants and call `t(key)` when rendering.
- Language order: stored `treer-lang`, then `navigator.language`, then English.
  `document.documentElement.lang` follows, and embedded UIs receive `lang`.
- Terms follow the launcher UI (`launchers/agent/ui/src/i18n`): 工作中, 空闲,
  已阻塞, Agent, 工作区.
- Adding a string: add the key to `en.ts` and `zh.ts`, use `t()`. App UIs
  (Issues, Telegram, email) are not part of the console dictionary.


### Tab page actions

Agent/App pages use a single tab bar. The active desktop tab and mobile title
have a small ⋯ button for page actions. Right-click a tab (including an inactive
one), or press Shift+F10 / the ContextMenu key on it, for its page actions.
On phones, long-press the active title or a card in the tab switcher; moving a
finger cancels the long-press so scrolling still works. Menus support arrow
keys, Enter and Escape, and return focus to their originating tab. Agent status
stays on tabs; full paths appear in tab titles and menu headers.

The menu contains App/logs switching, rename, upload, interface reload or
terminal reconnect, stop and delete, plus close/close others/close to the right.
Overview stays pinned. A reload affects only that browser page, not the running
Agent. Stop asks for confirmation; delete keeps its existing confirmation.
Uploads pin the target machine from the selected menu, even when another page
is active, and stay disabled for offline machines. On phones, the menu also
contains full-screen opening and Back to workloads. App settings retain their
existing lifecycle actions and authorization. No menu grants additional access;
the Core API still authorizes mutations.

### Organization invitation entry

Opening `?invite=…` shows an organization preview even when the user is already
signed in. Join is an explicit action: existing users confirm after signing in;
new users confirm before opening the registration/OAuth form. Only that confirmed
organization flow sends `join_organization: true`. Core creates the account,
consumes the invitation and adds a member atomically; the console never supplies
a role or grants membership itself. Personal account-registration invitations
remain distinct and cannot be accepted as organization invitations.

The preview and accept JSON endpoints are `/api/invitations/preview` and
`/api/invitations/accept`. Preview must advertise
`organization.invitation-accept.v1`; missing support fails closed with an upgrade
notice, rather than falling back to the old registration behavior. Accept uses
the human session cookie and Core's CSRF checks, not an Agent/App service token.
All Proxy replicas must be upgraded before enabling this console flow; a mixed
fleet of old and new Proxies does not provide the new consent guarantees.

The console removes the invite query parameter with history replacement and
keeps pending intent in component memory, never localStorage/sessionStorage.
Invitation OAuth starts with a JSON POST (`invite_token` and the consent
flag) and `X-Treer-Console: 1`, never a URL token. Core returns an authorization
URL, which the console validates as HTTPS at the selected GitHub/Google origin
before navigation. Credential URLs, fragments and invitation query parameters
are rejected. Core retains the hash and consent in its browser/state-bound
OAuth record. On return, an existing user gets a session-bound pending invitation
through `GET /api/invitations/pending`; Join sends `{pending_id}` to accept, and
Cancel clears it with `DELETE /api/invitations/pending`. An absent pending entry
is 204. The console probes once per authenticated account; normal workspace
changes do not poll this endpoint. Core controls the short TTL and single-use
behavior. New OAuth accounts join atomically during account creation.

Worker CSP is unchanged (`form-action 'self'`). Tests intercept or cancel OAuth
navigation before network access and use synthetic API responses; they do not
perform real IdP login or claim complete external OAuth compatibility.
Preview and accept send tokens in JSON bodies and display only localized errors,
not raw server responses. Organization names render as text. Unknown/used tokens
share an unavailable message; the UI does not claim expiry or revocation states
that the server does not implement. Reloading a pending in-memory confirmation
requires reopening the invitation link. Invitation acceptance, invalid links,
cancellation, legacy-server rejection, personal invitations, keyboard access and
390px layouts are covered by hermetic Playwright mocks with synthetic tokens.
