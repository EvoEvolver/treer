import { createHash } from 'node:crypto'
import { test, expect, type Page } from '@playwright/test'
import { fixture } from './fixtures/workspace-tabs'
import { renderEmbedBootstrap } from './embed-bootstrap'

// Actual Console caller + actual Rust bootstrap; HTTP endpoints are synthetic.
// No real credentials, cookie persistence, Proxy deployment or latency claim.
const id = 'ag_00000000000000000000000000000001'
const origin = 'https://ag-00000000000000000000000000000001.agents.example.test'
async function setup(page: Page, options: { ready?: boolean; initial?: number; post?: number; heldMint?: Promise<void> } = {}) {
  await page.clock.install()
  await fixture(page, { 'tab-1': origin }, { 'tab-1': id })
  await page.route('**/api/auth/config', route => route.fulfill({ json: { capabilities: ['agent.interface-origin.v1', 'agent.interface-embed.v1', 'agent.interface-embed-prewarm.v1'] } }))
  const counts = { hidden: 0, foreground: 0, checks: 0, exchanges: 0, ui: 0 }
  const mints: string[] = [], codes = new Map<string, string>()
  let ready = options.ready ?? false
  await page.route('**/interface/embed-authorization', async route => {
    const data = route.request().postDataJSON()
    expect(data.browser_nonce_hash).toMatch(/^[0-9a-f]{64}$/)
    mints.push(data.purpose ?? 'open')
    const code = 'iec_v1_' + mints.length.toString(16).padStart(64, '0')
    codes.set(code, data.browser_nonce_hash)
    if (options.heldMint) await options.heldMint
    await route.fulfill({ json: { code, origin, expires_in: 30 } }).catch(() => {})
  })
  await page.route(origin + '/**', async route => {
    const url = new URL(route.request().url())
    if (url.pathname === '/.treer/embed') {
      const hidden = url.searchParams.get('prewarm') === '1'
      if (hidden) counts.hidden++; else counts.foreground++
      return route.fulfill({ contentType: 'text/html', body: renderEmbedBootstrap(new URL(page.url()).origin, url.searchParams.get('reuse') === '1', hidden) })
    }
    if (url.pathname === '/.treer/embed/check') {
      counts.checks++
      expect(route.request().method()).toBe('GET')
      return route.fulfill({ status: ready ? options.post ?? 204 : options.initial ?? 200, body: '' })
    }
    if (url.pathname === '/.treer/embed/session') {
      counts.exchanges++
      const data = route.request().postDataJSON()
      expect(data.nonce).toMatch(/^[0-9a-f]{64}$/)
      expect(codes.get(data.code)).toBe(createHash('sha256').update(data.nonce).digest('hex'))
      expect(codes.delete(data.code)).toBe(true)
      ready = true
      return route.fulfill({ status: 204 })
    }
    expect(url.pathname).toBe('/')
    counts.ui++
    return route.fulfill({ contentType: 'text/html', body: '<h1>Composed Agent fixture</h1>' })
  })
  await page.goto('/')
  await expect(page.locator('[data-agent-id="tab-0"]')).toBeAttached()
  await page.clock.pauseAt(new Date(Date.now() + 1000))
  return { counts, mints, expire: () => { ready = false } }
}
const row = (page: Page) => page.getByRole('treeitem', { name: 'Alpha', exact: true })
async function hover(page: Page) { await row(page).hover(); await page.clock.runFor(401) }
async function click(page: Page) {
  await row(page).getByRole('link').click()
  await expect(page.frameLocator('iframe[title="/Alpha interface"]').getByRole('heading')).toHaveText('Composed Agent fixture')
}
for (const initial of [200, 401]) test(`real caller and bootstrap warm on initial${initial}, click rechecks without mint`, async ({ page }) => {
  const s = await setup(page, { initial })
  await hover(page)
  await expect.poll(() => s.counts.exchanges).toBe(1)
  await expect(page.locator('iframe[aria-hidden="true"]')).toHaveCount(0)
  expect(s.counts.ui).toBe(0); expect(s.mints).toEqual(['prewarm'])
  await click(page)
  expect(s.mints).toEqual(['prewarm'])
  expect(s.counts).toEqual({ hidden: 1, foreground: 1, checks: 3, exchanges: 1, ui: 1 })
})
test('real caller honors hidden initial204 without mint and never loads hidden UI', async ({ page }) => {
  const s = await setup(page, { ready: true })
  await hover(page)
  await expect.poll(() => s.counts.checks).toBe(1)
  await expect(page.locator('iframe[aria-hidden="true"]')).toHaveCount(0)
  expect(s.counts.ui).toBe(0); expect(s.mints).toEqual([])
  await click(page)
  expect(s.counts).toEqual({ hidden: 1, foreground: 1, checks: 2, exchanges: 0, ui: 1 })
  expect(s.mints).toEqual([])
})
test('real caller discards hidden403 without mint, exchange or UI', async ({ page }) => {
  const s = await setup(page, { initial: 403 })
  await hover(page)
  await expect.poll(() => s.counts.checks).toBe(1)
  await expect(page.locator('iframe[aria-hidden="true"]')).toHaveCount(0)
  expect(s.mints).toEqual([])
  expect(s.counts).toEqual({ hidden: 1, foreground: 0, checks: 1, exchanges: 0, ui: 0 })
})
test('real caller refuses hidden postexchange500 and never loads Agent HTML', async ({ page }) => {
  const s = await setup(page, { post: 500 })
  await hover(page)
  await expect.poll(() => s.counts.checks).toBe(2)
  await expect(page.locator('iframe[aria-hidden="true"]')).toHaveCount(0)
  expect(s.mints).toEqual(['prewarm']); expect(s.counts.ui).toBe(0)
})
test('real caller rechecks a lost warm session and performs a fresh foreground handoff', async ({ page }) => {
  const s = await setup(page)
  await hover(page)
  await expect.poll(() => s.counts.exchanges).toBe(1)
  await expect(page.locator('iframe[aria-hidden="true"]')).toHaveCount(0)
  s.expire()
  await click(page)
  expect(s.mints).toEqual(['prewarm', 'open'])
  expect(s.counts).toEqual({ hidden: 1, foreground: 1, checks: 4, exchanges: 2, ui: 1 })
})
test('actual bootstrap never receives a late code after the caller cancels', async ({ page }) => {
  let release!: () => void
  const heldMint = new Promise<void>(resolve => { release = resolve })
  const s = await setup(page, { heldMint })
  await hover(page)
  await expect.poll(() => s.mints.length).toBe(1)
  await page.getByRole('heading', { name: 'Workspace overview' }).hover()
  await expect(page.locator('iframe[aria-hidden="true"]')).toHaveCount(0)
  release(); await page.clock.runFor(100)
  expect(s.counts.exchanges).toBe(0); expect(s.counts.ui).toBe(0)
})
