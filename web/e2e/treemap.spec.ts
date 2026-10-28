import { backToWorkloads } from "./fixtures/tab-actions"
import { test, expect } from '@playwright/test'
import { browseTreeOnMobile, openOverviewMap } from './fixtures/navigation'
import { mockTreeMapApi } from './fixtures/treemap'

for (const mobile of [false, true]) for (const theme of ['light', 'dark']) {
  test(`Overview owns the map entry ${theme} ${mobile ? '390' : 'desktop'}`, async ({ page }, info) => {
    await page.setViewportSize({ width: mobile ? 390 : 1360, height: 900 })
    await mockTreeMapApi(page)
    await page.addInitScript((theme) => localStorage.setItem('treer-theme', theme), theme)
    await page.goto('/')
    const overview = page.locator('#workspace-panel-overview')
    const trigger = overview.getByRole('button', { name: 'Expand tree map', exact: true })
    await expect(trigger).toBeEnabled()
    await expect(trigger).toContainText('Workspace map')
    await expect(page.locator('#tree-sidebar button[aria-label="Expand tree map"]')).toHaveCount(0)
    await expect.poll(() => page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
    await page.screenshot({ path: info.outputPath(`overview-map-${theme}-${mobile ? '390' : 'desktop'}.png`) })
    await trigger.focus()
    await page.keyboard.press('Enter')
    await expect(page.getByRole('dialog', { name: 'Workspace map' })).toBeVisible()
    await page.keyboard.press('Escape')
    await expect(page.getByRole('dialog', { name: 'Workspace map' })).toHaveCount(0)
    await expect(trigger).toBeFocused()
  })
}

for (const mobile of [false, true]) for (const theme of ['light', 'dark']) {
  test(`treemap ${mobile ? '390px' : 'desktop'} ${theme}: drill down, breadcrumbs and stable-ID keyboard selection`, async ({ page }, testInfo) => {
    await page.setViewportSize(mobile ? { width: 390, height: 844 } : { width: 1360, height: 960 })
    await mockTreeMapApi(page)
    await page.addInitScript((value) => localStorage.setItem('treer-theme', value), theme)
    // Apply the theme explicitly so screenshots do not depend on storage-key implementation.
    await page.goto('/')
    await browseTreeOnMobile(page)
    await page.evaluate((value) => document.documentElement.classList.toggle('dark', value === 'dark'), theme)
    await expect(page.locator('#tree-sidebar').getByRole('button', { name: 'Expand tree map' })).toHaveCount(0)
    await expect(page.getByRole('link', { name: /^coordinator$/ })).toBeVisible()
    await openOverviewMap(page)
    const dialog = page.getByRole('dialog', { name: 'Workspace map' })
    await expect(dialog.getByRole('group', { name: 'Workspace treemap' })).toBeVisible()
    await expect(dialog.getByRole('button', { name: 'Explore folder /Research', exact: true })).toBeVisible()
    await page.screenshot({ path: testInfo.outputPath(`treemap-${theme}-${mobile ? '390' : 'desktop'}.png`) })
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
    await dialog.getByRole('button', { name: 'Explore folder /Research', exact: true }).focus()
    await page.keyboard.press('Enter')
    await expect(dialog.getByRole('navigation', { name: 'Map breadcrumbs' })).toBeFocused()
    await expect(dialog.getByRole('button', { name: 'Research', exact: true })).toHaveAttribute('aria-current', 'location')
    await dialog.getByRole('button', { name: 'Explore folder /Research/Experiments', exact: true }).click()
    await expect(dialog.getByRole('link', { name: /^Open \/Research\/Experiments\/prototype · agent · workstation · idle$/ })).toBeVisible()
    await dialog.getByRole('button', { name: 'Back to parent folder' }).click()
    await dialog.getByRole('button', { name: 'Workspace', exact: true }).click()
    await dialog.getByRole('combobox', { name: 'Go to', exact: true }).selectOption('empty')
    await expect(dialog.getByText('This folder is empty', { exact: true })).toBeVisible()
    await dialog.getByRole('button', { name: 'Workspace', exact: true }).click()
    await dialog.getByRole('button', { name: 'Explore folder /Product', exact: true }).click()
    const offline = dialog.getByRole('link', { name: /^Open \/Product\/reviewer · agent · build-node · offline$/ })
    await expect(offline).toBeVisible()
    if (mobile) await page.screenshot({ path: testInfo.outputPath(`treemap-${theme}-390-drilled.png`) })
    await offline.focus()
    await page.keyboard.press('Enter')
    await expect(dialog).toHaveCount(0)
    if (mobile) await backToWorkloads(page)
    await expect(page.locator('[data-agent-id="reviewer-b"]')).toHaveAttribute('aria-selected', 'true')
    await expect(page.locator('[data-agent-id="reviewer-a"][aria-selected="true"]')).toHaveCount(0)
  })
}

test('map renders hostile names as text and stays usable after folder removal', async ({ page }) => {
  await mockTreeMapApi(page)
  let current = { folders: [{ folder_id: 'hostile', name: '<img src=x onerror=alert(1)>', parent_id: null }], placements: [] }
  await page.route('**/workload-tree', (route) => route.fulfill({ contentType: 'application/json', body: JSON.stringify(current) }))
  await page.goto('/')
  await browseTreeOnMobile(page)
  await openOverviewMap(page)
  const dialog = page.getByRole('dialog', { name: 'Workspace map' })
  await dialog.getByRole('combobox', { name: 'Go to', exact: true }).selectOption('hostile')
  expect(await dialog.locator('img').count()).toBe(0)
  current = { folders: [], placements: [] }
  await page.evaluate(() => dispatchEvent(new Event('focus')))
  await expect(dialog.getByRole('button', { name: 'Workspace', exact: true })).toHaveAttribute('aria-current', 'location')
  await page.keyboard.press('Escape')
  await expect(dialog).toHaveCount(0)
  await expect(page.getByRole('button', { name: 'Expand tree map' })).toBeFocused()
})
