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
    await page.screenshot({ path: info.outputPath(`overview-${theme}-${mobile ? '390' : 'desktop'}.png`) })
    const attentionFilter = page.getByRole('button', { name: 'Filter need attention', exact: true })
    await attentionFilter.focus()
    await page.keyboard.press("Enter")
    await expect(attentionFilter).toHaveAttribute('aria-pressed', 'true')
    await expect(page.getByRole('region', { name: 'Workspace overview tree' }).getByRole('button', { name: /^Open / })).toHaveCount(1)
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
    await page.getByRole('button', { name: 'Open /Research/reviewer', exact: true }).click()
    await expect.poll(() => backend.terminals.length).toBeGreaterThan(0)
    expect(backend.terminals[0]).toContain('/agents/reviewer-a/terminal')
    if (mobile) {
      await page.getByRole('button', { name: 'Close full-screen terminal', exact: true }).click()
      await page.getByRole('button', { name: 'Workspace overview', exact: true }).click()
    }
    else await page.getByRole('button', { name: 'Back to workspace overview', exact: true }).click()
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
  await page.getByRole('button', { name: 'Open /Research/reviewer', exact: true }).click()
  await expect(page.locator('.xterm')).toBeVisible()
  backend.send({ ...mapSnapshot, revision: 3, agents: agents.filter((agent) => agent.agent_id !== 'reviewer-a') })
  await expect(page.getByRole('heading', { name: 'Workspace overview' })).toBeVisible()
  await expect(page.getByRole('button', { name: 'Open /Research/reviewer', exact: true })).toHaveCount(0)
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
  await page.getByRole('button', { name: 'Open /Research/reviewer', exact: true }).click()
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
  await page.getByRole('button', { name: 'Set up a machine', exact: true }).click()
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
  const open = page.getByRole('button', { name: 'Open /Dashboard', exact: true })
  await expect(open.locator('[data-workload-icon=app]')).toBeVisible()
  expect(backend.terminals).toEqual([])
  backend.send({ ...mapSnapshot, revision: 2, agents: [{ ...mapSnapshot.agents[0], agent_id: 'app-runtime', name: 'app:Dashboard', kind: 'command', path: '/Dashboard', status: 'idle' }] })
  await expect(open).toHaveCount(1)
  await expect(open.locator('[data-workload-icon=app]')).toBeVisible()
  await open.click()
  await expect(page.frameLocator('iframe').getByRole('heading', { name: 'Overview App', exact: true })).toBeVisible()
  await expect(page.getByRole('button', { name: 'Show logs', exact: true })).toBeVisible()
})
