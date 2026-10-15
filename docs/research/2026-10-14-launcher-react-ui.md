# Launcher browser UI: React rebuild design

2026-10-14. Status: **draft for review by launcher-dev, security-claude, leader.**
No implementation yet. Owner: ui-dev. Server/packaging sections need launcher-dev
sign-off (marked **[LD]**). Conflicts are recorded in "Open questions".

Companion: [launcher-react-contract](2026-10-14-launcher-react-contract.md)
(launcher-dev, server/DTO/packaging ground truth; wins on any conflict).

Scope: replace `launchers/agent/public/{app,files,goal,tasks,theme}.js` and
`styles.css` (~1,300 lines of vanilla JS/CSS) with a React UI while keeping the
server contract identical. The visual and interaction design borrows ideas from a
third-party reference (`pockymoe/thread-ui`, read-only analysis). That repository
has no LICENSE, so **nothing is copied**: tokens, CSS and markup here are written
fresh; only the design ideas (listed under "Borrowed ideas") are reused.

## 1. Ground truth: what the UI consumes today

The browser is a pure consumer of these routes (all relative, `<base href="./">`):

| Use | Route |
| --- | --- |
| Snapshot | `GET api/state` (also pushed over `ws` as `{type:"state"}`) |
| Older history | `POST api/history/older {limit}`; ws `{type:"history-limit"}` |
| Send / steer / queue | `POST api/prompt {prompt, mode:"steer"\|"queue", history_limit}` |
| Cancel queued | `DELETE api/queue/:id` |
| Interrupt | `POST api/interrupt` |
| Model / effort | `POST api/settings {model?, reasoningEffort?, history_limit}` |
| Goal | `PUT/DELETE api/goal`, `POST api/goal/reconcile`, `api/goal/retry` |
| Tasks | `GET api/tasks/:id/output?tail=65536` (poll 3 s while open) |
| Files | `GET v1/files/tree?path=`, `GET v1/files?path=` (64 KiB cap) |
| Theme | `?theme=light\|dark`, `postMessage {type:"treer:embed-theme"}`, system |

Note for the brief: the browser does **not** use `/v1/transcript`; pagination is
`api/history/older` plus the snapshot's `thread.hasOlderItems`. `/v1/transcript`
stays untouched. The React UI uses exactly the table above and adds no routes.

State shape (from `src/model.ts`, unchanged): `snapshot = {ready, runtime.state,
harness{name,capabilities{steer,goal{budget}}}, thread{title,cwd,status,model,
reasoningEffort,hasOlderItems,turns[TurnDto]}, modelOptions[], queue[], goal,
tasks, history.limit}`. `HistoryItem.kind` ∈ userMessage, agentMessage,
reasoning, commandExecution, fileChange, plan, toolCall, other.

## 2. Information architecture

```
App
├─ Header           identity (mark, title, cwd) · status dot · tasks button · files button · theme
├─ Banner           role=status: local error / notice (replaces #feedback)
├─ Workspace
│  ├─ Thread        scroll region, role=log, aria-live=polite
│  │   ├─ LoadEarlier
│  │   ├─ Turn*     user bubble · assistant prose · ActivityGroup(collapsed)
│  │   ├─ GoalNotice
│  │   └─ JumpLatest (appears when scrolled up)
│  ├─ FilesPanel    side drawer ≥ 900px, bottom sheet / full tab view on mobile
│  └─ TasksPanel    same drawer slot (they share one slot, as today)
├─ Composer         GoalBar · Queue · one editor (compact ⇄ expanded) · model/effort chip · Send / Steer / Queue / Stop
└─ Dialogs          GoalEditor · TextDialog (long text) · ImageViewer
```

Principles carried over from the reference: conversation is primary and machine
activity recedes; metadata on demand; familiar controls; AA contrast.

### 2.1 Timeline presentation (the biggest visible change)

The server already delivers flat `items` per turn. The UI groups on the client:

* A **turn** renders: user message → run of *activity items* (reasoning,
  commandExecution, fileChange, toolCall, other) → agent message(s).
* Consecutive activity items collapse into one **ActivityGroup** summary row:
  `Worked for 12s · 3 commands · 1 file change ▸`. The duration is shown only
  when the DTO supplies it: live elapsed while the turn is `inProgress` (from
  `startedAt`), or `endedAt − startedAt` once the optional `endedAt` exists. No
  guess from the next turn's start; otherwise the row is just `3 commands · 1
  file change`. Expanding shows each item as today's cards.
  An item that is `status: failed`/error forces its group to show a
  warning mark and stays discoverable without expanding.
* `plan` items stay visible (they are the agent's communication, not machine
  noise); `fileChange` diffs render inside the expanded group with the existing
  per-line classes (hunk/add/remove/context) as colored rows, not color-only
  (a `+`/`−` gutter glyph is kept).
* Sub-task grouping with a left rule: only when an adapter marks items with an
  optional `parentId` (see §4). Without it, no nesting. Not required for v1.
* Per-turn usage line, 11px muted: `1.2k in · 340 out · 4.1s`. Entirely optional
  (§4); rendered only if the DTO provides it.
* Counts come from item kinds only; hierarchy is never inferred.
* Open/closed state of groups is UI state keyed by `turnId:itemId`, preserved
  across snapshot updates (same guarantee as the current `details[open]` set).

### 2.2 Composer

One real `<textarea>` (not contenteditable), two layouts:

* **Compact** (idle, unfocused, empty/short): one line, controls on the right.
* **Expanded** (focused or multi-line): textarea grows to ≤ 180px (`30dvh` cap);
  controls drop to a second row, right-aligned.

To keep click targets from moving on blur→click, controls are anchored to the
bottom-right with fixed size, and expansion is triggered by `:focus-within` /
a `data-layout` attribute rather than by conditional unmounting (the editor and
its draft never remount). Model name + effort (10px) sit inside the box as one
button opening a small popover (native `<select>`s on mobile, 16px). Queued
prompts render **above** the box. Goal bar sits above the queue as today.
Behavior preserved verbatim: Enter submit, Shift+Enter newline, Tab queues a
non-empty draft, IME composition guard, `/goal` and `/goal clear` interception,
draft retained on failure, steer→queue downgrade notice.

### 2.3 Thread scroll

* Bottom padding equals composer height (ResizeObserver) so nothing hides behind
  the floating box.
* Stick-to-bottom only when within 120px of the end (current rule). New content
  while scrolled up does **not** pull; a "Jump to latest" pill appears (with a
  dot if new content arrived). Loading older history preserves the scroll anchor
  (current `scrollHeight` delta logic, ported into one hook).

### 2.4 Files, tasks, goal

* **Files** (read-only, no editor): tree with `role=tree`; Arrow keys move,
  Right/Left expand/collapse, Enter/Space opens; a filter box (client-side
  over loaded entries only — it never triggers extra listing requests);
  selecting a file previews immediately; a failed load renders an inline error
  in the affected row/preview and **never** replaces the page. Text preview
  keeps the 64 KiB truncation notice. Contents render as React text nodes.
* **Tasks**: panel as today (status, duration, exit code, summary, output tail
  poll 3 s while expanded and running, abort on collapse/unmount). Needs an
  `AbortController` cleanup effect — the one place a leak was possible.
* **Goal bar/editor**: port the state machine in `goal.js` (busy flag,
  `operationId` reuse across retries of an identical failed PUT, pending clear
  confirmation, completion notices keyed by turn) into a reducer; the editor
  stays a native `<dialog>`+`showModal()` for focus trap/Esc for free.

### 2.5 Small components

ImageViewer (zoom/fit with buttons and +/-/0 keys, only for `data:`/`blob:`
images the markdown can contain — remote images remain blocked by CSP),
TextDialog (long tool output/user text "View full"), export current transcript
as Markdown (client-side `Blob` download of already-loaded turns; a
download is not a new server capability), quiet loading (skeleton lines, no
spinners longer than needed), i18n.

### 2.6 i18n

Two flat dictionaries `en`, `zh` in `i18n.ts`; `t(key, {name})` with `{{name}}`
interpolation; missing key in `zh` falls back to `en`, missing in both returns
the key (and a dev-only console warning is **not** emitted in production). Locale
from `?lang=`, else `navigator.language` (`zh*` → zh). Server-provided text (harness
names, item text, errors) is never translated. Dates use `<time datetime>` with
`Intl.DateTimeFormat`.

## 3. Design tokens

Two layers; components read **only** the `--t-*` semantic aliases. Base values
are authored here (OKLCH with sRGB fallback not needed: evergreen browsers; the
supported matrix is the same as for `dialog`/`:has`, which the current CSS
already requires).

Base (light / dark), neutral-cool hue 220, one amber accent, radius 0.625rem,
no shadows (separation by border and surface step):

| Base token | Light | Dark |
| --- | --- | --- |
| `--g-0` page | `oklch(.975 .005 220)` | `oklch(.165 .008 235)` |
| `--g-1` panel | `oklch(.99 .003 220)` | `oklch(.205 .009 235)` |
| `--g-2` sunken | `oklch(.95 .008 220)` | `oklch(.185 .008 235)` |
| `--g-3` hover/muted | `oklch(.92 .012 220)` | `oklch(.26 .011 235)` |
| `--g-line` | `oklch(.86 .012 220)` | `oklch(.3 .012 235)` |
| `--g-line-strong` | `oklch(.74 .02 220)` | `oklch(.42 .016 235)` |
| `--g-fg` | `oklch(.24 .012 235)` | `oklch(.93 .006 235)` |
| `--g-fg-soft` | `oklch(.40 .016 235)` | `oklch(.80 .01 235)` |
| `--g-fg-muted` | `oklch(.50 .018 235)` | `oklch(.68 .012 235)` |
| `--g-amber` | `oklch(.72 .14 76)` | `oklch(.79 .13 76)` |
| `--g-amber-fg` (text on amber) | `oklch(.20 .03 76)` | `oklch(.20 .03 76)` |
| `--g-amber-text` (amber as text/link) | `oklch(.46 .12 70)` | `oklch(.83 .11 76)` |
| `--g-ok` / `--g-warn` / `--g-bad` | AA on `--g-1` | AA on `--g-1` |

Semantic aliases (what components use; switching theme = remapping these):
`--t-bg`, `--t-panel`, `--t-sunken`, `--t-hover`, `--t-line`, `--t-line-strong`,
`--t-fg`, `--t-fg-soft`, `--t-fg-muted`, `--t-accent`, `--t-accent-fg`,
`--t-accent-text`, `--t-ok`, `--t-warn`, `--t-danger`, `--t-focus`, `--t-radius`.
Type: system stack (no remote fonts) with CJK fallbacks; 14px body, 12px meta,
11px/10px for usage and effort; code 12.5px mono. Spacing 4px grid.

Contrast is verified by a script in the test suite (pairs `fg*`/`accent-text`
against `bg/panel/sunken/hover` in both themes ≥ 4.5:1; large/UI ≥ 3:1) — the
numbers above are provisional until that script runs; the prototype shows the
visual intent.

Theme selection is unchanged: `data-theme` on `<html>` set by a tiny module
(same logic as `theme.js`: `?theme=`, `treer:embed-theme` message with
`event.source === window.parent`, `matchMedia`). A manual toggle (system / light
/ dark) is added in the header and stored in `localStorage` only when the page
is not embedding-controlled (an explicit `?theme=` or parent message wins).

## 4. Types and DTO extensions

`src/types.ts` in the UI mirrors `model.ts` by hand (UI cannot import from
`src/`: different tsconfig, different runtime). To prevent drift, a unit test
loads a recorded `api/state` fixture from each fake harness
(`fake-claude`, `fake-codex`, `fake-acp`) and validates it against the UI type
guards. launcher-dev owns the fixtures **[LD]**.

Optional additions, all absent-safe, requested from launcher-dev **[LD]**:

```ts
interface TurnDto   { …; endedAt?: string | null;
                      usage?: { inputTokens?: number; outputTokens?: number;
                                cachedTokens?: number; durationMs?: number } }
interface HistoryItem { …; startedAt?: string | null; endedAt?: string | null;
                        parentId?: string | null }   // sub-task nesting
```

No field is required; if absent the corresponding UI is hidden. The UI never
parses `item.text` to guess these. (The existing `[来自 … ，排队于 …]` sender
prefix on user messages is kept as-is; a structured `sender` field would be
nicer but is out of scope.)

## 5. State management

`useReducer` + two contexts, no state library.

* `SnapshotContext` (server truth): `{snapshot, connected, loadingOlder}` fed by
  one `useStream()` effect (fetch `api/state`, WebSocket with 1 s reconnect,
  `history-limit` handshake — ported unchanged). Actions replace the snapshot
  wholesale (the server snapshot is authoritative; no client-side merging).
  The selected history limit lives in the reducer; it is re-sent on every
  reconnect, and an HTTP response carrying a smaller `history.limit` than the
  current one never replaces a larger window. Reconnect/poll errors keep drafts,
  expansion state and visible history.
* `UiContext` (local only): `{error, notice, settingsBusy, submitting, panel:
  "none"|"files"|"tasks", openGroups:Set, filesState, theme, locale}`.
* Mutations go through one `api()` helper (same JSON/error rules as `request`)
  and every mutation applies the returned snapshot, falling back to
  `GET api/state` exactly as today.
* Re-render cost: the timeline items are `React.memo`'d by `turn.id+item
  version`; markdown HTML is memoized per `item.id+text`. Streaming appends only
  re-render the last turn.

## 6. Security surface

* No `dangerouslySetInnerHTML` except in **one** component, `Markdown`, which
  sets `innerHTML` from the existing `renderMarkdown` (marked → DOMPurify with
  the current config). A lint-style test greps the built source tree for any
  other `dangerouslySetInnerHTML`/`innerHTML`/`eval`/`new Function`/`document.write`.
* File names, paths, diffs, tool output, errors, queue text: React text nodes only.
* No token or secret is read or rendered; `localStorage` holds only theme/locale.
* CSP unchanged: `script-src 'self'` (no inline, no eval — esbuild output with
  no `eval`, `new Function` or `import()` from data); `style-src` already allows
  `'unsafe-inline'` for the legacy UI. React uses `style=` props sparingly (CSS
  variables for dynamic heights). **Proposal for security-claude**: once the
  legacy UI is deleted, drop `'unsafe-inline'` from `style-src`, which requires
  that no code path in the React UI uses `style=` attributes. Cheap if decided
  now (use `CSSStyleDeclaration.setProperty` from a ref instead); costly later.
  Recommendation: yes, do it; this is a CSP tightening, not a relaxation.
* ImageViewer displays only `data:` and `blob:` images (already the CSP limit).
* Text export and clipboard are client-only and read data already in the page.

## 7. Dependencies (each reviewed)

| Package | Why | License | Notes |
| --- | --- | --- | --- |
| `react`, `react-dom` (19.x, pinned) | requested; | MIT | production-only min build ≈ 190 KB min / ~60 KB gz before our code. This is the dominant size item. |
| `marked`, `dompurify` | already used | MIT / Apache-2.0 + MPL-2.0 | unchanged; markdown bundle remains one esbuild artifact |
| `esbuild` 0.28.2, `typescript` | already devDependencies | MIT / Apache-2.0 | dev only; no new tooling |
| `@types/react`, `@types/react-dom` | typecheck only | MIT | dev only |

Not added: router, state library, icon library (≈ 15 inline SVG paths as
components), CSS-in-JS, class-name helper, date library (`Intl`), virtualization
library (see risk R2), testing-library (Playwright suite already exists; unit
tests use `node:test` + `react-dom/server`). Vulnerability status and exact
versions/lockfile integrity are to be re-checked with `npm audit` at the time the
lock is edited and attached to the security review; I make no claim about them
today.

## 8. Build, artifacts, payload **[LD]**

* Source in `launchers/agent/ui/` (TSX, CSS). `scripts/build-ui.mjs` runs esbuild
  (bundle, minify, `format=iife`, `target=es2022`, `jsx=automatic`,
  `define NODE_ENV=production`, `legalComments=inline`, deterministic, no
  sourcemap in the payload).
* Output: `public/ui.html`, `public/ui.js`, `public/ui.css` — **3 files, short
  names, flat** (no nested dirs → Windows long-path safe). `markdown.js` is
  folded into `ui.js` (the legacy `markdown.js` stays until deletion). Relative
  `<base href="./">`; the script tag is `<script src="ui.js" defer>` (classic IIFE
  avoids module-MIME and base-URL edge cases in the iframe tunnel).
* **Artifacts are committed**, same as `markdown.js` today. Reason:
  `bootstrap.sh` installs by sparse git checkout + `npm ci --omit=dev`, i.e.
  with no esbuild present, so the checkout must already contain the built UI.
  `npm run check` adds `build:ui` into a temp dir and fails if bytes differ from
  the committed files (reproducibility gate). `prepare-bundle.mjs` copies
  `public/` as now, so UI files enter the sha256 manifest with no change; it
  additionally verifies the committed UI matches a fresh build **[LD]**.
* Runtime needs no `node_modules` for the UI. The server serves `public/*`
  unchanged (`mime()` already covers `.html .js .css .svg .png .woff2`).
* Coexistence: during migration the legacy `index.html` stays the default and the
  React UI is reachable at **`ui.html`** (equivalent of the requested
  `?ui=react`, with zero server change: unknown paths fall back to
  `index.html`, but `public/ui.html` is served as a static file). The switch is a
  single commit that renames `ui.html → index.html` and deletes the legacy
  files; revert = revert that commit.
* Budget (uncompressed): `ui.js` ≤ 520 KB, `ui.css` ≤ 40 KB, `ui.html` ≤ 2 KB
  → **≤ 600 KB total**. Estimated: React+ReactDOM ≈ 190 KB, marked+DOMPurify
  ≈ 65 KB (today's `markdown.js` is the measured value), app ≈ 120–150 KB.
  Measured numbers will be posted after the first build; the budget gate is a
  test (`build:ui` fails over budget).

## 9. Tests

* Playwright suites `tests/browser*.mjs`, `windows-smoke.mjs`, `hermetic.mjs`
  take a `--ui legacy|react` switch (path `/` vs `/ui.html`) and the shared
  assertions are rewritten to **roles/labels and stable `data-testid`s** instead
  of today's `#file-content`/`.diff-add` selectors, so one assertion set covers
  both UIs during the migration. Parity matrix in §11 is the checklist.
* New: contrast script; a11y smoke (axe-style checks written by hand: names on
  all buttons, focus visible, tab order, dialog focus return); reduced-motion
  emulation; 390px no-horizontal-overflow; zh/en snapshot; Windows screenshot
  stays in windows-smoke (desktop + 390px + no `pageerror`).
* Unit (`node:test`): reducer, goal state machine, group builder, i18n fallback,
  duration/format helpers, type guards against recorded fixtures.

## 10. Migration plan

1. This document → review → prototype screenshots → user confirmation. *(now)*
2. Skeleton: build script, `ui.html`, theme/i18n/tokens, stream + header +
   plain thread; both UIs selectable. Commit.
3. Thread/Timeline (groups, markdown, diff, plan), Composer/queue.
4. Goal bar+editor, tasks, files, dialogs.
5. Test conversion to dual-UI; parity matrix green on legacy **and** react,
   macOS + Windows (launcher-dev runs windows-smoke on the official Controller).
6. Security review (deps, CSP, DOM sinks). Fix findings.
7. **Switch commit** (default → React, legacy deleted in a *separate* follow-up
   commit so the switch itself is a tiny revertible diff). README Browser
   interface section updated in the same change as the switch.

## 11. Parity matrix (must all pass before the switch)

Transcript + older-page append with scroll anchor · streaming · steer/queue/Tab ·
queue cancel · draft retention on failure · steer downgrade notice · interrupt ·
model/effort (hidden when no efforts; "Default" placeholder) · goal set/edit/
pause/resume/clear-confirm/retry/reconcile, `/goal` commands, completion notices,
budget/usage · tasks panel + output tail polling · files browse/preview/refresh/
truncation/text-only · diff rendering · plans · reasoning · markdown via
DOMPurify · XSS fixture stays inert · theme (`?theme`, parent message, system) ·
390px layout and drawer sharing · iframe `<base>` tunnel · no JS errors ·
Windows == POSIX.

## 12. Risks

* **R1 Size**: React alone is a third of the budget. Mitigation: production
  build, no extra libs; gate in test. If it overshoots, `preact/compat` is the
  fallback (not proposed now; would need its own review).
* **R2 Long transcripts**: no virtualization. History is paged by the
  server as the most recent N *items* (default 50; "Load earlier" raises the window
  by 50, up to 10,000). Memoized turns plus
  `content-visibility: auto` on turn containers should suffice; measured with a
  2,000-item fixture before the switch.
* **R3 Stream churn**: full-snapshot pushes while streaming. Mitigated by
  structural sharing in memo keys; if a profile shows jank we coalesce frames
  with `requestAnimationFrame`.
* **R4 Focus/IME**: composer must never remount; covered by a test that types
  during snapshot pushes (IME composition guarded as today).
* **R5 Iframe tunnel**: absolute paths break it. Lint test forbids `href="/` and
  `src="/` and root-relative `fetch("/`.
* **R6 Committed bundle review burden**: a minified diff is unreviewable.
  Mitigation: reproducibility gate + the source is the review surface.
* **R7 Machine-activity grouping** hides failures: handled by the warning mark
  and by auto-expanding the group of a failed/awaiting item.

## 13. Borrowed ideas → where they land

Two-layer tokens (§3) · compact/expanded single editor (§2.2) · conversation
first, activity collapsed with duration (§2.1) · optional per-turn usage (§2.1,
§4) · floating composer spacing and jump-to-latest (§2.3) · mobile bottom bar,
16px input, ≤ 40vh sheets (§2.2, prototype) · accessibility targets (§9) ·
file explorer keyboard/filter/error isolation (§2.4) · image viewer, text
dialog, export, i18n (§2.5–2.6). Not borrowed: editor/save, terminal, molecule
viewer, split workbench, account/device sharing.

## 14. Open questions

0. Resolved with launcher-dev: output names are `ui.js`/`ui.css` (fixed, no hash
   chunks); no invented durations/hierarchy (§2.1); images stay self/data/blob.
1. **[LD]** Commit built `ui.js`/`ui.css` (recommended, §8) vs. build in
   `prepare-bundle` only (breaks `bootstrap.sh` git installs).
2. **[LD]** Provide optional `usage`, `endedAt`, `parentId` per §4 — which
   harnesses can fill them? Claude Code transcripts carry usage; Codex and ACP
   unknown.
3. **[security-claude]** Drop `'unsafe-inline'` from `style-src` at the switch?
   (§6, recommended.)
4. **[leader/user]** Manual theme toggle persisted in `localStorage`: acceptable?
5. **[leader/user]** Default locale: browser language, or English unless
   `?lang=zh`? (Recommended: browser language.)
6. Mobile "bottom bar": proposed content = Files, Tasks, Theme; Model/effort stays
   in the composer chip. Confirm with prototype.

## 15. Static prototype

`2026-10-14-launcher-react-ui/prototype.html` is hand-written HTML/CSS (no
React, no JS) showing the thread, collapsed/expanded activity group, diff, plan,
goal bar, queue, compact composer with model/effort chip, jump-to-latest, and the
file drawer. Screenshots (2x): `desktop-{light,dark}.png` (1280px) and
`390-{light,dark}.png` (390px, bottom bar, 16px input, no horizontal overflow —
checked programmatically). It is a visual-intent mock only: the expanded
composer layout, mobile bottom sheets, zh strings and focus states are not drawn,
and the contrast numbers in §3 are not yet script-verified.
