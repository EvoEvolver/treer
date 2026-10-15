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
| After: index.html | 967,472 | 272,547 |
| Difference | +11,137 (+1.16%) | +3,080 (+1.14%) |

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
