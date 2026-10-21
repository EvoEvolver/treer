import { renderEmbedBootstrap } from './embed-bootstrap'
import { createServer, type Server } from 'node:http'
import type { AddressInfo } from 'node:net'
import { test, expect, type Page } from '@playwright/test'

// All traffic is fulfilled/aborted locally. Run the real Proxy-owned template;
// the parent supplies only synthetic messages/code, never real authentication.
const parentOrigin = 'http://127.0.0.1:49191'
const defaultAgentOrigin = 'https://ag-0123456789abcdef0123456789abcdef.agents.example.test'
const code = 'iec_v1_' + 'b'.repeat(64)
type Reply = number | 'network' | 'redirect'
let redirectServer: Server | undefined
test.afterEach(async () => {
  if (redirectServer) {
    const server = redirectServer; redirectServer = undefined
    server.closeAllConnections()
    await new Promise<void>((resolve, reject) => server.close(error => error ? reject(error) : resolve()))
  }
})
function bootstrap(reuse: boolean) {
  return renderEmbedBootstrap(parentOrigin, reuse)
}
async function scenario(page: Page, reuse: boolean, initial: Reply, post: Reply) {
  const counts = { checks: 0, mints: 0, exchanges: 0, ui: 0, unexpected: 0 }
  let agentOrigin = defaultAgentOrigin
  // WebKit cannot route.fulfill a redirect. Use an actual ephemeral loopback
  // HTTP response for this case; loopback is a secure context for crypto.
  if (initial === 'redirect' || post === 'redirect') {
    redirectServer = createServer((request, response) => {
      expect(request.url).toBe('/.treer/embed/check')
      response.writeHead(302, { location: '/unexpected', 'cache-control': 'no-store' }); response.end()
    })
    await new Promise<void>(resolve => redirectServer!.listen(0, '127.0.0.1', resolve))
    agentOrigin = `http://127.0.0.1:${(redirectServer.address() as AddressInfo).port}`
  }
  await page.route('**/*', route => { counts.unexpected++; return route.abort('blockedbyclient') })
  await page.route(agentOrigin + '/**', async route => {
    const request = route.request(), path = new URL(request.url()).pathname
    if (path === '/.treer/embed') return route.fulfill({ contentType: 'text/html', body: bootstrap(reuse) })
    if (path === '/.treer/embed/check') {
      counts.checks++; expect(request.method()).toBe('GET')
      const reply = counts.exchanges ? post : initial
      if (reply === 'network') return route.abort('failed')
      if (reply === 'redirect') return route.continue() // only the owned loopback server
      return route.fulfill({ status: reply, body: '', headers: { 'cache-control': 'no-store' } })
    }
    if (path === '/.treer/embed/session') {
      counts.exchanges++; expect(request.method()).toBe('POST')
      const payload = request.postDataJSON()
      expect(payload.code).toBe(code); expect(payload.nonce).toMatch(/^[0-9a-f]{64}$/)
      return route.fulfill({ status: 204 })
    }
    if (path === '/') { counts.ui++; return route.fulfill({ contentType: 'text/html', body: '<h1>Protected fixture UI</h1>' }) }
    counts.unexpected++; return route.abort('blockedbyclient')
  })
  await page.route(parentOrigin + '/', route => route.fulfill({ contentType: 'text/html', body: `
    <output id="events">[]</output><iframe title="Agent fixture" src="${agentOrigin}/.treer/embed${reuse ? '?reuse=1' : ''}"></iframe>
    <script>
      const frame = document.querySelector('iframe'), events = [];
      let hash, minted = false;
      addEventListener('message', event => {
        if (event.origin !== ${JSON.stringify(agentOrigin)} || event.source !== frame.contentWindow) return;
        const data = event.data;
        events.push({ kind: data.treer_embed, hash: data.nonce_hash });
        document.querySelector('output').textContent = JSON.stringify(events);
        if (data.treer_embed === 'ready') {
          hash = data.nonce_hash;
          if (data.version === 2) frame.contentWindow.postMessage({treer_embed:'probe',version:2,nonce_hash:hash},event.origin);
          else giveCode(event.origin);
        } else if (data.treer_embed === 'mint' && data.nonce_hash === hash) giveCode(event.origin);
      });
      function giveCode(origin) {
        if (minted) throw Error('duplicate mint');
        minted = true;
        fetch('/mint-marker', {method:'POST'}).then(() => frame.contentWindow.postMessage({treer_embed:'code',code:${JSON.stringify(code)}},origin));
      }
    </script>` }))
  await page.route(parentOrigin + '/mint-marker', route => { counts.mints++; return route.fulfill({ status: 204 }) })
  await page.goto(parentOrigin + '/')
  return counts
}
for (const miss of [200, 401] as const) test(`initial ${miss} falls back once, post204 alone permits UI`, async ({ page }) => {
  const c = await scenario(page, true, miss, 204)
  await expect(page.frameLocator('iframe').getByRole('heading')).toHaveText('Protected fixture UI')
  expect(c).toEqual({ checks: 2, mints: 1, exchanges: 1, ui: 1, unexpected: 0 })
})
test('initial204 reuses without mint or exchange', async ({ page }) => {
  const c = await scenario(page, true, 204, 500)
  await expect(page.frameLocator('iframe').getByRole('heading')).toHaveText('Protected fixture UI')
  await expect(page.locator('#events')).toContainText('reused')
  expect(c).toEqual({ checks: 1, mints: 0, exchanges: 0, ui: 1, unexpected: 0 })
})
for (const reply of [201, 202, 403, 404, 429, 500, 'redirect', 'network'] as const) test(`initial ${reply} stops without mint, exchange or UI`, async ({ page }) => {
  const c = await scenario(page, true, reply, 204)
  await expect(page.locator('#events')).toContainText('cookies_blocked')
  const events = JSON.parse(await page.locator('#events').innerText()) as { kind: string; hash: string }[]
  expect(events.map(e => e.kind)).toEqual(['ready', 'cookies_blocked'])
  expect(events[0].hash).toMatch(/^[0-9a-f]{64}$/); expect(events[1].hash).toBe(events[0].hash)
  expect(c).toEqual({ checks: 1, mints: 0, exchanges: 0, ui: 0, unexpected: 0 })
})
for (const reuse of [false, true]) for (const reply of [200, 201, 401, 403, 500, 'redirect', 'network'] as const) test(`${reuse ? 'v2' : 'v1'} post-exchange ${reply} cannot authorize UI`, async ({ page }) => {
  const c = await scenario(page, reuse, 200, reply)
  await expect(page.locator('#events')).toContainText('cookies_blocked')
  const events = JSON.parse(await page.locator('#events').innerText()) as { kind: string; hash: string }[]
  expect(events.at(-1)?.hash).toBe(events[0].hash)
  expect(c).toEqual({ checks: reuse ? 2 : 1, mints: 1, exchanges: 1, ui: 0, unexpected: 0 })
})
test('v1 post-exchange204 loads UI without an initial check', async ({ page }) => {
  const c = await scenario(page, false, 500, 204)
  await expect(page.frameLocator('iframe').getByRole('heading')).toHaveText('Protected fixture UI')
  expect(c).toEqual({ checks: 1, mints: 1, exchanges: 1, ui: 1, unexpected: 0 })
})
