import { openTabMenu, tabAction, backToWorkloads } from "./fixtures/tab-actions"
import { test, expect, type Page, type WebSocketRoute } from '@playwright/test'
import { mockTreeMapApi, mapSnapshot } from './fixtures/treemap'
const names = ['Terminal', 'Alpha', 'Beta', 'Gamma', 'Delta', 'Epsilon', 'Zeta', 'Archive']
const storageKey = 'treer:tabs:v1:map-user:map-workspace'
async function fixture(page: Page) {
  await mockTreeMapApi(page)
  const agents = names.map((name, i) => ({ agent_id: `tab-${i}`, name, kind: i === 7 ? 'app' : 'command', server_id: 'local', status: i === 7 ? 'idle' : i === 2 ? 'blocked' : 'working', path: `/${name}`, ...(i && i !== 7 ? { interface: { protocol: 'treer.agent-interface/v1', instance_id: `ui-${i}`, ui_path: '/', port: 8080, capabilities: [], registered_at: '2026-10-15T00:00:00Z' } } : {}) }))
  const app = { app_id: 'app-archive', runtime_agent_id: 'tab-7', name: 'Archive', server_id: 'local', status: 'running', hostname: 'archive.test', public_url: null }
  const build = { version: 'test', git_commit: 'test' }
  let snapshot = { ...mapSnapshot, servers: mapSnapshot.servers.map((machine) => ({ ...machine, controller_build: build, host_build: build })), agents }, apps = [app]
  await page.route('**/snapshot', (route) => route.fulfill({ json: snapshot }))
  await page.route('**/apps', (route) => route.fulfill({ json: { apps } }))
  await page.route('**/workload-tree', (route) => route.fulfill({ json: { folders: [], placements: [] } }))
  await page.route('**/groups', (route) => route.fulfill({ json: { groups: [] } }))
  await page.route('**/policy-provider', (route) => route.fulfill({ status: 404, json: {} }))
  await page.route('**/api/workspaces/**/ui/**', (route) => route.fulfill({ contentType: 'text/html', body: '<!doctype html><style>body{font:14px system-ui;padding:20px;background:#faf9f6;color:#24221e}body.dark{background:#22211e;color:#e7e5e0}textarea{display:block;box-sizing:border-box;width:100%;max-width:600px;height:100px;padding:12px;background:transparent;color:inherit;border:1px solid #888;border-radius:8px}main{height:1800px}</style><h1>Retained interface</h1><label>Draft<textarea aria-label="Draft"></textarea></label><main>Scroll stays here</main><script>document.body.classList.toggle("dark",new URLSearchParams(location.search).get("theme")==="dark");window.bootId=Math.random();</script>' }))
  let events: WebSocketRoute | undefined
  const connections: string[] = [], closed: string[] = [], inputs: string[] = []
  const terminals = new Map<string, WebSocketRoute>()
  await page.routeWebSocket(/\/events$/, (socket) => { events = socket })
  await page.routeWebSocket(/\/terminal(?:\?.*)?$/, (socket) => {
    const id = new URL(socket.url()).pathname.split('/').at(-2)!
    connections.push(id)
    terminals.set(id, socket)
    socket.onMessage((data) => { if (Buffer.isBuffer(data)) inputs.push(data.toString()) })
    socket.onClose(() => closed.push(id))
    socket.send(JSON.stringify({ type: 'ready', stream_epoch: 'test', revision: 1, replay_chunks: 0 }))
    socket.send(Buffer.from('retained terminal output\r\n'))
  })
  return { connections, closed, inputs, output: (id: string, text: string) => terminals.get(id)!.send(Buffer.from(text)), remove: (id: string) => { snapshot = { ...snapshot, revision: snapshot.revision + 1, agents: snapshot.agents.filter((agent) => agent.agent_id !== id) }; apps = apps.filter((entry) => entry.runtime_agent_id !== id); events!.send(JSON.stringify({ event: 'workspace.snapshot', data: snapshot })) } }
}
async function gotoWorkspace(page: Page) {
  await page.goto('/')
  // Auth renders the tab bar before workspace discovery. Discovery changes its
  // scope/key and closes a transient mobile switcher; wait for real inventory.
  await expect(page.getByRole('button', { name: 'Open /Terminal', exact: true })).toBeVisible()
}
async function switchTab(page: Page, name: string, mobile = false) {
  if (mobile) {
    await page.getByRole('button', { name: /^Show all tabs/ }).click()
    await page.getByRole('button', { name: `Open tab ${name === 'Overview' ? name : '/'+name}`, exact: true }).click()
  } else await page.getByRole('tab', { name: `Tab ${name === 'Overview' ? name : '/'+name}`, exact: true }).click()
}
async function open(page: Page, name: string, mobile = false) {
  await switchTab(page, 'Overview', mobile)
  await page.getByRole('button', { name: `Open /${name}`, exact: true }).click()
}
for (const mobile of [false, true]) for (const theme of ['light', 'dark']) {
  test(`tabs retain iframe drafts, scroll and terminal connection ${theme} ${mobile ? '390' : 'desktop'}`, async ({ page }, info) => {
    await page.setViewportSize({ width: mobile ? 390 : 1360, height: 900 })
    await page.addInitScript((theme) => localStorage.setItem('treer-theme', theme), theme)
    const backend = await fixture(page)
    await gotoWorkspace(page)
    await open(page, 'Alpha', mobile)
    const alpha = page.frameLocator('#workspace-panel-tab-1 iframe')
    await alpha.getByRole('textbox', { name: 'Draft' }).fill('Private draft remains in memory only')
    const boot = await page.locator('#workspace-panel-tab-1 iframe').evaluate((iframe: HTMLIFrameElement) => { iframe.contentWindow!.scrollTo(0, 400); return (iframe.contentWindow as any).bootId })
    await open(page, 'Terminal', mobile)
    await expect(page.locator('.xterm')).toBeVisible()
    await expect.poll(() => backend.connections.length).toBe(1)
    await open(page, 'Beta', mobile)
    await switchTab(page, 'Alpha', mobile)
    await expect(alpha.getByRole('textbox', { name: 'Draft' })).toHaveValue('Private draft remains in memory only')
    expect(await page.locator('#workspace-panel-tab-1 iframe').evaluate((iframe: HTMLIFrameElement) => ({ boot: (iframe.contentWindow as any).bootId, scroll: iframe.contentWindow!.scrollY }))).toEqual({ boot, scroll: 400 })
    expect(backend.connections).toEqual(['tab-0']); expect(backend.closed).toEqual([])
    backend.output('tab-0', 'background output while hidden\r\n')
    expect(await page.evaluate((key) => localStorage.getItem(key), storageKey)).toBe('["tab-1","tab-0","tab-2"]')
    await expect.poll(() => page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
    await page.locator('#workspace-panel-tab-1 iframe').evaluate((iframe: HTMLIFrameElement) => iframe.contentWindow!.scrollTo(0, 0))
    await page.screenshot({ path: info.outputPath(`tabs-${theme}-${mobile ? '390' : 'desktop'}.png`) })
    if (mobile) {
      await page.getByRole('button', { name: /^Show all tabs/ }).click()
      await page.screenshot({ path: info.outputPath(`tabs-${theme}-390-switcher.png`) })
      await expect(page.getByRole('dialog', { name: /^Open tabs/ })).toBeVisible()
      await page.getByRole('button', { name: 'Done', exact: true }).click()
    }
    await switchTab(page, 'Terminal', mobile)
    await expect(page.locator('.xterm-screen')).toContainText('retained terminal output')
    await expect(page.locator('.xterm-screen')).toContainText('background output while hidden')
    await page.locator('.xterm-helper-textarea').press('a')
    await expect.poll(() => backend.inputs).toContain('a')
    await switchTab(page, 'Overview', mobile)
    expect(backend.closed).toEqual([])
  })
}
for (const mobile of [false, true]) test(`tabs enforce LRU budget and reload on selection ${mobile ? '390' : 'desktop'}`, async ({ page }) => {
  await page.setViewportSize({ width: mobile ? 390 : 1360, height: 900 })
  const backend = await fixture(page)
  await gotoWorkspace(page)
  const count = mobile ? 4 : 6
  for (const name of names.slice(0, count)) await open(page, name, mobile)
  await expect.poll(() => backend.closed).toContain('tab-0')
  await expect(page.locator('iframe')).toHaveCount(count - 1)
  await expect(page.locator('.xterm')).toHaveCount(0)
  if (mobile) {
    await page.getByRole('button', { name: /^Show all tabs/ }).click()
    await expect(page.getByRole('button', { name: 'Open tab /Terminal', exact: true })).toContainText('Unloaded')
    await page.getByRole('button', { name: 'Done', exact: true }).click()
  } else await expect(page.getByRole('tab', { name: 'Tab /Terminal', exact: true })).toHaveAttribute('title', /Unloaded/)
  await switchTab(page, 'Terminal', mobile)
  await expect.poll(() => backend.connections.filter((id) => id === 'tab-0').length).toBe(2)
  await expect(page.getByRole('status').filter({ hasText: 'Reloaded /Terminal' })).toHaveCount(1)
  await expect(page.locator('iframe')).toHaveCount(count - 2)
})
test('tabs persist only bounded IDs, restore cold and prune deleted IDs', async ({ page }) => {
  const backend = await fixture(page)
  await gotoWorkspace(page)
  await open(page, 'Alpha'); await open(page, 'Terminal')
  // The URL names the selected workload and restores it on reload; leaving to Overview first keeps this a cold restore.
  await switchTab(page, 'Overview')
  await page.reload()
  await expect(page.getByRole('tab', { name: 'Tab /Alpha', exact: true })).toBeVisible()
  await expect(page.getByRole('tab', { name: 'Tab /Terminal', exact: true })).toHaveAttribute('title', /Unloaded/)
  await expect(page.getByRole('heading', { name: 'Workspace overview' })).toBeVisible()
  await expect(page.locator('iframe,.xterm')).toHaveCount(0)
  expect(backend.connections).toHaveLength(1)
  backend.remove('tab-1')
  await expect(page.getByRole('tab', { name: 'Tab /Alpha', exact: true })).toHaveCount(0)
  await expect.poll(() => page.evaluate((key) => localStorage.getItem(key), storageKey)).toBe('["tab-0"]')
  await switchTab(page, 'Terminal')
  await expect.poll(() => backend.connections.length).toBe(2)
  backend.remove('tab-0')
  await expect(page.getByRole('heading', { name: 'Workspace overview' })).toBeVisible()
  await expect(page.locator('.xterm')).toHaveCount(0)
})
test('tabs have roving keyboard navigation, shortcuts and close fallback', async ({ page }) => {
  const backend = await fixture(page)
  await gotoWorkspace(page)
  await open(page, 'Alpha'); await open(page, 'Beta'); await open(page, 'Terminal')
  await page.locator('.xterm-helper-textarea').press('Control+w')
  await expect.poll(() => backend.inputs).toContain('\x17')
  await expect(page.getByRole('tab', { name: 'Tab /Terminal', exact: true })).toHaveCount(1)
  const first = page.getByRole('tab', { name: 'Tab Overview', exact: true })
  await first.focus(); await page.keyboard.press('ArrowRight')
  await expect(page.getByRole('tab', { name: 'Tab /Alpha', exact: true })).toBeFocused()
  await page.keyboard.press('End')
  await expect(page.getByRole('tab', { name: 'Tab /Terminal', exact: true })).toBeFocused()
  await page.keyboard.press('Home'); await expect(first).toBeFocused()
  await page.keyboard.press('Control+3')
  await expect(page.getByRole('tab', { name: 'Tab /Beta', exact: true })).toHaveAttribute('aria-selected', 'true')
  await page.keyboard.press('Control+w')
  await expect(page.getByRole('tab', { name: 'Tab /Beta', exact: true })).toHaveCount(0)
  const tty = page.getByRole('tab', { name: 'Tab /Terminal', exact: true })
  await tty.focus(); await page.keyboard.press('Delete')
  await expect(tty).toHaveCount(0)
  await expect.poll(() => backend.closed).toContain('tab-0')
  await page.getByRole('button', { name: 'Close tab /Alpha', exact: true }).click()
  await expect(first).toHaveAttribute('aria-selected', 'true')
  await expect(page.getByRole('button', { name: 'Close tab Overview', exact: true })).toHaveCount(0)
})
test('App tab stays mounted through Agent, overview and settings; log choice is local', async ({ page }) => {
  const backend = await fixture(page)
  await gotoWorkspace(page)
  await open(page, 'Archive')
  const frame = page.frameLocator('#workspace-panel-tab-7 iframe')
  await frame.getByRole('textbox', { name: 'Draft' }).fill('App draft')
  await open(page, 'Terminal'); await switchTab(page, 'Archive')
  await expect(frame.getByRole('textbox', { name: 'Draft' })).toHaveValue('App draft')
  await page.getByRole('button', { name: 'Workspace settings', exact: true }).click()
  await switchTab(page, 'Archive')
  await expect(frame.getByRole('textbox', { name: 'Draft' })).toHaveValue('App draft')
  await tabAction(page, 'Show logs')
  await expect.poll(() => backend.connections).toContain('tab-7')
  await open(page, 'Alpha'); await switchTab(page, 'Archive')
  { await openTabMenu(page); await expect(page.getByRole('menuitem', { name: 'Show App', exact: true })).toBeVisible(); await page.keyboard.press('Escape') }
  await expect(page.locator('#workspace-panel-tab-7 .xterm')).toBeVisible()
})
for (const mode of ['throws', 'invalid'] as const) test(`tabs tolerate ${mode} storage and keep navigation usable`, async ({ page }) => {
  await fixture(page)
  await page.addInitScript(({ key, mode }) => {
    if (mode === 'throws') for (const name of ['getItem', 'setItem']) Object.defineProperty(Storage.prototype, name, { value() { throw new Error('synthetic storage denied') } })
    else localStorage.setItem(key, JSON.stringify(['tab-1','https://bad.test/token','tab-1',null,...Array.from({ length: 100 }, (_, i) => `missing-${i}`)]))
  }, { key: storageKey, mode })
  await gotoWorkspace(page)
  await open(page, 'Alpha')
  await expect(page.frameLocator('#workspace-panel-tab-1 iframe').getByRole('textbox', { name: 'Draft' })).toBeVisible()
  await page.getByRole('button', { name: 'Close tab /Alpha', exact: true }).click()
  await expect(page.getByRole('heading', { name: 'Workspace overview' })).toBeVisible()
})

test('workspace and user changes never retain another scope’s live surfaces or tab cache', async ({ page }) => {
  await fixture(page)
  const second = { ...mapSnapshot.workspace, workspace_id: 'second', name: 'Second' }
  await page.route('**/api/workspaces?*', (route) => route.fulfill({ json: { workspaces: [mapSnapshot.workspace, second] } }))
  await page.route('**/api/workspaces/second/snapshot', (route) => route.fulfill({ json: { ...mapSnapshot, workspace: second, agents: [{ ...mapSnapshot.agents[0], agent_id: 'tab-1', name: 'Other scope', path: '/Other scope' }] } }))
  await gotoWorkspace(page)
  await open(page, 'Alpha')
  await expect(page.locator('iframe')).toHaveCount(1)
  await page.getByRole('combobox', { name: 'Workspace', exact: true }).click()
  await page.getByRole('option', { name: 'Second', exact: true }).click()
  await expect(page.getByRole('heading', { name: 'Workspace overview' })).toBeVisible()
  await expect(page.locator('iframe')).toHaveCount(0)
  await expect(page.getByRole('tab', { name: 'Tab /Alpha', exact: true })).toHaveCount(0)
  await page.getByRole('combobox', { name: 'Workspace', exact: true }).click()
  await page.getByRole('option', { name: 'Studio', exact: true }).click()
  await expect(page.getByRole('tab', { name: 'Tab /Alpha', exact: true })).toHaveAttribute('title', /Unloaded/)
  await expect(page.locator('iframe')).toHaveCount(0)
  await page.route('**/api/auth/me', (route) => route.fulfill({ json: { user_id: 'other-user', email: 'other@example.test', preferred_name: 'Other' } }))
  await page.reload()
  await expect(page.getByRole('heading', { name: 'Workspace overview' })).toBeVisible()
  await expect(page.getByRole('tab')).toHaveCount(1)
  expect(await page.evaluate((key) => localStorage.getItem(key), storageKey)).toBe('["tab-1"]')
})

test('shrinking to phone clamps LRU without unloading the active terminal', async ({ page }) => {
  const backend = await fixture(page)
  await gotoWorkspace(page)
  await open(page, 'Alpha'); await open(page, 'Beta'); await open(page, 'Gamma'); await open(page, 'Delta'); await open(page, 'Terminal')
  await page.setViewportSize({ width: 390, height: 844 })
  await expect(page.locator('iframe')).toHaveCount(2)
  await expect(page.locator('.xterm')).toBeVisible()
  expect(backend.connections).toEqual(['tab-0']); expect(backend.closed).toEqual([])
  await page.getByRole('button', { name: /^Show all tabs/ }).click()
  await expect(page.getByRole('dialog', { name: /^Open tabs/ }).getByText('Live 4/4', { exact: true })).toBeVisible()
  await expect.poll(() => page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
})

for (const theme of ['light', 'dark']) test(`pinned Overview remains visible while tabs overflow ${theme}`, async ({ page }, info) => {
  await page.setViewportSize({ width: 1000, height: 900 })
  await page.addInitScript((theme) => localStorage.setItem('treer-theme', theme), theme)
  await fixture(page)
  await gotoWorkspace(page)
  for (const name of names) await open(page, name)
  const list = page.getByRole('tablist', { name: 'Open pages' })
  await expect.poll(() => list.evaluate((element) => element.scrollLeft)).toBeGreaterThan(100)
  const overview = page.getByRole('tab', { name: 'Tab Overview', exact: true })
  const listBox = (await list.boundingBox())!
  const overviewBox = (await overview.boundingBox())!
  expect(Math.abs(overviewBox.x - listBox.x)).toBeLessThan(1)
  expect(await overview.evaluate((element) => {
    const rect = element.getBoundingClientRect()
    return element.contains(document.elementFromPoint(rect.x + rect.width / 2, rect.y + rect.height / 2))
  })).toBe(true)
  await page.screenshot({ path: info.outputPath(`tabs-overflow-${theme}.png`) })
  await overview.click()
  await expect(page.getByRole('heading', { name: 'Workspace overview' })).toBeVisible()
})


test('mobile switcher recovers after delayed initial workspace discovery', async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 900 })
  await fixture(page)
  let release!: () => void
  const discovery = new Promise<void>((resolve) => { release = resolve })
  await page.route('**/api/workspaces?*', async (route) => {
    await discovery
    await route.fulfill({ json: { workspaces: [mapSnapshot.workspace] } })
  })
  await page.goto('/')
  await page.getByRole('button', { name: /^Show all tabs/ }).click()
  await expect(page.getByRole('dialog', { name: /^Open tabs/ })).toBeVisible()
  release()
  await expect(page.getByRole('button', { name: 'Open /Terminal', exact: true })).toBeVisible()
  await expect(page.getByRole('dialog', { name: /^Open tabs/ })).toHaveCount(0)
  await open(page, 'Terminal', true)
  await expect(page.locator('.xterm')).toBeVisible()
})

for (const theme of ['light', 'dark']) test(`tab context actions target background page and keyboard focus ${theme}`, async ({ page }, info) => {
  await page.addInitScript((theme) => localStorage.setItem('treer-theme', theme), theme)
  await fixture(page); await gotoWorkspace(page)
  await open(page, 'Archive'); await open(page, 'Alpha')
  await openTabMenu(page, '/Archive')
  await expect(page.getByRole('tab', { name: 'Tab /Alpha', exact: true })).toHaveAttribute('aria-selected', 'true')
  await expect(page.getByRole('menuitem', { name: 'Show logs', exact: true })).toBeVisible()
  await page.screenshot({ path: info.outputPath(`tab-actions-${theme}-desktop.png`) })
  await page.getByRole('menuitem', { name: 'Show logs', exact: true }).click()
  await expect(page.locator('#workspace-panel-tab-7 iframe')).toHaveCount(0)
  await expect(page.locator('#workspace-panel-tab-1 iframe')).toBeVisible()
  const archive = page.getByRole('tab', { name: 'Tab /Archive', exact: true })
  await archive.focus(); await page.keyboard.press('Shift+F10')
  await expect(page.getByRole('menuitem', { name: 'Show App', exact: true })).toBeVisible()
  await page.keyboard.press('Escape'); await expect(archive).toBeFocused()
  await archive.press('ContextMenu'); await expect(page.getByRole('menu')).toBeVisible()
  await page.keyboard.press('Home'); await page.keyboard.press('Enter')
  await expect(page.locator('#workspace-panel-tab-7 iframe')).toHaveCount(1)
  await expect(page.getByRole('tab', { name: 'Tab /Alpha', exact: true })).toHaveAttribute('aria-selected', 'true')
  await expect(page.getByRole('button', { name: 'Upload file', exact: true })).toHaveCount(0)
})

test('tab destructive actions require confirmation and target inactive stable IDs', async ({ page }) => {
  await fixture(page); const calls: string[] = []
  await page.route('**/agents/*/stop', (route) => { calls.push(route.request().url()); return route.fulfill({ json: {} }) })
  await gotoWorkspace(page); await open(page, 'Terminal'); await open(page, 'Alpha')
  await tabAction(page, 'Stop agent', '/Terminal')
  await expect(page.getByRole('heading', { name: 'Stop /Terminal?' })).toBeVisible()
  expect(calls).toEqual([])
  await page.getByRole('button', { name: 'Cancel', exact: true }).click()
  expect(calls).toEqual([])
  await tabAction(page, 'Stop agent', '/Terminal')
  await page.getByRole('dialog').getByRole('button', { name: 'Stop agent', exact: true }).click()
  await expect.poll(() => calls.length).toBe(1)
  expect(calls[0]).toContain('/agents/tab-0/stop')
  await tabAction(page, 'Delete agent', '/Terminal')
  await expect(page.getByRole('heading', { name: 'Delete agent', exact: true })).toBeVisible()
  await expect(page.getByRole('dialog')).toContainText('Terminal')
})

test('tab upload pins the background target machine; offline upload disabled', async ({ page }) => {
  await fixture(page)
  let uploadedTo = ''
  await page.route('**/machines/*/files', (route) => { uploadedTo = route.request().url(); return route.fulfill({ json: { path: 'sample.txt', bytes_written: 4 } }) })
  await page.route('**/snapshot', (route) => route.fulfill({ json: { ...mapSnapshot, agents: [
    { agent_id: 'one', name: 'One', kind: 'command', server_id: 'local', status: 'working' },
    { agent_id: 'two', name: 'Two', kind: 'command', server_id: 'remote', status: 'idle' },
  ] } }))
  await page.goto('/'); await expect(page.getByRole('button', { name: 'Open /One', exact: true })).toBeVisible()
  await page.getByRole('button', { name: 'Open /One', exact: true }).click()
  await switchTab(page, 'Overview'); await page.getByRole('button', { name: 'Open /Two', exact: true }).click()
  await openTabMenu(page, '/Two'); await expect(page.getByRole('menuitem', { name: 'Upload file' })).toBeDisabled(); await page.keyboard.press('Escape')
  await tabAction(page, 'Upload file', '/One')
  await expect(page.getByRole('dialog')).toContainText('Upload to workstation.')
  await expect(page.getByRole('tab', { name: 'Tab /Two', exact: true, includeHidden: true })).toHaveAttribute('aria-selected', 'true')
  await page.getByRole('dialog').locator('input[type=file]').setInputFiles({ name: 'sample.txt', mimeType: 'text/plain', buffer: Buffer.from('test') })
  await page.getByRole('dialog').getByRole('button', { name: 'Upload', exact: true }).click()
  await expect.poll(() => uploadedTo).toContain('/machines/local/files')
})

test('tab close others and close right are atomic and preserve Overview', async ({ page }) => {
  await fixture(page); await gotoWorkspace(page)
  await open(page, 'Terminal'); await open(page, 'Alpha'); await open(page, 'Beta')
  await tabAction(page, 'Close tabs to the right', '/Terminal')
  await expect(page.getByRole('tab')).toHaveCount(2)
  await expect(page.getByRole('tab', { name: 'Tab /Terminal', exact: true })).toHaveAttribute('aria-selected', 'true')
  await open(page, 'Alpha'); await open(page, 'Beta')
  await tabAction(page, 'Close other tabs', '/Alpha')
  await expect(page.getByRole('tab')).toHaveCount(2)
  await expect(page.getByRole('tab', { name: 'Tab /Alpha', exact: true })).toHaveAttribute('aria-selected', 'true')
  await openTabMenu(page, 'Overview'); await expect(page.getByRole('menuitem', { name: 'Close tab', exact: true })).toBeDisabled()
})

for (const theme of ['light', 'dark']) test(`390 long press active title and switcher card ${theme}`, async ({ page }, info) => {
  await page.setViewportSize({ width: 390, height: 844 })
  await page.addInitScript((theme) => localStorage.setItem('treer-theme', theme), theme)
  await fixture(page); await gotoWorkspace(page); await open(page, 'Archive', true)
  const active = page.getByRole('button', { name: /^Page .*; long-press for actions$/ })
  await active.dispatchEvent('pointerdown', { pointerType: 'touch', button: 0, clientX: 60, clientY: 20 })
  await expect(page.getByRole('menu', { name: 'Page actions' })).toBeVisible()
  await active.dispatchEvent('pointerup', { pointerType: 'touch' })
  await active.dispatchEvent('click')
  await expect(page.getByRole('dialog', { name: /^Open tabs/ })).toHaveCount(0)
  await expect(page.getByRole('menu')).toBeVisible()
  await page.screenshot({ path: info.outputPath(`tab-actions-${theme}-390.png`) })
  await page.keyboard.press('Escape')
  await page.getByRole('button', { name: /^Show all tabs/ }).click()
  const card = page.getByRole('button', { name: 'Open tab /Archive', exact: true })
  await card.dispatchEvent('pointerdown', { pointerType: 'touch', button: 0, clientX: 180, clientY: 200 })
  await expect(page.getByRole('menu')).toBeVisible()
  await card.dispatchEvent('pointerup', { pointerType: 'touch' })
  await page.keyboard.press('Escape'); await expect(card).toBeFocused()
  await expect.poll(() => page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
  await card.dispatchEvent('pointerdown', { pointerType: 'touch', button: 0, clientX: 180, clientY: 200 })
  await card.dispatchEvent('pointermove', { pointerType: 'touch', clientX: 180, clientY: 240 })
  await card.dispatchEvent('pointerup', { pointerType: 'touch' })
  await expect(page.getByRole('menu')).toHaveCount(0)
})

test('tab rename targets the inactive page and preserves backend denial in dialog', async ({ page }) => {
  await fixture(page); let target = ''
  await page.route('**/agents/tab-0', (route) => {
    target = route.request().url()
    return route.fulfill({ status: 403, json: { error: { code: 'forbidden', message: 'Synthetic policy denies rename' } } })
  })
  await gotoWorkspace(page); await open(page, 'Terminal'); await open(page, 'Alpha')
  await tabAction(page, 'Rename agent', '/Terminal')
  const dialog = page.getByRole('dialog')
  await expect(dialog.getByRole('heading', { name: 'Rename agent' })).toBeVisible()
  await dialog.getByRole('textbox').fill('Changed')
  await dialog.getByRole('button', { name: 'Rename', exact: true }).click()
  await expect(dialog).toContainText('Synthetic policy denies rename')
  expect(target).toContain('/agents/tab-0')
  await expect(page.getByRole('tab', { name: 'Tab /Alpha', exact: true, includeHidden: true })).toHaveAttribute('aria-selected', 'true')
})

test('tab context new actions and confirmation render in Chinese', async ({ page }) => {
  await page.addInitScript(() => localStorage.setItem('treer-lang', 'zh'))
  await fixture(page); await page.goto('/')
  await page.getByRole('button', { name: '打开 /Terminal', exact: true }).click()
  await page.getByRole('tab', { name: '标签页 /Terminal', exact: true }).click({ button: 'right' })
  await expect(page.getByRole('menuitem', { name: '关闭其他标签页', exact: true })).toBeVisible()
  await expect(page.getByRole('menuitem', { name: '关闭右侧标签页', exact: true })).toBeVisible()
  await page.getByRole('menuitem', { name: '停止 Agent', exact: true }).click()
  await expect(page.getByRole('heading', { name: '停止 /Terminal？', exact: true })).toBeVisible()
})

for (const mobile of [false, true]) for (const theme of ['light', 'dark']) test(`visible active tab menu trigger ${theme} ${mobile ? '390' : 'desktop'}`, async ({ page }, info) => {
  await page.setViewportSize({ width: mobile ? 390 : 1360, height: 900 })
  await page.addInitScript((theme) => localStorage.setItem('treer-theme', theme), theme)
  await fixture(page); await gotoWorkspace(page)
  await open(page, 'Terminal', mobile); await open(page, 'Archive', mobile)
  const trigger = page.getByRole('button', { name: 'More actions for /Archive', exact: true })
  await expect(trigger).toBeVisible()
  await expect(page.getByRole('button', { name: 'More actions for /Terminal', exact: true })).toHaveCount(0)
  await trigger.click()
  await expect(page.getByRole('menuitem', { name: 'Rename App', exact: true })).toBeVisible()
  await expect(page.getByRole('menuitem', { name: 'Stop App', exact: true })).toBeVisible()
  await expect(page.getByRole('menuitem', { name: 'Delete App', exact: true })).toBeVisible()
  await expect(page.getByRole('menuitem', { name: 'Rename agent', exact: true })).toHaveCount(0)
  await page.screenshot({ path: info.outputPath(`tab-actions-p3-${theme}-${mobile ? '390' : 'desktop'}.png`) })
  await page.keyboard.press('Escape'); await expect(trigger).toBeFocused()
  await expect.poll(() => page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
  await switchTab(page, 'Terminal', mobile)
  await page.getByRole('button', { name: 'More actions for /Terminal', exact: true }).click()
  await expect(page.getByRole('menuitem', { name: 'Rename agent', exact: true })).toBeVisible()
  await expect(page.getByRole('menuitem', { name: 'Stop agent', exact: true })).toBeVisible()
  await expect(page.getByRole('menuitem', { name: 'Delete agent', exact: true })).toBeVisible()
})

test('Chinese App menu uses type-specific action labels', async ({ page }) => {
  await page.addInitScript(() => localStorage.setItem('treer-lang', 'zh'))
  await fixture(page); await page.goto('/')
  await page.getByRole('button', { name: '打开 /Archive', exact: true }).click()
  await page.getByRole('button', { name: '/Archive 的更多操作', exact: true }).click()
  for (const name of ['重命名 App', '停止 App', '删除 App']) await expect(page.getByRole('menuitem', { name, exact: true })).toBeVisible()
})

for (const theme of ['light', 'dark']) for (const surface of ['Terminal', 'Alpha']) {
  test(`mobile top bar returns directly to Tree and retains ${surface} ${theme}`, async ({ page }, info) => {
    await page.setViewportSize({ width: 390, height: 844 })
    await page.addInitScript((theme) => localStorage.setItem('treer-theme', theme), theme)
    const backend = await fixture(page)
    await gotoWorkspace(page)
    await open(page, surface, true)
    const frame = page.frameLocator('#workspace-panel-tab-1 iframe')
    if (surface === 'Alpha') await frame.getByRole('textbox', { name: 'Draft' }).fill('Retain this draft')
    const connections = [...backend.connections]
    const browse = page.getByRole('button', { name: 'Browse tree', exact: true })
    await expect(browse).toBeVisible()
    await expect(browse).toHaveAttribute('aria-controls', 'tree-sidebar')
    const hit = (await browse.boundingBox())!
    expect(hit.width).toBeGreaterThanOrEqual(44)
    expect(hit.height).toBeGreaterThanOrEqual(44)
    await page.screenshot({ path: info.outputPath(`tree-return-${surface}-${theme}-390.png`) })
    await browse.focus()
    await page.keyboard.press('Enter')
    await expect(page.locator('#tree-sidebar')).toBeVisible()
    await expect(page.getByRole('heading', { name: 'Workspace overview', exact: true })).not.toBeVisible()
    await expect(page.locator(`[data-agent-id="tab-${surface === 'Alpha' ? 1 : 0}"]`)).toHaveAttribute('aria-selected', 'true')
    const selectedRow = page.locator(`[data-agent-id="tab-${surface === 'Alpha' ? 1 : 0}"] [data-tree-primary]`)
    await expect(selectedRow).toBeFocused()
    await selectedRow.click()
    if (surface === 'Alpha') await expect(frame.getByRole('textbox', { name: 'Draft' })).toHaveValue('Retain this draft')
    else expect(backend.connections).toEqual(connections)
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
  })
}

test('Tree top-bar entry works from mobile settings and is absent on desktop', async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 })
  await fixture(page)
  await gotoWorkspace(page)
  await page.getByRole('button', { name: 'Browse tree', exact: true }).click()
  await page.getByRole('button', { name: 'Workspace settings', exact: true }).click()
  await page.getByRole('button', { name: 'Browse tree', exact: true }).click()
  await expect(page.locator('#tree-sidebar')).toBeVisible()
  await page.setViewportSize({ width: 1360, height: 960 })
  await expect(page.getByRole('button', { name: 'Browse tree', exact: true })).toHaveCount(0)
})

const workspaceUrl = '/orgs/map-org/workspaces/map-workspace'
test.describe('workload deep links and new-tab entry points', () => {
  test('a deep link opens that workload, keeps the URL, and the URL follows the selection', async ({ page }) => {
    await page.setViewportSize({ width: 1360, height: 900 })
    await fixture(page)
    await page.goto(`${workspaceUrl}/workloads/tab-1`)
    await expect(page.getByRole('tab', { name: 'Tab /Alpha', exact: true })).toHaveAttribute('aria-selected', 'true')
    await expect(page.locator('#workspace-panel-tab-1 iframe')).toBeVisible()
    await expect(page).toHaveURL(new RegExp(`${workspaceUrl}/workloads/tab-1$`))
    await page.reload()
    await expect(page.getByRole('tab', { name: 'Tab /Alpha', exact: true })).toHaveAttribute('aria-selected', 'true')
    await switchTab(page, 'Overview')
    await expect(page).toHaveURL(new RegExp(`${workspaceUrl}$`))
    await switchTab(page, 'Alpha')
    await expect(page).toHaveURL(new RegExp(`${workspaceUrl}/workloads/tab-1$`))
  })

  test('an unknown or malformed workload id reports a clear error and selects nothing else', async ({ page }) => {
    await page.setViewportSize({ width: 1360, height: 900 })
    await fixture(page)
    await page.goto(`${workspaceUrl}/workloads/not-a-workload`)
    await expect(page.getByRole('alert').filter({ hasText: 'not found, or you do not have access' })).toBeVisible()
    await expect(page).toHaveURL(new RegExp(`${workspaceUrl}$`))
    await expect(page.getByRole('tab', { name: 'Tab Overview', exact: true })).toHaveAttribute('aria-selected', 'true')
    await page.goto(`${workspaceUrl}/workloads/..%2Fsecret`)
    await expect(page.getByRole('tab', { name: 'Tab Overview', exact: true })).toHaveAttribute('aria-selected', 'true')
    await expect(page.locator('iframe')).toHaveCount(0)
  })

  test('focus mode shows only the workload with a way back to the full Console', async ({ page }) => {
    await page.setViewportSize({ width: 1360, height: 900 })
    await fixture(page)
    await page.goto(`${workspaceUrl}/workloads/tab-1?focus=1`)
    await expect(page.locator('#workspace-panel-tab-1 iframe')).toBeVisible()
    await expect(page.getByRole('tablist')).toHaveCount(0)
    await expect(page.getByRole('tree')).toHaveCount(0)
    const back = page.getByRole('link', { name: 'Open full Console' })
    await expect(back).toHaveAttribute('href', `${workspaceUrl}/workloads/tab-1`)
    await back.click()
    await expect(page).toHaveURL(new RegExp(`${workspaceUrl}/workloads/tab-1$`))
    await expect(page.getByRole('tablist')).toBeVisible()
    await expect(page.getByRole('tree')).toBeVisible()
  })

  for (const mobile of [false, true]) test(`the tab menu opens the workload in a new browser tab ${mobile ? '390' : 'desktop'}`, async ({ page, context }) => {
    await page.setViewportSize({ width: mobile ? 390 : 1360, height: 900 })
    await fixture(page)
    await gotoWorkspace(page)
    await open(page, 'Alpha', mobile)
    if (mobile) { await page.getByRole('button', { name: /^More actions/ }).first().click() }
    else await page.getByRole('tab', { name: 'Tab /Alpha', exact: true }).click({ button: 'right' })
    const [popup] = await Promise.all([context.waitForEvent('page'), page.getByRole('menuitem', { name: 'Open in new browser tab' }).click()])
    expect(new URL(popup.url()).pathname + new URL(popup.url()).search).toBe(`${workspaceUrl}/workloads/tab-1?focus=1`)
    expect(popup.url()).not.toMatch(/token|key|secret/i)
    await popup.close()
  })

  test('the new-tab menu item is localized', async ({ page }) => {
    await page.addInitScript(() => localStorage.setItem('treer-lang', 'zh'))
    await page.setViewportSize({ width: 1360, height: 900 })
    await fixture(page); await page.goto('/')
    await page.getByRole('button', { name: '打开 /Alpha', exact: true }).click()
    await page.getByRole('tab', { name: '标签页 /Alpha', exact: true }).click({ button: 'right' })
    await expect(page.getByRole('menuitem', { name: '在新浏览器标签页中打开' })).toBeVisible()
  })
})

test.describe('ctrl/cmd-click and middle-click open a workload in a new browser tab', () => {
  const expected = `${workspaceUrl}/workloads/tab-1?focus=1`
  const where = (url: string) => new URL(url).pathname + new URL(url).search
  for (const how of ['ctrl', 'middle'] as const) {
    test(`overview rows and tree rows (${how})`, async ({ page, context }) => {
      await page.setViewportSize({ width: 1360, height: 900 })
      await fixture(page)
      await gotoWorkspace(page)
      const click = (locator: import('@playwright/test').Locator) => how === 'ctrl' ? locator.click({ modifiers: ['ControlOrMeta'] }) : locator.click({ button: 'middle' })
      for (const target of [page.getByRole('button', { name: 'Open /Alpha', exact: true }), page.getByRole('tree').getByRole('button', { name: /^Alpha/ })]) {
        const [popup] = await Promise.all([context.waitForEvent('page'), click(target)])
        expect(where(popup.url())).toBe(expected)
        await popup.close()
        // The current page did not navigate in place.
        await expect(page.getByRole('tab', { name: 'Tab Overview', exact: true })).toHaveAttribute('aria-selected', 'true')
      }
    })
  }

  test('a plain click still opens in place', async ({ page }) => {
    await page.setViewportSize({ width: 1360, height: 900 })
    await fixture(page)
    await gotoWorkspace(page)
    await page.getByRole('button', { name: 'Open /Alpha', exact: true }).click()
    await expect(page.getByRole('tab', { name: 'Tab /Alpha', exact: true })).toHaveAttribute('aria-selected', 'true')
  })

  test('map blocks open a new browser tab on ctrl-click and keep the map open', async ({ page, context }) => {
    await page.setViewportSize({ width: 1360, height: 900 })
    await fixture(page)
    await gotoWorkspace(page)
    await page.getByRole('button', { name: 'Expand tree map' }).click()
    const block = page.getByRole('dialog').getByRole('button', { name: /Alpha/ }).first()
    const [popup] = await Promise.all([context.waitForEvent('page'), block.click({ modifiers: ['ControlOrMeta'] })])
    expect(where(popup.url())).toBe(expected)
    await popup.close()
    await expect(page.getByRole('dialog')).toBeVisible()
  })
})
