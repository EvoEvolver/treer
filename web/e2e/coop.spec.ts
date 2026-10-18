import { test, expect } from '@playwright/test'
import { createServer } from 'node:https'
import { readFileSync } from 'node:fs'
import { key, cert } from './fixtures/synthetic-tls'
import { tabAction } from './fixtures/tab-actions'
import { fixture, open } from './fixtures/workspace-tabs'

// Run this focused spec with --browser=all for the COOP compatibility matrix.
// Real TLS responses exercise browser browsing-context switches; API/provider
// responses are synthetic. No deployed authentication credentials are involved.
test.use({ ignoreHTTPSErrors: true })
let consoleOrigin = '', embeddedOrigin = ''
const attacks: string[] = []
const popup = `<!doctype html><h1>Independent interface</h1><button onclick="
  if (!window.opener) { document.querySelector('output').textContent = 'severed'; return; }
  const results = [];
  for (const relation of ['top', 'parent']) {
    try { void window.opener[relation].document.body; results.push('read allowed'); } catch { results.push('read blocked'); }
    try { window.opener[relation].location.href = '${'CONSOLE'}/attack'; results.push('navigation allowed'); } catch { results.push('navigation blocked'); }
  }
  document.querySelector('output').textContent = results.join(',');
">Try Console access</button><output></output>`
const embedded = createServer({ key, cert }, (request, response) => {
  response.setHeader('Content-Type', 'text/html')
  response.end(request.url?.startsWith('/popup') ? popup.replace('CONSOLE', consoleOrigin) : `<!doctype html>
    <button onclick="window.open('/popup', '_blank')">Open interface</button><output></output>
    <script>addEventListener('message', e => { if (e.origin === '${consoleOrigin}' && e.source === parent && e.data.type === 'treer:embed-theme') document.querySelector('output').textContent = e.data.theme; });</script>`)
})
const consoleServer = createServer({ key, cert }, (request, response) => {
  const url = new URL(request.url!, 'https://fixture.invalid')
  if (url.pathname === '/synthetic-callback') { response.writeHead(302, { location: consoleOrigin }); return response.end() }
  if (url.pathname === '/config.json') { response.setHeader('Content-Type', 'application/json'); return response.end(JSON.stringify({ proxy_url: consoleOrigin })) }
  response.setHeader('Content-Type', 'text/html')
  if (url.pathname === '/attack') attacks.push(request.url!)
  if (url.searchParams.get('policy') === 'report') response.setHeader('Cross-Origin-Opener-Policy-Report-Only', 'same-origin')
  else if (url.searchParams.get('policy') !== 'none') response.setHeader('Cross-Origin-Opener-Policy', 'same-origin')
  if (url.pathname === '/probe') return response.end(`<!doctype html><button onclick="window.open('${embeddedOrigin}/popup', '_blank')">Open cross-origin</button>`)
  response.end(readFileSync(new URL('../dist/index.html', import.meta.url)))
})
test.beforeAll(async () => {
  for (const server of [consoleServer, embedded]) await new Promise<void>((resolve, reject) => { server.once('error', reject); server.listen(0, '127.0.0.1', resolve) })
  const a = consoleServer.address(), b = embedded.address()
  if (!a || typeof a === 'string' || !b || typeof b === 'string') throw new Error('Missing fixture port')
  consoleOrigin = `https://127.0.0.1:${a.port}`
  embeddedOrigin = `https://localhost:${b.port}` // distinct site, no DNS or resolver flags
})
test.afterAll(async () => { for (const server of [consoleServer, embedded]) await new Promise<void>(resolve => server.close(() => resolve())) })

for (const policy of ['none', 'report', 'enforce']) test(`top-level popup isolation: ${policy}`, async ({ page, context }) => {
  const response = await page.goto(`${consoleOrigin}/probe?policy=${policy}`)
  expect(response!.headers()['cross-origin-opener-policy']).toBe(policy === 'enforce' ? 'same-origin' : undefined)
  const ready = context.waitForEvent('page')
  await page.getByRole('button', { name: 'Open cross-origin' }).click()
  const child = await ready
  await expect(child.getByRole('heading', { name: 'Independent interface' })).toBeVisible()
  expect(await child.evaluate(() => window.opener === null)).toBe(policy === 'enforce')
  await child.close()
})

for (const name of ['Alpha', 'Archive']) test(`COOP keeps embedded ${name === 'Alpha' ? 'Agent' : 'App'} popup and theme messaging working`, async ({ page, context, browserName }, info) => {
  await fixture(page)
  await page.goto(consoleOrigin)
  await expect(page.getByRole('link', { name: 'Open /Terminal', exact: true })).toBeVisible()
  await open(page, name)
  const id = name === 'Alpha' ? 'tab-1' : 'tab-7'
  const selector = `#workspace-panel-${id} iframe`
  await page.locator(selector).evaluate((frame: HTMLIFrameElement, src) => { frame.src = src }, `${embeddedOrigin}/interface`)
  const frame = page.frameLocator(selector)
  await expect(frame.getByRole('button', { name: 'Open interface' })).toBeVisible()
  await page.locator(selector).evaluate((element: HTMLIFrameElement, origin) => element.contentWindow!.postMessage({ type: 'treer:embed-theme', theme: 'dark' }, origin), embeddedOrigin)
  await expect(frame.locator('output')).toHaveText('dark')
  const ready = context.waitForEvent('page')
  await frame.getByRole('button', { name: 'Open interface' }).click()
  const child = await ready
  await expect(child.getByRole('heading', { name: 'Independent interface' })).toBeVisible()
  const opener = await child.evaluate(() => window.opener !== null)
  console.info(`COOP ${browserName} ${name}: iframe popup opener=${opener}`)
  await info.attach('opener-observation', { body: JSON.stringify({ browserName, name, opener }), contentType: 'application/json' })
  await child.getByRole('button', { name: 'Try Console access' }).click()
  await expect(child.locator('output')).toHaveText(opener ? 'read blocked,navigation blocked,read blocked,navigation blocked' : 'severed')
  await child.waitForTimeout(500)
  expect(attacks).toEqual([])
  await expect(page.getByRole('tab', { name: `Tab /${name}`, exact: true })).toBeVisible()
  await child.close()
})

for (const provider of ['github', 'google'] as const) test(`COOP ${provider} same-window round trip completes and logout works`, async ({ page, context }) => {
  await fixture(page)
  let signedIn = false
  await page.route('**/api/auth/me', route => signedIn ? route.fallback() : route.fulfill({ status: 401, json: {} }))
  await page.route('**/api/auth/config', route => route.fulfill({ json: { github: true, google: true } }))
  await page.route(`**/api/auth/oauth/${provider}/start`, route => {
    expect(route.request().method()).toBe('POST')
    return route.fulfill({ json: { authorization_url: `https://${provider === 'github' ? 'github.com' : 'accounts.google.com'}/synthetic-provider` } })
  })
  await page.route('https://*/synthetic-provider', route => route.fulfill({ contentType: 'text/html', body: `<a href="${consoleOrigin}/synthetic-callback">Complete synthetic login</a>` }))
  await page.route('**/synthetic-callback', route => { signedIn = true; return route.continue() })
  await page.route('**/api/auth/logout', route => { signedIn = false; return route.fulfill({ json: {} }) })
  await page.goto(consoleOrigin)
  await page.getByRole('button', { name: `Continue with ${provider === 'github' ? 'GitHub' : 'Google'}`, exact: true }).click()
  await page.getByRole('link', { name: 'Complete synthetic login' }).click()
  await expect(page.getByRole('link', { name: 'Open /Terminal', exact: true })).toBeVisible()
  expect(context.pages()).toHaveLength(1)
  // Use the application logout control rather than a synthetic opener callback.
  await page.getByRole('button', { name: /user menu/i }).click()
  await page.getByRole('menuitem', { name: 'Log out', exact: true }).click()
  await expect(page.getByRole('heading', { name: 'Sign in to Treer' })).toBeVisible()
})


test('COOP Console new-tab navigation loads independently without an opener', async ({ page, context }) => {
  await fixture(page)
  // Install each popup's API mocks before releasing its real document request.
  let popupSetup: Promise<unknown> = Promise.resolve()
  context.on('page', target => { if (target !== page) popupSetup = fixture(target) })
  await context.route(`${consoleOrigin}/**`, async route => {
    await popupSetup
    return route.continue()
  })
  await page.goto(consoleOrigin)
  await expect(page.getByRole('link', { name: 'Open /Terminal', exact: true })).toBeVisible()
  await open(page, 'Terminal')
  const ready = context.waitForEvent('page')
  await tabAction(page, 'Open in new browser tab')
  const child = await ready
  await expect(child.getByRole('link', { name: /full console/i })).toBeVisible()
  expect(new URL(child.url()).searchParams.get('focus')).toBe('1')
  expect(await child.evaluate(() => window.opener)).toBeNull()
  await expect(page.getByRole('tab', { name: 'Tab /Terminal', exact: true })).toBeVisible()
  await child.close()
})

test('COOP severs incoming cross-origin opens of the Console', async ({ page, context }) => {
  await page.goto(`${embeddedOrigin}/interface`)
  await page.evaluate(url => {
    const button = document.createElement('button')
    button.textContent = 'Open Console'
    button.onclick = () => { window.open(url, '_blank') }
    document.body.appendChild(button)
  }, `${consoleOrigin}/probe`)
  const ready = context.waitForEvent('page')
  await page.getByRole('button', { name: 'Open Console' }).click()
  const child = await ready
  await expect(child.getByRole('button', { name: 'Open cross-origin' })).toBeVisible()
  expect(await child.evaluate(() => window.opener)).toBeNull()
  await child.close()
})
