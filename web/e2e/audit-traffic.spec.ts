import { test, expect, type Page, type Route } from '@playwright/test'
import { browseTreeOnMobile } from './fixtures/navigation'
import { mockTreeMapApi } from './fixtures/treemap'
import { trafficBuckets } from '../src/lib/traffic'
import type { MachineTrafficRecord } from '../src/lib/api'

const now = Date.parse('2026-10-15T12:30:00Z')
const hour = 3_600_000
const usage = (hoursAgo: number, bytes: number, overrides = {}): MachineTrafficRecord => ({ window_start: new Date(now - hoursAgo * hour - 30 * 60_000).toISOString(), source_server_id: 'local', destination_server_id: 'remote', payload_bytes: bytes * 2, billable_bytes: bytes, payload_frames: bytes / 10, ...overrides })
const records = [usage(0, 100), usage(2, 200), usage(25, 300), usage(150, 400), usage(500, 500), usage(0, 50, { traffic_class: 'direct_network', payload_frames: 999 })]
const ok = (route: Route, body: unknown, status = 200) => route.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) })
async function setup(page: Page) {
  await page.clock.setFixedTime(now)
  await mockTreeMapApi(page)
  await page.route('**/groups', (route) => ok(route, { groups: [] }))
  await page.route('**/audit-events?**', (route) => ok(route, { events: [] }))
  const requested: string[] = []
  await page.route('**/api/workspaces/*/traffic**', (route) => {
    const url = new URL(route.request().url()), hours = Number(url.searchParams.get('hours'))
    requested.push(`${url.pathname}?hours=${hours}`)
    const rows = records.filter((record) => Date.parse(record.window_start) >= Math.floor(now / hour) * hour - (hours - 1) * hour)
    return ok(route, { traffic: url.pathname.endsWith('/agents') ? [{ ...rows[0], source_type: 'agent', source_id: 'reviewer-a', destination_type: 'machine', destination_id: 'remote' }] : rows })
  })
  return requested
}
async function openAudit(page: Page) {
  await page.goto('/')
  await browseTreeOnMobile(page)
  await page.getByRole('button', { name: 'Organization settings', exact: true }).click()
  await page.getByRole('button', { name: 'Open audit', exact: true }).click()
  await expect(page.getByRole('heading', { name: 'Audit', exact: true })).toBeVisible()
}
async function range(page: Page, label: string) {
  await page.getByRole('combobox', { name: 'Traffic time range' }).click()
  await page.getByRole('option', { name: label, exact: true }).click()
}
const panel = (page: Page) => page.getByRole('region', { name: 'Workspace traffic' })

test('hour/day aggregation keeps billable units, excludes direct frames and includes current hour', () => {
  const hourly = trafficBuckets(records, 24, now)
  expect(hourly).toHaveLength(24)
  expect(hourly.at(-1)).toEqual({ start: Date.parse('2026-10-15T12:00Z'), bytes: 150, frames: 10 })
  expect(hourly.reduce((sum, item) => sum + item.bytes, 0)).toBe(350)
  const weekly = trafficBuckets([...records, usage(170, 900), { ...usage(0, 800), window_start: 'invalid' }], 168, now)
  expect(weekly.reduce((sum, item) => sum + item.bytes, 0)).toBe(1050)
  expect(weekly).toHaveLength(8)
  expect(weekly.at(-1)?.bytes).toBe(350)
  expect(trafficBuckets([], 720, now)).toHaveLength(31)
  expect(trafficBuckets([usage(0, 20, { billable_bytes: undefined, payload_bytes: 40 })], 6, now).at(-1)?.bytes).toBe(40)
})

for (const mobile of [false, true]) for (const theme of ['light', 'dark']) {
  test(`traffic ranges and accessible trend ${theme} ${mobile ? '390' : 'desktop'}`, async ({ page }, testInfo) => {
    await page.setViewportSize({ width: mobile ? 390 : 1360, height: 960 })
    await page.addInitScript((theme) => localStorage.setItem('treer-theme', theme), theme)
    const requested = await setup(page)
    await openAudit(page)
    await expect(panel(page)).toHaveAttribute('aria-busy', 'false')
    await expect(panel(page).getByText('350 B', { exact: true }).first()).toBeVisible()
    for (const [label, hours, total] of [['Last 6 hours', 6, '350 B'], ['Last 7 days', 168, '1.03 KB'], ['Last 30 days', 720, '1.51 KB']] as const) {
      await range(page, label)
      await expect(panel(page)).toHaveAttribute('aria-busy', 'false')
      await expect.poll(() => requested.includes(`/api/workspaces/map-workspace/traffic?hours=${hours}`) && requested.includes(`/api/workspaces/map-workspace/traffic/agents?hours=${hours}`)).toBe(true)
      await expect(panel(page).getByText(total, { exact: true }).first()).toBeVisible()
      await expect(page.getByRole('heading', { name: `Agent traffic · ${label.toLowerCase()}` })).toBeVisible()
    }
    await expect(page.getByText('/Research/reviewer', { exact: true }).last()).toBeVisible()
    const chart = page.getByRole('figure', { name: 'Billable usage trend' })
    const bars = chart.locator('rect[tabindex="0"]')
    await expect(bars).toHaveCount(31)
    await bars.last().focus()
    await expect(chart.getByText(/UTC · 350 B/)).toBeVisible()
    await page.keyboard.press('Tab')
    await page.getByText('View traffic data table', { exact: true }).click()
    await expect(page.getByRole('table', { name: 'Traffic by day, UTC' })).toBeVisible()
    await page.getByText('View traffic data table', { exact: true }).click()
    await page.getByRole('combobox', { name: 'Traffic metric' }).click()
    await page.getByRole('option', { name: 'Data frames', exact: true }).click()
    await expect(page.getByRole('figure', { name: 'Data frames trend' })).toBeVisible()
    await expect(page.getByRole('figure', { name: 'Data frames trend' }).locator('rect[tabindex="0"]').last()).toHaveAttribute('aria-label', /: 30$/)
    await expect.poll(() => page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
    await page.getByRole('combobox', { name: 'Traffic metric' }).click()
    await page.getByRole('option', { name: 'Billable usage', exact: true }).click()
    await page.screenshot({ path: testInfo.outputPath(`audit-${theme}-${mobile ? '390' : 'desktop'}.png`) })
    const before = requested.length
    await page.getByRole('button', { name: 'Refresh audit', exact: true }).click()
    await expect.poll(() => requested.length).toBeGreaterThan(before)
    expect(requested.slice(before).every((path) => path.endsWith('hours=720'))).toBe(true)
  })
}

test('empty range is explicit and unavailable agent detail does not hide machine usage', async ({ page }) => {
  await setup(page)
  await page.route('**/api/workspaces/*/traffic**', (route) => ok(route, { traffic: [] }, route.request().url().includes('/agents?') ? 404 : 200))
  await openAudit(page)
  await expect(page.getByText('No recorded traffic in this range.')).toBeVisible()
  await expect(page.getByText('Agent traffic detail is unavailable on this Proxy.')).toBeVisible()
  await expect(page.getByRole('figure', { name: 'Billable usage trend' })).toBeVisible()
  await expect(panel(page).getByText('0 B', { exact: true }).first()).toBeVisible()
  await page.route('**/api/workspaces/*/traffic?hours=168', (route) => ok(route, { traffic: [usage(25, 300)] }))
  await range(page, 'Last 7 days')
  await expect(panel(page).getByText('300 B', { exact: true }).first()).toBeVisible()
  await expect(page.getByText('Agent traffic detail is unavailable on this Proxy.')).toBeVisible()
})

test('API failure clears stale totals, shows a safe error and retries the selected range', async ({ page }) => {
  await setup(page)
  await openAudit(page)
  await expect(panel(page)).toHaveAttribute('aria-busy', 'false')
  let fail = true
  await page.route('**/traffic?hours=168', (route) => ok(route, fail ? { error: { message: 'synthetic-backend-detail' } } : { traffic: [usage(25, 300)] }, fail ? 403 : 200))
  await range(page, 'Last 7 days')
  await expect(page.getByRole('alert')).toContainText('Could not load traffic (HTTP 403)')
  await expect(page.getByText('synthetic-backend-detail')).toHaveCount(0)
  await expect(page.getByRole('figure', { name: 'Billable usage trend' })).toHaveCount(0)
  fail = false
  await page.getByRole('button', { name: 'Retry traffic' }).click()
  await expect(panel(page).getByText('300 B', { exact: true }).first()).toBeVisible()
  await expect(page.getByRole('alert')).toHaveCount(0)
})

test('late old-range response cannot overwrite a newer range', async ({ page }) => {
  await setup(page)
  let release!: () => void
  let entered = false
  let finished = false
  const gate = new Promise<void>((resolve) => { release = resolve })
  await page.route('**/traffic?hours=24', async (route) => { entered = true; await gate; await ok(route, { traffic: [usage(0, 999999)] }).catch(() => {}); finished = true })
  await openAudit(page)
  await expect.poll(() => entered).toBe(true)
  await range(page, 'Last 7 days')
  await expect(panel(page).getByText('1.03 KB', { exact: true }).first()).toBeVisible()
  release()
  await expect.poll(() => finished).toBe(true)
  await expect(panel(page).getByText('1.03 KB', { exact: true }).first()).toBeVisible()
  await expect(panel(page).getByText('976.6 KB', { exact: true })).toHaveCount(0)
})


test('periodic refresh keeps selected hours and stops when the audit view closes', async ({ page }) => {
  await page.clock.install({ time: now })
  const requested = await setup(page)
  await openAudit(page)
  await range(page, 'Last 7 days')
  await expect(panel(page)).toHaveAttribute('aria-busy', 'false')
  const before = requested.length
  await page.clock.fastForward(10_000)
  await expect.poll(() => requested.length).toBe(before + 2)
  expect(requested.slice(before).every((path) => path.endsWith('hours=168'))).toBe(true)
  await expect(panel(page)).toHaveAttribute('aria-busy', 'false')
  await page.getByRole('button', { name: 'Organization settings', exact: true }).click()
  await expect(panel(page)).toHaveCount(0)
  const after = requested.length
  await page.clock.fastForward(30_000)
  expect(requested.length).toBe(after)
  await page.getByRole('button', { name: 'Open audit', exact: true }).click()
  await expect(page.getByRole('combobox', { name: 'Traffic time range' })).toHaveText('Last 24 hours')
})
