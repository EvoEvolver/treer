import { test, expect, type Page } from '@playwright/test'
import { mapSnapshot, mockTreeMapApi } from './fixtures/treemap'
import { browseTreeOnMobile } from './fixtures/navigation'

const rows = [
  { agent_id: 'a-work', name: 'builder', kind: 'codex', status: 'working', server_id: 'local' },
  { agent_id: 'a-block', name: 'waiter', kind: 'claude', status: 'blocked', server_id: 'local' },
  { agent_id: 'a-fail', name: 'crasher', kind: 'command', status: 'failed', server_id: 'local' },
  { agent_id: 'a-idle', name: 'sleeper', kind: 'claude', status: 'idle', server_id: 'local' },
  { agent_id: 'a-exit', name: 'finisher', kind: 'command', status: 'exited', server_id: 'local' },
  { agent_id: 'a-off', name: 'remote-one', kind: 'codex', status: 'idle', server_id: 'remote' },
  { agent_id: 'a-run', name: 'service', kind: 'app', status: 'running', server_id: 'local' },
]
async function open(page: Page, width = 1360) {
  await page.setViewportSize({ width, height: 800 })
  await mockTreeMapApi(page)
  await page.route('**/snapshot', (route) => route.fulfill({ json: { ...mapSnapshot, agents: rows } }))
  await page.route('**/workload-tree', (route) => route.fulfill({ json: { folders: [], placements: [] } }))
  await page.route('**/groups', (route) => route.fulfill({ json: { groups: [] } }))
  await page.route('**/policy-provider', (route) => route.fulfill({ status: 404, json: {} }))
  await page.goto('/')
  await browseTreeOnMobile(page)
  await expect(page.locator('#tree-sidebar').getByRole('treeitem')).toHaveCount(rows.length)
}
const tree = (page: Page) => page.locator('#tree-sidebar')
const link = (page: Page, name: string) => tree(page).getByRole('treeitem', { name, exact: true }).getByRole('link')
// A CSS locator: while a menu is open Radix marks the rest of the page aria-hidden, so role queries cannot find the trigger.
const more = (page: Page, name: string) => tree(page).locator(`button[data-tree-actions][aria-label="Actions for ${name}"]`)
const opacity = (locator: ReturnType<Page['locator']>) => locator.evaluate((element) => Number(getComputedStyle(element).opacity))

for (const theme of ['light', 'dark']) test(`Tree rows are one line with a status dot only for non-idle states (${theme})`, async ({ page }, info) => {
  await page.addInitScript((value) => localStorage.setItem('treer-theme', value), theme)
  await open(page)
  // One line: the row text is just the name, no kind/machine/status second line.
  for (const row of rows) await expect(link(page, row.name)).toHaveText(row.name)
  await expect(tree(page)).not.toContainText(' · ')
  // Dots exist only for working, blocked, failed, exited and offline-machine rows, each with a text name.
  const dots = tree(page).locator('[data-tree-status]')
  await expect(dots).toHaveCount(5)
  const expected: Record<string, string> = { builder: 'working', waiter: 'blocked', crasher: 'failed', finisher: 'exited', 'remote-one': 'offline' }
  for (const [name, status] of Object.entries(expected)) {
    const dot = link(page, name).locator('[data-tree-status]')
    await expect(dot).toHaveAttribute('role', 'img')
    await expect(dot).toHaveAttribute('aria-label', status)
    // The row's link name includes the status.
    await expect(tree(page).getByRole('link', { name: `${name} ${status}`, exact: true })).toBeVisible()
  }
  for (const name of ['sleeper', 'service']) await expect(link(page, name).locator('[data-tree-status]')).toHaveCount(0)
  // Same colours as the Overview legend chips (the Overview chips use the status text colours).
  const colour = (selector: string) => page.locator(selector).first().evaluate((element) => getComputedStyle(element).color)
  const dotColour = (name: string) => link(page, name).locator('[data-tree-status]').evaluate((element) => getComputedStyle(element).backgroundColor)
  const working = await colour('[aria-label="Filter working"] svg'), attention = await colour('[aria-label="Filter need attention"] svg')
  expect(await dotColour('builder')).toBe(working)
  expect(await dotColour('waiter')).toBe(attention)
  expect(await dotColour('crasher')).toBe(attention)
  expect(await dotColour('remote-one')).toBe(attention)
  // The bottom user row is slim on desktop (same density as the Tree rows).
  const userRow = await tree(page).getByRole('button', { name: 'User menu' }).evaluate((button) => ({ row: button.parentElement!.getBoundingClientRect().height, trigger: button.getBoundingClientRect().height, collapse: button.parentElement!.querySelector('button')!.getBoundingClientRect().height }))
  expect(userRow.row).toBeLessThan(40)
  expect(userRow.trigger).toBeLessThan(32)
  expect(userRow.collapse).toBeLessThan(32)
  // One compact line per row.
  const heights = await tree(page).getByRole('treeitem').evaluateAll((items) => items.map((item) => item.getBoundingClientRect().height))
  for (const height of heights) expect(height).toBeLessThan(40)
  await page.screenshot({ path: info.outputPath(`tree-compact-${theme}-desktop.png`) })
})

test('the ⋯ button shows on hover, keyboard focus and while its menu is open, and the tooltip never covers it', async ({ page }) => {
  await open(page)
  await expect.poll(() => opacity(more(page, 'builder'))).toBe(0)
  await link(page, 'builder').hover()
  await expect.poll(() => opacity(more(page, 'builder'))).toBe(1)
  // The status tooltip is open on hover and does not block the neighbouring button.
  await expect(page.getByRole('tooltip')).toContainText('working')
  await more(page, 'builder').click()
  await expect(page.getByRole('menuitem', { name: 'Logs' })).toBeVisible()
  await page.mouse.move(700, 600)
  await expect.poll(() => opacity(more(page, 'builder'))).toBe(1)
  await page.keyboard.press('Escape')
  // Closing the menu returns focus to its trigger, so the row (focus-within) keeps showing it until focus leaves.
  await expect(more(page, 'builder')).toBeFocused()
  await expect.poll(() => opacity(more(page, 'builder'))).toBe(1)
  await page.evaluate(() => (document.activeElement as HTMLElement).blur())
  await page.mouse.move(700, 600)
  await expect.poll(() => opacity(more(page, 'builder'))).toBe(0)
  // Keyboard focus within the row reveals it and shows the same status tooltip.
  await link(page, 'waiter').focus()
  await expect.poll(() => opacity(more(page, 'waiter'))).toBe(1)
  await expect(page.getByRole('tooltip')).toContainText('blocked')
  expect(await opacity(more(page, 'sleeper'))).toBe(0)
  // Right-click (#109) still opens the row actions.
  await link(page, 'sleeper').click({ button: 'right' })
  await expect(page.getByRole('menuitem', { name: 'Logs' })).toBeVisible()
})

test.describe('touch', () => {
  test.use({ hasTouch: true, isMobile: true })
  test('the ⋯ button is always visible on a coarse pointer and keeps its 44px target', async ({ page }, info) => {
    await open(page, 390)
    for (const name of ['builder', 'sleeper', 'service']) {
      await expect.poll(() => opacity(more(page, name))).toBe(1)
      const box = (await more(page, name).boundingBox())!
      expect(box.width).toBeGreaterThanOrEqual(43.5)
      expect(box.height).toBeGreaterThanOrEqual(43.5)
    }
    // The bottom user row keeps a 44px target on touch.
    expect((await tree(page).getByRole('button', { name: 'User menu' }).boundingBox())!.height).toBeGreaterThanOrEqual(43.5)
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
    await page.screenshot({ path: info.outputPath('tree-compact-390.png') })
  })
})
