import { test, expect, type Page } from '@playwright/test'
import { readFileSync } from 'node:fs'
import { createHash } from 'node:crypto'
import { fixture, gotoWorkspace } from './fixtures/workspace-tabs'
import { metricsFixture } from './fixtures/machine-metrics'
import type { MetricsRequest, MachineMetrics } from '../src/lib/machine-metrics'
import type { GpuPresenceVectors } from './fixtures/machine-metrics-presence'
async function setup(page: Page, mode = 'normal', mutate?: (v: MachineMetrics) => void) {
  const live = await fixture(page)
  const machines = live.snapshot().servers.map(m => ({ ...m, capabilities: mode === 'unsupported' ? [] : ['machine.metrics.v1'], status: mode === 'offline' ? 'offline' : m.status }))
  await page.route('**/snapshot', r => r.fulfill({ json: { ...live.snapshot(), servers: machines } }))
  let queries = 0; const ranges: number[] = []
  await page.route('**/machines/*/metrics?*', async r => {
    queries++; const u = new URL(r.request().url()), request: MetricsRequest = { serverId: u.pathname.split('/').at(-2)!, from: u.searchParams.get('from')!, to: u.searchParams.get('to')!, maxPoints: Number(u.searchParams.get('max_points')) }
    ranges.push((Date.parse(request.to) - Date.parse(request.from)) / 60000)
    const headers = { 'cache-control': 'no-store' }
    if (['forbidden', 'timeout', 'busy', 'unavailable'].includes(mode)) return r.fulfill({ status: mode === 'forbidden' ? 403 : 503, headers, json: { error: { code: 'machine_metrics_' + mode, message: 'PRIVATE DIAGNOSTIC DO NOT DISPLAY' } } })
    const body = metricsFixture(request, mode); mutate?.(body); return r.fulfill({ json: body, headers })
  })
  await gotoWorkspace(page)
  await page.getByRole('button', { name: 'Workspace settings', exact: true }).click()
  await page.getByRole('button', { name: /^workstation(\s|$)/ }).click()
  return { queries: () => queries, ranges, live }
}
const panel = (page: Page) => page.getByTestId('machine-metrics')
test('real Machine view ranges, current/mean, gaps, keyboard/touch and untrusted multi-GPU labels', async ({ page }) => {
  const s = await setup(page)
  await expect(panel(page).locator('p').filter({ hasText: /^23\.4%$/ })).toBeVisible()
  for (const name of ['1h', '6h', '24h', '15m']) { await panel(page).getByRole('button', { name, exact: true }).click(); await expect(panel(page).getByRole('button', { name, exact: true })).toHaveAttribute('aria-pressed', 'true'); await expect(panel(page).locator('p').filter({ hasText: /^23\.4%$/ })).toBeVisible() }
  expect(s.ranges.filter((v, i) => !i || v !== s.ranges[i - 1])).toEqual([15, 60, 360, 1440, 15])
  const chart = panel(page).getByRole('figure', { name: 'CPU utilization', exact: true })
  await expect(chart.locator('[data-metric-line]')).toHaveCount(2)
  await expect(chart.locator('circle[data-partial=true]')).toHaveCount(1)
  await chart.getByRole('img').focus(); await page.keyboard.press('Home')
  await expect(chart.locator('[aria-live]')).toContainText('valid samples')
  await chart.getByRole('img').click({ position: { x: 180, y: 80 } })
  await chart.getByText('View sampled values', { exact: true }).click(); await expect(chart.getByRole('table')).toBeVisible(); await expect(chart.getByRole('table')).toContainText('10%')
  await expect(panel(page).getByRole('heading', { name: /<img src=x/ })).toBeVisible()
  await expect(panel(page).locator('img')).toHaveCount(0)
  await page.setViewportSize({ width: 390, height: 844 })
  await expect.poll(() => page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
  for (const button of await panel(page).getByRole('button').all()) expect((await button.boundingBox())!.height).toBeGreaterThanOrEqual(44)
  expect(await page.evaluate(() => [...Object.values(localStorage), ...Object.values(sessionStorage)].some(v => v.includes('cpu_utilization') || v.includes('opaque-a')))).toBe(false)
})
for (const [mode, label] of [['offline', 'Machine offline.'], ['unsupported', 'not supported'], ['forbidden', 'do not have access'], ['driver', 'NVIDIA driver unavailable'], ['no-gpu', 'No NVIDIA GPU detected'], ['empty', 'No samples in this range'], ['stale', 'Sample is stale'], ['failed-field', 'Collection failed'], ['removed', 'Removed device']] as const) test(`state ${mode} never substitutes zero or a raw diagnostic`, async ({ page }) => {
  const s = await setup(page, mode)
  await expect(panel(page).getByText(label, { exact: false }).first()).toBeVisible()
  await expect(panel(page)).not.toContainText('PRIVATE DIAGNOSTIC')
  if (mode === 'offline' || mode === 'unsupported') expect(s.queries()).toBe(0)
  if (mode === 'forbidden' || mode === 'offline') await expect(panel(page).locator('svg')).toHaveCount(0)
})
for (const [name, change] of [
  ['out-of-range percent', (v: MachineMetrics) => { v.series[0].current.value = 101 }],
  ['wrong machine', (v: MachineMetrics) => { v.server_id = 'foreign' }],
  ['over-cap GPUs', (v: MachineMetrics) => { v.gpus = Array.from({ length: 33 }, (_, i) => ({ id: String(i), name: 'test', present: true })) }],
  ['nonmonotonic buckets', (v: MachineMetrics) => { v.series[0].points[1].from = v.series[0].points[0].from }],
  ['unknown status', (v: MachineMetrics) => { v.series[0].current.status = 'PRIVATE DIAGNOSTIC' as never }],
  ['duplicate series', (v: MachineMetrics) => { v.series[1] = v.series[0] }],
  ['bad byte pair', (v: MachineMetrics) => { v.series[1].current.value = 100 * 2 ** 30 }],
  ['false full coverage', (v: MachineMetrics) => { v.series[0].points[3].partial = false }],
] as const) test(`rejects ${name} before rendering`, async ({ page }) => {
  await setup(page, 'normal', change)
  await expect(panel(page).getByRole('alert')).toContainText('could not be validated')
  await expect(panel(page).locator('svg')).toHaveCount(0)
})
test('range switch drops a late old response and leaving the page cancels refresh', async ({ page }) => {
  await setup(page)
  await expect(panel(page).locator('p').filter({ hasText: /^23\.4%$/ })).toBeVisible()
  let release!: () => void; const gate = new Promise<void>(r => { release = r })
  await page.route('**/machines/*/metrics?*', async r => { await gate; await r.fulfill({ json: {}, headers: { 'cache-control': 'no-store' } }).catch(() => {}) })
  await panel(page).getByRole('button', { name: '24h', exact: true }).click()
  await expect(panel(page).locator('p').filter({ hasText: /^23\.4%$/ })).toHaveCount(0)
  await page.getByRole('button', { name: 'Close', exact: true }).click()
  release(); await expect(panel(page)).toHaveCount(0)
})

test('repeated failure stops; refresh retries and logout clears metric memory', async ({ page }) => {
  await page.clock.install({ time: new Date() })
  const s = await setup(page, 'busy')
  await expect(panel(page)).toContainText('busy handling')
  // React development StrictMode may abort the first mount; measure completed polling after readiness.
  const initial = s.queries()
  await page.clock.fastForward(31000); await expect.poll(s.queries).toBe(initial + 1)
  await page.clock.fastForward(61000); await expect.poll(s.queries).toBe(initial + 2)
  await page.clock.fastForward(180000); expect(s.queries()).toBe(initial + 2)
  await panel(page).getByRole('button', { name: 'Refresh', exact: true }).click(); await expect.poll(s.queries).toBe(initial + 3)
  await page.getByRole('button', { name: 'Close', exact: true }).click()
  await page.getByRole('button', { name: 'User menu', exact: true }).click()
  await page.route('**/api/auth/me', r => r.fulfill({ status: 401, json: { error: { code: 'unauthorized' } } }))
  await page.getByRole('menuitem', { name: 'Log out', exact: true }).click()
  await expect(panel(page)).toHaveCount(0)
  await page.waitForURL('**/')
  await page.waitForLoadState('domcontentloaded')
  expect(await page.evaluate(() => [...Object.values(localStorage), ...Object.values(sessionStorage)].some(v => v.includes('cpu_utilization')))).toBe(false)
})

test('English/Chinese light/dark 390px and desktop screenshots expose full metric labels', async ({ page }, testInfo) => {
  await setup(page, 'stale'); await expect(panel(page).getByText('Sample is stale', { exact: false }).first()).toBeVisible()
  for (const locale of ['en', 'zh']) for (const dark of [false, true]) for (const width of [1360, 390]) {
    await page.evaluate(async ({ locale, dark }) => {
      const modulePath = '/src/i18n/index.ts'; const i18n = await import(/* @vite-ignore */ modulePath); i18n.setLocale(locale, false)
      document.documentElement.classList.toggle('dark', dark)
    }, { locale, dark })
    await page.setViewportSize({ width, height: 1000 })
    await expect.poll(() => page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
    for (const label of ['CPU', 'RAM', 'GPU 1']) {
      await panel(page).getByRole('heading', { name: label === 'GPU 1' ? /^GPU 1/ : label, exact: label !== 'GPU 1' }).scrollIntoViewIfNeeded()
      await page.screenshot({ path: testInfo.outputPath(`metrics-${locale}-${dark ? 'dark' : 'light'}-${width}-${label.replace(' ', '')}.png`) })
    }
  }
})

test('visibility pauses refresh and offline inventory removes previous readings', async ({ page }) => {
  await page.clock.install({ time: new Date() })
  const s = await setup(page)
  await expect(panel(page).locator('p').filter({ hasText: /^23\.4%$/ })).toBeVisible()
  const initial = s.queries()
  await page.evaluate(() => { Object.defineProperty(document, 'hidden', { configurable: true, value: true }); document.dispatchEvent(new Event('visibilitychange')) })
  await page.clock.fastForward(180000); expect(s.queries()).toBe(initial)
  await page.evaluate(() => { Object.defineProperty(document, 'hidden', { configurable: true, value: false }); document.dispatchEvent(new Event('visibilitychange')) })
  await expect.poll(s.queries).toBe(initial + 1)
  s.live.event('workspace.snapshot', { ...s.live.snapshot(), revision: 2, servers: s.live.snapshot().servers.map(m => ({ ...m, status: 'offline', capabilities: ['machine.metrics.v1'] })) })
  await expect(panel(page)).toContainText('Machine offline.')
  await expect(panel(page).locator('svg')).toHaveCount(0)
  const stopped = s.queries(); await page.clock.fastForward(180000); expect(s.queries()).toBe(stopped)
})


test('IEC memory units and long values table remain accurate and keyboard scrollable', async ({ page }) => {
  await setup(page, 'many')
  await expect(panel(page).locator('p').filter({ hasText: /^8\.00 GiB$/ })).toBeVisible()
  const chart = panel(page).getByRole('figure', { name: 'RAM used', exact: true })
  await expect(chart.locator('svg')).toContainText('8.00 GiB')
  await chart.getByRole('img').focus(); await page.keyboard.press('Home')
  await expect(chart.locator('[aria-live]')).toContainText('8.00 GiB')
  await chart.getByText('View sampled values', { exact: true }).click()
  const region = chart.getByRole('region')
  await region.focus(); await expect(region).toBeFocused()
  await page.keyboard.press('End')
  await expect.poll(() => region.evaluate(e => e.scrollTop)).toBeGreaterThan(0)
  await expect(region.getByRole('row').last()).toBeInViewport()
  await expect(region.getByRole('table')).toContainText('8.00 GiB')
  await page.evaluate(async () => { const path = '/src/i18n/index.ts'; const module = await import(/* @vite-ignore */ path); module.setLocale('zh', false) })
  await expect(panel(page).locator('p').filter({ hasText: /^8\.00 GiB$/ })).toBeVisible()
  await expect(panel(page).getByRole('figure').nth(1).getByRole('table')).toContainText('8.00 GiB')
})


test('fractional age uses exact UTC ordering before flooring milliseconds', async ({ page }) => {
  await setup(page)
  const request = { serverId: 'local', from: '2026-10-20T12:25:00Z', to: '2026-10-20T12:40:00Z', maxPoints: 360 }
  const body = metricsFixture(request)
  body.generated_at = '2026-10-20T12:40:00.000100Z'
  body.series[0].current.sampled_at = '2026-10-20T12:39:59.999900Z'
  body.series[0].current.age_ms = 0
  const result = await page.evaluate(async ({ body, request }) => {
    const path = '/src/lib/machine-metrics.ts'; const { parseMachineMetrics } = await import(/* @vite-ignore */ path)
    const received = Date.parse(body.generated_at)
    const valid = parseMachineMetrics(body, request, received).series[0].current.age_ms
    body.series[0].current.sampled_at = '2026-10-20T12:40:00.000300Z'
    let refused = false; try { parseMachineMetrics(body, request, received) } catch { refused = true }
    return { valid, refused }
  }, { body, request })
  expect(result).toEqual({ valid: 0, refused: true })
})


test('shared exact nanosecond span vectors accept source counts and reject decremented counts', async ({ page }) => {
  await page.goto('/')
  const raw = readFileSync(new URL('../../docs/fixtures/machine-metrics-span-vectors.json', import.meta.url))
  expect(createHash('sha256').update(raw).digest('hex')).toBe('851cdd7bfb7362ac4c5755a73f9442a577552751228b7ed39c935f150df6ed9b')
  const vectors = JSON.parse(raw.toString('utf8')) as {
    from: string; sample_interval_ms: number;
    vectors: { name: string; span_ns: string; max_points: number; bucket_width_ms: number; expected_counts: number[] }[];
  }
  const results = await page.evaluate(async data => {
    const path = '/e2e/fixtures/machine-metrics-spans.ts'
    const { exerciseSpanVectors } = await import(/* @vite-ignore */ path)
    return exerciseSpanVectors(data)
  }, vectors)
  expect(results).toEqual(vectors.vectors.map(v => ({ name: v.name, counts: v.expected_counts, decrementedRefused: true })))
})

test('GPU descriptor presence agrees with current status for all three GPU metrics', async ({ page }) => {
  await page.goto('/')
  const raw = readFileSync(new URL('../../docs/fixtures/machine-metrics-presence-vectors.json', import.meta.url))
  expect(createHash('sha256').update(raw).digest('hex')).toBe('85273f78b68942c49a04c51aa3c62ccbfaa7c2827acd79585dc4ee429d21bbc9')
  const vectors = JSON.parse(raw.toString('utf8')) as GpuPresenceVectors
  expect(vectors.schema).toBe(1)
  expect(vectors.vectors).toHaveLength(8)
  const results = await page.evaluate(async data => {
    const path = '/e2e/fixtures/machine-metrics-presence.ts'
    const { exerciseGpuPresence } = await import(/* @vite-ignore */ path)
    return exerciseGpuPresence(data)
  }, vectors)
  expect(results).toEqual(vectors.vectors.map(v => ({ name: v.name, accepted: v.accepted })))
})
