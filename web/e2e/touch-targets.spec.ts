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
  const box = await locator.boundingBox()
  expect(box).not.toBeNull()
  return box!
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
  for (const control of controls) { const box = await size(control); expect(box.width, await control.getAttribute('aria-label')).toBeGreaterThanOrEqual(44 - 0.5); expect(box.height).toBeGreaterThanOrEqual(44 - 0.5) }
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
  await sidebar.getByRole('button', { name: 'Actions for Product', exact: true }).click()
  const items = page.getByRole('menuitem')
  await expect(items.first()).toBeVisible()
  for (const item of await items.all()) expect((await size(item)).height).toBeGreaterThanOrEqual(44 - 0.5)
  await page.screenshot({ path: info.outputPath(`tree-touch-${theme}-390.png`) })
})

test('desktop keeps the compact Tree controls', async ({ page }) => {
  await open(page, 1360)
  expect((await size(page.getByRole('button', { name: 'Add to tree', exact: true }))).width).toBeLessThan(40)
  expect((await size(page.getByRole('button', { name: 'Workspace settings', exact: true }))).width).toBeLessThan(40)
})
