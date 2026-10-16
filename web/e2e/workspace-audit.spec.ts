import { test, expect, type Page, type Route } from '@playwright/test'
import { mockTreeMapApi, mapSnapshot } from './fixtures/treemap'
import { browseTreeOnMobile } from './fixtures/navigation'

const workspaceId = 'map-workspace'
const path = `**/api/workspaces/${workspaceId}/audit-events?*`
const ok = (route: Route, json: unknown, status = 200) => route.fulfill({ status, json })
function event(sequence = 80, overrides: Record<string, unknown> = {}) {
  return { sequence, event_id: `evt-${sequence}`, workspace_id: workspaceId, organization_id: 'map-org', occurred_at: '2026-10-16T02:00:00Z', actor_kind: 'agent', actor_id: 'reviewer-a', source: 'api', action: 'agent.created', outcome: 'succeeded', resource_kind: 'agent', resource_id: 'reviewer-b', payload: {}, ...overrides }
}
async function setup(page: Page, capability = true) {
  await mockTreeMapApi(page)
  const build = { version: "test", git_commit: "synthetic" }
  await page.route("**/snapshot", (route) => ok(route, { ...mapSnapshot, servers: mapSnapshot.servers.map((machine) => ({ ...machine, controller_build: build, host_build: build })) }))
  await page.route('**/api/auth/config', (route) => ok(route, { capabilities: capability ? ['workspace.audit.v1'] : [] }))
  await page.route('**/groups', (route) => ok(route, { groups: [] }))
  await page.route('**/policy-provider', (route) => ok(route, {}, 404))
}
async function openAudit(page: Page, mobile = false) {
  await page.goto('/')
  await expect(page.getByRole('heading', { name: 'Workspace overview', exact: true })).toBeVisible()
  if (mobile) await browseTreeOnMobile(page)
  await page.getByRole('button', { name: 'Workspace settings', exact: true }).click()
  await page.getByRole('tab', { name: 'Audit', exact: true }).click()
  return page.getByRole('region', { name: 'Workspace audit', exact: true })
}

for (const theme of ['light', 'dark']) for (const width of [1360, 390]) test(`workspace audit filters, paging and paths ${theme} ${width}`, async ({ page }, info) => {
  await page.setViewportSize({ width, height: 960 })
  await page.addInitScript((theme) => localStorage.setItem('treer-theme', theme), theme)
  await setup(page)
  const queries: URLSearchParams[] = [], orgRequests: string[] = []
  page.on('request', (r) => { if (/organizations.*audit-events/.test(r.url())) orgRequests.push(r.url()) })
  await page.route(path, (route) => {
    expect(route.request().headers()['authorization']).toBeUndefined()
    const query = new URL(route.request().url()).searchParams
    queries.push(query)
    return ok(route, query.has('before') ? { events: [event(70, { resource_kind: 'app', resource_id: 'older-app' })], next_cursor: null } : { events: [event()], next_cursor: 80 })
  })
  const audit = await openAudit(page, width === 390)
  await expect(audit.getByText('by /Research/reviewer', { exact: true })).toBeVisible()
  await expect(audit.getByText('/Product/reviewer', { exact: true })).toBeVisible()
  await expect(audit.getByText(/Coverage is incomplete/)).toBeVisible()
  await audit.getByRole('button', { name: 'Older', exact: true }).click()
  await expect(audit.getByText('older-app', { exact: true })).toBeVisible()
  await expect(audit.getByRole('status')).toHaveText('Page 2')
  await expect(audit.getByRole('button', { name: 'Older', exact: true })).toBeDisabled()
  expect(queries.at(-1)?.get('before')).toBe('80')
  await audit.getByRole('button', { name: 'Newer', exact: true }).click()
  await expect(audit.getByText('/Product/reviewer', { exact: true })).toBeVisible()
  if (width === 390) {
    const filters = audit.getByRole('button', { name: 'Audit filters', exact: true })
    await expect(filters).toHaveAttribute('aria-expanded', 'false')
    await expect(audit.getByRole('form', { name: 'Audit filters', exact: true })).toBeHidden()
    await filters.click()
    await expect(filters).toHaveAttribute('aria-expanded', 'true')
  }
  await audit.getByLabel('Actor ID', { exact: true }).fill('reviewer-a')
  await audit.getByLabel('Action', { exact: true }).fill('agent.created')
  await audit.getByLabel('Resource kind', { exact: true }).fill('agent')
  await audit.getByLabel('From (inclusive)', { exact: true }).fill('2026-10-15T00:00')
  await audit.getByLabel('Until (exclusive)', { exact: true }).fill('2026-10-16T00:00')
  await audit.getByRole('button', { name: 'Apply filters', exact: true }).click()
  await expect.poll(() => queries.at(-1)?.get('action')).toBe('agent.created')
  const query = queries.at(-1)!
  expect(query.get('actor_id')).toBe('reviewer-a')
  expect(query.get('resource_kind')).toBe('agent')
  expect(query.has('before')).toBe(false)
  expect(query.get('limit')).toBe('50')
  expect(Date.parse(query.get('until')!) - Date.parse(query.get('since')!)).toBe(86_400_000)
  await expect(audit.getByText('/Product/reviewer', { exact: true })).toBeVisible()
  if (width === 390) await audit.getByRole('button', { name: 'Audit filters', exact: true }).click()
  if (width === 390) await page.screenshot({ path: info.outputPath(`workspace-audit-events-${theme}-${width}.png`), fullPage: true })
  await audit.evaluate((element) => { for (let parent: HTMLElement | null = element; parent; parent = parent.parentElement) parent.scrollTop = 0 })
  await page.screenshot({ path: info.outputPath(`workspace-audit-${theme}-${width}.png`), fullPage: true })
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
  if (width === 390) await audit.getByRole('button', { name: 'Audit filters', exact: true }).click()
  await audit.getByRole('button', { name: 'Clear filters', exact: true }).click()
  await expect.poll(() => queries.at(-1)?.has('action')).toBe(false)
  expect(orgRequests).toEqual([])
  // Radix tabs are keyboard reachable; leaving unmounts the sensitive feed.
  await page.getByRole('tab', { name: 'Audit', exact: true }).focus()
  await page.keyboard.press('ArrowLeft')
  await expect(page.getByRole('region', { name: 'Workspace audit', exact: true })).toHaveCount(0)
})

for (const status of [403, 404, 405, 500]) test(`workspace audit HTTP ${status} is safe, explicit and retryable`, async ({ page }) => {
  await setup(page)
  await page.route('**/access', (route) => ok(route, { access: { current_role: 'member', members: [], groups: [] } }))
  let fail = true
  await page.route(path, (route) => fail ? ok(route, { error: { message: 'private-body@example.test Bearer synthetic-never-display' } }, status) : ok(route, { events: [], next_cursor: null }))
  const audit = await openAudit(page)
  await expect(audit.getByRole('alert')).toContainText(status === 403 ? 'do not have permission' : status === 404 || status === 405 ? 'not supported' : 'Unable to load')
  await expect(page.getByText(/private-body|synthetic-never-display/)).toHaveCount(0)
  fail = false
  await audit.getByRole('button', { name: 'Refresh audit', exact: true }).click()
  await expect(audit.getByText('No audit activity yet', { exact: true })).toBeVisible()
})

test('old Proxy without capability never receives workspace audit or org fallback requests', async ({ page }) => {
  await setup(page, false)
  const requests: string[] = []
  page.on('request', (r) => { if (r.url().includes('audit-events')) requests.push(r.url()) })
  const audit = await openAudit(page)
  await expect(audit.getByRole('alert')).toContainText('not supported')
  expect(requests).toEqual([])
})

test('workspace audit rejects cross-workspace rows and hides arbitrary fields', async ({ page }) => {
  await setup(page)
  let crossWorkspace = false
  await page.route(path, (route) => ok(route, { events: [event(80, { workspace_id: crossWorkspace ? 'other-workspace' : workspaceId, actor_name: 'private-name', resource_name: '<img src=x onerror=alert(1)>', payload: { body: 'private-body', argv: 'private-argv' } })], next_cursor: null }))
  const audit = await openAudit(page)
  await expect(audit.getByText('by /Research/reviewer', { exact: true })).toBeVisible()
  await expect(page.getByText(/private-name|private-body|private-argv|onerror/)).toHaveCount(0)
  crossWorkspace = true
  await audit.getByRole('button', { name: 'Refresh audit', exact: true }).click()
  await expect(audit.getByRole('alert')).toContainText('Unable to load')
  await expect(audit.getByText('by /Research/reviewer', { exact: true })).toHaveCount(0)
})

test('filter validation avoids requests for invalid IDs and oversized time ranges', async ({ page }) => {
  await setup(page)
  let requests = 0
  await page.route(path, (route) => { requests++; return ok(route, { events: [], next_cursor: null }) })
  const audit = await openAudit(page)
  await expect(audit.getByText('No audit activity yet', { exact: true })).toBeVisible()
  const baseline = requests
  await audit.getByLabel('Actor ID', { exact: true }).fill('/a path')
  await audit.getByRole('button', { name: 'Apply filters', exact: true }).click()
  await expect(audit.getByRole('alert')).toContainText('1–128')
  expect(requests).toBe(baseline)
  await audit.getByLabel('Actor ID', { exact: true }).fill('')
  await audit.getByLabel('From (inclusive)', { exact: true }).fill('2024-01-01T00:00')
  await audit.getByLabel('Until (exclusive)', { exact: true }).fill('2026-01-01T00:00')
  await audit.getByRole('button', { name: 'Apply filters', exact: true }).click()
  await expect(audit.getByRole('alert')).toContainText('366 days')
  expect(requests).toBe(baseline)
})

test('obsolete delayed filter responses cannot overwrite current audit data', async ({ page }) => {
  await setup(page)
  let release: (() => void) | undefined
  await page.route(path, async (route) => {
    const delayed = new URL(route.request().url()).searchParams.get('action') === 'old.action'
    if (delayed) await new Promise<void>((resolve) => { release = resolve })
    try { await ok(route, { events: [event(80, { resource_kind: 'app', resource_id: delayed ? 'old-result' : 'new-result' })], next_cursor: null }) } catch { /* Obsolete request is aborted. */ }
  })
  const audit = await openAudit(page)
  await expect(audit.getByText('new-result', { exact: true })).toBeVisible()
  await audit.getByLabel('Action', { exact: true }).fill('old.action')
  await audit.getByRole('button', { name: 'Apply filters', exact: true }).click()
  await expect.poll(() => typeof release).toBe('function')
  await audit.getByRole('button', { name: 'Clear filters', exact: true }).click()
  await expect(audit.getByText('new-result', { exact: true })).toBeVisible()
  release!()
  await expect(audit.getByText('old-result', { exact: true })).toHaveCount(0)
})

test('workspace audit has Chinese labels and refuses safely', async ({ page }) => {
  await setup(page)
  await page.addInitScript(() => localStorage.setItem('treer-lang', 'zh'))
  await page.route(path, (route) => ok(route, {}, 403))
  await page.goto('/')
  // Language preference uses the same persisted setting as the rest of Console.
  const settings = page.getByRole('button', { name: '工作区设置', exact: true })
  await expect(settings).toBeVisible()
  await settings.click()
  await page.getByRole('tab', { name: '审计', exact: true }).click()
  await expect(page.getByRole('region', { name: '工作区审计', exact: true }).getByRole('alert')).toContainText('没有读取')
})

test('workspace audit uses no-store and text rendering, including failed outcomes', async ({ page }) => {
  await setup(page)
  await page.addInitScript(() => {
    const original = window.fetch
    ;(window as unknown as { auditFetchOptions: unknown[] }).auditFetchOptions = []
    window.fetch = (input, init) => {
      if (String(input).includes('/audit-events')) (window as unknown as { auditFetchOptions: unknown[] }).auditFetchOptions.push({ cache: init?.cache, credentials: init?.credentials })
      return original(input, init)
    }
  })
  const literal = '<img src=x onerror=window.syntheticXss=true>'
  await page.route(path, (route) => ok(route, { events: [event(80, { resource_kind: 'app', resource_id: literal, outcome: 'failed' })], next_cursor: null }))
  const audit = await openAudit(page)
  await expect(audit.getByText(literal, { exact: true })).toBeVisible()
  await expect(audit.getByText('Failed', { exact: true })).toBeVisible()
  await expect(audit.locator('img')).toHaveCount(0)
  expect(await page.evaluate(() => (window as unknown as { auditFetchOptions: unknown[] }).auditFetchOptions)).toEqual([{ cache: 'no-store', credentials: 'include' }])
})

test('switching workspace drops the old feed and ignores a delayed old response', async ({ page }) => {
  await setup(page)
  const second = { workspace_id: 'second-workspace', organization_id: 'map-org', name: 'Second studio' }
  await page.route('**/api/workspaces?*', (route) => ok(route, { workspaces: [mapSnapshot.workspace, second] }))
  await page.route('**/api/workspaces/second-workspace/snapshot', (route) => ok(route, { revision: 1, workspace: second, servers: [], agents: [] }))
  let release: (() => void) | undefined
  await page.route(path, async (route) => {
    await new Promise<void>((resolve) => { release = resolve })
    try { await ok(route, { events: [event(80, { resource_kind: 'app', resource_id: 'first-private-result' })], next_cursor: null }) } catch { /* Leaving workspace aborts the pending request. */ }
  })
  await page.route('**/api/workspaces/second-workspace/audit-events?*', (route) => ok(route, { events: [event(79, { workspace_id: second.workspace_id, resource_kind: 'app', resource_id: 'second-result' })], next_cursor: null }))
  await openAudit(page)
  await expect.poll(() => typeof release).toBe('function')
  await page.getByRole('combobox', { name: 'Workspace', exact: true }).click()
  await page.getByRole('option', { name: second.name, exact: true }).click()
  await page.getByRole('button', { name: 'Workspace settings', exact: true }).click()
  await page.getByRole('tab', { name: 'Audit', exact: true }).click()
  const audit = page.getByRole('region', { name: 'Workspace audit', exact: true })
  await expect(audit.getByText('second-result', { exact: true })).toBeVisible()
  release!()
  await expect(audit.getByText('first-private-result', { exact: true })).toHaveCount(0)
  await expect(audit.getByRole('status')).toHaveText('Page 1')
})


test('audit actor names resolve only from the already-visible directory, with stable ID fallback', async ({ page }) => {
  await setup(page)
  await page.route('**/members', (route) => ok(route, { members: [{ user_id: 'known-user', preferred_name: 'Visible colleague', email: 'synthetic@example.test', role: 'member' }] }))
  await page.route(path, (route) => ok(route, { events: [event(80, { actor_kind: 'user', actor_id: 'known-user', actor_name: 'do-not-use-server-name' }), event(79, { actor_kind: 'user', actor_id: 'unknown-user', outcome: 'failed' })], next_cursor: null }))
  const audit = await openAudit(page)
  await expect(audit.getByText('by Visible colleague', { exact: true })).toBeVisible()
  await expect(audit.getByText('by unknown-user', { exact: true })).toBeVisible()
  await expect(audit.getByText('do-not-use-server-name')).toHaveCount(0)
  await expect(audit.locator('svg.lucide-bot')).toHaveCount(2)
  await expect(audit.getByText('Failed', { exact: true })).toHaveClass(/text-destructive/)
  await expect(audit.getByText('Succeeded', { exact: true })).toHaveClass(/text-status-working/)
  expect(await audit.getByText('Failed', { exact: true }).evaluate((element) => parseFloat(getComputedStyle(element).fontSize))).toBeGreaterThanOrEqual(12)
})
