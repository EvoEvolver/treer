import { test, expect, type Page, type Route } from '@playwright/test'

const ok = (route: Route, body: unknown, status = 200) => route.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) })
function sample(hours: number) {
  const hour = Math.floor(Date.now() / 3_600_000) * 3_600_000
  return { hours, traffic: [
    { window_start: new Date(hour).toISOString(), traffic_class: 'virtual_network', payload_bytes: 1024, payload_frames: 12, billable_bytes: 2048 },
    { window_start: new Date(hour - 3_600_000).toISOString(), traffic_class: 'service_ingress', payload_bytes: 2048, payload_frames: 8, billable_bytes: 4096 },
    { window_start: new Date(hour).toISOString(), traffic_class: 'direct_network', payload_bytes: 1024, payload_frames: 999, billable_bytes: 1024 },
  ] }
}
async function mockAdmin(page: Page) {
  await page.route('**/api/admin/**', async (route) => {
    const path = new URL(route.request().url()).pathname
    if (path === '/api/admin/me') return ok(route, { admin: true })
    if (path === '/api/admin/dashboard') return ok(route, { user_count: 40, organization_count: 3, machine_count: 12, agent_count: 28 })
    if (path === '/api/admin/logout') return ok(route, {})
    return ok(route, { error: { message: 'not supported' } }, 404)
  })
}
for (const theme of ['light', 'dark'] as const) for (const width of [1360, 390]) test(`platform traffic is aggregate, responsive and keyboard accessible ${theme} ${width}`, async ({ page }, info) => {
  await mockAdmin(page)
  const requests: string[] = []
  page.on('request', (request) => { if (request.url().includes('/api/')) requests.push(new URL(request.url()).pathname) })
  const ranges: number[] = []
  await page.route('**/api/admin/traffic?*', async (route) => {
    const hours = Number(new URL(route.request().url()).searchParams.get('hours'))
    ranges.push(hours)
    return ok(route, sample(hours))
  })
  await page.setViewportSize({ width, height: 960 })
  await page.goto('/admin')
  await page.evaluate((value) => document.documentElement.classList.toggle('dark', value === 'dark'), theme)
  const section = page.getByRole('region', { name: 'Platform traffic' })
  await expect(section.getByRole('group', { name: 'Traffic totals' }).getByText('7.00 KB', { exact: true })).toBeVisible()
  await expect(section.getByRole('group', { name: 'Traffic totals' }).getByText('20', { exact: true })).toBeVisible()
  await expect(section.getByRole('figure', { name: 'Billable usage trend' })).toBeVisible()
  for (const [label, hours, count] of [['Last 6 hours', 6, 6], ['Last 7 days', 168, undefined], ['Last 30 days', 720, undefined], ['Last 24 hours', 24, 24]] as const) {
    await section.getByRole('combobox', { name: 'Traffic time range' }).click()
    await page.getByRole('option', { name: label, exact: true }).click()
    await expect.poll(() => ranges.at(-1)).toBe(hours)
    await expect(section.getByRole('figure', { name: 'Billable usage trend' })).toBeVisible()
    if (count) await expect(section.locator('svg rect[tabindex="0"]')).toHaveCount(count)
    else await expect(section.getByText('Daily · UTC', { exact: true })).toBeVisible()
  }
  await section.locator('svg rect[tabindex="0"]').last().focus()
  await expect(section.getByRole('figure').locator('p[aria-live]')).toContainText('3.00 KB')
  await section.getByText('View traffic data table', { exact: true }).click()
  await expect(section.getByRole('table')).toBeVisible()
  await section.getByRole('combobox', { name: 'Traffic metric' }).click()
  await page.getByRole('option', { name: 'Data frames', exact: true }).click()
  await expect(section.getByRole('figure', { name: 'Data frames trend' })).toBeVisible()
  expect(requests.every((path) => ['/api/admin/me', '/api/admin/dashboard', '/api/admin/update', '/api/admin/traffic'].includes(path))).toBe(true)
  await page.screenshot({ path: info.outputPath(`admin-traffic-${theme}-${width}.png`), fullPage: true })
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
})

for (const status of [401, 403, 404, 405, 500]) test(`platform traffic HTTP ${status} stays safe and never falls back to private inventories`, async ({ page }) => {
  await mockAdmin(page)
  const paths: string[] = []
  page.on('request', (request) => { if (request.url().includes('/api/')) paths.push(new URL(request.url()).pathname) })
  await page.route('**/api/admin/traffic?*', (route) => ok(route, { error: { message: 'secret-person@example.com /private-machine <script>bad()</script>' } }, status))
  await page.goto('/admin')
  const section = page.getByRole('region', { name: 'Platform traffic' })
  await expect(section.getByRole('alert')).toContainText([404, 405].includes(status) ? 'not supported by this Proxy' : `HTTP ${status}`)
  await expect(page.getByText(/secret-person|private-machine/)).toHaveCount(0)
  await expect(section.getByRole('figure')).toHaveCount(0)
  expect(paths.some((path) => /workspaces|users|machines|agents|organizations|activity/.test(path))).toBe(false)
  await page.route('**/api/admin/traffic?*', (route) => ok(route, sample(24)))
  await section.getByRole('button', { name: 'Retry traffic' }).click()
  await expect(section.getByRole('figure')).toBeVisible()
})

test('malformed aggregate data is not rendered as zero or echoed', async ({ page }) => {
  await mockAdmin(page)
  await page.route('**/api/admin/traffic?*', (route) => ok(route, { hours: 24, traffic: [{ ...sample(24).traffic[0], traffic_class: 'private-org@example.com' }] }))
  await page.goto('/admin')
  await expect(page.getByRole('region', { name: 'Platform traffic' }).getByRole('alert')).toBeVisible()
  await expect(page.getByText('private-org@example.com')).toHaveCount(0)
  await expect(page.getByRole('figure')).toHaveCount(0)
})

test('late prior-range data cannot replace the selected aggregate range', async ({ page }) => {
  await mockAdmin(page)
  let release: (() => void) | undefined
  await page.route('**/api/admin/traffic?*', async (route) => {
    const hours = Number(new URL(route.request().url()).searchParams.get('hours'))
    if (hours === 168) await new Promise<void>((resolve) => { release = resolve })
    try { await ok(route, sample(hours)) } catch { /* The obsolete request is aborted on range change. */ }
  })
  await page.goto('/admin')
  const section = page.getByRole('region', { name: 'Platform traffic' })
  await expect(section.getByRole('figure')).toBeVisible()
  await section.getByRole('combobox', { name: 'Traffic time range' }).click()
  await page.getByRole('option', { name: 'Last 7 days', exact: true }).click()
  await expect.poll(() => typeof release).toBe('function')
  await section.getByRole('combobox', { name: 'Traffic time range' }).click()
  await page.getByRole('option', { name: 'Last 6 hours', exact: true }).click()
  release!()
  await expect(section.locator('svg rect[tabindex="0"]')).toHaveCount(6)
})

test('empty aggregate windows are explicitly unrecorded, and logout removes the chart', async ({ page }) => {
  await mockAdmin(page)
  await page.route('**/api/admin/traffic?*', (route) => ok(route, { hours: 24, traffic: [] }))
  await page.goto('/admin')
  const section = page.getByRole('region', { name: 'Platform traffic' })
  await expect(section.getByRole('figure')).toBeVisible()
  await expect(section.getByText(/does not prove there was no traffic/)).toBeVisible()
  await page.getByRole('button', { name: 'Log out', exact: true }).click()
  await expect(page.getByRole('heading', { name: 'Treer administration', exact: true })).toBeVisible()
  await expect(section).toHaveCount(0)
})

test('admin login gate prevents aggregate requests before a platform session exists', async ({ page }) => {
  await mockAdmin(page)
  const traffic: string[] = []
  page.on('request', (request) => { if (request.url().includes('/api/admin/traffic')) traffic.push(request.url()) })
  await page.route('**/api/admin/me', (route) => ok(route, { error: { message: 'unauthorized' } }, 401))
  await page.goto('/admin')
  await expect(page.getByLabel('Admin password', { exact: true })).toBeVisible()
  expect(traffic).toHaveLength(0)
})

test('aggregate refresh retains the range and stops after logout', async ({ page }) => {
  await page.clock.install()
  await mockAdmin(page)
  const ranges: number[] = []
  await page.route('**/api/admin/traffic?*', async (route) => {
    const hours = Number(new URL(route.request().url()).searchParams.get('hours'))
    ranges.push(hours)
    await ok(route, sample(hours))
  })
  await page.goto('/admin')
  const section = page.getByRole('region', { name: 'Platform traffic' })
  await expect(section.getByRole('figure')).toBeVisible()
  await section.getByRole('combobox', { name: 'Traffic time range' }).click()
  await page.getByRole('option', { name: 'Last 7 days', exact: true }).click()
  await expect(section.getByRole('figure').getByText('Daily · UTC')).toBeVisible()
  const before = ranges.length
  await page.clock.runFor(10_001)
  await expect.poll(() => ranges.length).toBeGreaterThan(before)
  expect(ranges.at(-1)).toBe(168)
  await page.getByRole('button', { name: 'Log out', exact: true }).click()
  await expect(section).toHaveCount(0)
  const after = ranges.length
  await page.clock.runFor(30_000)
  expect(ranges).toHaveLength(after)
})
