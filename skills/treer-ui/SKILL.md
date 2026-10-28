---
name: treer-ui
description: Treer UI conventions and review checklist for the Console (web/), the launcher Agent UI and App browser UIs. Repo-development guidance; not printed by `treer --skill`.
---

# Treer UI conventions

Use this when you change or review a screen in `web/`, `launchers/agent/ui` or an
App's browser UI. It is a checklist, not a design system: tokens, components and
copy already exist, so reuse them and never pick a new visual style per feature.
Checklist ordering is adapted in our own words from the priority idea in
[ui-ux-pro-max-skill](https://github.com/nextlevelbuilder/ui-ux-pro-max-skill)
(MIT); no text or data is copied.

Several rules name Console (`web/`) implementations (`workloadLinkProps`,
`menuItems`, `appOwnUrl`, `en.ts`/`zh.ts`, the singlefile build). The launcher UI
and App UIs follow the same intent with their own equivalent components, build
and i18n; do not apply the Console mechanics mechanically.

Work through the sections in order. An earlier failure outranks a later polish.

## 1. Accessibility (blocks handoff)

- Text contrast 4.5:1, non-text (focus ring, control edge, status dot) 3:1, in
  light and dark. Run `pnpm run check:contrast` in `web/` after touching tokens;
  document any exception in `web/README.md` and `docs/quality.md`.
- Every control is reachable and operable by keyboard with a visible focus ring.
  Icon-only buttons have an accessible name (`aria-label` through `t()`).
- Never convey state by colour alone: pair status dots with a label or icon.
- Dialogs trap focus and return it on close; errors use a `role="alert"` region.
- `prefers-reduced-motion: reduce` disables animation.

## 2. Touch and interaction

- Interactive targets are at least 44x44px on touch layouts; keep spacing between
  neighbours so a thumb cannot hit both.
- Anything that navigates is a real `<a href>` (stable-ID deep link) so
  ctrl/cmd/middle click and "copy link" work; a plain left click may still open
  in place. In the Console use `workloadLinkProps`; do not hand-roll `onClick` navigation.
- Nothing essential is hover-only; hover reveals duplicate a visible or
  keyboard path.
- An App row, block or menu item that opens in a new tab uses the App's own
  server-provided `public_url` through `appOwnUrl` (https, no credentials, real
  DNS name, not the Console or Proxy origin) and falls back to the Console deep
  link. Cross-origin opens use `noopener,noreferrer`. Agents keep the Console
  link.

## 3. Menus and destructive actions

- The tree "⋯" menu and the right-click menu share one `menuItems` list; add an
  action once. Shift+F10 and the Menu key open it.
- Stop and Delete come last, after a separator, and each asks for confirmation.
  The tab menu has no Stop or Delete (accidental-click risk).
- After a menu closes, focus returns to its trigger unless a dialog took it.

## 4. Performance

- Console: the singlefile build stays one `dist/index.html`; no new runtime dependency, CDN,
  web font or remote asset. App UIs use app-relative assets only
  (`apps/GUIDELINES.md`). Check the `dist/index.html` size when adding code.
- Avoid layout shift: reserve space for loading content and skeleton or hold the
  previous view; do not poll faster than the data changes.

## 5. Layout, density and empty states

- Use the shared tokens (`web/src/index.css` HSL variables) and existing shadcn
  components; no ad-hoc hex colours or one-off radii.
- Keep the information density of the neighbouring screen. Tree, Overview and
  map share hierarchy and sibling order; never regroup into a different order.
- An empty state says what to do next, once. Do not add a second hint when the
  screen already has a primary prompt; keep deliberate footers.
- Long names, paths and IDs truncate with a title or wrap; they never push
  controls off screen.

## 6. Typography and colour

- Text and status colours come from tokens; status text uses the
  `status-*-text` tokens that pass contrast on hover backgrounds too.
- The documented dark-theme exceptions (blue/red as plain text reach 3:1; input
  borders stay a 1.4:1 hairline) are not a licence for new ones.

## 7. Motion

- Short, purposeful transitions only (state change, enter/exit). No decorative
  or looping animation; it must stop under reduced motion.

## 8. Forms and feedback

- Every field has a visible label; validate inline and keep the user's input on
  error. Say why a control is disabled.
- Success and failure feedback are visible and announced; an error never
  silently falls back (a missing workspace or organization shows an error rather
  than the default workspace).
- Times entered by users are local time; the stored form is UTC.

## 9. Navigation and routing

- URLs are the state: org, workspace and workload IDs stay in the path
  (`/orgs/:o/workspaces/:w/workloads/<stable id>`); focus mode is `?focus=1`.
- Back behaves predictably; a deep link to an inaccessible target shows an error.

## Internationalization (every user-visible string)

- Console: add keys to `en.ts` and `zh.ts` and call `t()` in render code; never evaluate
  `t()` at module load. Use `tnodes`, `statusLabel`, `roleLabel`, `intlLocale()`.
- Terms follow the launcher UI (`launchers/agent/ui/src/i18n`: 工作中, 空闲,
  已阻塞, Agent, 工作区). App UIs (for example `apps/issues/ui`,
  `apps/telegram/ui`, `apps/mail/web`) are not in the Console dictionary: follow
  their own language handling.
- Check long Chinese and English strings both fit.

## Pre-delivery pass

1. Desktop and 390px viewport, light and dark: no horizontal scroll, nothing
   clipped, 44px targets, readable contrast.
2. Keyboard-only walk through the changed flow; screen-reader names on icons.
3. Empty, loading, error and long-content states of the changed view.
4. Switch language en/zh.
5. `pnpm run check:contrast`, `typecheck`, build and the focused Playwright
   spec (`web/e2e/` keeps only key journeys; add one only for a new journey).
   Keep screenshots outside the repository.
6. Update `web/README.md` (or the App README) in the same change.
