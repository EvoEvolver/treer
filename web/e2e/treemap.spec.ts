import { openTabMenu, tabAction, backToWorkloads } from "./fixtures/tab-actions"
import { test, expect } from '@playwright/test'
import { browseTreeOnMobile, openOverviewMap } from './fixtures/navigation'
import { mockTreeMapApi, mockDeepTreeMapApi, mapSnapshot } from './fixtures/treemap'

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
    await expect(page.getByRole('button', { name: /^coordinator agent · workstation · idle$/ })).toBeVisible()
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
    await expect(dialog.getByRole('button', { name: /^Open \/Research\/Experiments\/prototype · agent · workstation · idle$/ })).toBeVisible()
    await dialog.getByRole('button', { name: 'Back to parent folder' }).click()
    await dialog.getByRole('button', { name: 'Workspace', exact: true }).click()
    await dialog.getByRole('combobox', { name: 'Go to', exact: true }).selectOption('empty')
    await expect(dialog.getByText('This folder is empty', { exact: true })).toBeVisible()
    await dialog.getByRole('button', { name: 'Workspace', exact: true }).click()
    await dialog.getByRole('button', { name: 'Explore folder /Product', exact: true }).click()
    const offline = dialog.getByRole('button', { name: /^Open \/Product\/reviewer · agent · build-node · offline$/ })
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


test('single creation menu preserves independent disabled states and the tour anchor', async ({ page }) => {
  await mockTreeMapApi(page)
  await page.route('**/snapshot', (route) => route.fulfill({ contentType: 'application/json', body: JSON.stringify({ ...mapSnapshot, servers: mapSnapshot.servers.map((machine) => ({ ...machine, status: 'offline' })) }) }))
  await page.goto('/')
  await browseTreeOnMobile(page)
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
  await browseTreeOnMobile(page)
  await expect(page.getByText('Loading folders…', { exact: true })).toBeVisible()
  await page.getByRole('button', { name: 'Add to tree', exact: true }).click()
  await expect(page.getByRole('menuitem', { name: 'Add folder', exact: true })).toHaveAttribute('data-disabled')
  await expect(page.getByRole('menuitem', { name: 'Add agent', exact: true })).not.toHaveAttribute('data-disabled')
  release()
  await page.keyboard.press('Escape')
  await page.route('**/workspaces?*', (route) => route.fulfill({ contentType: 'application/json', body: '{"workspaces":[]}' }))
  await page.reload()
  await browseTreeOnMobile(page)
  await expect(page.getByRole('button', { name: 'Add to tree', exact: true })).toBeDisabled()
  await page.getByRole('button', { name: 'Workspace overview', exact: true }).click()
  await expect(page.getByRole('button', { name: 'Expand tree map', exact: true })).toBeDisabled()
})

async function noMapOverflow(page: import('@playwright/test').Page) {
  const dialog = page.getByRole('dialog', { name: 'Workspace map' })
  await expect.poll(() => dialog.evaluate((el) => {
    const box = el.getBoundingClientRect()
    return box.left >= 0 && box.right <= innerWidth && box.top >= 0 && box.bottom <= innerHeight
      && [el, ...el.querySelectorAll<HTMLElement>('*')].every((child) => {
        const css = getComputedStyle(child)
        return !['auto', 'scroll'].includes(css.overflowX) && !['auto', 'scroll'].includes(css.overflowY)
      })
  })).toBe(true)
  const canvas = dialog.getByRole('group', { name: 'Workspace treemap' })
  await expect.poll(() => canvas.evaluate((el) => el.scrollWidth <= el.clientWidth + 1 && el.scrollHeight <= el.clientHeight + 1)).toBe(true)
  for (const box of await canvas.locator('button').all()) {
    const rect = (await box.boundingBox())!
    expect(rect.width).toBeGreaterThanOrEqual(44)
    expect(rect.height).toBeGreaterThanOrEqual(44)
  }
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
}

for (const sample of ["small", "medium", "large"] as const) for (const mobile of [false, true]) for (const theme of ['light', 'dark']) {
  test(`fixed map ${sample} ${mobile ? '390px' : 'desktop'} ${theme}: fits, scales and opens by ID`, async ({ page }, testInfo) => {
    await page.setViewportSize(mobile ? { width: 390, height: 844 } : { width: 1360, height: 960 })
    await mockDeepTreeMapApi(page, sample === "large" ? true : sample === "medium" ? "medium" : false)
    await page.addInitScript((value) => localStorage.setItem('treer-theme', value), theme)
    await page.goto('/')
    await browseTreeOnMobile(page)
    await expect(page.getByRole('treeitem', { name: 'dashboard', exact: true }).locator('[data-workload-icon="app"]')).toBeVisible()
    await openOverviewMap(page)
    const dialog = page.getByRole('dialog', { name: 'Workspace map' })
    await expect(dialog.locator('[data-map-id="level-0"]')).toBeVisible()
    await noMapOverflow(page)
    await page.screenshot({ path: testInfo.outputPath(`fixed-${sample}-${theme}-${mobile ? '390' : 'desktop'}.png`) })
    for (const id of ['level-0', 'level-1', 'level-2', 'level-3']) {
      await dialog.getByRole('combobox', { name: 'Go to', exact: true }).selectOption(id)
    }
    await expect(dialog.getByRole('button', { name: 'Lab', exact: true })).toHaveAttribute('aria-current', 'location')
    const picker = dialog.getByRole('combobox', { name: 'Go to', exact: true })
    await expect(picker.locator('option[value="deep-0"]')).toHaveText(/\/Team\/Research\/Experiments\/Lab\/agent-01 · agent · workstation · working/)
    await noMapOverflow(page)
    if (sample === 'large' && mobile) {
      const dot = dialog.locator('[data-map-id="deep-0"]')
      await expect(dot).toHaveAttribute('role', 'img')
      await expect(dot).toHaveAttribute('title', /\/Team\/Research\/Experiments\/Lab\/agent-01.*working/)
    }
    await picker.focus()
    await picker.selectOption('deep-0')
    await expect(dialog).toHaveCount(0)
    await expect(page.locator('#workspace-panel-deep-0')).toBeVisible()
    if (!mobile) await expect(page.locator('[data-agent-id="deep-0"]')).toHaveAttribute('aria-selected', 'true')
  })
}

test('resize recomputes rectangles; status and input order do not move blocks', async ({ page }) => {
  await mockTreeMapApi(page)
  let events: import('@playwright/test').WebSocketRoute | undefined
  await page.routeWebSocket(/\/events$/, (socket) => { events = socket })
  let agents = mapSnapshot.agents
  await page.route('**/snapshot', (route) => route.fulfill({ json: { ...mapSnapshot, agents } }))
  await page.setViewportSize({ width: 1360, height: 960 })
  await page.goto('/')
  await openOverviewMap(page)
  const dialog = page.getByRole('dialog', { name: 'Workspace map' })
  const blocks = dialog.locator('[data-map-id]')
  await expect(blocks.first()).toBeVisible()
  const geometry = () => blocks.evaluateAll((els) => els.map((el) => [el.getAttribute('data-map-id'), el.getAttribute('style')]))
  const before = await geometry()
  agents = [...agents].reverse().map((agent) => ({ ...agent, status: 'stopped' }))
  events!.send(JSON.stringify({ event: 'workspace.snapshot', data: { ...mapSnapshot, revision: 2, agents } }))
  await expect(dialog.getByRole('button', { name: /Open .*coordinator.*stopped/ })).toBeVisible()
  expect(await geometry()).toEqual(before)
  await page.setViewportSize({ width: 390, height: 640 })
  await expect.poll(geometry).not.toEqual(before)
  await noMapOverflow(page)
  await page.setViewportSize({ width: 1360, height: 960 })
  await expect.poll(geometry).toEqual(before)
  await noMapOverflow(page)
})

test('very deep trees bound nested DOM, retain ancestors and allow keyboard drill-down', async ({ page }) => {
  await mockDeepTreeMapApi(page, false, 110)
  await page.setViewportSize({ width: 390, height: 640 })
  await page.goto('/')
  await openOverviewMap(page)
  const dialog = page.getByRole('dialog', { name: 'Workspace map' })
  await expect(dialog.getByRole('combobox', { name: 'Folder chain /Team', exact: true }).locator('option[value="level-0"]')).toHaveText('/Team')
  expect(await dialog.getByRole('region', { name: /^Folder / }).count()).toBeLessThanOrEqual(13)
  const picker = dialog.getByRole('combobox', { name: 'Go to', exact: true })
  for (let i = 0; i < 12; i++) await picker.selectOption(`level-${i}`)
  await noMapOverflow(page)
  await dialog.getByRole('combobox', { name: 'Earlier folders', exact: true }).selectOption('level-0')
  await expect(dialog.getByRole('button', { name: 'Team', exact: true })).toHaveAttribute('aria-current', 'location')
  await page.keyboard.press('Escape')
  await expect(page.getByRole('button', { name: 'Expand tree map', exact: true })).toBeFocused()
})


test('Chinese map localizes navigation, counts and folder descriptions', async ({ page }) => {
  await mockTreeMapApi(page)
  await page.addInitScript(() => localStorage.setItem('treer-lang', 'zh'))
  await page.goto('/')
  await page.getByRole('button', { name: '展开树图', exact: true }).click()
  const dialog = page.getByRole('dialog')
  await expect(dialog.getByRole('combobox', { name: '前往', exact: true })).toBeVisible()
  await expect(dialog.getByRole('region', { name: '文件夹 /Research', exact: true })).toBeVisible()
  await expect(dialog.getByText('7 个工作负载', { exact: true })).toBeVisible()
})


test('single-child folder chains share one title row; every segment still drills down', async ({ page }) => {
  await mockDeepTreeMapApi(page, true)
  await page.setViewportSize({ width: 1360, height: 960 })
  await page.goto('/')
  await openOverviewMap(page)
  const dialog = page.getByRole('dialog', { name: 'Workspace map' })
  const segments = ['/Team', '/Team/Research', '/Team/Research/Experiments', '/Team/Research/Experiments/Lab']
  const positions = []
  for (const path of segments) positions.push((await dialog.getByRole('button', { name: `Explore folder ${path}`, exact: true }).boundingBox())!.y)
  expect(new Set(positions).size).toBe(1)
  for (const [index, path] of segments.entries()) {
    await dialog.getByRole('button', { name: `Explore folder ${path}`, exact: true }).focus()
    await page.keyboard.press('Enter')
    await expect(dialog.getByRole('button', { name: path.split('/').at(-1), exact: true })).toHaveAttribute('aria-current', 'location')
    await dialog.getByRole('button', { name: 'Workspace', exact: true }).click()
    if (index === segments.length - 1) await noMapOverflow(page)
  }
})
