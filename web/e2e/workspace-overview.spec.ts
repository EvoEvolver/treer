import { openTabMenu, tabAction, backToWorkloads } from "./fixtures/tab-actions"
import { test, expect, type Page, type WebSocketRoute } from '@playwright/test'
import { mapSnapshot, mockTreeMapApi } from './fixtures/treemap'

async function fixture(page: Page) {
  await mockTreeMapApi(page)
  await page.route('**/groups', (route) => route.fulfill({ json: { groups: [] } }))
  await page.route('**/policy-provider', (route) => route.fulfill({ status: 404, json: {} }))
  await page.route('**/bootstrap', (route) => route.fulfill({ json: { install_command: 'echo synthetic', connect_command: 'echo synthetic' } }))
  let events: WebSocketRoute | undefined
  const terminals: string[] = []
  await page.routeWebSocket(/\/events$/, (socket) => { events = socket })
  await page.routeWebSocket(/\/terminal(?:\?.*)?$/, (socket) => { terminals.push(socket.url()) })
  return { terminals, send: (data: unknown) => events!.send(JSON.stringify({ event: 'workspace.snapshot', data })) }
}

for (const mobile of [false, true]) for (const theme of ['light', 'dark']) {
  test(`workspace overview ${theme} ${mobile ? '390' : 'desktop'} defaults to attention, not a PTY`, async ({ page }, info) => {
    await page.setViewportSize({ width: mobile ? 390 : 1360, height: 900 })
    await page.addInitScript((value) => localStorage.setItem('treer-theme', value), theme)
    const backend = await fixture(page)
    await page.goto('/')
    await expect(page.getByRole('heading', { name: 'Workspace overview' })).toBeVisible()
    await expect(page.getByRole('button', { name: 'Filter need attention', exact: true })).toHaveText('1need attention')
    await expect(page.getByRole('button', { name: 'Filter working', exact: true })).toHaveText('2working')
    await expect(page.getByRole('button', { name: 'Filter idle', exact: true })).toHaveText('4idle')
    await expect(page.getByRole('button', { name: 'Folder /Research', exact: true })).toHaveAttribute('aria-expanded', 'true')
    await expect(page.getByRole('button', { name: 'Folder /Research/Experiments', exact: true })).toHaveAttribute('aria-expanded', 'false')
    expect(backend.terminals).toEqual([])
    await expect(page.locator('.xterm')).toHaveCount(0)
    await expect.poll(() => page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
    // Check the rendered small text against its composited background, not just
    // token values: attention rows are tinted and hover changes the row fill.
    const statusText = page.getByRole('region', { name: 'Workspace overview tree' }).locator('[class*="text-status-"]')
    const contrast = () => statusText.evaluateAll((elements) => elements.map((element) => {
      const rgba = (value: string) => value.match(/[\d.]+/g)!.map(Number)
      const luminance = (rgb: number[]) => rgb.slice(0, 3).reduce((sum, value, i) => {
        const c = value / 255
        return sum + [0.2126, 0.7152, 0.0722][i] * (c <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4)
      }, 0)
      const layers: number[][] = []
      for (let ancestor: Element | null = element; ancestor; ancestor = ancestor.parentElement) layers.push(rgba(getComputedStyle(ancestor).backgroundColor))
      const background = layers.reverse().reduce((base, color) => base.map((channel, i) => channel * (1 - (color[3] ?? 1)) + color[i] * (color[3] ?? 1)), [255, 255, 255])
      const foreground = luminance(rgba(getComputedStyle(element).color)), back = luminance(background)
      return (Math.max(foreground, back) + 0.05) / (Math.min(foreground, back) + 0.05)
    }))
    expect(Math.min(...await contrast())).toBeGreaterThanOrEqual(4.5)
    await page.getByRole('link', { name: 'Open /Product/reviewer', exact: true }).hover()
    expect(Math.min(...await contrast())).toBeGreaterThanOrEqual(4.5)
    await page.mouse.move(0, 0)
    await page.screenshot({ path: info.outputPath(`overview-${theme}-${mobile ? '390' : 'desktop'}.png`) })
    const attentionFilter = page.getByRole('button', { name: 'Filter need attention', exact: true })
    await attentionFilter.focus()
    await page.keyboard.press("Enter")
    await expect(attentionFilter).toHaveAttribute('aria-pressed', 'true')
    await expect(page.getByRole('region', { name: 'Workspace overview tree' }).getByRole('link', { name: /^Open / })).toHaveCount(1)
    await expect(page.getByRole('button', { name: 'Folder /Research', exact: true })).toHaveCount(0)
    await expect(page.getByRole('button', { name: 'Folder /Product', exact: true })).toContainText('1 need attention')
    await page.screenshot({ path: info.outputPath(`overview-${theme}-${mobile ? '390' : 'desktop'}-filter-attention.png`) })
    await attentionFilter.click()
    const research = page.getByRole('button', { name: 'Folder /Research', exact: true })
    await research.click()
    await page.getByRole('button', { name: 'Filter working', exact: true }).click()
    await expect(research).toHaveAttribute('aria-expanded', 'true')
    await page.getByRole('button', { name: 'Filter working', exact: true }).click()
    await expect(research).toHaveAttribute('aria-expanded', 'false')
    await research.click()
    await page.getByRole('link', { name: 'Open /Research/reviewer', exact: true }).click()
    await expect.poll(() => backend.terminals.length).toBeGreaterThan(0)
    expect(backend.terminals[0]).toContain('/agents/reviewer-a/terminal')
    if (mobile) {
      await backToWorkloads(page)
      await page.getByRole('button', { name: 'Workspace overview', exact: true }).click()
    }
    else await page.getByRole('tab', { name: 'Tab Overview', exact: true }).click()
    await expect(page.getByRole('heading', { name: 'Workspace overview' })).toBeVisible()
    if (mobile) await page.getByRole('button', { name: 'Browse tree', exact: true }).click()
    await expect(page.getByRole('tree', { name: 'Workspace workloads' })).toBeVisible()
    await page.getByRole('button', { name: 'Workspace overview', exact: true }).click()
    await expect(page.getByRole('heading', { name: 'Workspace overview' })).toBeVisible()
  })
}

test('attention matches iOS status, connectivity and ordering rules', async ({ page }) => {
  await page.goto('/')
  const groups = await page.evaluate(async () => {
    const { attentionGroups } = await import('/src/lib/attention.ts')
    const machines = [{ server_id: 'on', name: 'Alpha', status: 'online' }, { server_id: 'off', name: 'Beta', status: 'offline' }]
    const cases = [
      ['blocked', 'blocked', 'on'], ['failed', 'failed', 'off'], ['offline', 'idle', 'off'], ['missing', 'unknown', 'missing'],
      ['starting', 'starting', 'on'], ['working', 'working', 'on'], ['idle', 'idle', 'on'], ['unknown', 'unknown', 'on'],
      ['legacy', 'running', 'on'], ['exited', 'exited', 'off'], ['stopped', 'stopped', 'on'], ['app', 'blocked', 'off'],
    ].map(([name, status, server_id], index) => ({ agent_id: name, name, status, server_id, kind: name === 'app' ? 'app' : 'command', updated_at: new Date(index * 1000).toISOString() }))
    return Object.fromEntries(Object.entries(attentionGroups(cases, machines)).map(([key, agents]) => [key, agents.map((agent) => agent.name)]))
  })
  expect(groups).toEqual({ attention: ['missing', 'offline', 'failed', 'blocked'], working: ['working', 'starting'], idle: ['idle', 'legacy', 'unknown'], inactive: ['exited', 'stopped'] })
})

test('live snapshots update tree attention without opening a PTY; removing the selected Agent returns to overview', async ({ page }) => {
  const backend = await fixture(page)
  await page.goto('/')
  await expect(page.getByRole('button', { name: 'Filter working', exact: true })).toHaveText('2working')
  const agents = mapSnapshot.agents.map((agent) => ({ ...agent, status: 'blocked' }))
  backend.send({ ...mapSnapshot, revision: 2, agents })
  await expect(page.getByRole('button', { name: 'Filter need attention', exact: true })).toHaveText('7need attention')
  expect(backend.terminals).toEqual([])
  await page.getByRole('link', { name: 'Open /Research/reviewer', exact: true }).click()
  await expect(page.locator('.xterm')).toBeVisible()
  backend.send({ ...mapSnapshot, revision: 3, agents: agents.filter((agent) => agent.agent_id !== 'reviewer-a') })
  await expect(page.getByRole('heading', { name: 'Workspace overview' })).toBeVisible()
  await expect(page.getByRole('link', { name: 'Open /Research/reviewer', exact: true })).toHaveCount(0)
})

test('tree counts and traversal handle deep folders, invalid parents and saved sibling order', async ({ page }) => {
  await fixture(page)
  await page.goto('/')
  const result = await page.evaluate(async () => {
    const { attentionGroups } = await import('/src/lib/attention.ts')
    const { overviewForest, visibleOverviewRows } = await import('/src/lib/overview-tree.ts')
    const folders = Array.from({ length: 1000 }, (_, index) => ({ folder_id: `f${index}`, name: `Folder ${index}`, parent_id: index ? `f${index - 1}` : null }))
    const agents = ['a', 'z'].map((id) => ({ agent_id: id, name: id, kind: 'command', status: 'working', server_id: 'on' }))
    const groups = attentionGroups(agents, [{ server_id: 'on', status: 'online' }])
    const tree = { folders, placements: [{ agent_id: 'z', folder_id: 'f999', position: 0 }, { agent_id: 'a', folder_id: 'f999', position: 1 }] }
    const normal = overviewForest(tree, agents, groups, null)
    const rows = visibleOverviewRows(normal.roots, null, {})
    const invalid = overviewForest({ folders: [{ folder_id: 'cycle', name: 'Cycle', parent_id: 'cycle' }], placements: [{ agent_id: 'a', folder_id: 'deleted', position: 0 }] }, agents, groups, 'working')
    const attention = overviewForest(tree, agents, groups, 'attention')
    return { count: normal.roots[0].counts.working, length: rows.length, order: rows.slice(-2).map((row) => row.node.id), collapsed: visibleOverviewRows(normal.roots, null, { f0: false }).length, invalid: visibleOverviewRows(invalid.roots, 'working', {}).map((row) => row.node.id), empty: visibleOverviewRows(attention.roots, 'attention', {}).length }
  })
  expect(result).toEqual({ count: 2, length: 1002, order: ['z', 'a'], collapsed: 1, invalid: ['a', 'z'], empty: 0 })
})

test('overview opens an Agent Interface first, with its existing terminal fallback', async ({ page }) => {
  const backend = await fixture(page)
  const agent = { ...mapSnapshot.agents[0], interface: { protocol: 'treer.agent-interface/v1', instance_id: 'ui-test', port: 8080, capabilities: [], ui_path: '/', registered_at: new Date().toISOString() } }
  await page.route('**/snapshot', (route) => route.fulfill({ json: { ...mapSnapshot, agents: [agent] } }))
  await page.route('**/agents/reviewer-a/interface/ui/**', (route) => route.fulfill({ contentType: 'text/html', body: '<h1>Overview agent interface</h1>' }))
  await page.goto('/')
  await page.getByRole('link', { name: 'Open /Research/reviewer', exact: true }).click()
  await expect(page.frameLocator('iframe').getByRole('heading', { name: 'Overview agent interface' })).toBeVisible()
  expect(backend.terminals).toEqual([])
  await page.locator('[data-agent-id=reviewer-a]').getByRole('button', { name: 'Actions for reviewer', exact: true }).click()
  await page.getByRole('menuitem', { name: 'Logs', exact: true }).click()
  await expect(page.locator('.xterm')).toBeVisible()
  await expect(page.locator('iframe')).toHaveCount(0)
})

test('empty workspace gives visible machine setup guidance without attaching a terminal', async ({ page }) => {
  const backend = await fixture(page)
  await page.route('**/snapshot', (route) => route.fulfill({ json: { ...mapSnapshot, servers: [], agents: [] } }))
  await page.goto('/')
  await expect(page.getByText('Connect a machine, then add your first agent.', { exact: true })).toBeVisible()
  await page.locator('#workspace-panel-overview').getByRole('button', { name: 'Set up a machine', exact: true }).click()
  await expect(page.getByRole('heading', { name: 'Machines', exact: true })).toBeVisible()
  await page.locator('[data-tour=add-machine]').getByRole('button', { name: 'Add', exact: true }).click()
  await expect(page.getByRole('dialog', { name: 'Add machine', exact: true })).toBeVisible()
  expect(backend.terminals).toEqual([])
})

test('overview shares canonical App identity with Tree, including a missing runtime record', async ({ page }) => {
  const backend = await fixture(page)
  const app = { app_id: 'app-1', runtime_agent_id: 'app-runtime', name: 'Dashboard', server_id: 'local', status: 'running', hostname: 'dashboard.test', public_url: null }
  await page.route('**/apps', (route) => route.fulfill({ json: { apps: [app] } }))
  await page.route('**/snapshot', (route) => route.fulfill({ json: { ...mapSnapshot, agents: [] } }))
  await page.route('**/apps/app-1/ui/**', (route) => route.fulfill({ contentType: 'text/html', body: '<h1>Overview App</h1>' }))
  await page.goto('/')
  const open = page.getByRole('link', { name: 'Open /Dashboard', exact: true })
  await expect(open.locator('[data-workload-icon=app]')).toBeVisible()
  expect(backend.terminals).toEqual([])
  backend.send({ ...mapSnapshot, revision: 2, agents: [{ ...mapSnapshot.agents[0], agent_id: 'app-runtime', name: 'app:Dashboard', kind: 'command', path: '/Dashboard', status: 'idle' }] })
  await expect(open).toHaveCount(1)
  await expect(open.locator('[data-workload-icon=app]')).toBeVisible()
  await open.click()
  await expect(page.frameLocator('iframe').getByRole('heading', { name: 'Overview App', exact: true })).toBeVisible()
  { await openTabMenu(page); await expect(page.getByRole('menuitem', { name: 'Show logs', exact: true })).toBeVisible(); await page.keyboard.press('Escape') }
})

test('an empty workspace shows its guidance and no "No workloads in this workspace" text', async ({ page }) => {
  await fixture(page)
  await page.route('**/snapshot', (route) => route.fulfill({ json: { ...mapSnapshot, agents: [] } }))
  await page.route('**/workload-tree', (route) => route.fulfill({ json: { folders: [], placements: [] } }))
  await page.route('**/apps', (route) => route.fulfill({ json: { apps: [] } }))
  await page.goto('/')
  await expect(page.getByRole('heading', { name: 'Your workspace is ready' })).toBeVisible()
  await expect(page.getByRole('tree', { name: 'Workspace workloads' })).toBeVisible()
  await expect(page.getByText(/No workloads in this workspace/)).toHaveCount(0)
})

test.describe('Tree row context menu', () => {
  const workspaceUrl = '/orgs/map-org/workspaces/map-workspace'
  async function expandResearch(page: Page) {
    const folder = page.getByRole('treeitem', { name: 'Research', exact: true })
    if ((await folder.getAttribute('aria-expanded')) !== 'true') await folder.getByRole('button').first().click()
  }

  test('right-click on an Agent row opens its actions at the pointer, including Open in new browser tab', async ({ page, context }) => {
    await page.setViewportSize({ width: 1360, height: 900 })
    await fixture(page)
    await page.goto('/')
    await expandResearch(page)
    const row = page.getByRole('tree').getByRole('link', { name: /^reviewer/ }).first()
    await expect(row).toBeVisible()
    const box = (await row.boundingBox())!
    const point = { x: box.x + 30, y: box.y + box.height / 2 }
    await row.click({ button: 'right', position: { x: 30, y: box.height / 2 } })
    const menu = page.getByRole('menu', { name: 'Actions for reviewer' })
    await expect(menu).toBeVisible()
    const menuBox = (await menu.boundingBox())!
    expect(Math.abs(menuBox.x - point.x)).toBeLessThan(40)
    // Right-click does not select or open the workload.
    await expect(page.getByRole('tab', { name: 'Tab /Research/reviewer', exact: true })).toHaveCount(0)
    const [popup] = await Promise.all([context.waitForEvent('page'), page.getByRole('menuitem', { name: 'Open in new browser tab' }).click()])
    await expect.poll(() => new URL(popup.url()).pathname + new URL(popup.url()).search).toBe(`${workspaceUrl}/workloads/reviewer-a?focus=1`)
    await popup.close()
  })

  test('the context menu offers the same actions as the row button and a folder has no new-tab item', async ({ page }) => {
    await page.setViewportSize({ width: 1360, height: 900 })
    await fixture(page)
    await page.goto('/')
    await page.getByRole('treeitem', { name: 'Research', exact: true }).locator(':scope > div').first().click({ button: 'right' })
    const folderMenu = page.getByRole('menu', { name: 'Actions for Research' })
    await expect(folderMenu).toBeVisible()
    await expect(folderMenu.getByRole('menuitem', { name: 'New folder' })).toBeVisible()
    await expect(folderMenu.getByRole('menuitem', { name: 'Open in new browser tab' })).toHaveCount(0)
    await folderMenu.getByRole('menuitem', { name: 'Rename', exact: true }).click()
    await expect(page.getByRole('dialog').getByRole('textbox', { name: 'Folder name' })).toBeFocused()
    await page.keyboard.press('Escape')
    await expandResearch(page)
    await page.getByRole('tree').getByRole('link', { name: /^reviewer/ }).first().click({ button: 'right' })
    const agentMenu = page.getByRole('menu', { name: 'Actions for reviewer' })
    for (const name of ['Open in new browser tab', 'Rename', 'Copy path', 'Move to folder', 'Logs']) await expect(agentMenu.getByRole('menuitem', { name, exact: true })).toBeVisible()
    await agentMenu.getByRole('menuitem', { name: 'Rename', exact: true }).click()
    await expect(page.getByRole('dialog')).toBeVisible()
    // The menu's close autofocus must not pull focus back to the Tree row from the new dialog.
    await expect(page.getByRole('dialog').getByRole('textbox')).toBeFocused()
  })

  test('Shift+F10 and the Menu key open the menu for the focused row and Escape returns focus', async ({ page }) => {
    await page.setViewportSize({ width: 1360, height: 900 })
    await fixture(page)
    await page.goto('/')
    await expandResearch(page)
    const row = page.getByRole('tree').getByRole('link', { name: /^reviewer/ }).first()
    for (const key of ['Shift+F10', 'ContextMenu']) {
      await row.focus()
      await page.keyboard.press(key)
      await expect(page.getByRole('menu', { name: 'Actions for reviewer' })).toBeVisible()
      await page.keyboard.press('Escape')
      await expect(page.getByRole('menu', { name: 'Actions for reviewer' })).toHaveCount(0)
      await expect(row).toBeFocused()
    }
  })
})

test.describe('Tree Stop and Delete are at the bottom and confirm first', () => {
  async function setup(page: Page) {
    await page.setViewportSize({ width: 1360, height: 900 })
    await fixture(page)
    await page.goto('/')
    const folder = page.getByRole('treeitem', { name: 'Research', exact: true })
    if ((await folder.getAttribute('aria-expanded')) !== 'true') await folder.getByRole('button').first().click()
  }
  async function openMenu(page: Page, how: 'button' | 'right-click') {
    if (how === 'button') await page.getByRole('button', { name: 'Actions for reviewer', exact: true }).first().click()
    else await page.getByRole('tree').getByRole('link', { name: /^reviewer/ }).first().click({ button: 'right' })
  }
  for (const how of ['button', 'right-click'] as const) {
    test(`Stop asks for confirmation, Cancel sends nothing, and the destructive items are last (${how})`, async ({ page }) => {
      await setup(page)
      // Registered after the fixture so it takes precedence over its catch-all API mock.
      const stops: string[] = []
      await page.route('**/agents/*/stop', (route) => { stops.push(route.request().url()); return route.fulfill({ json: {} }) })
      await openMenu(page, how)
      const names = await page.getByRole('menu').getByRole('menuitem').allInnerTexts()
      expect(names.slice(-2)).toEqual(['Stop', 'Delete'])
      // A separator sits between the regular actions and the destructive pair.
      await expect(page.getByRole('menu').getByRole('separator')).toHaveCount(1)
      await page.getByRole('menuitem', { name: 'Stop', exact: true }).click()
      await expect(page.getByRole('dialog').getByRole('heading', { name: /^Stop .*reviewer\?$/ })).toBeVisible()
      expect(stops).toEqual([])
      await page.getByRole('dialog').getByRole('button', { name: 'Cancel', exact: true }).click()
      expect(stops).toEqual([])
      await openMenu(page, how)
      await page.getByRole('menuitem', { name: 'Stop', exact: true }).click()
      await page.getByRole('dialog').getByRole('button', { name: 'Stop', exact: true }).click()
      await expect.poll(() => stops.length).toBe(1)
      expect(stops[0]).toMatch(/\/agents\/reviewer-a\/stop$/)
    })
  }

  test('Delete still asks for confirmation from the Tree', async ({ page }) => {
    await setup(page)
    await openMenu(page, 'right-click')
    await page.getByRole('menuitem', { name: 'Delete', exact: true }).click()
    await expect(page.getByRole('dialog')).toBeVisible()
  })
})
