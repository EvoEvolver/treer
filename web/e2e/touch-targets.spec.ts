import { test, expect, type Page } from '@playwright/test'
import { mockTreeMapApi } from './fixtures/treemap'
import { browseTreeOnMobile } from './fixtures/navigation'

async function open(page: Page, width: number) {
  await page.setViewportSize({ width, height: 844 })
  await mockTreeMapApi(page)
  await page.route('**/groups', (route) => route.fulfill({ json: { groups: [] } }))
  await page.route('**/policy-provider', (route) => route.fulfill({ status: 404, json: {} }))
  await page.routeWebSocket(/\/events$/, () => {})
  await page.goto('/')
  await expect(page.getByRole('heading', { name: 'Workspace overview' })).toBeVisible()
  await browseTreeOnMobile(page)
}
async function size(locator: ReturnType<Page['locator']>) {
  await expect(locator).toBeVisible()
  return locator.evaluate((element) => { const rect = element.getBoundingClientRect(); return { width: rect.width, height: rect.height } })
}
// The app re-renders when the snapshot arrives, so poll until the control settles at its final size.
async function expectTouch(locator: ReturnType<Page['locator']>, axes: ('width' | 'height')[] = ['width', 'height']) {
  for (const axis of axes) await expect.poll(async () => (await size(locator))[axis], { message: String(locator) }).toBeGreaterThanOrEqual(43.5)
}

for (const theme of ['light', 'dark']) test(`Tree sidebar controls and menu rows are at least 44px on a 390px phone (${theme})`, async ({ page }, info) => {
  await page.addInitScript((value) => localStorage.setItem('treer-theme', value), theme)
  await open(page, 390)
  const sidebar = page.locator('#tree-sidebar')
  const controls = [
    sidebar.getByRole('button', { name: 'Organization settings', exact: true }),
    sidebar.getByRole('button', { name: 'Workspace settings', exact: true }),
    sidebar.getByRole('button', { name: 'Add to tree', exact: true }),
    sidebar.getByRole('button', { name: 'Actions for Product', exact: true }),
    sidebar.getByRole('button', { name: 'Actions for coordinator', exact: true }),
    sidebar.getByRole('combobox', { name: 'Organization', exact: true }),
    sidebar.getByRole('combobox', { name: 'Workspace', exact: true }),
  ]
  for (const control of controls) await expectTouch(control)
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
  // The 44px "Add to tree" button must fit inside its own header row.
  await expect.poll(() => sidebar.getByRole('button', { name: 'Add to tree', exact: true }).evaluate((button) => { const row = button.parentElement!.parentElement!.getBoundingClientRect(), box = button.getBoundingClientRect(); return box.top >= row.top - 0.5 && box.bottom <= row.bottom + 0.5 })).toBe(true)
  await sidebar.getByRole('button', { name: 'Actions for Product', exact: true }).click()
  const items = page.getByRole('menuitem')
  await expect(items.first()).toBeVisible()
  for (const item of await items.all()) await expectTouch(item, ['height'])
  await page.screenshot({ path: info.outputPath(`tree-touch-${theme}-390.png`) })
})

for (const theme of ['light', 'dark']) test(`Overview and the phone top bar controls are at least 44px at 390px (${theme})`, async ({ page }, info) => {
  await page.addInitScript((value) => localStorage.setItem('treer-theme', value), theme)
  await page.setViewportSize({ width: 390, height: 844 })
  await mockTreeMapApi(page)
  await page.route('**/groups', (route) => route.fulfill({ json: { groups: [] } }))
  await page.route('**/policy-provider', (route) => route.fulfill({ status: 404, json: {} }))
  await page.routeWebSocket(/\/events$/, () => {})
  await page.goto('/')
  await expect(page.getByRole('heading', { name: 'Workspace overview' })).toBeVisible()
  const controls = [
    page.getByRole('button', { name: 'Browse tree', exact: true }),
    page.getByRole('button', { name: /^Page /, }),
    page.getByRole('button', { name: /^More actions for/ }),
    page.getByRole('button', { name: /^Show all tabs/ }),
    page.getByRole('button', { name: 'Expand tree map', exact: true }),
    page.getByRole('button', { name: 'Add agent', exact: true }),
    page.getByRole('button', { name: /^Filter /, }).first(),
  ]
  for (const control of controls) await expectTouch(control)
  for (const chip of await page.getByRole('group', { name: 'Attention filters' }).getByRole('button').all()) await expectTouch(chip, ['height'])
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
  await page.screenshot({ path: info.outputPath(`overview-touch-${theme}-390.png`) })
})

test('the full Agent menu stays reachable on a short 390px viewport', async ({ page }) => {
  await open(page, 390)
  await page.setViewportSize({ width: 390, height: 420 })
  await page.locator('#tree-sidebar').getByRole('button', { name: 'Actions for coordinator', exact: true }).click()
  const last = page.getByRole('menuitem', { name: 'Delete', exact: true })
  await expect(last).toBeVisible()
  await last.scrollIntoViewIfNeeded()
  await expect(last).toBeInViewport()
  await expectTouch(last, ['height'])
})

test('desktop keeps the compact Tree controls', async ({ page }) => {
  await open(page, 1360)
  expect((await size(page.getByRole('button', { name: 'Add to tree', exact: true }))).width).toBeLessThan(40)
  expect((await size(page.getByRole('button', { name: 'Workspace settings', exact: true }))).width).toBeLessThan(40)
})
