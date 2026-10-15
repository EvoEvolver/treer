# Launcher browser UI: React rebuild (design and implementation record)

2026-10-14. Status: **implemented** (sections 1-15 are the design; section 16 records what was built, how it
was verified, and what is still open). Owner: ui-dev. Server/packaging sections need launcher-dev
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
├─ Composer         one frame: StatusStrip (Goal · Plan n/m · Queue n) over one editor (compact ⇄ expanded) · model/effort chip · Send / Steer / Queue / Stop
│   └─ StatusPopover  one of Goal / Plan / Queue, above the frame (bottom sheet ≤ 640px)
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
button opening a small popover (native `<select>`s on mobile, 16px). Goal, plan progress and queued prompts no longer stack above the editor; see
§2.2a.
Behavior preserved verbatim: Enter submit, Shift+Enter newline, Tab queues a
non-empty draft, IME composition guard, `/goal` and `/goal clear` interception,
draft retained on failure, steer→queue downgrade notice.

### 2.2a Status strip (replaces the Goal bar + Queue stack)

The prototype review showed that a stacked goal bar, plan card, queue rows and a
floating "Jump to latest" crowd the bottom edge at every width. Rules:

1. **One 32px strip** is the top row of the composer frame; the frame is the only
   thing floating over the thread. Segments: **Goal** (status word + one-line
   objective, truncated), **Plan n/m** (only when a plan exists), **Queue n**
   (only when non-empty). With no data the strip is not rendered.
2. **One popover at a time.** Clicking a segment opens its popover above the
   frame and closes any other; click again, Esc, the close button or an outside
   click closes it and returns focus to the segment (`aria-expanded`,
   `aria-controls`). It overlays the thread (the thread never resizes), height
   <= `min(320px, 40vh)`. At <= 640px it is a fixed bottom sheet, <= 40vh, with a
   close button.
3. **Needs-attention beats tidiness.** Goal states `blocked`, `stale`,
   `usageLimited`, `budgetLimited`, `syncStatus != synced` or a goal error turn the
   Goal segment danger-colored with its status word; it stays visible without
   opening. Popovers never open on their own, so layout never jumps under the
   user's hands; the clear-goal confirmation happens inside the Goal popover.
4. **Queue is count-first.** The Queue segment always shows the count; cancel
   buttons and sender paths live in its popover. A polite live region announces
   count changes. This deliberately changes the old always-visible queue rows:
   `browser*.mjs` assertions move to "open Queue popover, then cancel" (parity
   item, section 11).
5. **Plan** stays in the timeline in order as history; the strip/popover shows
   the latest plan's progress only.
6. **Jump to latest** is pinned to the bottom-right of the scroll pane at
   `bottom: dock height + 12px`, never inside a card. The dock height is
   written to a CSS variable by a `ResizeObserver` on the composer, which also
   sets the thread's bottom padding to `dock height + 24px`.

### 2.3 Thread scroll

* Bottom padding equals the dock height + 24px (ResizeObserver) so nothing hides
  behind the floating frame.
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

### 2.7 Looks: three visual identities, user-selectable (decided)

**Decision (2026-10-15).** The user rejected the first two visual attempts (the
existing launcher style kept as-is; the reference palette with amber), then
chose to **keep all three** free-design directions as selectable *looks* after
seeing them, once B was recoloured to match the existing launcher (green accent,
warm greys). A look is a theme pack: tokens + font strategy + shape/spacing
language + icon stroke style + small decorative differences, over **shared
component structure**. Looks are orthogonal to light/dark: 3 looks x 2 themes = 6
variants.

| | Intent | Layout signature | Type | Accent (light / dark) | Radius / icons / motion |
| --- | --- | --- | --- | --- | --- |
| A Quiet Editor | calm, dense, out of the way | strip is the top row of the composer frame (header actions top right like every look; the side rail was dropped) | system sans 13.5px, tabular figures | ink blue `#3358d4` / `#7ea3ff` | 8/5px; 16px 1.5 stroke; 120ms colour only |
| **B Warm Paper (default)** | readable, unhurried, closest to today's launcher | centered title; pill chips above a rounded composer; floating panel cards | serif prose 16.5px/1.8 + sans UI | launcher green `#18725a` / `#58b99a` | 14/10px, pills; 18px 1.7 stroke; 220ms fades |
| C Terminal Contrast | legible at a glance | glyph gutters (`❯ ● $ [x]`), inverse status line for goal/plan/queue | monospace throughout 13px | blue `#0a47c4` / mint `#4de3a2` | square; 15px 2px square-cap; blink cursor only |

**B recolour** (from the oxblood/rose draft): page `#f5f5f3`, panel `#ffffff`,
sunken `#eeefeb`, line `#d8d8d3`, text `#292724`, muted `#66645e` (the launcher's
`#716f69` darkened for AA on `#eeefeb`), accent `#18725a` (unchanged hue and
lightness; passes AA as text), shadow neutral `rgb(25 24 21/8%)`; dark: page
`#191b1c`, panel `#222526`, sunken `#1b1e1f`, line `#3a3d3e`, text `#ecebe8`,
accent `#58b99a`. **Restrained danger in all looks**: B `#a63a30`/`#eb8f87`,
A `#b3302c`/`#f0847f`, C `#a5302a`/`#f08880`; diff delete tints are paler; errors
are a neutral panel with a 3-4px danger left bar (no red fills; the C connection
banner is neutral with a danger bar and `!`).

**Softer lines in A and B (user feedback: dark solid outlines are unpleasant).**
Border colours are split into role tokens so a look can soften them without
touching text or focus colours: `--b-div` (dividers, panel separators), `--b-card`
(cards, tool rows, diff, plan, tasks), `--b-pop` (popovers, menu; plus a faint shadow
instead of an outline), `--b-pill` (buttons, pills, tabs), `--b-ctl` (composer and
filter at rest). Values are low-contrast hairlines (about 1.1-1.8:1 against the
panel; see `contrast.mjs --table`); the previous strength remains reachable as
`data-borders="classic"` for the comparison page only (not shipped). C keeps its
terminal outlines by design.

Classification and reasoning (WCAG 1.4.11 needs 3:1 only for visual information
that is *required* to identify a control):
* **Decorative (no 3:1; floor ratio only so the UI does not melt together):**
  dividers (`--b-div`), card and panel edges (`--b-card`), the diff and code-block
  frames, the plan card, the status-strip separator, panel header rules.
* **Functional, identified without a dark outline (enforced):** text buttons,
  pills and segments are identified by their label (>= 4.5:1) and a fill that
  steps from the surface (`--panel`/`--sunken`/`--accent-soft`); selected items add
  an accent fill and accent text. Text inputs (composer, file filter) are
  identified by placeholder text (>= 4.5:1), the adjacent primary button or search
  icon, and a visible-but-soft resting edge (`--b-ctl`, floor 1.25:1); on focus
  the edge becomes the accent and a second ring appears (A: 3px accent-soft halo; B:
  2px accent outline with a 2px offset), all >= 3:1. The `--edge` token (3:1) still
  exists: C uses it at rest, and A/B can switch to it as a one-line "strict"
  fallback if a stricter reading of 1.4.11 is required.
* The judgement call (soft resting edge on text inputs) is deliberate and recorded
  here for review by security/accessibility owners; focus visibility is not softened.

**Structure (what is shared).** One component tree and one markup. Looks differ
by CSS only, scoped as `:root[data-look="a|b|c"] ...` inside one `ui.css`, plus
a few markup slots that every look contains and the others hide (brand mark,
empty-state copy). Layout differences (rail vs header actions, strip position,
status line) are grid/flex/`order` changes, not different React trees; the
semantic contract (section 3) and the behaviour (one popover at a time, jump to
latest, keyboard tree, bottom sheets <= 640px) never vary per look.

**Switching mechanism.**
* Entry: an **Appearance** button (desktop: header right / A's left rail; mobile:
  bottom bar next to Theme, opening a bottom sheet <= 40vh). The panel is a
  dialog with a Look radio group (swatch + name) and a Theme radio group
  (System / Light / Dark). Esc closes and returns focus.
* Look source, in order: `?look=a|b|c` (whitelisted; not persisted) >
  `localStorage["treer-launcher-look"]` (whitelist-validated, every access in
  try/catch) > default **b**.
* Theme source, in order: parent `postMessage {type:"treer:embed-theme"}` (only
  from `window.parent`) or `?theme=light|dark` (explicit, not persisted) > stored
  manual mode `localStorage["treer-launcher-theme"]` (`system|light|dark`) >
  `prefers-color-scheme`. The header theme button toggles light/dark and stores
  the mode.
* A tiny synchronous `boot.js` in `<head>` sets `data-look` and `data-theme` on
  `<html>` before first paint (no flash) and exposes a small API used by the React
  UI. No inline scripts.

**One file or per-look files?** One `ui.css`. Measured on the prototype
(unminified sources, gzip in brackets): shared 10.0 KB (3.2), A 12.3 (3.4), B
13.8 (3.9), C 12.8 (3.5); built `ui.css` 58.3 KB (10.8); esbuild-minified
57.3 KB (10.2), since the sources are already compact. Splitting per look would
save about 26 KB raw / 7 KB gzip for a user who never switches, but costs files
against the budget (<= 5), a loading flash on first switch and an async-load
failure mode on the theme path. Not worth it at this size; revisit with a fourth
look.

**Costs and mitigations.** Three looks triple the visual surface to test and
maintain. (1) Tokens and markup are shared; looks are CSS only, so logic bugs are
fixed once. (2) **Test matrix**: 3 looks x light/dark x desktop/390px x key states
(chat, empty, loading, error, expanded composer, each popover, files, tasks,
appearance menu) with screenshot regression plus no-overflow, no-console-error and
no-CSP-violation assertions. (3) The contrast script covers every look and theme
on every build. (4) New components use only the token contract and ship styles
for all three looks in the same change. (5) Dropping a look later is deleting one
scoped section.

**Default look recommendation: B.** B is the continuity choice (the green, warm
greys and white panels users know), the most readable for long transcripts, and the
user's favourite. Counter-argument on record: A is denser and friendlier to
narrow panes (embedded iframes), so it may suit power users better; the default is
one constant in `boot.js`. The user decides.

## 3. Design tokens

All looks implement one semantic token contract; components read only these:
`--bg --panel --sunken --line --line-strong --edge --fg --fg2 --fg3 --accent
--accent-fg --accent-soft --danger --warn --ok --add --del`, radii `--r --rs`,
fonts `--font-ui --font-prose --font-mono`, `--fs --lh`, `--icon --stroke --cap
--join`, `--dur`, `--menu-top`, `--sheet-r`. Values per look live in
`directions/{a,b,c}.css` today and in `ui/src/looks/*.css` once implementation
starts. `--line-strong` is decorative; text-input and composer boundaries use
`--edge` (>= 3:1).

Contrast: `directions/contrast.mjs` reads the hex tokens of every look and theme
and checks text >= 4.5:1 on bg/panel/sunken, accent on accent-soft, diff text and
gutters, the inverse status line (C), input edges and the focus ring >= 3:1.
**Result: 222 checks, all pass** (text, accent-on-soft, diff, strict edge, focus ring, plus floors for the soft role borders). It moves into the test suite at implementation
time and runs on every build.

Theme selection is unchanged: `data-theme` on `<html>` set by a tiny module
(same logic as `theme.js`: `?theme=`, `treer:embed-theme` message with
`event.source === window.parent`, `matchMedia`). The Appearance panel and header button are the manual path; the mode is stored
in `localStorage["treer-launcher-theme"]`, and an explicit `?theme=` or parent
message wins for the session. Look is stored in `treer-launcher-look`. Both
values are allowlist-validated on read (section 6).

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

Incorporates security-claude review round 1 (d4893f3); starred items are
implementation gates.

* **One HTML sink.** `Markdown.tsx` is the only component that sets `innerHTML`
  (existing marked -> DOMPurify). DOMPurify config stays as today plus explicit
  `FORBID_ATTR: ["style"]`, no `ADD_ATTR`/`ADD_TAGS`, and every link gets
  `rel="noopener noreferrer"`. File names, paths, diffs, tool output, errors,
  queue text, sender/name, goal `syncError`: React text nodes only. i18n rich text
  uses component slots, never raw HTML.
* **★ Static checks on source, not the bundle.** ESLint (`react/no-danger`
  allowlisting only `Markdown.tsx`; ban `eval`, `new Function`, `document.write`).
  Scanning the built bundle would false-positive on React/marked/DOMPurify.
* **★ Runtime checks in Playwright.** Fail on any `securitypolicyviolation` or
  console error; XSS fixture strings (`<img onerror>`, `javascript:` links,
  `<svg onload>`, `style=`, `data:` URLs) injected through markdown, file names,
  diffs, task output, errors, queue text, goal `syncError`, and i18n
  interpolation must stay inert.
* **★ `safeUrl()`.** Any `href`/`src` driven by data other than the markdown
  pipeline (file links, image viewer, task-output links) goes through one shared
  `safeUrl()`: http(s)/mailto only; images only `data:image/(png|jpeg|gif|webp)`
  or `blob:`. ImageViewer shows SVG only through `<img>`, never inline, and
  revokes blob URLs after use. The WebSocket URL is `new URL("ws",
  document.baseURI)`; no URL from page data is accepted.
* **★ Prototype-pollution safety.** UI state keyed by server strings (item id,
  queue id, task id, file path) lives in `Map` or `Object.create(null)`; no deep
  merge or `Object.assign` of server JSON into shared config. `localStorage`
  holds only theme and locale and both are allowlist-validated on read; never a
  token, URL or content.
* **CSP.** `script-src 'self'` unchanged (no inline scripts, no eval). At the
  switch, drop `'unsafe-inline'` from `style-src` and add `object-src 'none';
  base-uri 'self'; form-action 'none'`. React sets styles through CSSOM, which
  `style-src` does not restrict, so `style={{...}}` is allowed; what CSP blocks
  is `style="..."` in markup and `<style>` elements, which is desirable for
  markdown. `frame-ancestors` is not hardcoded; launcher-dev/Proxy set it with the
  embedding headers. Later hardening candidate: `require-trusted-types-for
  'script'` (feasible because of the single sink); not required now.
* **Migration period.** While both UIs coexist the legacy CSP must not leak into
  the new page: serve the strict policy for `ui.html` (per-path) or the stricter
  of the two. The commit that deletes the legacy UI tightens the header and adds
  a regression assertion that the response header lacks `'unsafe-inline'`.
  **[LD]**
* No token or secret is read, rendered or stored. UI is not an authentication
  source (Proxy forwarding markers + Host/Origin guards remain); `api/session/export`
  stays unexposed to the browser. Text export and clipboard are client-only and
  read data already in the page.

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
tests use `node:test` + `react-dom/server`). **Dependency gates (security-claude, round 1).**
* Exact versions pinned (no `^`/`~`); `package-lock.json` committed with
  integrity hashes; installs use `npm ci --ignore-scripts`.
* The review list includes transitive dependencies (e.g. `scheduler`) with
  licenses, and the output of `npm ls` and `npm pkg get scripts` showing no
  install/postinstall scripts.
* When the lock is edited: `npm audit --omit=dev` over react, react-dom, marked
  and dompurify, with the output attached. I make no claim about vulnerability
  status today.
* Upgrading marked or dompurify (the only HTML-injection surface) needs its own
  review.

## 8. Build, artifacts, payload **[LD]**

* Source in `launchers/agent/ui/` (TSX, CSS). `scripts/build-ui.mjs` runs esbuild
  (bundle, minify, `format=iife`, `target=es2022`, `jsx=automatic`,
  `define NODE_ENV=production`, `legalComments=inline`, deterministic, no
  sourcemap in the payload).
* Output: `public/ui.html`, `public/boot.js`, `public/ui.js`, `public/ui.css` — **4 files, short
  names, flat** (no nested dirs → Windows long-path safe). `markdown.js` is
  folded into `ui.js` (the legacy `markdown.js` stays until deletion). Relative
  `<base href="./">`; the script tag is `<script src="ui.js" defer>` (classic IIFE
  avoids module-MIME and base-URL edge cases in the iframe tunnel).
* **Reproducibility gate (security-claude).** CI rebuilds with the pinned
  Node, esbuild and lockfile and compares byte-for-byte with the committed
  `ui.js`/`ui.css`/`ui.html`; any difference fails. Production `define`, no source
  map, no timestamps, legal comments either off or collected into a LICENSES file;
  an `esbuild --metafile` audit asserts no absolute paths, hostnames or test
  fixture strings in the output. Changes to the artifacts and the lock need CODEOWNERS
  review. The manifest stays an exact file set with sha256; bundle-contract limits
  are not relaxed.
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

**Stage 1 as built (measured).** Sources under `launchers/agent/ui/`; `npm run
build:ui` writes `public/ui.html` 466 B, `boot.js` 1,041 B, `ui.js` 244,002 B
(React 19.3.1 + react-dom + scheduler, minified), `ui.css` 57,940 B: **4 files,
303,449 B total** against the 600,000 B budget. `npm run check:ui` is the
reproducibility, budget, leaked-path and source-pattern gate; `prepare-bundle.mjs`
re-runs the byte comparison; the UI tooling scripts are excluded from payloads.
`ui.html` is served with `style-src 'self'; object-src 'none'; base-uri 'self';
form-action 'none'` (legacy page unchanged until the switch).

Dependency evidence (react 19.3.1, react-dom 19.3.1, `@types/react` 19.3.0,
`@types/react-dom` 19.3.0, all pinned exactly, dev dependencies because they are
bundled into the committed artifact): transitive `scheduler` 0.28.0 and `csstype`
3.2.3 (types); all MIT; none declares an install/postinstall script (csstype lists
only maintainer scripts such as `prepublish`, which npm does not run for
dependencies); installs use `npm ci --ignore-scripts`. `npm audit` reports **one low
advisory in the existing `dompurify@3.4.14`** (GHSA-p98j-92pf-mc4p and
GHSA-6688-9rhm-gjv2, both about the `IN_PLACE` option, fixed in 3.4.17). The
launcher's markdown pipeline calls `sanitize(string, ...)` without `IN_PLACE`, so it is
not exercised, but the bump needs its own review (outside this change). No advisory
affects react, react-dom, scheduler or marked.

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
queue count + cancel (via the Queue popover) · draft retention on failure · steer downgrade notice · interrupt ·
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

0. Review round 1 (leader): status strip (section 2.2a) and contrast results
   (section 3) added. Resolved with launcher-dev: output names are `ui.js`/`ui.css` (fixed, no hash
   chunks); no invented durations/hierarchy (§2.1); images stay self/data/blob.
1. **[LD]** Commit built `ui.js`/`ui.css` (recommended, §8) vs. build in
   `prepare-bundle` only (breaks `bootstrap.sh` git installs).
2. **[LD]** Provide optional `usage`, `endedAt`, `parentId` per §4 — which
   harnesses can fill them? Claude Code transcripts carry usage; Codex and ACP
   unknown.
3. **[security-claude]** Resolved: drop `'unsafe-inline'`, add `object-src 'none'; base-uri
   'self'; form-action 'none'` (§6).
4. Resolved: look and theme mode persist in `localStorage` (guarded); default look B.
5. **[leader/user]** Default locale: browser language, or English unless
   `?lang=zh`? (Recommended: browser language.)
6. Mobile "bottom bar": proposed content = Files, Tasks, Theme; Model/effort stays
   in the composer chip. Confirm with prototype.

## 15. Static prototypes

`2026-10-14-launcher-react-ui/directions/` holds the sources of three interactive
static prototypes (shared `template.html`, `shared.css`, `app.js`; per-direction
`a.css`, `b.css`, `c.css`; `build.mjs`; `contrast.mjs`). They are mock-only
HTML/CSS/JS (no framework, no network). Views: chat, empty, loading, error;
panels: files (keyboard tree, filter, preview), tasks; Goal/Plan/Queue popovers;
compact/expanded composer; light/dark; 390px layout with bottom sheets and tab
bar. Not drawn: goal editor dialog, image viewer, model/effort menu, `zh` strings.

## 16. Implementation record

**Where.** Branch `launcher-react-ui` (not pushed). Sources `launchers/agent/ui/`; committed
artifacts `public/{index.html,boot.js,ui.js,ui.css}` (the page is `index.html`; there is no
`ui.html`). Commit groups, each reviewable and revertible on its own:

| Stage | Commits | Content |
| --- | --- | --- |
| Design | prototypes and this document | three looks, token contract, contrast script |
| 1 Pipeline | scaffold, strict CSP | esbuild build, `check:ui` byte comparison, AST source scan, pinned deps, looks and appearance switching, three theme sources |
| 2 Conversation | conversation, tests | markdown sink, folded activity, diffs, plans, paging with scroll anchoring, jump to latest |
| 3 Composer | strip/goal/model/queue, tests | status strip with one popover, goal lifecycle, model and effort, Steer/Queue/interrupt, zh dictionary |
| 4 Panels | files/tasks/dialogs, tests | read-only file browser, task panel, text and image dialogs, Markdown export |
| 5 Parity and switch | contract port, **Make the React UI the default page**, **Remove the legacy browser UI** | the legacy mock-contract assertions ported; default switched; legacy deleted separately |

**Measured.** Four files, 437,118 bytes of the 600,000 byte budget: `ui.js` ~328 KB (React 19.3.1,
marked, DOMPurify, app), `ui.css` ~64 KB (shared plus three looks), `boot.js` ~1 KB, `index.html`
< 1 KB. `npm test`: 158 tests green, including the ported runtime-controls contract fixture
(queue, tasks, goal, slash commands, deliberate 503/204 responses, phone panels) and 8
unit tests; `tests/browser-ui.mjs` drives the real server with fake Codex and fake Claude
processes (looks x themes, CSP violations limited to the two inline handlers the hostile
fixture plants, phone layout without overflow). The Windows smoke UI step passes in bundle
mode on macOS against the prepared payload.

**Long transcripts (risk R2).** A fixture with 3,045 rendered messages (about 11,000 DOM nodes,
reached by 60 "Load earlier" clicks in 4.2 s) kept typing frames at about 17 ms and the heap at
13 MB, so no virtualization was added; unchanged turns keep their identity across snapshots.

**Parity matrix result.** Transcript and older paging, streaming, steer/queue/Tab, queue
cancel, draft retention, steer downgrade notice, interrupt, model and effort, goal
(set, edit, pause/resume, confirm-clear, retry, reconcile, `/goal` commands, completion
notices, budget and usage), tasks and output polling, files (browse, preview, refresh,
truncation, text-only), diffs, plans, reasoning, markdown through DOMPurify, theme
(`?theme`, parent message, system), 390px layout and shared panel slot, iframe `<base>`,
no console errors: all covered by automated checks. Deliberate differences from the
legacy page:
* The queue is a count in the strip with a popover for the entries (design 2.2a).
* A goal whose status is `cleared` collapses to "Set goal" instead of showing a Cleared card.
* The model and effort pickers sit in a popover opened from the composer chip.
* Goal notices and statuses use words, not glyph prefixes.
* Task output, file text and errors are unchanged in meaning; markdown images that are not
  same-origin, `data:` raster or `blob:` render as their alt text.

**Bugs the port found and fixed.** A button pressed while the field was focused could be moved
away from under the pointer by the layout change; a send in flight wiped a newer draft; a
queue popover reopened by itself after its queue drained; a 204 acknowledgement was treated
as an error; a label wrapping a field leaked the field's value into its accessible name.

**Not done or not verifiable here.** Native Windows runtime acceptance on a real machine
and the official Controller path (the smoke test ran in bundle mode on macOS); the
real-provider goal script (`test:goal:live`) was ported to the new DOM but not run (it needs
provider credentials); security-claude's review of the final dependency list, CSP and
sinks; release-claude's gate. The pre-existing `dompurify@3.4.14` low advisory (IN_PLACE
option, not used here) still needs its own upgrade review.

**Maintenance.** A new component must style itself in all three looks in the same change;
`npm run check:ui` and `npm run test:browser` are the gates; the three looks and the token
contract live in `ui/src/styles/`.
