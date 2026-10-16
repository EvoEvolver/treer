// Closed browser regression: use the repository's existing Web Playwright
// installation. All HTTPS hosts below are synthetic and every request is
// fulfilled locally; no Proxy, real session, credentials or DNS is used.
import assert from 'node:assert/strict'
import { createRequire } from 'node:module'
const require = createRequire(new URL('../../../web/package.json', import.meta.url))
const { chromium } = require('@playwright/test')
const env = Object.fromEntries(Object.entries(process.env).filter(([key]) => /^(PATH|HOME|TMPDIR|LANG|LC_ALL)$/.test(key)))
const browser = await chromium.launch({ headless: true, env })
try {
  const context = await browser.newContext({ serviceWorkers: 'block' })
  const name = `__Host-treer_ingress_nonce_${'a'.repeat(64)}`
  await context.route('**/*', (route) => {
    const url = new URL(route.request().url())
    const headers = url.hostname === 'attacker.apps.example.test' ? {
      'set-cookie': `${name}=attacker-known; Domain=apps.example.test; Path=/; Secure; HttpOnly; SameSite=Lax`,
    } : url.pathname === '/start' ? {
      'set-cookie': `${name}=victim-secret; Path=/; Secure; HttpOnly; SameSite=Lax`,
    } : {}
    return route.fulfill({ status: 200, contentType: 'text/html', headers, body: '<p>synthetic</p>' })
  })
  const page = await context.newPage()
  await page.goto('https://attacker.apps.example.test/')
  assert.equal((await context.cookies('https://victim.apps.example.test/')).length, 0,
    'Domain cookie tossing must not create a verifier on the victim host')
  await page.goto('https://victim.apps.example.test/start')
  await page.goto('https://attacker.apps.example.test/')
  const cookies = await context.cookies('https://victim.apps.example.test/.treer/callback')
  assert.equal(cookies.length, 1)
  assert.equal(cookies[0].value, 'victim-secret', 'sibling host cannot overwrite verifier')
  assert.equal(cookies[0].domain, 'victim.apps.example.test')
  assert.equal(cookies[0].httpOnly, true)
  assert.equal(cookies[0].secure, true)
  assert.equal(cookies[0].sameSite, 'Lax')
  assert.equal(await page.evaluate(() => document.cookie.includes('__Host-treer_ingress_nonce_')), false)
  console.log('Chromium host-only ingress verifier / cookie-tossing checks passed (synthetic hosts only)')
} finally { await browser.close() }
