# UI beyond the launcher and Apps: what to improve next

2026-10-15. Owner: ui-dev. Scope: every user-facing surface except `launchers/agent`
(just rebuilt) and workspace Apps (`apps/`). Read-only research; no code changed.
Evidence: `web/` at `origin/main` 43aeca4 rendered through its own mocked e2e
fixture (desktop 1440 px and 390 px, light and dark, 32 screenshots in
`/Users/dooms/.cache/ui-proto-demo/web-research/`), plus source review of
`mobile/ios`, `mobile/android` and `crates/treer-cli`. Mobile apps were not run.

## Surfaces and their state

| Surface | Where | Size | State |
| --- | --- | --- | --- |
| Web control plane | `web/src` | 5.3k lines TS/TSX; `workspace-app.tsx` 1,386 lines with ~85 `useState` | Functional, consistent shadcn/Tailwind look, mocked e2e at desktop/390; no visual regression suite (`docs/quality.md`) |
| iOS app | `mobile/ios` | 36 Swift files, SwiftUI | Fleet orchestrator (Home / Machines / Inbox + Voice sheet), Agent UI via WKWebView |
| Android app | `mobile/android` | 33 Kotlin files, Compose Material 3 | Same IA as iOS; separate Material theme (green primary) |
| CLI | `crates/treer-cli` + Controller `cli_*` | `treer` prints JSON for everything (`status` = 28 KB for this workspace) | Machine-oriented; humans and agents both read raw JSON |
| Embedded Agent Interface | `web/src/lib/embed-ui.ts`, iframe in `workspace-app.tsx` | Theme passed by `?theme=` and `treer:embed-theme`; look (a/b/c) not passed | Works; the launcher now has 3 looks the console does not know about |

## Findings, by impact

### 1. The web console's first screen is an empty terminal (highest impact)

Opening a workspace selects the first workload and shows a 1,120 px dark PTY
box that says `CONNECTING` / `NOT ATTACHED` (screenshots `desktop-light-01-home`,
`desktop-DARK-01-home`). With 37 agents in the real workspace (`treer status`),
the sidebar is a flat list sorted by name whose only status word is the raw
runtime string, and nothing on screen answers "who needs me?". The iOS app
already models this correctly (`Attention.swift`: *Needs attention / Working now /
Idle & ready* plus a fleet strip); the web has no equivalent.

Proposal: a **workspace overview** as the default main view (no agent selected):
attention-first groups (blocked / failed / offline-machine / working / idle),
machine health strip, last activity time, one-click open. The tree stays as the
navigator; the overview is the landing page. Reuse the mobile attention rules so
both clients agree on what "needs attention" means.

### 2. Agent rows carry no state a human cares about

Rows show `agent · workstation · running`. Missing: last activity / turn time,
blocked reason, unread Core Messages, whether the Agent has an interface
(`ui_path`) or only a PTY, generation/restart count. The Agent header in the main
pane (`/api-server  ag-1 · workstation`) is the same. Proposal: a compact status
chip set per row (state dot + "2m ago", "⚠ blocked", "UI" badge) and a header
strip with cwd, machine, interface capabilities, uptime. All of this already
exists in the snapshot (`agents[].status`, `interface.capabilities`,
`updated_at`, `run_generation`).

### 3. Workspace map is a treemap of equal empty rectangles

`desktop-DARK-04-treemap`, `mobile-DARK-04-treemap`: three equal tiles with only
the name and status in the corners; the footer admits "Equal-weight agents".
A treemap only pays off when area means something. Options: weight by recent
activity or attention, render as a card grid with status/last-turn/machine, or
retire it in favour of the overview in (1). Keep the folder breadcrumb navigation.

### 4. Dark mode is fine; theme and look are not shared with embedded UIs

Dark renders correctly when the preference is set before load. But the console
only forwards light/dark to the iframe; the launcher's new `look` (a/b/c) and
`lang` are not forwarded, so an operator can get a serif "Warm Paper" page inside
a cool-grey console. Proposal: add `look` (and `lang`) to `withEmbedTheme` and the
`treer:embed-theme` message (launcher `boot.js` already validates a whitelist),
and expose a console-level Appearance that maps to the launcher looks.

### 5. Density and typography are inconsistent across views

`workspace-settings-view.tsx` uses `text-xs` 25 times; `admin-panel.tsx` mixes
`text-sm`, `text-xs` and `text-[11px]`; the audit view uses `text-[9px]` and
`text-[10px]` labels (hard to read, below 11 px). Settings pages are a long
single column with 220 px label gutters (`desktop-light-06-workspace-settings`).
Proposal: a small type scale (11 / 12 / 13 / 14 / 16 / 20) as Tailwind theme
tokens, no arbitrary `text-[Npx]`, and a shared `SettingsSection` component so
Workspace, Organization, App and Account settings look identical. The Account
settings dialog (`desktop-light-10b-settings-dialog`) has a different navigation
model (left tabs in a modal) from the page-style Workspace/Organization settings;
pick one.

### 6. `workspace-app.tsx` is a maintainability risk for any UI work

1,386 lines, ~85 pieces of state, data loading, routing, dialogs and layout in
one component. Every UI change above touches it. Proposal (mechanical, no
behaviour change): extract `useWorkspaceData` (snapshot, apps, profiles, tree),
`useRoute`, `CreateAgentDialog`, `LaunchProfileDialogs`, `InstallDialog`,
`UploadDialog`, `MobileTerminalKeys`; keep tests green. Do this before (1)-(5).

### 7. Mobile web: the sidebar is the whole phone screen

At 390 px the tree fills the screen and tapping an agent opens a full-screen PTY
with 14 touch keys (`mobile-light-02-terminal-fullscreen`). The native apps show
the right model (attention list, Agent detail with transcript + composer, PTY
only as an emergency overlay). The mobile web should converge: overview list,
Agent detail = embedded interface when `ui_path` exists, PTY as a secondary
action. The 14-key strip should also respect `env(safe-area-inset-bottom)` more
visibly and avoid taking 20 % of the viewport when the keyboard is closed.

### 8. Accessibility and motion

52 `aria-label`s, 9 in the tree, but `role="tree"` has no roving tabindex or
arrow-key handling (`workload-tree.tsx`: 3 keyboard hits, none for navigation);
`prefers-reduced-motion` is not referenced anywhere in `web/src`; focus styles
rely on browser defaults; the error toast at the bottom (`fixed bottom-4`) has no
`role="alert"`. Proposal: adopt the keyboard tree from the launcher's file
browser (same roles, Arrow/Home/End/Left/Right), add a reduced-motion media rule
globally, and run the same contrast script used for the launcher looks over the
console tokens in both themes.

### 9. The first-run tour is good but starts on an empty terminal

11 steps (`desktop-light-11-tour`), driver.js, replayable. It would land better
on the overview from (1) and could reuse the "empty state" copy; today the
preview banner, the empty tree and the dark PTY compete for attention.

### 10. iOS and Android are not the same product visually

iOS: custom `TreerTheme` with navy accent, SF Symbols, inset-grouped lists.
Android: Material 3 with a green primary and Material icons. Both differ from the
web (blue primary, warm greys). Status colours differ per platform
(`statusColor` in Swift vs `Theme.kt`). Proposal: one documented status palette
(idle / working / blocked / failed / offline) and one accent, applied in all three
clients; let each platform keep its native controls.

### 11. CLI output is JSON only

`treer status` returns 28 KB of JSON; `treer agent ls` likewise. For agents this
is right; for humans at a terminal it is unreadable. Proposal: `--format table`
(or auto when stdout is a TTY) for `status`, `agent ls`, `machine list`,
`message list`, keeping JSON as the default for non-TTY so skills do not change.
Low effort, high daily value for operators.

### 12. Small fixes seen in screenshots

* "Workspace root" footer in the tree reads like a label without a purpose;
  make it the drop target / breadcrumb or remove it.
* Machine overview: "foreground fallback" warning is the most important line on
  the page and is the smallest text (`desktop-light-07-machine`).
* Organization settings: "Current" workspace tag in green with no action;
  "Groups" create input before the list reads backwards.
* Row menu has 11 items (`desktop-light-03-row-menu`); group destructive
  (Delete) and reorder (Move up/down) with separators.
* Auth screen is clean; add the product tagline from `docs/product.md` and the
  OAuth buttons' state when providers are configured (not visible in mock).

## Suggested order

1. (6) split `workspace-app.tsx` — enables everything else safely.
2. (1)+(2) workspace overview and status chips — biggest daily impact.
3. (4) forward look/lang to embedded UIs — ties the launcher work together.
4. (8) keyboard tree, reduced motion, contrast script — cheap, reuses launcher code.
5. (5)+(12) type scale, settings component, small fixes.
6. (3) decide the map's fate once the overview exists.
7. (11) CLI table output.
8. (7)+(10) mobile web convergence and cross-platform palette.

Each step is a separate, revertible commit group with the existing mocked e2e
extended (and a screenshot matrix like the launcher's) before behaviour changes.
