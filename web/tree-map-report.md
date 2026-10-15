# Tree menu and treemap review notes

Base: `origin/main` at `6f38c3f5f5860adc72192d2cd78c6be405188111`.

The header combines creation into one + dropdown, preserving the existing
per-action disabled conditions, root/folder destinations, machine setup guidance
and tour target. The neighboring expand button opens a read-only treemap. It uses
existing authorized inventories, stable Agent IDs and Tree folder placements;
there are no new API routes, writes, storage keys or credentials. No chart library
or other dependency was added. The existing single-file build plugin is retained.

Folder containers nest Agent cards. Each Agent has unit weight; empty folders
retain visible space. Desktop uses a small squarified layout; phone layouts use
proportional vertical strips with enough space for readable labels. Deep or small
folder previews stop after two levels and can be drilled into. Breadcrumbs return
to any ancestor. Folder exploration is local to the dialog and does not alter
sidebar collapse preferences. Live inventories continue to update while open;
a deleted current folder returns to the workspace. Malformed parent cycles are
broken for rendering, rather than recursively traversed forever. Status always
has text alongside theme-colored dots. Hover/focus exposes canonical paths.
Native buttons support Enter/Space, the dialog traps focus, Escape dismisses it,
and dismissal returns focus to the expand trigger. Agent activation uses the
existing open callback and closes the dialog.

## Build size and deployment

Measured with `pnpm build` using the same locked dependencies and toolchain.
Both builds use the existing single-file Vite plugin and emit only index.html
(plus the unchanged public favicon). Numbers are raw bytes and Python gzip
level 9; these are bundle measurements, not live network timings.

| Payload | Raw bytes | gzip bytes |
| --- | ---: | ---: |
| Before: index.html | 956,335 | 269,467 |
| After: index.html | 969,220 | 272,992 |
| Difference | +12,885 (+1.35%) | +3,525 (+1.31%) |

Per the final requirement the small map component uses a normal static import;
only its dialog DOM/layout is mounted when opened. No build, dependency, Worker,
CSP or release-script changes remain. There are no additional assets to seal or
restore, and the existing single-index.html publication contract is unchanged.
The actual local Worker/browser test opens and drills into the map under the
unchanged nonce CSP and checks for security-policy violations.

## Validation

Focused tests cover original creation flows plus menu disabled states, no-machine
setup, layout area/bounds/non-overlap, desktop and 390px in both themes, drill-down,
empty folders, breadcrumbs, keyboard selection of same-name Agents by stable ID,
text-only rendering of hostile names, live deletion, focus return, independent creation-menu disabled states,
and unchanged production Worker CSP. Checks passed: frozen-lockfile pnpm install, pnpm build, hermetic pnpm typecheck,
full hermetic Playwright (200/200, 2 workers), documentation and diff checks.
PostgreSQL/Rust checks were not run: no Core code changed. The final full browser
log is `/tmp/treer-tree-map-final-e2e.log` and the isolated typecheck log is
`/tmp/treer-tree-map-typecheck.log`.

Screenshots were visually inspected and retained under the ignored local directory
`/Users/dooms/treer-tree-map/.treer/reviews/tree-map/`: `treemap-light-desktop.png`,
`treemap-dark-desktop.png`, `treemap-light-390.png`, `treemap-dark-390.png`, and
`treemap-light-390-drilled.png` / `treemap-dark-390-drilled.png`. They are review
artifacts, not bundled assets.

## Issue #7 and #4 follow-up

The independent follow-up commit on this branch changes only display categories:
Tree row subtitles and map text/aria/title now say `agent` or `app`. Raw runtime
kinds remain untouched; Core App mappings retain the App category even if their
runtime reports `command`. Existing creation and opening behavior is retained.

Previously the no-machine Add agent item already visibly said “Set up a machine
first” and navigated to Workspace settings, but settings only explained “No
machines connected”. The empty settings page now adds an explicit explanation
that machines run Agents and a **Set up a machine** button opening the existing
Add machine instructions. It appears only after the machine count confirms zero;
there is no automatic enrollment or change to an existing machine. Desktop and
390px tests verify the visible menu hint, explanation and installation dialog
using a synthetic bootstrap response.

Follow-up checks passed: full hermetic Playwright **203/203** (2 workers),
`pnpm build`, isolated `pnpm typecheck`, documentation and diff checks. Logs:
`/tmp/treer-tree-map-issues-e2e.log` and
`/tmp/treer-tree-map-issues-typecheck.log`. Updated light/dark desktop and 390px
screenshots are retained under the ignored local directory
`/Users/dooms/treer-tree-map/.treer/reviews/tree-map-issues7-4/`.

## Issue #9: collapsible desktop sidebar

A small arrow at the top of the existing separator toggles the sidebar; its
native button supports Enter/Space and reports `aria-expanded`/`aria-controls`.
When hidden, a 24px rail keeps expansion reachable; invisible sidebar content is
removed from keyboard navigation by CSS visibility. The Tree stays mounted, so
selection and folder expansion state are preserved. The resize handle is removed
while collapsed and restored on expansion, retaining its original keyboard and
pointer semantics. The arrow avoids the separator midpoint so double-click reset
remains available. Toggle completion fits the active terminal after layout.

Collapse is saved per user, across workspaces, in
`treer:tree-collapsed:v1:<user_id>` alongside the existing numeric width key.
Separate keys preserve existing width caches. Missing/invalid collapse values
mean expanded; storage errors allow in-memory toggling. Collapse does not overwrite
the saved width. Below 768px, the arrow/rail are hidden and desktop collapse is
ignored; native mobile and Agent-owned WebViews are unchanged. The shared Console
iframe/embed presentation uses the same controls.

Regression tests cover mouse and keyboard toggles, focus retention, refresh and
workspace switching, saved width restoration, user-scoped/invalid caches, storage
getter/read/write failures, terminal fit in both directions, iframe continuity,
and 390px navigation without horizontal overflow. No dependencies, build
configuration, Worker headers, or release mechanics changed.

Issue #9 checks passed: full hermetic Playwright **204/204** (2 workers),
`pnpm build`, isolated `pnpm typecheck`, documentation and diff checks. The
full browser log is `/tmp/treer-tree-collapse-e2e.log`; build/typecheck logs are
`/tmp/treer-tree-collapse-build.log` and
`/tmp/treer-tree-collapse-typecheck.log`. Visually checked desktop collapsed and
390px screenshots are saved under the ignored local review directory
`/Users/dooms/treer-tree-map/.treer/reviews/tree-collapse/`. Relative to 92371db,
the single-file bundle grows by 1,048 raw bytes / 286 gzip bytes (Python level 9).
