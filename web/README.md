# Treer web Console

The Console is the standalone browser control plane for Treer: a React 19 +
Vite single-page app that talks to one Treer Proxy over its HTTP/WebSocket API.
It shows a workspace's Tree of Agents, Managed Apps and folders, opens each
workload's terminal or embedded interface, and covers machine, workspace,
organization and platform-admin settings. It holds no server state of its own;
the Proxy (Core) authorizes every read and mutation.

UI conventions and the review checklist for changing a screen live in
[the Treer UI skill](../skills/treer-ui/SKILL.md).

## Source layout

| Path | Contents |
| --- | --- |
| `src/main.tsx`, `src/App.tsx` | Loads `/config.json`, applies the stored theme, mounts the router (`/admin`, `/orgs/:org/workspaces/:ws/*`) |
| `src/components/workspace-app.tsx` | Workspace shell: selection, views, user actions; dialogs in `workspace-dialog-state.ts` / `workspace-dialogs.tsx`, live data in `use-workspace-*.ts` |
| `src/components/workload-tree.tsx` | Sidebar Tree, row menus, drag/move, keyboard navigation |
| `src/components/workspace-overview.tsx`, `workload-treemap.tsx` | Overview (filters, pins) and the read-only workspace map |
| `src/components/workload-surface.tsx`, `workspace-tab-bar.tsx` | Retained workload tabs and their terminal / interface surfaces |
| `src/components/terminal-pane.tsx` | xterm.js terminal over the Proxy PTY stream |
| `src/components/isolated-agent-interface.tsx`, `use-agent-*.ts` | Agent Interface embed handshake, prewarm and fallbacks |
| `src/components/app-interface.tsx`, `app-owned-tree.tsx`, `app-node-*.tsx` | Managed App embeds and App-owned subtrees |
| `src/components/machine-view.tsx`, `machine-upload-dialog.tsx`, `controller-updater.tsx` | Machine detail, file upload, Controller update |
| `src/components/*settings-view.tsx`, `workspace-audit*.tsx`, `policy-publish-authority.tsx` | Workspace / organization settings, audit, Policy Provider |
| `src/components/admin-panel.tsx`, `admin-traffic.tsx` | Platform admin at `/admin` |
| `src/components/ui/` | Radix/shadcn primitives |
| `src/lib/api.ts` | Runtime config, `api()` fetch helper, wire types |
| `src/i18n/` | `en.ts` (source), `zh.ts`, `t()` helpers |
| `worker/` | Cloudflare Worker that serves `dist/` with security headers |
| `e2e/` | Playwright browser tests (mocked API) |
| `scripts/` | Contrast check, nginx header smoke, dist cleanup, embed timing diagnostic |

## Develop, build, check

Requires Node 22 and pnpm 10 (`packageManager` pins the version; `corepack enable`).

```sh
pnpm install --frozen-lockfile
pnpm dev                 # Vite on :5173
pnpm build               # tsc -b, vite build, strip trailing whitespace -> dist/index.html
pnpm typecheck           # app, generated Worker types, Worker
pnpm check:contrast      # WCAG contrast of the theme tokens
pnpm test:e2e            # Playwright, see below
pnpm preview             # serve dist/ locally
```

In `pnpm dev` the Console skips `/config.json` and uses its own origin; Vite
proxies `/api` (including WebSockets) to `TREER_DEV_PROXY`, default
`http://127.0.0.1:8787` (the Proxy's default listen address). The operator guide
has the full local Proxy + Console recipe
([operator guide](../docs/operator-guide.md)); start the Proxy with
`--app-public-url http://127.0.0.1:5173` so sign-in returns to Vite.

The build uses `vite-plugin-singlefile`: everything, including CSS and assets,
is inlined into one `dist/index.html`. Keep it that way: no CDN, remote font or
separately loaded asset. `@` aliases `src/`.

## Runtime Proxy URL

A production build is not tied to a Proxy. On startup it fetches
`/config.json` from its own origin (`cache: no-store`):

```json
{ "proxy_url": "https://proxy.example.com/" }
```

The URL must be `http:` or `https:` without credentials; otherwise the page shows
a configuration error instead of rendering. All API calls go to that origin with
`credentials: "include"`, so the Proxy must allow the Console origin for
credentialed CORS and its session cookies must reach it. Three hosts provide
`/config.json` and `/health`:

- **Cloudflare Worker** (`worker/`, `wrangler.jsonc`): `TREER_PROXY_PUBLIC_URL`
  and optional `TREER_INGRESS_PUBLIC_URL` vars.
- **nginx image** (`Dockerfile`, `default.conf.template`): `PORT` (default
  8080) and `TREER_PROXY_PUBLIC_URL` are substituted at container start.
- **The Proxy itself** with `TREER_APP_DIST_DIR` / `--app-dist-dir`, for a
  single-container deployment (see the operator guide).

## Hosting and browser security

### Cloudflare Worker

`worker/index.ts` runs before every asset/SPA response (`run_worker_first`,
SPA `not_found_handling`). It:

- serves `/health` (with environment and Worker version) and `/config.json`;
- sets a per-response script nonce and CSP from `worker/security-headers.ts`:
  `script-src 'nonce-…'`, `connect-src` limited to self and the Proxy
  (`https`/`wss`), `frame-src` self + Proxy + `https://*.<ingress host>` when
  `TREER_INGRESS_PUBLIC_URL` is set, `frame-ancestors 'none'`,
  `form-action 'self'`; plus `X-Frame-Options: DENY`, `nosniff` and
  `Referrer-Policy: no-referrer`;
- adds `Cross-Origin-Opener-Policy: same-origin` to HTML documents only;
- marks HTML, config and health `no-store` and strips validators, because the
  nonce must match the CSP. Do not cache rewritten HTML at an upstream edge.

An invalid Proxy or ingress URL fails closed with HTTP 503. The ingress
wildcard is a coarse hosting bound; the Console still checks the exact Agent
origin before framing it.

```sh
pnpm worker:dev                  # wrangler dev --env canary
pnpm worker:upload:canary        # versions upload
pnpm worker:deploy:canary        # app.canary.treer.ai
pnpm worker:deploy:production    # app.treer.ai
```

### nginx image

`Dockerfile` builds `dist/` and serves it from `nginx:1.27-alpine` with SPA
fallback, `X-Frame-Options`, a minimal CSP (`frame-ancestors`, `object-src`,
`base-uri`), `nosniff`, `no-referrer`, COOP on HTML and `no-store` for
config/health. It has no nonce-based script restriction. `node
scripts/check-nginx-headers.mjs` is an opt-in Docker smoke test of these
headers against a built `dist/`.

### Embedded code trust

Managed App iframes and the compatibility Agent view run on the Proxy origin
with the user's workspace authority; their sandbox is not a tenant boundary.
Only the isolated Agent embed (below) runs on a separate origin. Embedded
interfaces may open top-level pages (`allow-popups-to-escape-sandbox`) and
should use `noopener,noreferrer`, as all Console-owned new-tab actions do. See
[security](../docs/security.md).

## Screens and concepts

### Sign-in and invitations

Sign-in with GitHub/Google starts with a JSON POST to
`/api/auth/oauth/{provider}/start` and navigates to the returned, validated
HTTPS authorization URL (`lib/oauth-start.ts`); there is no GET fallback.
Opening `?invite=…` shows an organization preview
(`/api/invitations/preview`, requires `organization.invitation-accept.v1`)
and joins only after explicit confirmation (`/api/invitations/accept`). After
OAuth, a pending invitation is read from `/api/invitations/pending`. Invitation
tokens stay in memory and JSON bodies, never URLs or storage.

### Sidebar

The sidebar is the navigator, ordered by how often each part is used:
organization and workspace switchers, **Overview**, **Search**, **Needs
attention**, the **Tree**, then **Machines**, in one scroll region with sticky
section headings (`sidebar-sections.tsx`).

- **Search** (Cmd/Ctrl+K outside terminals and editors) filters the Tree by
  name or path, showing matches with their folders expanded, and also filters
  the attention list and machines. Escape clears; ArrowDown enters the rows.
- **Needs attention** is the triage inbox: blocked and failed Agents (flat,
  with their folder), then one row per offline machine ("build-node offline ·
  N Agents down", opens the machine page), then Agents that **finished**: this
  browser saw them go from working to idle/exited while they were not open
  (kept per workspace in localStorage, cleared on open; the API has no
  per-user read state, so this only covers what the open Console observed).
  First five rows, then "Show N more". On phones the **Browse tree** button
  carries the row count.
- **Machines** lists each machine with its workload count and online/offline
  state; a row opens the machine page, **+** opens Add machine. With no
  machines the Tree shows a guide; it, the Overview card and "Add agent" all
  open the same Add machine dialog.
- Attention and Machines collapse from their heading (remembered per browser).

### Workspace Tree

The sidebar Tree shows folders, Agents and Apps in persisted sibling order,
addressed by stable IDs; display names are unique only within a folder.

- Every level (folders, Agents, Apps, App-owned nodes) uses one row component
  (`tree-row.tsx`): indent, a reserved chevron slot, icon, name, then status or
  actions. Rows are 28px on fine pointers and 44px on touch / below 768px;
  guide lines sit under each parent's chevron. Long names truncate; the row
  title shows the full Tree path and machine.
- Status is shape plus colour and has a text name: a dot for working/starting,
  a hollow ring (and muted name) for stopped/exited, an icon for blocked,
  failed and machine offline. Idle and running Apps show nothing. A collapsed
  folder shows how many hidden Agents need attention (else how many work).
- The row "⋯" button appears on hover/focus (always on touch) and the
  right-click menu and Shift+F10 share one action list (`menuItems` in
  `workload-tree.tsx`), grouped as open (new tab, logs, App settings), run
  (start, restart, startup command), organise (rename, copy path, move, pin)
  and finally Stop/Delete after the last separator, each confirmed. Folder
  menus add new folder/Agent. Tab menus deliberately have no Stop/Delete.
- An App with an App-owned subtree has a disclosure chevron ("App contents")
  on its own row; nested App nodes expand the same way.
- Dragging highlights the target folder or the insertion line, and shows a
  workspace-root drop zone at the bottom.
- The header **+** menu has Add folder, Add agent and Add machine (opens the
  enrollment dialog). With no machines the Tree shows a setup button.
- Keyboard: one Tab stop with roving tabindex; Up/Down/Home/End move, Right
  expands (folders, Apps, App nodes) or reaches the actions button, Left
  collapses or goes to the parent.
- The sidebar resizes (240–480px, double-click resets to 272px) and collapses
  with the footer button or Cmd+B / Ctrl+B (not captured inside terminals or
  rich-text editors). Below 768px the layout is mobile: **Browse tree** opens
  the Tree full screen and touch targets are 44px.
- **Copy path** yields an absolute Tree path such as `/research/reviewer`,
  usable as one `treer` CLI argument.

### Overview, pins and map

Workspaces open at **Overview** without attaching a PTY. It is a status
board, not a second Tree: filter chips (attention, working, idle) with
counts, then flat sections **Needs attention**, **Working**, **Idle**,
**Stopped** and **Apps** (each row shows folder, machine and status text; long
sections show a few rows and "Show N more"), then machine health cards that
open the machine page. Attention means blocked/failed Agents or live Agents on
offline machines (`lib/attention.ts`). Without an organization or workspace it
shows one primary action (Create organization / Create workspace). Pages other
than the terminal have no separate header bar on desktop; phones keep a bar
with Back.

On Proxies advertising `workspace.pins.v1`, **Pin to Overview** manages a
shared, Core-stored list of up to 50 pins shown above the filters (pinned
folders still expand in place there). Writes are
not optimistic; older Proxies hide the feature.

**Workspace map** opens a read-only squarified treemap
(`lib/treemap-layout.ts`) in a dialog: each workload weighs 1, siblings are
ordered by path, folder headers drill down, and a **Go to** picker reaches
blocks too small to target.

### Tabs, deep links and focus mode

Tabs are the working set of open pages: switching keeps a page's terminal
connection or interface draft alive, which search and the inbox cannot.
Opening a workload opens or selects its tab. Overview is pinned. Hidden tabs
stay mounted up to 6 live pages on desktop / 4 on phones; older ones unmount
and reconnect when reselected (a reconnect icon marks them; the all-tabs menu
explains this in one line).
Closing a tab never stops the workload. Desktop shortcuts: Left/Right,
Home/End, Delete, Ctrl/Cmd+1–9, Ctrl/Cmd+W (only while focus is in Console
chrome).

Every workload has a stable URL
`/orgs/<org>/workspaces/<ws>/workloads/<id>` (an App uses its runtime Agent
ID). The address bar follows the selection; an inaccessible target shows an
error rather than a default. `?focus=1` renders only that workload with a bar
back to the full Console; `app_node` selects a node inside an App-owned
subtree. Tree rows, Overview rows and map blocks are real `<a href>` links
(`lib/workload-link.ts`), so middle-click and "copy link" work.

New-tab destinations: an App with a valid server-provided `public_url`
(HTTPS, real DNS name, not the Console/Proxy origin; `lib/app-url.ts`) opens
there. With `agent.interface-origin.v1` an Agent opens its isolated
`https://ag-<id>.<apps domain>/` origin (`lib/agent-origin.ts`); on older
Proxies an Agent with a registered UI opens the Proxy route
`/api/workspaces/{ws}/agents/{id}/interface/ui/`. Otherwise the Console deep
link is used.

### Agent surfaces

Selecting an Agent opens its Agent Interface UI when it has one, otherwise its
terminal; **Logs** always reaches the terminal.

**Terminal** (`terminal-pane.tsx`): full-bleed under one compact header
(`workload-surface.tsx`: path, machine, Agent status, connection state only
when not live, and Upload / Open in new tab / Reconnect). xterm.js over the Proxy PTY WebSocket, with
reconnect from a stream cursor, fit-on-resize (never resizing the PTY to a
hidden zero-size container) and mobile key rows (`mobile-terminal-keys.tsx`).
Historical output replay swallows terminal queries (DA, CPR/DSR, DECRQM, OSC
colour, window reports) so replay does not answer stale queries; live queries
and user input are untouched (`lib/terminal-replay.ts`).

**Agent Interface embed** (`isolated-agent-interface.tsx`), when the Proxy
advertises `agent.interface-embed.v1`:

1. The Console frames the Proxy-owned bootstrap
   `https://ag-<id>.<apps domain>/.treer/embed?reuse=1` with theme hints.
2. The bootstrap posts `ready` with a nonce hash (v2 adds `version: 2`); the
   Console accepts only the first message from that exact window and origin
   and replies `probe`.
3. The bootstrap answers `reused` (an existing session) or `mint`; for `mint`
   the Console requests a short-lived authorization code from the Proxy and
   posts it only to that origin. The bootstrap redeems it for a host-only
   partitioned cookie and replaces itself with the Agent UI.
4. Handshake and UI load have a 20-second deadline. Codes never enter URLs,
   storage, DOM attributes or logs.

If the bootstrap reports `cookies_blocked`, the Console probes whether the
deployment allows the shared-origin compatibility view and, if so, offers
**Load compatibility view (shared origin)**; it never switches automatically.
`TREER_AGENT_EMBED_FALLBACK=refuse` on the Proxy leaves only the standalone
link. Proxies without the capability keep the legacy Proxy-origin iframe.

With `agent.interface-embed-prewarm.v1`, a 400 ms hover or deliberate focus on
an online Tree Agent may run a hidden `/.treer/embed?reuse=1&prewarm=1`
handshake that stops at `warmed` without loading Agent HTML
(`use-agent-embed-prewarm.ts`). It is rate-limited locally (two concurrent per
workspace, per-Agent cooldown, per-minute caps), skipped on Save-Data, touch
and for Apps, and the click still runs a normal fresh handshake.

Theme, look (`treer-look`) and language are passed as URL hints on first load
and as exact-origin messages afterwards (`lib/embed-ui.ts`); they carry no
authority. The launcher Agent UI is in `launchers/agent/ui`.

**Create Agent** (`workspace-dialogs.tsx`) picks machine, folder, name and
command or launch profile. Structured creation failures
(`lib/agent-creation-failure.ts`) can leave a reserved identity, which
**Clean up reserved identity** removes through the reservation cleanup
endpoint before a new explicit submission. **Edit startup command** saves new
argv with one restart. The **Prepared Claude target** mode and its idle-session
copy (`lib/prepared-claude.ts`, `session-copy-wizard.tsx`) generate fixed
`session inspect/prepare/run` argv for a trusted launcher entry; see
[launchers/agent/SESSION-TRANSFER.md](../launchers/agent/SESSION-TRANSFER.md).

### Managed App embeds

Apps are discovered from Core's App list, refreshed (debounced) after runtime,
snapshot and machine events and on page focus; there is no polling. A runtime
recognised as an App switches from terminal to its interface unless the user
chose **Show logs**.

`app-interface.tsx` frames the App UI on the Proxy origin. Before the first
load (and after an App restart) it sends credentialed, uncached `HEAD`
requests to the App UI root: at most 8 attempts in 30 seconds with backoff,
stopping on auth refusal, then **Retry connection**. A matching Proxy also
posts `treer.app-ui.unavailable` when its tunnel fails before the first
response; it consumes the same budget.

An App can expose its own children in the Tree through
`/api/workspaces/{ws}/apps/{app}/ui/api/tree?path=…` (`lib/app-tree.ts`,
`app-owned-tree.tsx`): bounded pages (64 KiB, 100 children, depth 8), explicit
loading/empty/denied states, and conversations over Core Message routes.

### Machines

The machine view shows status, Controller/Host builds and capabilities.

- **Upload file** (`lib/machine-upload.ts`): with `machine.upload.v3`,
  resumable raw parts (1–8 MiB adaptive, SHA-256 per part via Web Crypto, so
  HTTPS or localhost is required) up to 128 MiB, with bounded retries, resume
  and cancel. Older machines use a base64 upload capped at 16 MiB, offered
  explicitly when capability data is missing.
- **Controller update** (`controller-updater.tsx`): with
  `machine.controller-update.v2`, a release version form POSTs
  `{"version":"vX.Y.Z"}` to
  `/api/workspaces/{ws}/machines/{id}/controller-update` and polls it every two
  seconds while `downloading`, `installing` or `restarting`. The Controller
  downloads the signed release, replaces itself and restarts through its Host.
  Older Controllers show a manual-upgrade hint.
- **Add machine** opens the enrollment dialog with the connect commands.

### Workspace and organization settings

- **Policy Provider** (`policy-publish-authority.tsx`): install the default
  Policy App, and (with `policy.publish-authority.v1`) let owners switch
  `publish_authority` between `owner_session` and `provider_operator` after a
  warning. Deep link: `/orgs/<org>/workspaces/<ws>#policy-provider`.
- **Audit** (`workspace-audit*.tsx`, requires `workspace.audit.v1`): filtered,
  paged `/api/workspaces/{ws}/audit-events` with ID suggestions; with
  `workspace.audit-summary.v1`, aggregate charts from `/audit-summary` that
  drill into the list. Traffic charts (`traffic-*.tsx`) show billable bytes and
  frames for 6h/24h/7d/30d. Audit is an operational log, not a complete ledger.
- Organization settings manage members, groups and invitations.

### Platform admin

`/admin` signs in with the operator's `ADMIN_PASSWORD` (separate admin
session). It shows platform-wide counts only, combined traffic
(`/api/admin/traffic`), invitation links (summaries with token prefixes; a new
link is shown once), and control-plane updates when the Proxy's update
sidecar is configured (`/api/admin/update`, check via POST).

## Cross-cutting conventions

**Internationalization.** English and Simplified Chinese live in `src/i18n/`.
`en.ts` is the source; `zh.ts` is typed against it, so a missing key fails
`typecheck`, and `e2e/i18n.spec.ts` checks placeholders and tags. Call
`t("key", { name })` in render code (never at module load); use `tnodes`,
`statusLabel`, `roleLabel` and `intlLocale()`. Language comes from stored
`treer-lang`, then `navigator.language`, then English.

**Accessibility.** Keyboard operation everywhere, visible focus, 44px touch
targets on coarse pointers, `role="alert"` error toast, reduced motion
respected. `pnpm check:contrast` checks text (4.5:1) and non-text (3:1) tokens
in both themes; documented exceptions are dark-theme blue/red as plain text
(3:1) and the 1.4:1 input-border hairline.

**Browser storage.** Only per-user conveniences, all guarded so the Console
works in memory when storage is unavailable:
`treer:folder-expansion:v1:<ws>`, `treer:tree-width:v1:<user>`,
`treer:tree-collapsed:v1:<user>`, `treer:tabs:v1:<user>:<ws>` (stable IDs
only), `treer-theme`, `treer-look`, `treer-lang` and the product-tour flags.
Never store tokens, drafts or terminal output.

**First-run tour.** `lib/first-run-tour.ts` (driver.js) targets
`data-tour` anchors; replay it from the user menu.

## Browser tests

`pnpm test:e2e [spec] [playwright args]` runs `e2e/run.mjs`, which:

- creates a throwaway `HOME` and passes only basic environment variables (no
  Agent identities, credentials or HTTP proxies);
- picks a free port triple unless `PW_PORT` is set: Vite on `PW_PORT`, a
  local-only Wrangler security Worker on `+1`, its inspector on `+2`
  (`e2e/ports.ts`);
- uses `PLAYWRIGHT_BROWSERS_PATH` or the default Playwright cache; browsers must
  already be installed.

`playwright.config.ts` starts both servers and never reuses existing ones. All
API traffic is mocked; tests never reach a real Proxy, IdP or machine. Specs
cover key journeys: Tree/Overview/tabs/layout, navigation and deep links, the
Agent embed handshake (`agent-embed.spec.ts`, real cross-origin frames with
synthetic responses), App embeds and App-owned trees, admin, invitations, OAuth
POST, i18n, mobile menu, Add machine and the treemap. Shared helpers are in
`e2e/fixtures/`.

`security.spec.ts` checks the Worker headers through the local Wrangler server,
which serves `dist/`, so build first:

```sh
pnpm build && pnpm test:e2e security.spec.ts --workers=1
```

`node scripts/measure-embed.mjs` is a manual timing diagnostic for the embed
handshake against a disposable canary fixture; its required `EMBED_*`
environment variables are validated at the top of the script. Never point it at
a user's Agent or put credentials in arguments or reports.
