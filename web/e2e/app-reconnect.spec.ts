import { test, expect } from '@playwright/test'
import { createServer, request, type Server } from 'node:http'
import type { AddressInfo } from 'node:net'
import { readFileSync } from 'node:fs'

// Actual HTTP gateway + independently stopped/delayed listener. Only Console
// runtime config is supplied by the fixture; App navigations/GETs are not mocked.
async function stack(consoleOrigin: string) {
  const errorFixture = process.env.TREER_APP_UI_ERROR_FIXTURE ? JSON.parse(readFileSync(process.env.TREER_APP_UI_ERROR_FIXTURE, 'utf8')) : null
  let listener: Server | null = null
  let port = 0
  let timer: ReturnType<typeof setTimeout> | undefined
  let gets = 0, heads = 0, posts = 0, denied = false, cors = true
  let headMode: 200 | 302 | 404 | 405 = 200
  let probeRedirects = 0, failHeads = 0, failGets = 0, staleHead = false
  const delayed = new Set<ReturnType<typeof setTimeout>>()
  const start = async () => {
    listener = createServer((req, res) => {
      if (req.method === 'POST') posts++
      res.writeHead(200, { 'Content-Type': 'text/html', 'Cache-Control': 'no-store' })
      res.end('<!doctype html><meta name="viewport" content="width=device-width"><h1>App is ready</h1><form method="post"><button>Action</button></form>')
    })
    await new Promise<void>(resolve => listener!.listen(port, '127.0.0.1', resolve))
    port = (listener.address() as AddressInfo).port
  }
  await start()
  const gateway = createServer((req, res) => {
    if (req.method === 'GET') gets++
    if (req.method === 'HEAD') heads++
    if (cors) {
      res.setHeader('Access-Control-Allow-Origin', consoleOrigin)
      res.setHeader('Access-Control-Allow-Credentials', 'true')
    }
    res.setHeader('Cache-Control', 'no-store')
    if (denied || !req.headers.cookie?.includes('fixture_session=owner')) {
      res.writeHead(401, { 'Content-Type': 'application/json' }); res.end('{"error":{"code":"authentication_required"}}'); return
    }
    if (req.method === 'HEAD' && staleHead) {
      staleHead = false
      const pending = setTimeout(() => { delayed.delete(pending); res.writeHead(502); res.end() }, 1500)
      delayed.add(pending)
      return
    }
    if (req.method === 'HEAD' && failHeads-- > 0) { res.writeHead(502); res.end(); return }
    if (req.method === 'GET' && failGets-- > 0) {
      // Byte-for-byte production Core renderer export, including nonce CSP.
      // The gateway remains a local producer, not a deployed Rust Proxy.
      res.writeHead(errorFixture.status, errorFixture.headers)
      res.end(errorFixture.body)
      return
    }
    if (req.url?.startsWith('/probe-must-not-follow')) probeRedirects++
    if (req.method === 'HEAD' && headMode !== 200) {
      res.writeHead(headMode, headMode === 302 ? { Location: '/probe-must-not-follow' } : {}); res.end(); return
    }
    if (headMode === 302 && req.method === 'GET' && !req.url?.includes('/home')) {
      res.writeHead(302, { Location: './home' }); res.end(); return
    }
    const upstream = request({ hostname: '127.0.0.1', port, path: req.url, method: req.method }, response => {
      res.writeHead(response.statusCode!, response.headers); response.pipe(res)
    })
    upstream.on('error', () => { res.writeHead(502, { 'Content-Type': 'application/json' }); res.end('{"error":{"code":"stream_failed","message":"failed to connect to service"}}') })
    req.pipe(upstream)
  })
  await new Promise<void>(resolve => gateway.listen(0, '127.0.0.1', resolve))
  const origin = `http://127.0.0.1:${(gateway.address() as AddressInfo).port}`
  return { origin, get gets() { return gets }, get posts() { return posts }, get heads() { return heads }, get probeRedirects() { return probeRedirects }, staleHead() { staleHead = true }, failHeads(n: number) { failHeads = n }, failGets(n: number) {
    expect(errorFixture, 'Export Core navigation error via the documented ignored test and set TREER_APP_UI_ERROR_FIXTURE').toBeTruthy()
    expect(errorFixture.console_origin).toBe(consoleOrigin)
    expect(errorFixture.status).toBe(502)
    failGets = n
  }, headMode(mode: 200 | 302 | 404 | 405) { headMode = mode }, deny() { denied = true }, noCors() { cors = false },
    async restart(delay: number | null) {
      const old = listener; listener = null
      if (old) { old.closeAllConnections(); await new Promise<void>(resolve => old.close(() => resolve())) }
      if (delay !== null) timer = setTimeout(() => { void start() }, delay)
    },
    async close() {
      clearTimeout(timer)
      for (const pending of delayed) clearTimeout(pending)
      for (const server of [gateway, listener]) if (server) { server.closeAllConnections(); await new Promise<void>(resolve => server.close(() => resolve())) }
    },
  }
}

test('real delayed listener: generation recovers, normal/hidden tabs stay quiet; no POST replay', async ({ page, baseURL }) => {
  test.setTimeout(60_000)
  const backend = await stack(baseURL!)
  try {
    await page.context().addCookies([{ name: 'fixture_session', value: 'owner', url: backend.origin, httpOnly: true, sameSite: 'Lax' }])
    await page.route('**/config.json', route => route.fulfill({ json: { proxy_url: backend.origin } }))
    await page.goto('/e2e/fixtures/app-reconnect/index.html')
    const ready = page.frameLocator('iframe').getByRole('heading', { name: 'App is ready' })
    await expect(ready).toBeVisible()
    await page.screenshot({ path: test.info().outputPath('app-ready-desktop-light.png') })
    await page.frameLocator('iframe').getByRole('button', { name: 'Action' }).click()
    await expect.poll(() => backend.posts).toBe(1)
    const healthy = { gets: backend.gets, heads: backend.heads }
    await page.waitForTimeout(1500)
    expect({ gets: backend.gets, heads: backend.heads }).toEqual(healthy)
    await backend.restart(5000)
    await page.getByRole('button', { name: 'New generation' }).click()
    if (process.env.TREER_EXPECT_OLD === '1') {
      await expect(page.frameLocator('iframe').locator('body')).toContainText('stream_failed')
      await page.waitForTimeout(6500)
      await expect(page.frameLocator('iframe').locator('body')).toContainText('stream_failed')
      return
    }
    await expect(page.getByRole('status')).toContainText('Connecting')
    await expect(ready).toBeVisible({ timeout: 16_000 })
    expect(backend.posts).toBe(1)
    await page.setViewportSize({ width: 390, height: 844 })
    await page.getByRole('button', { name: 'Theme', exact: true }).click()
    await page.getByRole('button', { name: 'Toggle tab' }).click()
    await backend.restart(200)
    await page.getByRole('button', { name: 'New generation' }).click()
    const hiddenRequests = { gets: backend.gets, heads: backend.heads }
    await page.waitForTimeout(1200)
    expect({ gets: backend.gets, heads: backend.heads }).toEqual(hiddenRequests)
    await page.getByRole('button', { name: 'Toggle tab' }).click()
    await expect(ready).toBeVisible()
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
    expect(backend.posts).toBe(1)
    await page.screenshot({ path: test.info().outputPath('app-ready-mobile-dark.png') })
  } finally { await page.goto('about:blank'); await backend.close() }
})

test('permanent failure is bounded; keyboard retry, zh/mobile and authentication/CORS refusal', async ({ page, baseURL }) => {
  test.skip(process.env.TREER_EXPECT_OLD === '1')
  test.setTimeout(75_000)
  const backend = await stack(baseURL!)
  try {
    await page.context().addCookies([{ name: 'fixture_session', value: 'owner', url: backend.origin, httpOnly: true }])
    await page.route('**/config.json', route => route.fulfill({ json: { proxy_url: backend.origin } }))
    await page.setViewportSize({ width: 390, height: 844 })
    await page.goto('/e2e/fixtures/app-reconnect/index.html')
    await expect(page.frameLocator('iframe').getByRole('heading')).toBeVisible()
    await backend.restart(null)
    await page.getByRole('button', { name: 'New generation' }).click()
    await expect(page.getByRole('alert')).toBeVisible({ timeout: 40_000 })
    const exhausted = backend.heads
    expect(exhausted).toBeLessThanOrEqual(10)
    await page.waitForTimeout(1500)
    expect(backend.heads).toBe(exhausted)
    await page.getByRole('button', { name: 'Language' }).click()
    await expect(page.getByRole('alert')).toContainText('无法')
    await page.getByRole('button', { name: 'Theme', exact: true }).click()
    await page.screenshot({ path: test.info().outputPath('app-error-mobile-zh.png') })
    const retry = page.getByRole('button', { name: '重试连接' })
    await retry.focus(); await page.keyboard.press('Enter')
    await expect(page.getByRole('status')).toBeVisible()
    backend.deny()
    await expect(page.getByRole('alert')).toContainText('登录', { timeout: 10_000 })
    const authHeads = backend.heads
    await page.waitForTimeout(1500); expect(backend.heads).toBe(authHeads)
    backend.noCors()
    await retry.click()
    await expect(page.getByRole('alert')).toBeVisible({ timeout: 40_000 })
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
    expect(backend.posts).toBe(0)
  } finally { await page.goto('about:blank'); await backend.close() }
})


test('HEAD 405 and root redirects are responsive; probes never follow redirects or duplicate GET', async ({ page, baseURL }) => {
  test.skip(process.env.TREER_EXPECT_OLD === '1')
  const backend = await stack(baseURL!)
  try {
    await page.context().addCookies([{ name: 'fixture_session', value: 'owner', url: backend.origin, httpOnly: true }])
    await page.route('**/config.json', route => route.fulfill({ json: { proxy_url: backend.origin } }))
    backend.headMode(405)
    await page.goto('/e2e/fixtures/app-reconnect/index.html')
    await expect(page.frameLocator('iframe').getByRole('heading')).toBeVisible()
    expect(backend.gets).toBe(1)
    await expect.poll(() => backend.heads).toBe(1)
    backend.headMode(302)
    await page.getByRole('button', { name: 'New generation' }).click()
    await expect(page.frameLocator('iframe').getByRole('heading')).toBeVisible()
    expect(backend.gets).toBe(3)
    expect(backend.probeRedirects).toBe(0)
    const stable = backend.heads
    await page.waitForTimeout(1200)
    expect(backend.heads).toBe(stable)
  } finally { await page.goto('about:blank'); await backend.close() }
})


test('actual GET failure after successful HEAD recovers through bounded bridge; later App actions stay intact', async ({ page, baseURL }) => {
  test.skip(process.env.TREER_EXPECT_OLD === '1')
  const backend = await stack(baseURL!)
  try {
    await page.context().addCookies([{ name: 'fixture_session', value: 'owner', url: backend.origin, httpOnly: true }])
    await page.route('**/config.json', route => route.fulfill({ json: { proxy_url: backend.origin } }))
    backend.failGets(1)
    await page.goto('/e2e/fixtures/app-reconnect/index.html')
    await expect(page.frameLocator('iframe').getByRole('heading', { name: 'App is ready' })).toBeVisible({ timeout: 10_000 })
    expect(backend.gets).toBe(2)
    const heads = backend.heads
    await page.frameLocator('iframe').getByRole('button', { name: 'Action' }).click()
    await expect.poll(() => backend.posts).toBe(1)
    await page.evaluate(() => window.postMessage({ type: 'treer.app-ui.unavailable', workspaceId: 'workspace', appId: 'app' }, '*'))
    await page.waitForTimeout(800)
    expect(backend.heads).toBe(heads)
    expect(backend.gets).toBe(2)
    backend.failGets(20)
    await page.getByRole('button', { name: 'New generation' }).click()
    await expect(page.getByRole('alert')).toBeVisible({ timeout: 12_000 })
    const exhausted = backend.gets
    await page.waitForTimeout(800)
    expect(backend.gets).toBe(exhausted)
    expect(exhausted).toBe(10) // 2 earlier + exactly 8 in this generation
  } finally { await page.goto('about:blank'); await backend.close() }
})

test('last permitted HEAD success displays App instead of rejecting it on load', async ({ page, baseURL }) => {
  test.skip(process.env.TREER_EXPECT_OLD === '1')
  test.setTimeout(35_000)
  const backend = await stack(baseURL!)
  try {
    await page.context().addCookies([{ name: 'fixture_session', value: 'owner', url: backend.origin, httpOnly: true }])
    await page.route('**/config.json', route => route.fulfill({ json: { proxy_url: backend.origin } }))
    backend.failHeads(7)
    await page.goto('/e2e/fixtures/app-reconnect/index.html')
    await expect(page.frameLocator('iframe').getByRole('heading')).toBeVisible({ timeout: 28_000 })
    expect(backend.heads).toBe(8)
    expect(backend.gets).toBe(1)
    await page.waitForTimeout(1000)
    await expect(page.frameLocator('iframe').getByRole('heading')).toBeVisible()
    expect(backend.heads).toBe(8)
  } finally { await page.goto('about:blank'); await backend.close() }
})


test('late failed probe from an older generation cannot replace the new frame', async ({ page, baseURL }) => {
  test.skip(process.env.TREER_EXPECT_OLD === '1')
  const backend = await stack(baseURL!)
  try {
    await page.context().addCookies([{ name: 'fixture_session', value: 'owner', url: backend.origin, httpOnly: true }])
    await page.route('**/config.json', route => route.fulfill({ json: { proxy_url: backend.origin } }))
    backend.staleHead()
    await page.goto('/e2e/fixtures/app-reconnect/index.html')
    await expect.poll(() => backend.heads).toBe(1)
    await page.getByRole('button', { name: 'New generation' }).click()
    await expect(page.frameLocator('iframe').getByRole('heading')).toBeVisible()
    await page.waitForTimeout(1800)
    await expect(page.frameLocator('iframe').getByRole('heading')).toBeVisible()
    expect(backend.heads).toBe(2)
    expect(backend.gets).toBe(1)
  } finally { await page.goto('about:blank'); await backend.close() }
})


test('connection error fits desktop/mobile, light/dark, en/zh with keyboard retry', async ({ page, baseURL }) => {
  test.skip(process.env.TREER_EXPECT_OLD === '1')
  const backend = await stack(baseURL!)
  try {
    await page.context().addCookies([{ name: 'fixture_session', value: 'owner', url: backend.origin, httpOnly: true }])
    await page.route('**/config.json', route => route.fulfill({ json: { proxy_url: backend.origin } }))
    backend.headMode(404)
    await page.goto('/e2e/fixtures/app-reconnect/index.html')
    await expect(page.getByRole('alert')).toBeVisible()
    for (const width of [1280, 390]) {
      await page.setViewportSize({ width, height: 844 })
      for (const theme of ['light', 'dark']) {
        if (theme === 'dark') {
          await page.getByRole('button', { name: 'Theme', exact: true }).click()
          await page.getByRole('button', { name: 'Language' }).click()
        }
        const retry = page.getByRole('button', { name: /Retry connection|重试连接/ })
        await retry.focus()
        await expect(retry).toBeFocused()
        expect((await retry.boundingBox())!.height).toBeGreaterThanOrEqual(44)
        expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
        await page.screenshot({ path: test.info().outputPath(`error-${width}-${theme}.png`) })
        await page.keyboard.press('Enter')
        await expect(page.getByRole('alert')).toBeVisible()
        if (theme === 'dark') {
          await page.getByRole('button', { name: 'Theme', exact: true }).click()
          await page.getByRole('button', { name: 'Language' }).click()
        }
      }
    }
    expect(backend.gets).toBe(0)
  } finally { await page.goto('about:blank'); await backend.close() }
})
