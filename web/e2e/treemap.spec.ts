import { test, expect } from '@playwright/test'
import { treemap } from '../src/lib/treemap'
import { mockTreeMapApi, mapSnapshot } from './fixtures/treemap'

test('squarified cells preserve area, bounds and non-overlap for wide, tall and empty layouts', () => {
  expect(treemap([], { x: 0, y: 0, width: 100, height: 100 })).toEqual([])
  for (const [width, height] of [[900, 500], [300, 1500], [1, 1]]) {
    const weights = [9, 4, 1, 1, 1, 1, 1]
    const cells = treemap(weights, { x: 7, y: 11, width, height })
    expect(cells).toHaveLength(weights.length)
    cells.forEach((cell, index) => {
      expect(cell.x).toBeGreaterThanOrEqual(7 - 1e-8)
      expect(cell.y).toBeGreaterThanOrEqual(11 - 1e-8)
      expect(cell.x + cell.width).toBeLessThanOrEqual(7 + width + 1e-8)
      expect(cell.y + cell.height).toBeLessThanOrEqual(11 + height + 1e-8)
      expect(cell.width * cell.height).toBeCloseTo(weights[index] / 18 * width * height)
      for (const other of cells.slice(index + 1)) {
        const overlap = Math.max(0, Math.min(cell.x + cell.width, other.x + other.width) - Math.max(cell.x, other.x)) * Math.max(0, Math.min(cell.y + cell.height, other.y + other.height) - Math.max(cell.y, other.y))
        expect(overlap).toBeLessThan(1e-7)
      }
    })
  }
})

for (const mobile of [false, true]) for (const theme of ['light', 'dark']) {
  test(`treemap ${mobile ? '390px' : 'desktop'} ${theme}: drill down, breadcrumbs and stable-ID keyboard selection`, async ({ page }, testInfo) => {
    await page.setViewportSize(mobile ? { width: 390, height: 844 } : { width: 1360, height: 960 })
    await mockTreeMapApi(page)
    await page.addInitScript((value) => localStorage.setItem('treer-theme', value), theme)
    // Apply the theme explicitly so screenshots do not depend on storage-key implementation.
    await page.goto('/')
    await page.evaluate((value) => document.documentElement.classList.toggle('dark', value === 'dark'), theme)
    await expect(page.getByRole('button', { name: 'Expand tree map' })).toBeEnabled()
    await page.getByRole('button', { name: 'Expand tree map' }).click()
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
    await expect(dialog.getByRole('button', { name: /^Open \/Research\/Experiments\/prototype · command · workstation · idle$/ })).toBeVisible()
    await dialog.getByRole('button', { name: 'Back to parent folder' }).click()
    await dialog.getByRole('button', { name: 'Workspace', exact: true }).click()
    await dialog.getByRole('button', { name: 'Explore folder /Ideas', exact: true }).click()
    await expect(dialog.getByText('This folder is empty', { exact: true })).toBeVisible()
    await dialog.getByRole('button', { name: 'Workspace', exact: true }).click()
    await dialog.getByRole('button', { name: 'Explore folder /Product', exact: true }).click()
    const offline = dialog.getByRole('button', { name: /^Open \/Product\/reviewer · claude · build-node · offline$/ })
    await expect(offline).toBeVisible()
    if (mobile) await page.screenshot({ path: testInfo.outputPath(`treemap-${theme}-390-drilled.png`) })
    await offline.focus()
    await page.keyboard.press('Enter')
    await expect(dialog).toHaveCount(0)
    if (mobile) await page.getByRole('button', { name: 'Back to workloads', exact: true }).click()
    await expect(page.locator('[data-agent-id="reviewer-b"]')).toHaveAttribute('aria-selected', 'true')
    await expect(page.locator('[data-agent-id="reviewer-a"][aria-selected="true"]')).toHaveCount(0)
  })
}

test('map renders hostile names as text and stays usable after folder removal', async ({ page }) => {
  await mockTreeMapApi(page)
  let current = { folders: [{ folder_id: 'hostile', name: '<img src=x onerror=alert(1)>', parent_id: null }], placements: [] }
  await page.route('**/workload-tree', (route) => route.fulfill({ contentType: 'application/json', body: JSON.stringify(current) }))
  await page.goto('/')
  await page.getByRole('button', { name: 'Expand tree map' }).click()
  const dialog = page.getByRole('dialog', { name: 'Workspace map' })
  await dialog.getByRole('button', { name: 'Explore folder /<img src=x onerror=alert(1)>', exact: true }).click()
  expect(await dialog.locator('img').count()).toBe(0)
  current = { folders: [], placements: [] }
  await page.evaluate(() => dispatchEvent(new Event('focus')))
  await expect(dialog.getByRole('button', { name: 'Workspace', exact: true })).toHaveAttribute('aria-current', 'location')
  await page.keyboard.press('Escape')
  await expect(dialog).toHaveCount(0)
  await expect(page.getByRole('button', { name: 'Expand tree map' })).toBeFocused()
})


test('single creation menu preserves independent disabled states and the tour anchor', async ({ page }) => {
  await mockTreeMapApi(page)
  await page.route('**/snapshot', (route) => route.fulfill({ contentType: 'application/json', body: JSON.stringify({ ...mapSnapshot, servers: mapSnapshot.servers.map((machine) => ({ ...machine, status: 'offline' })) }) }))
  await page.goto('/')
  const add = page.getByRole('button', { name: 'Add to tree', exact: true })
  await expect(add).toBeEnabled()
  await expect(page.getByRole('treeitem', { name: 'coordinator', exact: true })).toBeVisible()
  await expect(add).toHaveAttribute('data-tour', 'create-agent')
  await expect(page.getByRole('button', { name: 'New folder', exact: true })).toHaveCount(0)
  await add.focus()
  await page.keyboard.press('Enter')
  await expect(page.getByRole('menuitem', { name: 'Add agent', exact: true })).toHaveAttribute('data-disabled')
  await expect(page.getByRole('menuitem', { name: 'Add folder', exact: true })).not.toHaveAttribute('data-disabled')
  await page.getByRole('menuitem', { name: 'Add folder', exact: true }).focus()
  await page.keyboard.press('Enter')
  await expect(page.getByRole('textbox', { name: 'Folder name' })).toBeVisible()
})

test('loading folders disables only Add folder; an empty workspace disables both entry buttons', async ({ page }) => {
  await mockTreeMapApi(page)
  let release!: () => void
  const pending = new Promise<void>((resolve) => { release = resolve })
  await page.route('**/workload-tree', async (route) => { await pending; return route.fulfill({ contentType: 'application/json', body: '{"folders":[],"placements":[]}' }) })
  await page.goto('/')
  await expect(page.getByText('Loading folders…', { exact: true })).toBeVisible()
  await page.getByRole('button', { name: 'Add to tree', exact: true }).click()
  await expect(page.getByRole('menuitem', { name: 'Add folder', exact: true })).toHaveAttribute('data-disabled')
  await expect(page.getByRole('menuitem', { name: 'Add agent', exact: true })).not.toHaveAttribute('data-disabled')
  release()
  await page.keyboard.press('Escape')
  await page.route('**/workspaces?*', (route) => route.fulfill({ contentType: 'application/json', body: '{"workspaces":[]}' }))
  await page.reload()
  await expect(page.getByRole('button', { name: 'Add to tree', exact: true })).toBeDisabled()
  await expect(page.getByRole('button', { name: 'Expand tree map', exact: true })).toBeDisabled()
})
