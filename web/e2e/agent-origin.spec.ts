import { createServer } from 'node:https'
import { key, cert } from './fixtures/synthetic-tls'
import { test, expect, type Page } from '@playwright/test'
import { openTabMenu } from './fixtures/tab-actions'
import { fixture, gotoWorkspace, open, switchTab } from './fixtures/workspace-tabs'
// Full Chromium avoids the bundled macOS headless shell's SIGSEGV on
// native cross-site tabs. Loopback TLS avoids popup-route/DNS races; the
// test-only certificate flag applies before a native popup attaches to CDP.
test.use({ channel: 'chromium', ignoreHTTPSErrors: true, launchOptions: { args: ['--host-resolver-rules=MAP *.agents.example.test 127.0.0.1', '--ignore-certificate-errors'] } })
const alphaId = 'ag_0123456789abcdef0123456789abcdef'
const terminalId = 'ag_00000000000000000000000000000000'
let isolatedAgentUrl = ''
let originStatus = 200
const server = createServer({ key, cert }, (_request, response) => {
  response.writeHead(originStatus, { 'Content-Type': 'text/html' })
  response.end('<!doctype html><h1>Agent interface</h1><a href="/">Retry</a>')
})
test.beforeAll(async () => {
  await new Promise<void>((resolve, reject) => { server.once('error', reject); server.listen(0, '127.0.0.1', resolve) })
  const address = server.address()
  if (!address || typeof address === 'string') throw new Error('No synthetic origin port')
  isolatedAgentUrl = `https://ag-0123456789abcdef0123456789abcdef.agents.example.test:${address.port}/`
})
test.afterAll(async () => { await new Promise<void>((resolve, reject) => server.close(error => error ? reject(error) : resolve())) })
test.beforeEach(() => { originStatus = 200 })
async function originFixture(page: Page, url: string | undefined = isolatedAgentUrl) {
  const live = await fixture(page, url === undefined ? {} : { 'tab-1': url }, { 'tab-0': terminalId, 'tab-1': alphaId })
  await page.route('**/api/auth/config', route => route.fulfill({ json: { capabilities: ['agent.interface-origin.v1'] } }))
  return live
}
for (const width of [1360, 390]) test(`isolated Agent native links and synchronous menu preserve the isolated origin (${width})`, async ({ page, context }) => {
  await page.setViewportSize({ width, height: 900 })
  await originFixture(page)
  await gotoWorkspace(page)
  const link = page.getByRole('link', { name: 'Open /Alpha', exact: true })
  await expect(link).toHaveAttribute('href', isolatedAgentUrl)
  // Copy-link uses this same native href. Modified clicks must remain native.
  for (const click of [{ button: 'middle' as const }, { modifiers: ['ControlOrMeta' as const] }]) {
    const [popup] = await Promise.all([context.waitForEvent('page'), link.click(click)])
    await expect.poll(() => popup.url()).toBe(isolatedAgentUrl)
    await expect(popup.getByRole('heading', { name: 'Agent interface' })).toBeVisible()
    await popup.close()
  }
  await open(page, 'Alpha', width === 390)
  // Embedding deliberately keeps its Proxy tunnel. It must not change the
  // standalone destination, even if that tunnel reports the Phase 1 refusal.
  await expect(page.locator('iframe[title="/Alpha interface"]')).toHaveCount(1)
  await page.evaluate(() => {
    const host = window as unknown as { originOpens: unknown[][] }
    host.originOpens = []
    const original = window.open
    window.open = function (...args) { host.originOpens.push(args); return original.apply(window, args) }
  })
  await openTabMenu(page)
  const [popup] = await Promise.all([context.waitForEvent('page'), page.getByRole('menuitem', { name: 'Open in new browser tab' }).click()])
  await expect.poll(() => popup.url()).toBe(isolatedAgentUrl)
  expect(await page.evaluate(() => (window as unknown as { originOpens: unknown[][] }).originOpens)).toEqual([[isolatedAgentUrl, '_blank', 'noopener,noreferrer']])
  expect(await popup.evaluate(() => window.opener)).toBeNull()
  await popup.close()
})

for (const [label, value] of [['absent', () => ''], ['script', () => 'javascript:alert(1)'], ['credentials', () => 'https://user:secret@ag-0123456789abcdef0123456789abcdef.example.test/'], ['query', () => isolatedAgentUrl+'?token=secret'], ['path', () => isolatedAgentUrl+'api/'], ['another Agent', () => isolatedAgentUrl.replace('0123456789abcdef0123456789abcdef', 'ffffffffffffffffffffffffffffffff')]] as const) test(`isolated capability never downgrades missing or invalid origin: ${label}`, async ({ page }) => {
  await originFixture(page, value())
  await gotoWorkspace(page)
  await expect(page.getByRole('link', { name: 'Open /Alpha', exact: true })).toHaveAttribute('href', /\/workloads\/ag_0123456789abcdef0123456789abcdef$/)
  await open(page, 'Alpha')
  await switchTab(page, 'Overview')
  await expect(page.getByRole('link', { name: 'Open /Alpha', exact: true })).toHaveAttribute('href', /\/workloads\/ag_0123456789abcdef0123456789abcdef$/)
})

test('authorized origins survive inventory updates; changed interfaces reconcile without accepting event URLs', async ({ page }) => {
  await page.clock.install()
  const live = await originFixture(page)
  let current = live.snapshot(), reads = 0
  await page.route('**/snapshot', route => { reads++; return route.fulfill({ json: current }) })
  await gotoWorkspace(page)
  const link = page.getByRole('link', { name: 'Open /Alpha', exact: true })
  await expect(link).toHaveAttribute('href', isolatedAgentUrl)
  const alpha = current.agents.find(agent => agent.agent_id === alphaId)!
  const { origin_url: _origin, ...descriptor } = alpha.interface!
  live.event('agent.updated', { ...alpha, status: 'idle', interface: { ...descriptor, origin_url: 'https://event-injected.example.test/' } })
  await expect(link).toHaveAttribute('href', isolatedAgentUrl)
  const inventory = { ...current, agents: current.agents.map(agent => agent.agent_id === alphaId ? { ...agent, interface: descriptor } : agent) }
  live.event('workspace.snapshot', inventory)
  await expect(link).toHaveAttribute('href', isolatedAgentUrl)
  // Hold the read response to check that an interface replacement immediately
  // drops the old grant, rather than trusting a URL delivered by inventory.
  let release!: () => void
  const pending = new Promise<void>(resolve => { release = resolve })
  await page.route('**/snapshot', async route => { reads++; await pending; await route.fulfill({ json: current }) })
  const changed = { ...descriptor, instance_id: 'replacement', origin_url: 'https://event-injected.example.test/' }
  const before = reads
  live.event('agent.updated', { ...alpha, interface: changed })
  await expect(link).toHaveAttribute('href', /\/workloads\/ag_0123456789abcdef0123456789abcdef$/)
  await expect.poll(() => reads).toBeGreaterThan(before)
  current = { ...current, agents: current.agents.map(agent => agent.agent_id === alphaId ? { ...agent, interface: { ...changed, origin_url: isolatedAgentUrl } } : agent) }
  release()
  await expect(link).toHaveAttribute('href', isolatedAgentUrl)
  // An authoritative read omitting the URL revokes the navigation grant.
  current = { ...current, agents: current.agents.map(agent => agent.agent_id === alphaId ? { ...agent, interface: { ...descriptor, instance_id: 'replacement', origin_url: undefined } } : agent) }
  await page.evaluate(() => window.dispatchEvent(new Event('focus')))
  await page.clock.fastForward(5100)
  await expect(link).toHaveAttribute('href', /\/workloads\/ag_0123456789abcdef0123456789abcdef$/)
})

test('unknown capability does not select a Proxy UI destination', async ({ page }) => {
  await fixture(page)
  await page.route('**/api/auth/config', route => route.fulfill({ status: 503, json: { error: { code: 'unavailable' } } }))
  await gotoWorkspace(page)
  await expect(page.getByRole('link', { name: 'Open /Alpha', exact: true })).toHaveAttribute('href', /\/workloads\/tab-1$/)
})

for (const status of [401, 403, 404, 409, 503]) test(`isolated origin ${status} never downgrades new-tab navigation`, async ({ page, context }) => {
  await originFixture(page)
  originStatus = status
  await gotoWorkspace(page)
  await open(page, 'Alpha')
  await openTabMenu(page)
  const [popup] = await Promise.all([context.waitForEvent('page'), page.getByRole('menuitem', { name: 'Open in new browser tab' }).click()])
  await expect.poll(() => popup.url()).toBe(isolatedAgentUrl)
  await expect(popup.getByRole('heading', { name: 'Agent interface' })).toBeVisible()
  await popup.getByRole('link', { name: 'Retry' }).click()
  await expect(popup).toHaveURL(isolatedAgentUrl)
  await popup.close()
})


test('authorized origin cache is scoped to its workspace', async ({ page }) => {
  const live = await originFixture(page)
  const original = live.snapshot()
  const second = { ...original.workspace, workspace_id: 'second', name: 'Second' }
  await page.route('**/api/workspaces?*', route => route.fulfill({ json: { workspaces: [original.workspace, second] } }))
  await page.route('**/api/workspaces/second/snapshot', route => route.fulfill({ json: {
    ...original, workspace: second, agents: original.agents.map(agent => agent.interface ? { ...agent, interface: { ...agent.interface, origin_url: undefined } } : agent),
  } }))
  await gotoWorkspace(page)
  await expect(page.getByRole('link', { name: 'Open /Alpha', exact: true })).toHaveAttribute('href', isolatedAgentUrl)
  await page.getByRole('combobox', { name: 'Workspace', exact: true }).click()
  await page.getByRole('option', { name: 'Second', exact: true }).click()
  await expect(page.getByRole('link', { name: 'Open /Alpha', exact: true })).toHaveAttribute('href', /workspaces\/second\/workloads\/ag_0123456789abcdef0123456789abcdef$/)
})

test('an interface registered after initial load gets its authorized origin without a reload', async ({ page }) => {
  const live = await originFixture(page)
  let current = live.snapshot(), reads = 0
  let release!: () => void
  const pending = new Promise<void>(resolve => { release = resolve })
  await page.route('**/snapshot', async route => {
    if (++reads > 1) await pending
    await route.fulfill({ json: current })
  })
  await gotoWorkspace(page)
  const link = page.getByRole('link', { name: 'Open /Terminal', exact: true })
  await expect(link).toHaveAttribute('href', /workloads\/ag_00000000000000000000000000000000$/)
  const terminal = current.agents.find(agent => agent.agent_id === terminalId)!
  const terminalUrl = isolatedAgentUrl.replace('0123456789abcdef0123456789abcdef', '00000000000000000000000000000000')
  const registered = { ...current.agents.find(agent => agent.agent_id === alphaId)!.interface!, instance_id: 'new-ui', origin_url: terminalUrl }
  current = { ...current, agents: current.agents.map(agent => agent.agent_id === terminalId ? { ...agent, interface: registered } : agent) }
  live.event('agent.updated', { ...terminal, interface: { ...registered, origin_url: 'https://ag-ffffffffffffffffffffffffffffffff.agents.example.test/' } })
  await expect(link).toHaveAttribute('href', /workloads\/ag_00000000000000000000000000000000$/)
  await expect.poll(() => reads).toBeGreaterThan(1)
  release()
  await expect(link).toHaveAttribute('href', terminalUrl)
})


test('origin reconciliation backs off sustained registration churn and resets after quiet', async ({ page }) => {
  await page.clock.install({ time: new Date('2026-10-17T12:00:00Z') })
  await page.clock.pauseAt(new Date('2026-10-17T12:00:00Z'))
  const live = await originFixture(page)
  let current = live.snapshot(), reads = 0, revision = 0
  await page.route('**/snapshot', route => { reads++; return route.fulfill({ json: current }) })
  await gotoWorkspace(page)
  const link = page.getByRole('link', { name: 'Open /Alpha', exact: true })
  await expect(link).toHaveAttribute('href', isolatedAgentUrl)
  const baseline = reads
  const update = async () => {
    const alpha = current.agents.find(agent => agent.agent_id === alphaId)!
    const changed = { ...alpha, interface: { ...alpha.interface!, registered_at: String(++revision) } }
    current = { ...current, agents: current.agents.map(agent => agent.agent_id === alphaId ? changed : agent) }
    live.event('agent.updated', changed)
    // Inventory changes still drop stale grants immediately, including while
    // the next authoritative read is rate-limited.
    await expect(link).toHaveAttribute('href', /workloads\/ag_0123456789abcdef0123456789abcdef$/)
  }
  await update()
  await page.clock.runFor(200)
  await expect.poll(() => reads).toBe(baseline + 1)
  await expect(link).toHaveAttribute('href', isolatedAgentUrl)
  for (let i = 0; i < 5; i++) { await update(); await page.clock.runFor(1000) }
  await expect.poll(() => reads).toBe(baseline + 2)
  await expect(link).toHaveAttribute('href', isolatedAgentUrl)
  for (let i = 0; i < 9; i++) { await update(); await page.clock.runFor(1000) }
  expect(reads).toBe(baseline + 2)
  await update()
  await page.clock.runFor(1000)
  await expect.poll(() => reads).toBe(baseline + 3)
  await expect(link).toHaveAttribute('href', isolatedAgentUrl)
  await page.clock.runFor(30000)
  await update()
  await page.clock.runFor(200)
  await expect.poll(() => reads).toBe(baseline + 4)
  await expect(link).toHaveAttribute('href', isolatedAgentUrl)
  await update()
  await page.clock.runFor(4999)
  expect(reads).toBe(baseline + 4)
  await page.clock.runFor(1)
  await expect.poll(() => reads).toBe(baseline + 5)
})
