import { test, expect } from '@playwright/test'
import { mockTreeMapApi, mockDeepTreeMapApi, mapSnapshot } from './fixtures/treemap'

for (const mobile of [false, true]) for (const theme of ['light', 'dark']) {
  test(`treemap ${mobile ? '390px' : 'desktop'} ${theme}: drill down, breadcrumbs and stable-ID keyboard selection`, async ({ page }, testInfo) => {
    await page.setViewportSize(mobile ? { width: 390, height: 844 } : { width: 1360, height: 960 })
    await mockTreeMapApi(page)
    await page.addInitScript((value) => localStorage.setItem('treer-theme', value), theme)
    // Apply the theme explicitly so screenshots do not depend on storage-key implementation.
    await page.goto('/')
    await page.evaluate((value) => document.documentElement.classList.toggle('dark', value === 'dark'), theme)
    await expect(page.getByRole('button', { name: 'Expand tree map' })).toBeEnabled()
    await expect(page.getByRole('button', { name: /^coordinator agent · workstation · idle$/ })).toBeVisible()
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
    await expect(dialog.getByRole('button', { name: /^Open \/Research\/Experiments\/prototype · agent · workstation · idle$/ })).toBeVisible()
    await dialog.getByRole('button', { name: 'Back to parent folder' }).click()
    await dialog.getByRole('button', { name: 'Workspace', exact: true }).click()
    await dialog.getByRole('button', { name: 'Explore folder /Ideas', exact: true }).click()
    await expect(dialog.getByText('This folder is empty', { exact: true })).toBeVisible()
    await dialog.getByRole('button', { name: 'Workspace', exact: true }).click()
    await dialog.getByRole('button', { name: 'Explore folder /Product', exact: true }).click()
    const offline = dialog.getByRole('button', { name: /^Open \/Product\/reviewer · agent · build-node · offline$/ })
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

for (const sample of ["small", "medium", "large"] as const) for (const mobile of [false, true]) for (const theme of ['light', 'dark']) {
  test(`nested map ${sample} ${mobile ? '390px' : 'desktop'} ${theme}: compact identity and budgeted previews`, async ({ page }, testInfo) => {
    await page.setViewportSize(mobile ? { width: 390, height: 844 } : { width: 1360, height: 960 })
    const large = sample === "large"
    await mockDeepTreeMapApi(page, large ? true : sample === "medium" ? "medium" : false)
    await page.addInitScript((value) => localStorage.setItem('treer-theme', value), theme)
    await page.goto('/')
    const appRow = page.getByRole('treeitem', { name: 'dashboard', exact: true })
    await expect(appRow.locator('[data-workload-icon="app"]')).toBeVisible()
    await page.getByRole('button', { name: 'Expand tree map' }).click()
    const dialog = page.getByRole('dialog', { name: 'Workspace map' })
    const deepAgent = dialog.getByRole('button', { name: /^Open \/Team\/Research\/Experiments\/Lab\/agent-01 · agent/ })
    await expect(dialog.locator('[data-workload-icon="app"]')).toBeVisible()
    if (large) {
      await expect(deepAgent).toHaveCount(0)
      await expect(dialog.getByRole('button', { name: 'View contents of /Team', exact: true })).toContainText('80 agents')
      const folder = dialog.getByRole('region', { name: 'Folder /Team', exact: true })
      await expect(folder).toContainText('offline 7')
      await expect(folder.getByLabel('Preview of /Team', { exact: true })).toContainText('agent-01')
      expect((await folder.boundingBox())!.height).toBeLessThan(220)
    } else {
      // Four folder levels automatically expand; no drilling required.
      await expect(deepAgent).toBeVisible()
      await expect(deepAgent.locator('[data-workload-icon="agent"]')).toBeVisible()
      const name = await deepAgent.locator('span.truncate').first().boundingBox()
      const status = await deepAgent.getByText('working', { exact: true }).boundingBox()
      expect(status!.y - name!.y).toBeLessThan(52)
      await expect(deepAgent).toHaveCSS('height', '72px')
      await expect(deepAgent).toHaveAttribute('title', /\/Team\/Research\/Experiments\/Lab\/agent-01/)
    }
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
    await page.screenshot({ path: testInfo.outputPath(`nested-${sample}-${theme}-${mobile ? '390' : 'desktop'}.png`) })
    if (large) {
      for (const path of ['/Team', '/Team/Research', '/Team/Research/Experiments', '/Team/Research/Experiments/Lab']) {
        await dialog.getByRole('button', { name: `Explore folder ${path}`, exact: true }).click()
      }
      await expect(deepAgent).toBeVisible()
      await expect(dialog.getByRole('button', { name: /^Open \/Team\/Research\/Experiments\/Lab\/agent-/ })).toHaveCount(80)
    }
    await deepAgent.focus()
    await page.keyboard.press('Enter')
    await expect(dialog).toHaveCount(0)
    if (mobile) await page.getByRole('button', { name: 'Close full-screen terminal', exact: true }).click()
    await expect(page.locator('[data-agent-id="deep-0"]')).toHaveAttribute('aria-selected', 'true')
    await expect(page.locator('[data-agent-id="deep-0"] [data-workload-icon="agent"]')).toBeVisible()
  })
}


test('a very deep small tree respects the node budget instead of recursively mounting every folder', async ({ page }) => {
  await mockDeepTreeMapApi(page, false, 110)
  await page.goto('/')
  await page.getByRole('button', { name: 'Expand tree map' }).click()
  const dialog = page.getByRole('dialog', { name: 'Workspace map' })
  await expect(dialog.getByRole('button', { name: 'View contents of /Team', exact: true })).toContainText('3 agents')
  await expect(dialog.getByRole('region', { name: /^Folder / })).toHaveCount(1)
  await expect(dialog.getByRole('button', { name: /^Open .*agent-01/ })).toHaveCount(0)
  await dialog.getByRole('button', { name: 'View contents of /Team', exact: true }).click()
  await expect(dialog.getByRole('button', { name: 'Team', exact: true })).toHaveAttribute('aria-current', 'location')
})
