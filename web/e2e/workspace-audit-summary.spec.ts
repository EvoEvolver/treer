import { test, expect, type Page } from '@playwright/test'
import { mockTreeMapApi, mapSnapshot } from './fixtures/treemap'
import { browseTreeOnMobile } from './fixtures/navigation'
import { auditCategories } from '../src/lib/audit-summary'

test.use({ timezoneId: 'America/Toronto' })

const ws = 'map-workspace'
const summaryPath = `**/api/workspaces/${ws}/audit-summary?*`
const feedPath = `**/api/workspaces/${ws}/audit-events?*`
function summary(query = new URLSearchParams()) {
  const until = query.get('until') ?? '2026-10-19T00:00:00Z'
  const days = query.get('range') === '24h' ? 1 : query.get('range') === '30d' ? 30 : 7
  const since = query.get('since') ?? new Date(Date.parse(until) - days * 86400000).toISOString()
  const hours = (Date.parse(until) - Date.parse(since)) / 3600000
  const bucket = hours <= 96 ? 'hour' : hours <= 2160 ? 'day' : 'week'
  const step = (bucket === 'hour' ? 1 : bucket === 'day' ? 24 : 168) * 3600000
  const series = []
  for (let start = Date.parse(since), i = 0; start < Date.parse(until); start += step, i++) {
    const end = Math.min(start + step, Date.parse(until)), events = i === 6 ? 30 : i + 4
    series.push({ bucket_start: new Date(start).toISOString(), bucket_end: new Date(start + step).toISOString(), from: new Date(start).toISOString(), to: new Date(end).toISOString(), events, failed: i === 6 ? 6 : 0, by_category: Object.fromEntries(auditCategories.map((category) => [category, category === 'messages' ? events : 0])) })
  }
  const events = series.reduce((n, b) => n + b.events, 0), failed = series.reduce((n, b) => n + b.failed, 0)
  const filters: Record<string, string | boolean> = { notable: query.get('notable') === 'true' }
  for (const key of ['actor_kind', 'actor_id', 'action', 'category', 'resource_kind', 'resource_id', 'outcome']) if (query.has(key)) filters[key] = query.get(key)!
  return { workspace_id: ws, category_table_version: 1, range: { since, until, bucket, tz: 'UTC' }, filters, totals: { events, failed },
    categories: auditCategories.map((category) => ({ category, events: category === 'messages' ? events : 0, failed: category === 'messages' ? failed : 0 })), series,
    actors: { distinct: 2, omitted_events: 4, top: [{ actor_kind: 'agent', actor_id: 'reviewer-a', drillable: true, events: events - 4, failed }] },
    resources: { agent: { distinct: 1, omitted_events: 0, top: [{ resource_id: 'reviewer-b', resource_name: 'Renamed <current> Agent', drillable: true, events, failed }] }, machine: { distinct: 0, omitted_events: 0, top: [] }, app: { distinct: 0, omitted_events: 0, top: [] } },
    notable_total: 3, notable: [{ sequence: 812, occurred_at: series.at(-1)!.from, action: 'machine.exec', category: 'machines_files', actor_kind: 'agent', actor_id: 'reviewer-a', outcome: 'failed', resource_kind: 'agent', resource_id: 'reviewer-b', resource_name: 'Renamed <current> Agent' }], notable_capped: { 'machine.exec': 1 } as Record<string, number>,
    anomalies: { volume_evaluated: true, failure_evaluated: false, items: [] }, maintenance: { retention_days: 180, deleted_through: null }, coverage: { backfill_complete: true },
  }
}
async function setup(page: Page, capability = true) {
  await page.addInitScript(() => {
    const original = window.fetch
    ;(window as unknown as { summaryFetch: unknown[] }).summaryFetch = []
    window.fetch = (input, init) => {
      if (String(input).includes('/audit-summary')) (window as unknown as { summaryFetch: unknown[] }).summaryFetch.push({ cache: init?.cache, credentials: init?.credentials })
      return original(input, init)
    }
  })
  await mockTreeMapApi(page)
  const build = { version: 'test', git_commit: 'synthetic' }
  await page.route('**/snapshot', (route) => route.fulfill({ json: { ...mapSnapshot, servers: mapSnapshot.servers.map((machine) => ({ ...machine, controller_build: build, host_build: build })) } }))
  await page.route('**/api/auth/config', (route) => route.fulfill({ json: { capabilities: ['workspace.audit.v1', ...(capability ? ['workspace.audit-summary.v1'] : [])] } }))
  await page.route('**/groups', (route) => route.fulfill({ json: { groups: [] } }))
  await page.route('**/policy-provider', (route) => route.fulfill({ status: 404, json: {} }))
  const feeds: URLSearchParams[] = []
  await page.route(feedPath, (route) => { feeds.push(new URL(route.request().url()).searchParams); return route.fulfill({ json: { events: [], next_cursor: null } }) })
  await page.route(summaryPath, (route) => route.fulfill({ json: summary(new URL(route.request().url()).searchParams) }))
  return feeds
}
async function open(page: Page, mobile = false, zh = false) {
  await page.goto('/')
  await expect(page.getByRole('heading', { name: zh ? '工作区总览' : 'Workspace overview', exact: true })).toBeVisible()
  if (mobile && zh) { await page.getByRole('button', { name: '浏览树', exact: true }).click(); await expect(page.locator('#tree-sidebar')).toBeVisible() }
  else if (mobile) await browseTreeOnMobile(page)
  await page.getByRole('button', { name: zh ? '工作区设置' : 'Workspace settings', exact: true }).click()
  await page.getByRole('tab', { name: zh ? '审计' : 'Audit', exact: true }).click()
  return page.getByRole('region', { name: zh ? '活动概览' : 'Activity at a glance', exact: true })
}
for (const theme of ['light', 'dark']) for (const width of [1360, 390]) for (const lang of ['en', 'zh']) test(`aggregate audit charts ${theme} ${width} ${lang}`, async ({ page }, info) => {
  await page.setViewportSize({ width, height: 960 })
  await page.addInitScript(({ theme, lang }) => { localStorage.setItem('treer-theme', theme); localStorage.setItem('treer-lang', lang) }, { theme, lang })
  await setup(page)
  const view = await open(page, width === 390, lang === 'zh')
  await expect(view.getByText(lang === 'zh' ? '主要操作者' : 'Top actors', { exact: true }).first()).toBeVisible()
  await expect(view.getByText(lang === 'zh' ? /所示名称为当前名称/ : /Names shown are current names/).first()).toBeVisible()
  await expect(view.getByText(lang === 'zh' ? /某一类别占比超过/ : /One category exceeds/)).toBeVisible()
  await expect(view.getByText(lang === 'zh' ? /受限选集/ : /Capped selection/)).toBeVisible()
  // Patterned bars are supplemented by counts, exact tables and keyboard controls.
  const graph = view.getByRole('img', { name: lang === 'zh' ? /^消息与提示词：/ : /^Messages and prompts:/ })
  await expect(graph.locator('rect')).toHaveCount(7)
  const table = view.locator('details').filter({ has: page.locator('table') }).first()
  await table.locator('summary').click()
  await expect(table.getByRole('table')).toBeVisible()
  await expect(table.getByRole('columnheader', { name: lang === 'zh' ? '其他' : 'Other', exact: true })).toBeVisible()
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
  await view.getByRole('region', { name: lang === 'zh' ? '按类别查看活动' : 'Activity by category', exact: true }).evaluate((element) => element.scrollIntoView({ block: 'start' }))
  await page.screenshot({ path: info.outputPath(`activity-${theme}-${width}-${lang}.png`) })
  await view.getByRole('region', { name: lang === 'zh' ? '合并时间桶' : 'Combined time buckets', exact: true }).evaluate((element) => element.scrollIntoView({ block: 'center' }))
  await page.screenshot({ path: info.outputPath(`combined-${theme}-${width}-${lang}.png`) })
  await view.getByRole('region', { name: lang === 'zh' ? '主要操作者' : 'Top actors', exact: true }).screenshot({ path: info.outputPath(`actors-${theme}-${width}-${lang}.png`) })
  await view.evaluate((element) => { for (let parent: HTMLElement | null = element; parent; parent = parent.parentElement) parent.scrollTop = 0 })
  await page.screenshot({ path: info.outputPath(`charts-${theme}-${width}-${lang}.png`), fullPage: true })
})

test('chart selections send exact actor/category/resource/bucket/notable/sequence filters and reset paging', async ({ page }) => {
  const feeds = await setup(page)
  const view = await open(page)
  await view.getByRole('button', { name: /^Messages and prompts\s/ }).click()
  await expect.poll(() => feeds.at(-1)?.get('category')).toBe('messages')
  expect(feeds.at(-1)?.get('since')).toBe('2026-10-12T00:00:00.000Z')
  expect(feeds.at(-1)?.has('before')).toBe(false)
  await expect(page.getByRole('heading', { name: 'Selected events: Messages and prompts' })).toBeFocused()
  await view.getByRole('button', { name: /\/Research\/reviewer · agent/ }).click()
  await expect.poll(() => feeds.at(-1)?.get('actor_id')).toBe('reviewer-a')
  expect(feeds.at(-1)?.get('actor_kind')).toBe('agent')
  expect(feeds.at(-1)?.has('category')).toBe(false)
  await view.getByRole('button', { name: /^Renamed <current> Agent\s/ }).click()
  await expect.poll(() => feeds.at(-1)?.get('resource_id')).toBe('reviewer-b')
  expect(feeds.at(-1)?.get('resource_kind')).toBe('agent')
  await view.getByRole('button', { name: /^Messages and prompts\s/ }).click()
  const graph = view.getByRole('region', { name: 'Combined time buckets', exact: true })
  await graph.getByRole('button').first().click()
  await expect.poll(() => feeds.at(-1)?.get('until')).toBe('2026-10-13T00:00:00.000Z')
  expect(feeds.at(-1)?.get('category')).toBe('messages')
  await view.getByRole('button', { name: /machine.exec/ }).click()
  await expect.poll(() => feeds.at(-1)?.get('sequence')).toBe('812')
  await view.getByRole('button', { name: 'View all notable events', exact: true }).click()
  await expect.poll(() => feeds.at(-1)?.get('notable')).toBe('true')
  expect(feeds.at(-1)?.has('sequence')).toBe(false)
  await page.getByRole('button', { name: 'Clear chart selection' }).click()
  await expect.poll(() => feeds.at(-1)?.has('notable')).toBe(false)
})

test('range changes, custom local times, keyboard scroll, reduced motion and no-store', async ({ page }) => {
  await page.emulateMedia({ reducedMotion: 'reduce' })
  await setup(page)
  const requests: URLSearchParams[] = []
  await page.route(summaryPath, (route) => {
    const query = new URL(route.request().url()).searchParams; requests.push(query)
    return route.fulfill({ json: summary(query) })
  })
  const view = await open(page)
  await view.getByLabel('Chart time range').selectOption('30d')
  await expect.poll(() => requests.at(-1)?.get('range')).toBe('30d')
  expect(await page.evaluate(() => (window as unknown as { summaryFetch: unknown[] }).summaryFetch)).toEqual([{ cache: 'no-store', credentials: 'include' }, { cache: 'no-store', credentials: 'include' }])
  const graph = view.getByRole('region', { name: 'Combined time buckets', exact: true })
  await expect(graph.getByRole('button')).toHaveCount(30)
  await page.setViewportSize({ width: 390, height: 960 })
  await graph.focus(); await page.keyboard.press('Home')
  await expect.poll(() => graph.evaluate((el) => el.scrollLeft)).toBe(0)
  await page.keyboard.press('End')
  await expect.poll(() => graph.evaluate((el) => el.scrollLeft)).toBeGreaterThan(0)
  await view.getByLabel('Chart time range').selectOption('custom')
  await view.getByLabel('Chart from (local time)').fill('2026-10-10T00:00')
  await view.getByLabel('Chart until (exclusive, local time)').fill('2026-10-12T00:00')
  await view.getByRole('button', { name: 'Apply filters', exact: true }).click()
  await expect.poll(() => requests.at(-1)?.get('since')).toBe('2026-10-10T04:00:00.000Z')
  expect(requests.at(-1)?.get('tz')).toBe('UTC')
  expect(requests.at(-1)?.has('range')).toBe(false)
  await view.getByLabel('Chart from (local time)').fill('2025-01-01T00:00')
  const before = requests.length
  await view.getByRole('button', { name: 'Apply filters', exact: true }).click()
  await expect(view.getByRole('alert')).toContainText('180 days')
  expect(requests.length).toBe(before)
})

for (const mutation of ['workspace', 'version', 'totals', 'range', 'filters']) test(`aggregate rejects invalid ${mutation} without showing counts`, async ({ page }) => {
  await setup(page)
  await page.route(summaryPath, (route) => {
    const data = summary()
    if (mutation === 'workspace') data.workspace_id = 'other-workspace'
    if (mutation === 'version') data.category_table_version = 2
    if (mutation === 'totals') data.totals.events++
    if (mutation === 'range') data.series[0].from = '2020-01-01T00:00:00Z'
    if (mutation === 'filters') data.filters.actor_id = 'another-user'
    return route.fulfill({ json: data })
  })
  const view = await open(page)
  await expect(view.getByRole('alert')).toContainText(mutation === 'version' ? 'Update the Console' : 'Unable to load activity charts')
  await expect(view.getByRole('region', { name: 'Top actors', exact: true })).toHaveCount(0)
})

test('incomplete coverage withholds KPIs, exact IDs survive current names, and names render as text', async ({ page }) => {
  await setup(page)
  await page.route(summaryPath, (route) => {
    const data = summary()
    data.coverage.backfill_complete = false
    data.resources.agent.top[0].resource_name = '<img src=x onerror=alert(1)>'
    return route.fulfill({ json: data })
  })
  const view = await open(page)
  await expect(view.getByText(/Incomplete coverage/)).toBeVisible()
  await expect(view.locator('dl')).toHaveCount(0)
  await expect(view.locator('img')).toHaveCount(0)
  const table = view.getByRole('region', { name: 'Agent', exact: true }).locator('details')
  await table.locator('summary').click()
  await expect(table.getByRole('cell', { name: 'reviewer-b', exact: true })).toBeVisible()
  await expect(view.getByRole('button', { name: /^<img src=x/ })).toBeVisible()
})

test('missing capability makes no aggregate request; 403 has no org fallback or raw errors', async ({ page }) => {
  await setup(page, false)
  let requests = 0
  page.on('request', (request) => { if (/\/api\/workspaces\/[^/]+\/audit-summary\?/.test(request.url())) requests++ })
  await open(page)
  await expect(page.getByText('Activity charts are unavailable on this Proxy.', { exact: false })).toBeVisible()
  expect(requests).toBe(0)
  await page.route('**/api/auth/config', (route) => route.fulfill({ json: { capabilities: ['workspace.audit.v1', 'workspace.audit-summary.v1'] } }))
  await page.route(summaryPath, (route) => route.fulfill({ status: 403, json: { message: 'private-secret-body' } }))
  await page.getByRole('button', { name: 'Refresh audit', exact: true }).click()
  const view = page.getByRole('region', { name: 'Activity at a glance', exact: true })
  await expect(view.getByRole('alert')).toContainText('do not have permission')
  await expect(page.getByText('private-secret-body')).toHaveCount(0)
})

test('an obsolete aggregate cannot overwrite a new range, and leaving Audit drops sensitive charts', async ({ page }) => {
  await setup(page)
  let release: (() => void) | undefined
  await page.route(summaryPath, async (route) => {
    const query = new URL(route.request().url()).searchParams
    if (query.get('range') === '30d') await new Promise<void>((resolve) => { release = resolve })
    const data = summary(query)
    data.resources.agent.top[0].resource_name = query.get('range') === '30d' ? 'obsolete-private-result' : 'current-result'
    try { await route.fulfill({ json: data }) } catch { /* Changing range aborts the old request. */ }
  })
  const view = await open(page)
  await expect(view.getByRole('button', { name: /^current-result/ })).toBeVisible()
  await view.getByLabel('Chart time range').selectOption('30d')
  await expect.poll(() => typeof release).toBe('function')
  await view.getByLabel('Chart time range').selectOption('24h')
  await expect(view.getByRole('button', { name: /^current-result/ })).toBeVisible()
  release!()
  await expect(page.getByText('obsolete-private-result')).toHaveCount(0)
  await page.getByRole('tab', { name: 'Audit', exact: true }).focus()
  await page.keyboard.press('ArrowLeft')
  await expect(page.getByRole('region', { name: 'Activity at a glance', exact: true })).toHaveCount(0)
})

test('non-drillable IDs are preserved exactly; rate limits retry without leaking server text', async ({ page }) => {
  await setup(page)
  let fail = true
  const longId = 'x'.repeat(180)
  await page.route(summaryPath, (route) => {
    if (fail) return route.fulfill({ status: 429, json: { error: 'private-rate-limit-body' } })
    const data = summary()
    data.resources.agent.top[0].resource_id = longId
    data.resources.agent.top[0].drillable = false
    return route.fulfill({ json: data })
  })
  const view = await open(page)
  await expect(view.getByRole('alert')).toContainText('Wait a minute')
  await expect(page.getByText('private-rate-limit-body')).toHaveCount(0)
  fail = false
  await view.getByRole('button', { name: 'Retry charts', exact: true }).click()
  const resource = view.getByRole('region', { name: 'Agent', exact: true })
  await expect(resource.getByRole('button')).toBeDisabled()
  await resource.locator('summary').click()
  await expect(resource.getByRole('cell', { name: longId, exact: true })).toBeVisible()
})

test('empty summary and failed aggregate never fabricate activity or partial numbers', async ({ page }) => {
  await setup(page)
  let failed = false
  await page.route(summaryPath, (route) => {
    if (failed) return route.fulfill({ status: 503, json: { message: 'partial-count-123456' } })
    const data = summary()
    data.totals = { events: 0, failed: 0 }
    for (const row of data.categories) { row.events = 0; row.failed = 0 }
    for (const row of data.series) { row.events = 0; row.failed = 0; for (const key of auditCategories) row.by_category[key] = 0 }
    data.actors = { distinct: 0, omitted_events: 0, top: [] }
    for (const kind of ['agent', 'machine', 'app'] as const) data.resources[kind] = { distinct: 0, omitted_events: 0, top: [] }
    data.notable = []; data.notable_total = 0; data.notable_capped = {}
    data.anomalies = { volume_evaluated: false, failure_evaluated: false, items: [] }
    return route.fulfill({ json: data })
  })
  const view = await open(page)
  await expect(view.getByText('No recorded events in this range. Try a wider range.', { exact: true })).toBeVisible()
  await expect(view.getByText(/Insufficient comparable buckets/)).toBeVisible()
  failed = true
  await page.getByRole('button', { name: 'Refresh audit', exact: true }).click()
  await expect(view.getByRole('alert')).toContainText('No partial totals')
  await expect(view.locator('dl')).toHaveCount(0)
  await expect(page.getByText('partial-count-123456')).toHaveCount(0)
})
