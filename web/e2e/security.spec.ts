import { openOverviewMap } from './fixtures/navigation'
import { test, expect } from '@playwright/test'
import { securityHeaders } from '../worker/security-headers'
import { postEmbedTheme } from '../src/lib/embed-ui'
import { mockTreeMapApi } from './fixtures/treemap'
import { formatCommandLine, parseCommandLine } from '../src/lib/command-line'
import { workerOrigin as origin } from './ports'

test('CSP restricts scripts, framing, and configured connection origins', () => {
  const csp = securityHeaders('https://proxy.example.test/', 'synthetic-nonce').get('content-security-policy')!
  expect(csp).toContain("script-src 'nonce-synthetic-nonce'")
  expect(csp).not.toContain("'unsafe-eval'")
  expect(csp).toContain("frame-ancestors 'none'")
  expect(csp).toContain('https://proxy.example.test wss://proxy.example.test')
  expect(() => securityHeaders('https://user:fake@proxy.example.test/', 'fake')).toThrow()
})

test('actual local Worker protects shell, SPA, assets, config and errors', async ({ page }) => {
  for (const path of ['/', '/admin', '/organizations/fake/workspaces/fake', '/favicon.svg', '/config.json', '/health', '/missing.txt']) {
    const response = await fetch(origin + path)
    expect(response.headers.get('x-frame-options')).toBe('DENY')
    expect(response.headers.get('referrer-policy')).toBe('no-referrer')
    expect(response.headers.get('x-content-type-options')).toBe('nosniff')
    expect(response.headers.get('content-security-policy')).toContain("frame-ancestors 'none'")
    expect(response.headers.get('cross-origin-opener-policy')).toBe(
      (response.headers.get('content-type') ?? '').includes('text/html') ? 'same-origin' : null)
    expect(response.headers.get('cross-origin-embedder-policy')).toBeNull()
    if ((response.headers.get('content-type') ?? '').includes('text/html') || ['/health', '/config.json'].includes(path)) expect(response.headers.get('cache-control')).toBe('no-store')
  }
  const first = await fetch(origin)
  const second = await fetch(origin, { headers: { 'if-none-match': 'synthetic-etag' } })
  expect(second.status).toBe(200)
  expect(first.headers.get('content-security-policy')).not.toBe(second.headers.get('content-security-policy'))
  expect(first.headers.get('etag')).toBeNull()
  const body = await second.text()
  const nonce = second.headers.get('content-security-policy')!.match(/nonce-([^']+)/)![1]
  const nonces = await page.evaluate((html) => [...new DOMParser().parseFromString(html, 'text/html').scripts].map((script) => script.getAttribute('nonce')), body)
  expect(nonces.length).toBeGreaterThan(0)
  expect(nonces.every((value) => value === nonce)).toBe(true)
})

test('built shell runs under CSP while script injection, eval and framing fail', async ({ page, context }) => {
  await page.route('**/api/**', (route) => route.fulfill({ status: route.request().url().endsWith('/me') ? 401 : 200, contentType: 'application/json', body: '{}' }))
  await page.goto(origin)
  await expect(page.getByRole('heading', { name: 'Sign in to Treer' })).toBeVisible()
  await page.evaluate(() => { const script = document.createElement('script'); script.textContent = 'window.untrustedScriptRan = true'; document.body.appendChild(script) })
  expect(await page.evaluate(() => (window as Window & { untrustedScriptRan?: boolean }).untrustedScriptRan)).toBeUndefined()
  // Playwright evaluation bypasses CSP; execute eval in an ordinary nonce-authorized page script.
  await page.evaluate(() => { const script = document.createElement('script'); script.nonce = document.scripts[0].nonce; script.textContent = 'document.body.addEventListener("click", () => { try { eval("1"); window.pageEvalAllowed = true } catch { window.pageEvalAllowed = false } }, {once: true});'; document.body.appendChild(script) })
  await page.locator('h1').click()
  expect(await page.evaluate(() => (window as Window & { pageEvalAllowed?: boolean }).pageEvalAllowed)).toBe(false)
  const outer = await context.newPage()
  await outer.route('http://127.0.0.1:4200/', (route) => route.fulfill({ contentType: 'text/html', body: `<iframe src="${origin}"></iframe>` }))
  await outer.goto('http://127.0.0.1:4200/')
  await expect(outer.frameLocator('iframe').getByRole('heading', { name: 'Sign in to Treer' })).toHaveCount(0)
})

test('theme postMessage pins origin and never broadcasts to data or missing URLs', () => {
  const calls: unknown[][] = []
  const original = globalThis.window
  Object.defineProperty(globalThis, 'window', { configurable: true, value: { location: { href: 'https://app.example.test/' } } })
  const frame = (src: string) => ({ src, contentWindow: { postMessage: (...args: unknown[]) => calls.push(args) } } as unknown as HTMLIFrameElement)
  postEmbedTheme(frame('https://proxy.example.test/ui/'), 'dark')
  postEmbedTheme(frame('data:text/html,fake'), 'light')
  postEmbedTheme(frame(''), 'light')
  expect(calls).toEqual([[{ type: 'treer:embed-theme', theme: 'dark' }, 'https://proxy.example.test']])
  Object.defineProperty(globalThis, 'window', { configurable: true, value: original })
})

// Synthetic same-origin fixture reproduces the trust boundary, not a live Proxy exploit.
test('same-origin script-enabled iframe can reach parent and user API despite sandbox', async ({ page }) => {
  await page.route('http://127.0.0.1:4200/**', (route) => {
    const path = new URL(route.request().url()).pathname
    if (path === '/api/fake-user') return route.fulfill({ contentType: 'application/json', body: '{"syntheticUserAuthority":true}' })
    if (path === '/child') return route.fulfill({ contentType: 'text/html', body: '<script>fetch("/api/fake-user").then(r=>r.json()).then(v=>{parent.document.body.dataset.childAuthority=String(v.syntheticUserAuthority);frameElement.removeAttribute("sandbox")})</script>' })
    return route.fulfill({ contentType: 'text/html', body: '<iframe sandbox="allow-scripts allow-same-origin allow-forms allow-modals allow-downloads" src="/child"></iframe>' })
  })
  await page.goto('http://127.0.0.1:4200/')
  await expect(page.locator('body')).toHaveAttribute('data-child-authority', 'true')
  await expect(page.locator('iframe')).not.toHaveAttribute('sandbox')
})

test('HttpOnly prevents reads but SameSite does not block a sibling-origin request', async ({ page, context }) => {
  await context.addCookies([{ name: 'treer_session', value: 'synthetic-only', url: 'http://127.0.0.1:4200/', httpOnly: true, sameSite: 'Strict' }])
  const cookies: Record<string, boolean> = {}
  await page.route('http://127.0.0.1:4200/**', (route) => {
    const url = new URL(route.request().url())
    cookies[url.pathname] = (route.request().headers().cookie ?? '').includes('synthetic-only')
    return route.fulfill({ contentType: 'text/html', body: '<main>synthetic fixture</main>' })
  })
  await page.route('http://localhost:4200/', (route) => route.fulfill({ contentType: 'text/html', body: '<main>cross-site fixture</main>' }))
  await page.route('http://127.0.0.1:4203/', (route) => route.fulfill({ contentType: 'text/html', body: '<main>sibling-origin fixture</main>' }))
  await page.goto('http://127.0.0.1:4200/')
  expect(await page.evaluate(() => document.cookie.includes('synthetic-only'))).toBe(false)
  await page.goto('http://localhost:4200/')
  await page.evaluate(() => fetch('http://127.0.0.1:4200/cross-site', { method: 'POST', credentials: 'include', mode: 'no-cors', body: 'fake' }).then(() => undefined))
  expect(cookies['/cross-site']).toBe(false)
  await page.goto('http://127.0.0.1:4203/')
  await page.evaluate(() => fetch('http://127.0.0.1:4200/same-site', { method: 'POST', credentials: 'include', mode: 'no-cors', body: 'fake' }).then(() => undefined))
  expect(cookies['/same-site']).toBe(true)
})


test('command formatter preserves newline arguments and rejects shell comment/operator tokens', () => {
  const args = ['#literal', 'first\nsecond', '$(synthetic)', '; fake', "single'quote"]
  expect(parseCommandLine(formatCommandLine('synthetic-command', args))).toEqual({ command: 'synthetic-command', args })
  expect(() => parseCommandLine('synthetic-command # comment\nother-command')).toThrow()
  expect(() => parseCommandLine('synthetic-command && other-command')).toThrow()
})


test('built treemap works inside the unchanged single-file Worker CSP', async ({ page }) => {
  await mockTreeMapApi(page)
  await page.addInitScript(() => {
    (window as Window & { cspViolations?: string[] }).cspViolations = []
    document.addEventListener('securitypolicyviolation', (event) => (window as Window & { cspViolations: string[] }).cspViolations.push(event.violatedDirective))
  })
  await page.goto(origin)
  await openOverviewMap(page)
  await expect(page.getByRole('button', { name: 'Explore folder /Research', exact: true })).toBeVisible()
  await page.getByRole('button', { name: 'Explore folder /Product', exact: true }).click()
  await expect(page.getByRole('link', { name: /^Open \/Product\/reviewer · agent · build-node · offline$/ })).toBeVisible()
  expect(await page.evaluate(() => (window as Window & { cspViolations: string[] }).cspViolations)).toEqual([])
})
