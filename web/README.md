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

Agent and App rows are a single line: icon and name only. A small status dot
follows the name for non-idle states only (working/starting in the working colour;
blocked, failed and offline-machine in the attention colour; exited/stopped muted),
using the same colour tokens as the Overview chips. Idle, running (an App's normal
state) and unknown rows show no dot. Colour is never the only cue: the dot is an
`img` with a text name (so the row link's accessible name is e.g. "builder working"),
and row paths/statuses do not show hover or focus tooltips or native titles. The "⋯" button of Agent/App rows appears on row hover
or keyboard focus within the row, stays visible while its menu is open, and is
always visible on coarse pointers (phones and wide touch tablets) with its 44px target (rows, the folder/Agent/App "⋯" and the bottom user row all switch to 44px on `pointer: coarse`, not only below 768px); right-click
and Shift+F10 open the same menu. Folder rows use the same 36px height and the same
hover/focus/coarse-pointer "⋯" rule.

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

The desktop Tree sidebar is divided from the main area by a single 1px line. A
focusable vertical separator with a 24px invisible mouse/touch zone sits over it
(the line thickens and takes the accent colour on hover/focus). Drag to resize
immediately; Left/Right change 16px (Shift: 64px), Home/End select the limits,
and double-click restores 272px. The minimum is 240px; the maximum is the
smallest of 480px, 45% of the viewport and the space left after reserving 400px
for main content plus the 1px divider. Window resizing clamps the displayed width
while retaining the preferred width for larger windows.

The sidebar shows no user avatar. The bottom row is a **Collapse tree sidebar**
button (desktop only, in the old avatar slot), the user's name and the "⋯" user
menu (a slim 36px row on desktop with the same density as the Tree rows; 44px+ below 768px) (Settings, Replay product tour, Log out; the email is in the menu header).
**Cmd+B** (macOS) or **Ctrl+B** (Windows/Linux) toggles the sidebar like the collapse button, from anywhere in the Console on desktop layouts. It is not captured while focus is in a terminal (Ctrl+B is the tmux prefix) or a rich-text editable, ignores held/repeated keys and extra modifiers, does nothing below 768px or in focus mode, and only moves focus when it was inside the sidebar. The resize handle has no hover/focus tooltip; its accessible separator label and keyboard controls remain. Both collapse/expand buttons carry `aria-keyshortcuts` and the label in their tooltip. Collapsing hides the sidebar and the divider; a single **Expand tree sidebar**
button then appears at the left end of the tab bar. Both buttons expose
`aria-expanded` and `aria-controls="tree-sidebar"`, work with Enter/Space, and
focus moves to the counterpart button after a toggle. The original width is
restored on expansion, and the terminal is fitted after either toggle.
Preferences use `treer:tree-width:v1:<user_id>` and
`treer:tree-collapsed:v1:<user_id>` in localStorage across workspaces.
Unavailable storage falls back to the default on load and allows in-memory
resizing and toggling. Collapse defaults to off when not cached. Pointer capture and a temporary overlay protect drags across terminals
and embedded iframes; release/cancel/focus loss removes protection and fits the
terminal after layout commits. Divider colors use theme tokens.

The Tree list uses an overlay scrollbar (`thin-scroll`, `lib/thin-scroll.ts`):
hidden by default, a 4px thumb shows while the element scrolls and fades about
800ms after it stops. Wheel, touch and keyboard scrolling are unchanged; only a
`data-scrolling` attribute is toggled. Other scroll areas keep their native bars.

Below 768px the divider and the collapse/expand buttons are hidden, desktop
collapse is ignored, and the existing mobile layout stays intact: the bottom row is
just the name and the user-menu trigger (44px).
Managed App/Agent Interface embeds retain the same surrounding console sidebar.
Native iOS/Android clients are retired. Agent-owned embedded interfaces remain
separate from this responsive browser sidebar.

**Edit startup command** reads the current argv and saves it through one restart
operation. The editor scrolls on short mobile screens and preserves input after
a denied operation. Managed Apps have separate settings; their startup editing
requires the forthcoming App update API.

## Shared Overview pins

On Proxies advertising `workspace.pins.v1`, **Pin to Overview** / **Unpin** in
Tree context/⋯ menus and Overview row menus manage the workspace's shared list.
Members with Tree read access see the same pins; only members with Tree manage
access get write controls (`can_pin` is a display hint, and Core authorizes each
write). Up to 50 Agent, App and folder pins are stored in Core, newest first.
Stable IDs preserve pins across renames/moves; Apps use their App ID rather than
the runtime Agent ID. Deleted/unavailable items are omitted without browser-side
cleanup writes. Unknown or deleted pin authors display **Unknown member**.

The **Pinned** section appears above Overview filters, remains visible while
filtering, and disappears when empty. Pinned folders start collapsed and expand
independently of the regular Overview and sidebar. Rows retain normal navigation
and native new-tab links. Shared change signals on the existing workspace socket,
reconnection, focus and visibility restoration trigger bounded, coalesced reads;
there is no polling. Switching workspace/user discards late responses.

Writes are not optimistic or retried automatically. Failed reads disable writes;
a permission denial also clears the visible pins. Errors offer a read-only Retry
and distinguish the 50-pin limit from other write failures. Older Proxies hide
pin controls and the section; no localStorage substitute is used. This Web
consumer must ship with the corresponding Core API.

## Console composition

Workspaces open at **Overview** without attaching a PTY. The main view preserves
folder hierarchy and persisted sibling order; it never regroups Agents into flat
attention lists. Status badges and subtree counts overlay the Tree. Attention
rules retain the original fleet semantics: blocked/failed Agents and nonterminal Agents on offline/missing
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

## Managed App restart recovery

An App's current runtime generation and explicit tab reload revision each own a
fresh interface recovery budget. Before the initial iframe navigation, the
Console sends credentialed, uncached **HEAD** requests to the same fixed App UI
root (including its appearance parameters). It never guesses a health endpoint,
follows a probe redirect, falls back to GET, reads arbitrary App content, or
replays a user POST. A successful response, HEAD-not-supported 405, or manual
redirect means only that the route responded; the iframe still performs its
ordinary authenticated root GET and any normal navigation redirects.

Recovery makes at most eight HEAD attempts within 30 seconds, with a three-second
request timeout and backoff. Authentication refusals stop immediately; other
non-transient responses and an exhausted budget show an accessible error and
**Retry connection**. A hung iframe load also stops with an explicit retry.
Hidden workload tabs defer probes; already loaded normal Apps are not polled or
reloaded by appearance changes or later in-App navigation. A fresh user retry
starts a new finite budget. App identity/generation changes cancel old requests.

A matching Proxy additionally emits `treer.app-ui.unavailable` only for its own
pre-response transient tunnel failures on an initial root iframe GET. This closes
the HEAD-success / failed-GET race without inspecting an App's HTML or adding
scripts to successful pages. The parent checks the exact current iframe window,
Proxy origin, workspace and App IDs, and consumes the same recovery budget.
The bridge is a narrow startup signal, not an authentication or health authority;
trusted App code sharing that origin retains its existing authority. It does not
classify upstream App-generated errors or failures after response headers, and
older Proxies cannot report this final navigation race.

Console-to-Proxy probing requires the existing configured credentialed CORS and
normal workspace cookie contract. Missing CORS, cookies, or browser permission
is an unavailable check, never opaque `no-cors` success or an identity workaround.
The Agent isolated/compatibility iframe paths are unchanged.

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
When a workspace has no machines, the Tree also shows an always-visible setup
button linking to the same workspace settings and enrollment flow; it is hidden
in tour previews and disappears when a machine joins.
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
hint and accessible name. A visible hover/focus tooltip and status legend explain
dense maps. Labels stay at the top-left and progressively reduce from full path to name,
initials and status dot as space decreases. No button is smaller than 44×44px.
Small blocks remain visual markers: drill into their parent folder to enlarge
its children, or use the native **Go to** picker to open any immediate child
without targeting tiny blocks. This picker also keeps very dense, flat folders
fully keyboard accessible. Opening an Agent uses its stable ID and closes the
dialog. Single-child folder chains share one header with individually accessible path
segments (a native picker retains earlier segments when space is tight).
Folder headers drill down; breadcrumbs, an ancestor picker and the back
button return to ancestors. Navigation never changes sidebar expansion
preferences or sends mutations. Nested previews are bounded to 12 nested containers
and approximately 700 nodes; branches fold earlier when their child folder
headers cannot fit a touch target. Explicit drill-down resets that budget.
The dialog traps focus, supports native button keyboard actions and Escape, and
returns focus to its trigger on dismissal. It has no new library dependency.

The map adds no dependencies and keeps the existing single-file build and release
contract. The local Worker security test verifies the map under the unchanged
nonce CSP. See [the size/review notes](tree-map-report.md).

## Retained workspace tabs

On phones, the page top bar has a **Browse tree** button with a 44px touch
target. Below 768px every Tree sidebar control also has a 44px target: the
organization/workspace switchers and settings buttons, **Add to tree**, the row
"⋯" buttons, and every dropdown menu row (`max-md:` classes; desktop keeps the
compact sizes). The phone top bar (Browse tree, page title, "⋯", tab count) and
the Overview buttons and filter chips are 44px too. It opens the existing Tree directly from Overview, Agent/App pages or
settings, preserving selected workloads, retained tabs, terminals and iframe
drafts. It does not require visiting Overview first. Desktop navigation is
unchanged.

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
state. Refresh at the workspace URL restores the list with Overview selected and
all workload pages cold; a workload deep link restores that workload. Returning
to Overview clears the workload URL in the click handler, so an immediate refresh
or URL copy cannot select the workload just left. Successful inventory reads prune
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
management and never reuses an existing server. It picks a random free port
triple (Vite, security Worker, Worker inspector) unless `PW_PORT` (1-65533)
selects one: Vite uses `PW_PORT`, the Worker `PW_PORT + 1` and its inspector
`PW_PORT + 2`. An occupied port fails startup explicitly. Both child processes are stopped by Playwright;
Worker state stays inside the throwaway HOME. Playwright browser binaries must be installed;
`PLAYWRIGHT_BROWSERS_PATH` may point to an existing installation.

The same-name Agent and startup editor cases cover desktop and 390px viewports;
the long-command editor test writes desktop and mobile screenshots under the
ignored `test-results/` directory for visual inspection.

## Browser security boundary

The Cloudflare Worker runs before every static/SPA response. It adds a fresh
script nonce and CSP, denies framing, disables MIME sniffing and referrer
transmission, and marks HTML/config/health as `no-store`. For isolated Agent
frames, configure `TREER_INGRESS_PUBLIC_URL` to the Proxy's HTTPS ingress base
URL (canary: `https://canary.apps.treer.ai/`). Worker `frame-src` then adds only
`https://*.<configured base host>`; an unset value preserves the Proxy-only
frame policy, and an invalid configured URL fails closed with HTTP 503. This
wildcard includes nested hosts, so it is only a coarse hosting bound: the
Console still validates the exact Agent-ID origin before creating a frame.
Do not relax script, connection, frame-ancestor or sandbox rules to enable it. Do not cache rewritten
HTML at an upstream edge: its script nonce must match its CSP. Nginx adds the
anti-framing, MIME and referrer headers, but its static shell does not have the
Worker's nonce-based script restriction. These changes require a normal release;
source inspection and local Wrangler tests do not verify deployed Canary headers.

Managed App and legacy/compatibility Agent iframes remain trusted code with
Proxy-origin user authority. Isolated Agent embedding is described below.
Their sandbox is not a tenant or user-session security boundary. See the
[security review](../docs/research/2026-10-12-security-web.md).
Embedded Agent/App interfaces may open ordinary top-level browser pages using
`allow-popups allow-popups-to-escape-sandbox`, including their own full-page
view. Interface authors should use `noopener,noreferrer` with `window.open`
or `rel="noopener noreferrer"` on links; the Console cannot strip an opener
from arbitrary cross-origin frame scripts. Console-owned new-tab actions
continue to use `noopener,noreferrer`.

### Console opener isolation

The Worker and standalone nginx host set
`Cross-Origin-Opener-Policy: same-origin` on Console HTML, including SPA
fallbacks and HTML errors. JSON configuration, health and non-HTML assets do
not receive it. The matching Proxy `app_dist` header change must be released
with these hosts; never install this as blanket middleware on API, OAuth
callback, App ingress or Agent interface responses. A custom static host must
add the same response header to every Console document; an HTML meta tag is
not a substitute. Use HTTPS outside loopback development.

COOP separates cross-origin top-level opener relationships; it does not
restrict iframe access, replace the sandbox or Origin/CSRF checks, prevent
network requests, or grant `crossOriginIsolated`. No COEP header is added.
The same-origin credential-bearing iframe trust described above still applies.
Console GitHub/Google sign-in uses a same-window redirect and cookie-bound
callback, not popup messages; appearance messages target iframe windows.
Third-party interfaces that rely on an opener to communicate with the Console
must use independent navigation instead. Keep `noopener,noreferrer` on all
Console and interface-owned external opens even with COOP.

Report-only COOP does not enforce isolation. No reporting endpoint or URL
telemetry is configured. If an operator stages reporting, use a self-hosted
endpoint with URL/credential minimization, not a third-party collector.
Release to canary first and verify the document header, login return and popup
flows there. Roll back by restoring the previous immutable Console deployment
(and matching Proxy release for `app_dist`); do not relax CSP, CSRF or iframe
sandbox settings to repair an opener-dependent integration. See the
[COOP specification](https://html.spec.whatwg.org/multipage/browsers.html#cross-origin-opener-policies).

The security suite
starts its own local-only Wrangler and uses synthetic identities; build the
bundle before running `pnpm test:e2e security.spec.ts --workers=1`.

## Accessibility

- The workload tree is one Tab stop (roving tabindex). Arrow Up/Down, Home and
  End move between rows; Right expands a folder, enters it, or moves to a row's
  actions menu button; Left collapses, returns to the row, or goes to the parent
  folder. Actions buttons are reached with Right, not Tab.
- Right-clicking a Tree row (or Shift+F10 / the Menu key on the focused row) opens the same actions as its ⋯
  button at the pointer. For an Agent or App that includes **Open in new browser tab**, which opens the focus-mode
  page; Escape returns focus to the row. Right-click does not select or open the workload.
  The menu keeps the lifecycle actions (Start, Restart, Stop, Delete): the Tree is where they live, since the tab
  menu has none. Stop and Delete are last, after a separator, and both ask for confirmation first.
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

The menu contains App/logs switching, rename, upload, open in a new browser tab,
interface reload or terminal reconnect, plus close/close others/close to the
right. It deliberately has no Stop or Delete: those are destructive and a stray
click in a tab strip is too easy; use the Tree row menu or the App settings page.
Overview stays pinned. A reload affects only that browser page, not the running
Agent. Uploads pin the target machine from the selected menu, even when another page
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

## Platform administration privacy

The `/admin` resource overview shows platform-wide user, organization, machine
and Agent **counts only**. Cards do not expand and the console does not request
user/member profiles, machine hostnames, Agent paths, organization details,
password-reset links, session-revocation controls or per-resource activity.
Platform invitation management and control-plane updates remain separate
operator actions. Organization/workspace Audit remains scoped to its authorized
members. This UI change alone does not restrict a previous Proxy's admin APIs;
server-side access and retention guarantees require the corresponding Core
change and deployment.

Platform traffic uses `/api/admin/traffic?hours=6|24|168|720`, authenticated
with the separate platform-admin cookie session. It charts the combined
database-recorded usage of **all organizations**, using the same UTC hourly/daily
windows and byte/frame semantics as workspace Audit. It requests no individual
organization, machine, Agent or route inventories. The view refreshes every
10 seconds while open; refreshes are not concurrent and stop on logout. Current
hours/edge days are partial, database flushes can lag by about 10 seconds, and
unrecorded windows are shown as zero without asserting that no traffic occurred.
Direct traffic is machine-reported and does not contribute frame counts.
An older Proxy shows an explicit unsupported notice; errors clear stale totals
and offer retry, without displaying server response bodies or falling back to
private inventories. No new dependencies or build changes are required.

Invitation lists accept only the privacy-preserving summary contract: a
non-bearer `invitation_id`, up to eight characters of `token_prefix` (or the fixed `[redacted]` placeholder
for short/legacy tokens), creation
time and used state (up to the latest 100 entries). Existing invitation URLs
are not shown or copied; used invitations cannot be revoked. Revocation sends
only the non-bearer handle. An older list schema is refused with an upgrade
notice. Creating a new invitation still displays its link in a temporary dialog
for distribution, without writing it to browser storage.


## Cookie mutation compatibility

All Console provider sign-ins start with JSON POST to
`/api/auth/oauth/{provider}/start`, including ordinary sign-in without an
invitation. Navigation uses the returned, validated GitHub/Google authorization
URL. A rejected or unsupported POST stays on the sign-in screen with a clear
error; there is no GET fallback. Invitation consent and its POST-only body are
unchanged. Admin **Check for updates** also uses POST; viewing update status
remains a read-only GET. Browser cookies and Origin are managed by the browser;
the Console does not set the native-client header or use a service token.

These client changes must land before Core disables the old OAuth-start and
update-check GET handlers. Deploy the client with the corresponding Core POST
support; do not publish a client-only candidate whose Proxy lacks update-check POST. Core owns CSRF checks and documented OAuth exceptions;
changing the client method alone is not a server-side CSRF guarantee.

## Workspace audit

Workspace settings → Audit reads `/api/workspaces/{workspace_id}/audit-events`
when `/api/auth/config` advertises `workspace.audit.v1`. It uses the existing
browser session, `cache: no-store`, exact workspace attribution, and server-side
filters: inclusive `since`, exclusive `until` (RFC3339), exact `actor_id`,
`action`, `resource_kind`, and 50-event pages with an exclusive `before` cursor.
Times entered in the form are local time and converted to UTC; empty bounds
include all available history. A bounded time range must not exceed 366 days.
The exact-input filters also offer keyboard-accessible menus: already-visible
Agent tree paths and member names resolve to stable IDs; common action/kind
codes and codes on the current validated page are suggestions, not a complete
catalog. Each menu is capped at 200 valid suggestions; exact input supports
unknown identities and newer action/kind codes. Suggestion menus issue no extra
directory or event-list requests before Apply (apart from explicit paging/refresh).
Changing filters resets pagination; switching workspaces or leaving the tab
aborts pending requests and discards the displayed feed. There is no polling.

Workspace owners are allowed by default, subject to Policy denial. Other
principals require an explicit Policy grant; the UI does not infer permission
from organization membership or hide authorization errors. A denied request
shows a clear refusal. Missing capability or HTTP 404/405 shows unsupported,
without falling back to organization audit. Organization audit remains separate.

The shared event list resolves known Agent IDs to full tree paths and renders
text only. Workspace rows explicitly show “by” the actor (Agent path, a name from
the already-loaded member directory, or the stable ID), a resource-type icon and
a colored outcome badge. Secondary row text is at least 12px. On phones the
filters are collapsed by default behind an accessible Filters button. No extra
member-directory request is introduced for audit. The event-list reader validates
scope and projects display fields; credentials, arbitrary payloads, server-supplied
display names and raw server error bodies are not shown in event rows.
Coverage is incomplete, missing historical attribution is not backfilled, and
reads are not themselves audited. Workspace deletion can remove attribution.
This is an operational log, not a complete or tamper-proof ledger; see the Core
[security model](../docs/security.md) and [operator guide](../docs/operator-guide.md).

## Deep links and new browser tabs

Every Agent and App has a stable Console URL: `/orgs/<org>/workspaces/<workspace>/workloads/<id>`, where `<id>`
is the stable Agent ID (an App uses its runtime Agent ID), never the path. The address bar follows the selected
workload (replace, no history entry), so it can be copied, reloaded and shared; a member without access sees
"not found, or you do not have access" and nothing else is selected. The same applies when the organization or workspace in the link is unavailable (no default workspace is substituted) and when the `/workloads/` segment is malformed. URLs carry IDs only, never tokens, and the
new page is the same Console origin with the same session. The iframe sandbox and CSRF rules are unchanged.

`?focus=1` is focus mode: only that workload fills the window, with a narrow bar to open the full Console. Returning to the full Console preserves the live workload; deep-link initialization commits the workload selection before synchronizing the URL, so a stale empty selection cannot erase the target and later restore an old focus query. Plain
links open the full Console. **Open in new browser tab** (tab menu, and the phone menu) opens the focus-mode URL with
`window.open(..., "noopener,noreferrer")` when no standalone UI is registered.

Tree rows, Overview rows and map blocks are real links (`<a href>` to the deep link). A plain left click opens the
workload in place; ctrl/cmd/shift-click, middle-click and the browser's own "Open in new tab" and "Copy link address"
use the href natively, with the standalone UI exceptions below. A link is never nested inside another
interactive element (a row's actions button is a sibling), and Space activates it like a button.

An App that the Proxy ingress routes (its server-provided `public_url`, e.g. `https://svc-….<apps domain>/`) opens
on that URL instead of the Console deep link wherever a new browser tab is opened: the tab/Tree menu item, the
native link behind a Tree row, Overview row or map block (ctrl/cmd/middle-click, "Open in new tab"). The ingress URL
is workspace-authenticated, so no token is added. The URL is never built from the name and is used only if it is
`https:`, has no credentials, is a real DNS name (no IP literal, `localhost` or single label) and is neither the
Console nor the Proxy origin; otherwise the Console deep link is used. A plain left click still opens the App inside
the Console. The server fills `public_url` only for the platform-hosted ingress hostname it computes from its configured
base domain (it is not user-settable), so the Console does not pin the host to a domain itself; the checks above are
defense in depth. Menu opens use `noopener,noreferrer` so the Console URL is not sent as a referrer.

When the Proxy advertises `agent.interface-origin.v1`, Agent native links and new-tab menus use its
authorized snapshot `interface.origin_url` synchronously (`https://ag-<id>.<apps domain>/`). The URL must
be an HTTPS origin root without credentials, query or fragment, distinct from the Console and Proxy;
its `ag-<32 hex>` hostname label must match the selected Agent ID.
Missing/invalid URLs and unavailable capability discovery keep the Console detail destination. Origin
failures never silently downgrade to the Proxy UI. The standalone host owns its fixed error/retry page;
the Console's embedded iframe stays on the Proxy tunnel (embedded isolation is tracked separately).
Inventory events cannot supply origin grants: ordinary updates retain a read-model URL only for the
same registered interface. Interface appearance/replacement triggers a 200ms debounced snapshot refresh
(maximum debounce wait 2s), subject to a 5s floor between origin-driven reads. Sustained churn doubles
that floor up to 60s; 30s without signals resets it to 5s. Returning to the page uses the same bounded refresh. Removed/replaced interfaces drop their old URL
immediately; a fresh authorized snapshot can also revoke a URL. No high-frequency polling or persisted
origin cache is used, and account/workspace changes discard the grants.

Only when the capability is confirmed absent (including an older Proxy's config 404),
an Agent with a registered `interface.ui_path` opens its UI directly in a new tab at the fixed Proxy route
`/api/workspaces/{workspace_id}/agents/{agent_id}/interface/ui/`, without a Console shell. The descriptor path
is never used as an external URL; IDs are encoded. Agent interfaces without a UI retain the Console deep link.
Plain clicks still open in place. Native links and menu opens use `noopener,noreferrer`, even when Console
and Proxy share an origin. Appearance uses theme/look/lang query hints; framed URLs also specify the exact
Console `embed_origin` for the launcher parent-message check. These hints grant no authority and carry no tokens.
Proxy session cookies remain required; an unauthenticated or cross-site-cookie-blocked request must fail closed,
never fall back to a URL token. The launcher top-level response must retain its restrictive Permissions-Policy,
Referrer-Policy and CSP through the Proxy tunnel. Top-level pages do not have the iframe sandbox restrictions;
legacy standalone and embedded pages share the same origin and authority. This
legacy path remains trusted workspace UI on the Proxy origin. Deployments rejected by Core's separate-origin check
return `503 embedded_ui_requires_separate_app_origin` at this route, including top-level navigation; opening
a new tab cannot bypass this restriction. After the user opens an online Agent, Console performs a bounded, read-only probe of its fixed
interface root (no appearance query). Results are scoped per user/workspace/Proxy/Agent, cached in
memory for 60 seconds with at most 64 entries and cleared on session changes or logout; unselected Agents are never probed. Only an explicit 503 JSON error with that code
switches only that Agent’s links and new-tab menus to the Console deep link; Apps retain their own URLs. Before the
probe finishes, or on an unrelated error, the normal standalone URL remains. The probe does not follow
redirects, execute UI bytes, or retry. This is a navigation fallback only: returning to Console does not
restore a refused embedded UI. No token fallback is attempted; Core's embedded-UI origin and frame
policies remain authoritative.

### Isolated embedded Agent interfaces

With `agent.interface-embed.v1`, Console loads the Proxy-owned `/.treer/embed`
bootstrap on the Agent origin from the authenticated inventory. It accepts one
strict ready message per assigned iframe source (exact origin and WindowProxy,
64-character nonce hash), then mints a 30-second authorization and posts the code
only to that origin. The code stays out of URLs, storage, DOM attributes and logs.
The bootstrap redeems it for a host-only partitioned session and replaces itself
with the Agent UI. The existing sandbox grants are unchanged. Tab hiding and
appearance changes preserve the frame; reload, interface replacement, origin
revocation and workspace/account changes cancel its handshake. The handshake
and UI-load deadline is 20 seconds, including hidden tabs; a slow or backgrounded
load that misses it fails closed with a timeout message and can be retried by
reloading the tab. A successfully loaded frame stays mounted while hidden.

A cookie failure must match the accepted bootstrap hash within the handshake
window. It never automatically switches to the shared-origin Proxy tunnel: after
a bounded read-only probe confirms the deployment permits it, a visible notice
requires the user to choose **Load compatibility view (shared origin)**. With
`TREER_AGENT_EMBED_FALLBACK=refuse`, only the safe standalone link is offered.
Permission errors, malformed replies, unknown capabilities and timeouts fail
closed. Older Proxies with a successful config lacking the capability (or config
404) retain their legacy embedding behavior.

Partitioned cookies are keyed by the top-level **site**, not origin; they do not
isolate same-site Console and Apps by themselves. Core's host-only cookie,
Fetch Metadata checks and exact Console framing policy remain required.
Managed Apps are outside this Agent-only handshake and keep their Proxy-origin
tunnel, with a visible shared-origin notice; a later App contract is required.
Browser tests exercise genuine cross-origin frames with synthetic mint/bootstrap
responses. They verify the Console contract, not live partitioned-cookie support
or the Core's authorization/revocation implementation.

Creating an Agent from Overview starts at the workspace root and shows no Tree folder hint.
Creating from a folder in the Tree shows that folder’s full path and reserves placement there.

### Reserved Agent identities

Creation errors retain optional Core `code`/`details`; the Console never infers a
provider or cleanup outcome from error text. Structured `command_not_found`
with a validated bare command and matching machine displays “command is not
installed on machine”, never the raw PATH or argv. A proven removed identity
allows manual submission after fixing the command; unconfirmed identities remain
blocked. The Console never installs provider CLIs or displays an install button. If Core reports an unconfirmed or
failed reservation cleanup, creation remains blocked and the draft is preserved.
The explicit **Clean up reserved identity** confirmation uses the human-session
reservation cleanup endpoint, with existing `agent.delete` Policy; Core rechecks
ownership and absence and fences late creates before removing the reservation.
Old or offline Controllers and observed workloads fail visibly without claiming
cleanup. Successful cleanup enables an explicit new submission; it never creates
an Agent automatically. This requires the Core reservation-cleanup capability;
the Web UI alone does not provide it.

Choosing a provider fetches Core's observation, current and pinned target versions,
and authoritative action: install, replace or noop. A same-version noop has no
token or install button and says **self-reported version**, not verified integrity.
Replacement warnings distinguish upgrade, downgrade, unreadable version and
incomparable versions. The last three require an extra explicit checkbox;
installation sends only the token and its matching `acknowledged_relation`.
Cancellation or a new preparation resets that acknowledgement. Core remains
responsible for version comparison and binding the observation into consent.
Web does not independently classify downgrades or compare semver; it gates
acknowledgement exclusively on Core's authoritative `version_relation`.

For a mutation, prepare fetches a fresh, 120-second confirmation. The database clock owns expiry; the browser uses a relative display timeout so local wall-clock skew does not reject a fresh token. The dialog displays
Core's complete ordered plan, integrity digest, target machine/platform and
network/global-prefix effects. The browser sends recipe ID/version/hash to
prepare/check and the in-memory confirmation token (plus the required acknowledgement) to install. Cancellation
or closing discards the token; neither tokens nor output enter browser storage,
URLs or logs. Output is React text, never HTML or terminal input. Polling is
read-only, once per second while viewed, and stops at terminal/unknown outcomes.
Closing does not cancel the server job; reopening resumes observation of its
in-memory operation ID. An explicit status refresh can resolve an unknown result;
installation requests are never automatically replayed. Page reload/navigation
loses that local operation reference, so inspect the machine before another
installation. No browser operation-history endpoint is provided in this version.
If Core detects installation state changed after consent, the dialog compares
the confirmed observation with the fresh path-free summary, and offers an
explicit **Review new plan** action. It never automatically prepares or installs
again. Core performs the checks; the browser cannot guarantee the absence of a
race between a final observation and the package manager writing to disk.

Success means the version probe in npm's global prefix passed. It does **not**
prove that directory is on the Controller PATH. Check the installed version to
obtain the absolute executable for a profile, or have the operator configure the
service environment; Treer does not silently edit PATH or elevate permissions.
Login remains manual. The creation draft stays intact and Create requires another
explicit submission. Treer ignores npm user configuration and uses npm's default
prefix; a non-writable prefix fails visibly. The top-level tarball is pinned and
integrity-checked, but transitive dependencies are not locked. Any enabled npm
lifecycle scripts run across the resolved dependency closure, not just the
top-level package. Initial release data supports Codex/Gemini on POSIX; Claude,
OpenCode, Cursor, Grok and Windows stay unavailable until their separate review.

Browser acceptance uses fake API responses only. Real package functionality must
be verified separately in the sealed environment selected by release/security;
Web tests never install packages on a live machine.

Installation results show the outcome and latest version check first, with recovery guidance once and diagnostic output in expandable details. Terminal control bytes in that output are shown as literal escapes.

Pin errors are announced with `role="alert"` and the existing error theme. Retry pins is a read-only
refresh offered for network/server failures and transient HTTP 408/425/429, never for 403 permission denial or the 409 pin limit.

Machine **Upload file** supports files up to 128 MiB when the Proxy and machine
both advertise `machine.upload.v3`. The Console sends raw sequential parts,
starting at 1 MiB and adapting to measured throughput (up to the server's 8 MiB
maximum). SHA-256 is computed per part using Web Crypto; the whole file is never
read into a single buffer on this path. HTTPS (or a browser-trusted localhost)
is required for Web Crypto. Older versions retain the base64 upload capped at
16 MiB. If a successful machine inventory omits its capability field while the
Proxy advertises resumable upload, the dialog offers an explicit **Use compatibility
upload (up to 16 MiB)** choice; it never selects this automatically. Retrying the
check can discover support after the machine inventory is corrected. A failed
network or permission check does not offer compatibility upload, and an upload
already begun with resumable parts never downgrades.

The dialog reports overall bytes sent, then **Finishing upload** until the
machine confirms publication. Temporary transport failures have three bounded
retries; service rate limiting honors Retry-After with a separate finite budget.
A lost begin response reuses its idempotency key; after a lost part response the
Console asks for the machine-confirmed offset before continuing. **Resume
upload** keeps the same file and upload handle in memory. No file, key, handle or
credential is saved to browser storage, and resume does not survive a reload.
Keep the dialog open to resume. Leaving the account/workspace or unmounting the
dialog aborts local requests and fences old results; server expiry handles
abandoned temporary files rather than sending a cleanup request with a new
account's cookies.

**Cancel upload** stops the request and asks the server to remove the temporary
file. It reports cancellation only after cleanup is acknowledged; unreachable
machines or a busy cleanup report **cleanup pending**. Cancellation is disabled
once commit starts. A dropped commit response is resolved using the same upload's
status/receipt, never by interpreting an existing destination file as success.
An expired or forgotten receipt reported as `unknown` / `outcome_unknown` keeps
the result uncertain: check the file on the machine before uploading again; it
is not proof that publication failed. Upload handles accept `upl_` plus 32–64
lowercase hex characters (Core currently generates 64). A rejected begin handle
that still matches the Controller's bounded identifier syntax gets a scoped
best-effort abort; unsafe strings are never used as request paths.
See the operator guide for temporary-file visibility, expiry and server quotas.

Iframe appearance hints are included in the initial URL. Exact-origin theme
messages start after that iframe loads, and continue on later theme changes;
new iframe instances wait for their own load event.

### Aggregate activity charts

When `workspace.audit-summary.v1` is advertised, Workspace settings → Audit also
requests the authorized `/api/workspaces/{workspace_id}/audit-summary` endpoint.
The charts consume bounded server aggregates, never a downloaded event feed:
24h/7d/30d presets or a custom local-time interval of at most 180 days. Bucket
labels use UTC. Existing applied From/Until list filters override preset bounds;
custom chart bounds take precedence. Refresh is explicit; there is no polling.

The view validates workspace, table version 1, echoed filters and range,
contiguous clipped buckets, counts and totals before displaying them. Missing
capability leaves the existing event list available; malformed or failed summary
responses show an explicit error without partial aggregates or server error text.
Incomplete coverage withholds KPI totals and warns that zeros do not prove no
activity. Fixed category totals include Other; if one category exceeds 70%,
compact previews of non-zero categories show the full interval before the combined
chart (zero categories remain listed with totals), with
a warning against comparing heights across independent scales. The combined
chart starts at the newest buckets and supports scrolling and Home/End navigation.
Eight categorical chart tokens (not status colors) pass 3:1 against the card in
both themes; the contrast script checks them. Patterned legend swatches, distinct
notable/anomaly markers, visible bucket totals, text counts, exact data
tables, UTC labels and keyboard-operable bucket buttons supplement color.

Selecting a category, bucket, actor, resource or notable event resets list paging
and uses server-echoed inclusive/exclusive time bounds and exact filter IDs.
The list heading receives focus and identifies the selected range. Clear chart
selection restores the applied list filters; changing the chart range clears the
selection. Rankings disclose distinct and omitted counts per resource kind.
Non-drillable IDs remain visible in data tables. Display names are labelled as
**current names**, not historical event-time names. No new directory calls are
introduced. Notable activity labels its per-action caps instead of promising a
complete newest-100 list; anomaly explanations use server baseline/threshold
values and explicitly identify insufficient data. This remains an operational,
incomplete audit view, not a tamper-proof ledger. Shipping requires the #191/#192
Core API plus UI review and real-browser acceptance against deployed headers.


On phones, opening the Tree while a deep link is still waiting for workspace
inventory takes precedence over automatic terminal display. Once inventory
arrives, the linked workload remains selected, but the Tree stays open until
the user explicitly opens a workload. A delayed-inventory browser test covers
this startup ordering.
### Local embed timing diagnostic

`node web/scripts/measure-embed.mjs` measures the existing isolated-interface
handshake against the disposable canary launcher fixture. It injects local
fixed-label Performance marks through Playwright; it does not change deployed
code, replace responses, relax headers, or send telemetry. Use only the fixture,
never a user's Agent. The fixture operator supplies a temporary mode-0600
storage-state file through `EMBED_STORAGE_STATE` and removes it afterwards.
Never put credentials in command arguments, reports, traces, or HAR files.

Required environment variables are `EMBED_PLAYWRIGHT_FROM` (the installed
Playwright checkout's `web/package.json`), `EMBED_CONSOLE_URL`,
`EMBED_PROXY_URL`, `EMBED_AGENT_URL` (the Console fixture deep link),
`EMBED_AGENT_ORIGIN`, `EMBED_DEPLOYED_SHA`, `EMBED_STORAGE_STATE`, and
`EMBED_REPORT_FILE` (an untracked local output). `EMBED_ENGINES` defaults to
`chromium,firefox,webkit`; `EMBED_PAIRS` defaults to 3, bounded to 1–5. Run this
small diagnostic outside a full gate and coordinate with the fixture operator
to avoid overlapping mint bursts.

`EMBED_ALLOWED_HOST_SUFFIXES` is also required: an explicit comma-separated
allowlist of fixture DNS suffixes with at least three labels (for example
`canary.apps.treer.ai`). Only an `ag-<32 hex digits>` host directly below one
of these suffixes is accepted. `EMBED_FIXTURE_AGENT_HOST` must also equal the
exact host derived from the operator's fixture state file, and is compared to
the requested origin. The operator verifies that fixture identity.
The report includes the executed runner's SHA-256 and exact deployed SHA;
release pins that hash with the measurement evidence.

Each pair launches a fresh browser and opens the fixture twice in one context.
“Cold” means browser-process/context cold, not flushed OS DNS or upstream
caches; “warm” retains that context's cookie jar and connection pool. Timing
zero is the bootstrap navigation request. Request timing exposes bootstrap,
mint, session exchange, cookie check and UI document stages, including DNS,
connection and TLS fields. A value of -1 means unavailable or a reused phase,
not a measured zero. Browser paint entries are reported when available;
fixture composer visibility is a separately labelled readiness measurement,
not a substitute for missing paint entries. Marks also cover bootstrap script
start/load, ready reception and code reception. Millisecond precision and
injection overhead limit interpretation of sub-millisecond differences.

The report has fixed endpoint/stage labels, engine/version, run/temperature,
status codes and numeric offsets/durations only. It omits URLs, request bodies,
headers, cookies, codes, nonce/hash values and workspace/user/Agent IDs. No
session reuse or hover prewarming is enabled by this diagnostic. Real-browser
acceptance remains required before shipping subsequent handshake changes.

The Console hints `reuse=1` when opening the Proxy-owned bootstrap; it never
loads the Agent UI directly from the Console origin or caches an authorization
decision. A version-2 bootstrap announces `ready` with its nonce hash and
`version: 2`. The Console binds that first hash and replies with `probe` carrying
the same hash/version. Only one matching `mint` or `reused` outcome is accepted
from the exact frame window and Agent origin. A `mint` outcome runs the existing
nonce-bound code handoff once; `reused` only changes the loading notice, while
the UI request still enforces authorization. Old two-field `ready` messages
continue to mint immediately. Unknown versions and replayed outcomes are
ignored. After a code handoff, the existing nonce-bound `cookies_blocked`
failure remains accepted until the UI loads, preserving the compatibility choice or operator
refusal. After UI load all handshake controls are ignored. Tab visibility and
appearance updates do not run the handshake again; scope/interface changes
replace the frame and cancel its pending requests as before.
After a successful `reused` outcome every later control is ignored immediately.

### Bounded Agent interface prewarm

Only a Proxy advertising `agent.interface-embed-prewarm.v1` can receive a hidden
prewarm bootstrap. Existing embed capability alone does not enable it: older
Proxies would ignore the prewarm hint and load Agent HTML in the background.
A 400 ms mouse hover or deliberate Tab focus on an accessible, online Tree
Agent may start the bootstrap; dragging with a mouse button held, arrow
navigation, touch hover emulation, hidden
documents, Save-Data, Apps, and workloads already open in a tab are excluded.
There are at most two concurrent attempts per workspace, ten hidden bootstrap
starts per minute per account in the current Console document (even without
a mint), a 60 s per-Agent
cooldown, and six background mint attempts per minute per authenticated account
in the current Console document. The Proxy must also enforce the purpose-specific six/minute limit per user
across tabs and devices, alongside its unchanged global 30/minute and
eight-outstanding limits; the local budget is a courtesy, not enforcement.

Each inert, hidden frame has a separate source/origin/hash-bound v2 state machine.
It loads `/.treer/embed?reuse=1&prewarm=1`, sends a purpose-bound prewarm mint only
after its own ready/probe/mint sequence, and stops at the terminal `warmed`
message. It never loads Agent HTML. Failures, denials and timeouts are silent
and do not retry or offer compatibility. Leaving/blurring the row, starting a
foreground click, logout, workspace/interface generation changes, visibility or
Save-Data changes remove the frame/listener and abort its request. Cancellation
after exchange can leave only the Core's short-lived unused session.

The click always uses the normal fresh session check; there is no positive
authorization cache. Core must implement the purpose-bound 10-minute session,
document-only one-time rotation, bounded session rows, and prewarm audit label
before advertising the capability. New tabs already start their foreground
handshake; they do not mint an additional background session. Real-browser
acceptance of the final Core/Web union remains required.

### Explicit prepared Claude target (#247)

The existing Create Agent dialog has an opt-in **Prepared Claude target** launch
mode. It reuses machine, Tree folder, name and ordinary command creation. Supply
the trusted installed launcher entry, physical absolute prepared directory and
fresh UUID from the prepared plan; the working directory is machine-root-relative
and must lexically match that directory. This UI currently supports normalized
Unix paths. Native code remains responsible for physical-path/plan/private-root
validation; browser checks are not proof a target exists or authentication works.

The explicit acknowledgement binds the selected machine, root, paths and UUID.
The only generated argv is `ENTRY session run --directory DIRECTORY --session-id
UUID --auth environment`; there is no credential form, environment-source option,
shell interpolation, automatic fallback or restart endpoint. A sent creation locks
this prepared mode against another submission during the current page lifetime,
including unknown outcomes; inspect the reserved Agent and existing reservation
recovery rather than retrying blindly. Success states process creation/startup
saved only, while ordinary snapshots establish subsequent interface readiness.
This mode is not an export/import wizard and does not advertise sessionTransfer.

The prepared Claude Create mode can copy an idle Claude session through its
normal authenticated interface tunnel. Select the source, a trusted absolute
launcher entry, a new target working directory, and an existing owner-only
(0700) staging directory relative to the machine root. Explicitly acknowledge
that the sensitive snapshot remains there for manual cleanup. No credentials
are transferred; target environment authentication must already be configured.

The browser bounds the exported bytes to 16 MiB, hashes the original bytes,
uploads once with a new UUID filename and `overwrite:false`, then invokes fixed
`session inspect` argv with the file/hash/target/new UUID. Only a matching native
receipt unlocks explicit `session prepare`, with the same binding. This uses
existing authenticated file/exec APIs; it does not pass session contents in
argv, use a shell, or add exec stdin support. The native implementation enforces
file/directory custody and validates/re-writes the complete allowlisted bundle.
The browser does not claim to verify physical permissions or provider auth.

A matching prepared receipt fills the existing normal Create confirmation.
Create is issued once and preserves reservation recovery on unknown outcomes.
Subsequent interface health/state/status reads can verify the new Agent and
session are ready; process creation alone is not readiness. Unknown upload,
validation or preparation outcomes stop the page's flow, even across dialog
close/reopen; no automatic resend, cleanup or rollback is attempted. Changing
or closing the target before upload prevents late snapshot responses from
submitting a stale upload. `sessionTransfer` capability remains null.

### Controller update

`controller-updater.tsx` sits beside Install agent CLIs on the machine view
when the machine advertises `machine.controller-update.v2`. It POSTs
`{"version":"vX.Y.Z"}` to `/api/workspaces/{ws}/machines/{id}/controller-update`
and polls the same path every two seconds while the state is `downloading`,
`installing` or `restarting`; `done` and `failed` end polling. Older
Controllers show the one-time manual upgrade hint instead.

### Target authentication for Claude session transfer

The existing prepared-target Create flow explicitly selects `environment` (the
unchanged default) or `owner-default`. The latter uses the target owner's normal
Claude configuration, login, hooks and settings without isolation; the launcher
environment must leave `CLAUDE_CONFIG_DIR` unset. The Console neither reads nor
copies credentials. Source export stays authenticated and idle-bound. Owner-default explicitly sends
`{threadId, auth:"owner-default"}` and requires matching `runtimeSnapshot.auth`;
the default environment mode keeps the legacy `{threadId}` body. A missing or
mismatched owner-default snapshot marker refuses upload. Native alone enforces
that the source launched against the default owner root; the UI does not switch
roots or read configuration files.

Authentication is bound into staging/prepare acknowledgements, the scope key and
the final Create acknowledgement. The selector locks when the single snapshot
flow starts. Owner-default inspect/prepare add fixed `--auth owner-default`;
Create uses the same mode in its normal `session run` argv. Its receipt must
explicitly identify that mode, contain an empty environment, and bind normalized
absolute owner-project and publication-intent paths to the target cwd/session.
The owner-project suffix is `/projects/<exact UTF-16 cwd key>`; its parent
need not be named `.claude` because native canonicalizes an existing owner
symlink. Native owns HOME/root/fd validation; the browser does not infer HOME.
Both paths and file/byte counts must stay identical from inspect to prepare;
owner-default allows the additional marker (1026 files). The UI shows these two
publication locations. Partial/unknown results keep the existing no-retry latch
and normal reservation recovery; Create success alone still does not mean ready.

Tree header **+ → Add machine** opens the existing workspace install/enrollment
dialog through `openInstall`, just like Workspace settings. It remains available
with zero or offline machines; Add folder and Add agent keep their existing
behavior. Opening the dialog uses the existing bootstrap request only and does
not install or start anything. `tree-add-machine.spec.ts` covers this entry in
English/Chinese at desktop/390px with online, offline and empty inventory, plus
keyboard selection and Escape focus return. These are closed browser controls,
not actual machine enrollment or deployed acceptance.


### Tree row and App subtree integration (#256)

The primary link/button fills each painted Agent/App/folder row; a sibling
positioned menu preserves native links, drag targets and keyboard actions without
nesting controls. Current no-tooltip behavior and Tree Add machine are retained.
An App can lazily expose its own immediate app/agent children through authenticated
`/api/workspaces/:workspace/apps/:app/ui/api/tree?path=...`. Nodes have stable
single-segment keys and explicit `tree.read`, `message.read`, `message.send`
capabilities; internal nodes are App-owned, not Host processes. Pages are bounded
to 64 KiB, 100 children and depth 8, with explicit loading/empty/denied/unavailable
states. Full endpoint paths are rooted at the owning App; no App-supplied URLs are
followed. Deep links use the owner workload plus `app_node`, revalidate each level,
and clear on workspace/normal-workload navigation. Auth/workspace changes reset
cached branches and conversations.

Conversations use existing Core Message routes, filter owner plus full endpoint,
show the parent and child sender identity, and retain an immutable body, recipient,
context and idempotency key after an uncertain send. They do not create a second
inbox or claim message acceptance is final delivery.
