import { test, expect, type Page, type Locator } from '@playwright/test'
import { mkdir } from 'node:fs/promises'
import { fixture } from './fixtures/workspace-tabs'

async function setup(page: Page, width = 1360, theme = 'light', lang = 'en') {
  await page.setViewportSize({ width, height: 900 })
  await page.addInitScript(({ theme, lang }) => {
    localStorage.setItem('treer-theme', theme)
    localStorage.setItem('treer-lang', lang)
  }, { theme, lang })
  await fixture(page)
  const tree = {
    folders: [
      { folder_id: 'parent', parent_id: null, name: 'Parent' },
      { folder_id: 'child', parent_id: 'parent', name: 'Child with a long folder name' },
      { folder_id: 'empty', parent_id: null, name: 'Empty' },
    ],
    placements: [
      { agent_id: 'tab-1', folder_id: 'child', position: 0 },
      { agent_id: 'tab-7', folder_id: 'parent', position: 0 },
    ],
  }
  const moves: unknown[] = []
  await page.route('**/workload-tree', route => route.fulfill({ json: tree }))
  await page.route('**/workload-tree/placements/**', route => {
    moves.push(route.request().postDataJSON())
    return route.fulfill({ json: {} })
  })
  await page.goto('/')
  await expect(page.locator('[data-agent-id="tab-0"] [data-tree-primary]')).toHaveCount(1)
  await showTree(page)
  return { moves }
}
async function showTree(page: Page) {
  if (await page.locator('#tree-sidebar').isVisible()) return
  // Stable aria-controls also works in the translated UI.
  await page.locator('button[aria-controls="tree-sidebar"]:visible').click()
  await expect(page.getByRole('tree')).toBeVisible()
}
function item(page: Page, name: string) { return page.getByRole('treeitem', { name, exact: true }) }
function folderRow(page: Page, name: string) { return item(page, name).locator(':scope > div').first() }
async function bounds(row: Locator) {
  await row.scrollIntoViewIfNeeded()
  const b = await row.boundingBox()
  if (!b) throw new Error('Missing row bounds')
  return b
}
async function fullPrimary(row: Locator) {
  const a = await bounds(row)
  const b = await bounds(row.locator('[data-tree-primary]'))
  for (const key of ['x', 'y', 'width', 'height'] as const) expect(Math.abs(a[key] - b[key])).toBeLessThan(0.6)
}
// Corners sit inside the existing 4 px rounded paint (not its transparent cutout).
const positions = (width: number, height: number) => [
  { x: 2, y: 2 }, { x: width - 2, y: 2 },
  { x: 2, y: height - 2 }, { x: width - 2, y: height - 2 },
  { x: 1, y: height / 2 }, { x: width - 1, y: height / 2 },
  { x: width * .65, y: height / 2 },
]
for (const width of [1360, 390]) for (const theme of ['light', 'dark']) {
  test(`Tree hit area fills folder Agent App rows ${width} ${theme}`, async ({ page }) => {
    const lang = theme === 'dark' ? 'zh' : 'en'
    await setup(page, width, theme, lang)
    for (const name of ['Parent', 'Child with a long folder name', 'Empty']) {
      const row = folderRow(page, name)
      await fullPrimary(row)
      const b = await bounds(row)
      if (width === 390) expect(b.height).toBeGreaterThanOrEqual(44)
      for (const position of positions(b.width, b.height)) {
        const previous = await item(page, name).getAttribute('aria-expanded')
        await row.click({ position })
        await expect(item(page, name)).toHaveAttribute('aria-expanded', previous === 'true' ? 'false' : 'true')
      }
      // Leave ancestors expanded for the next depth.
      if (await item(page, name).getAttribute('aria-expanded') !== 'true') await row.locator('[data-tree-primary]').click()
    }
    for (const [name, id] of [['Beta', 'tab-2'], ['Alpha', 'tab-1'], ['Archive', 'tab-7']]) {
      const row = item(page, name)
      await fullPrimary(row)
      const b = await bounds(row)
      for (const position of positions(b.width, b.height)) {
        await row.click({ position })
        await expect(page.locator(`#workspace-panel-${id}`)).toBeVisible()
        await showTree(page)
      }
      await expect(row.locator('[data-tree-primary] a,[data-tree-primary] button')).toHaveCount(0)
      const menu = row.locator('[data-tree-actions]')
      if (width === 390) expect((await bounds(menu)).height).toBeGreaterThanOrEqual(44)
      await menu.click()
      await expect(page.getByRole('menu')).toBeVisible()
      await page.keyboard.press('Escape')
    }
    const parent = item(page, 'Parent')
    await folderRow(page, 'Parent').locator('[data-tree-actions]').click()
    await expect(page.getByRole('menu')).toBeVisible()
    await page.keyboard.press('Escape')
    await expect(parent).toHaveAttribute('aria-expanded', 'true')
    await expect.poll(() => page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
    // Evidence has synthetic fixture data only, outside the repository.
    await page.addStyleTag({ content: '[role=tree] [data-tree-primary]{outline:1px solid currentColor;outline-offset:-1px}' })
    const dir = process.env.TREE_HITAREA_SHOTS
    if (dir) { await mkdir(dir, { recursive: true }); await page.screenshot({ path: `${dir}/${width}-${theme}-${lang}.png` }) }
  })
}
test('Tree full-row primary keeps keyboard navigation, native links and drag targets', async ({ page }) => {
  const { moves } = await setup(page)
  const parent = folderRow(page, 'Parent').locator('[data-tree-primary]')
  await parent.focus()
  await page.keyboard.press('ArrowRight')
  await expect(item(page, 'Parent')).toHaveAttribute('aria-expanded', 'true')
  await page.keyboard.press('ArrowRight')
  await expect(folderRow(page, 'Child with a long folder name').locator('[data-tree-primary]')).toBeFocused()
  await page.keyboard.press('Space')
  await expect(item(page, 'Child with a long folder name')).toHaveAttribute('aria-expanded', 'true')
  const alpha = item(page, 'Alpha').locator('[data-tree-primary]')
  await alpha.focus()
  expect(await alpha.getAttribute('href')).toContain('/agents/tab-1/interface/ui/')
  await page.keyboard.press('Enter')
  await expect(page.locator('#workspace-panel-tab-1')).toBeVisible()
  await alpha.focus()
  await page.keyboard.press('ArrowRight')
  await expect(item(page, 'Alpha').locator('[data-tree-actions]')).toBeFocused()
  await page.keyboard.press('ArrowLeft')
  await expect(alpha).toBeFocused()
  // A real mouse drag beginning in the formerly dead upper-left padding.
  const source = await bounds(item(page, 'Beta'))
  const target = await bounds(folderRow(page, 'Empty'))
  await page.mouse.move(source.x + 2, source.y + 2)
  await page.mouse.down()
  await page.mouse.move(target.x + 12, target.y + target.height / 2, { steps: 12 })
  await page.mouse.move(target.x + 13, target.y + target.height / 2)
  await page.mouse.up()
  await expect.poll(() => moves.length).toBe(1)
  expect(moves[0]).toMatchObject({ folder_id: 'empty' })
})
