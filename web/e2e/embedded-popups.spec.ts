import { test, expect } from '@playwright/test'
import { createServer } from 'node:https'
import { key, cert } from './fixtures/synthetic-tls'
import { fixture, gotoWorkspace, open } from './fixtures/workspace-tabs'

// Native popup handling uses full Chromium, as in agent-origin.spec.ts.
test.use({ channel: 'chromium', ignoreHTTPSErrors: true, launchOptions: { args: ['--host-resolver-rules=MAP *.agents.example.test 127.0.0.1', '--ignore-certificate-errors'] } })
let embeddedOrigin = ''
const server = createServer({ key, cert }, (request, response) => {
  response.writeHead(200, { 'Content-Type': 'text/html' })
  response.end(request.url === '/interface'
    ? `<!doctype html><button onclick="window.open('/popup', '_blank')">Open with opener</button>`
    : `<!doctype html><h1>Popup with opener</h1><button onclick="
      const results = [];
      for (const target of [window.opener.top, window.opener.parent]) {
        try { void target.document.body; results.push('read allowed') } catch { results.push('read blocked') }
        try { target.location.href = '/console-navigation-attack'; results.push('navigation allowed') } catch { results.push('navigation blocked') }
      }
      document.querySelector('output').textContent = results.join(',');
    ">Try Console access</button><output></output>`)
})
test.beforeAll(async () => {
  await new Promise<void>((resolve, reject) => { server.once('error', reject); server.listen(0, '127.0.0.1', resolve) })
  const address = server.address()
  if (!address || typeof address === 'string') throw new Error('No synthetic origin port')
  embeddedOrigin = `https://popup.agents.example.test:${address.port}`
})
test.afterAll(async () => { await new Promise<void>((resolve, reject) => server.close(error => error ? reject(error) : resolve())) })

test('cross-origin embedded popup cannot read or navigate the Console', async ({ page, context }) => {
  await fixture(page)
  const attacks: string[] = []
  await context.route('**/console-navigation-attack', route => {
    attacks.push(route.request().url())
    return route.abort()
  })
  await gotoWorkspace(page)
  await open(page, 'Alpha')
  // Keep the actual Console-rendered sandbox and replace only the fixture URL
  // with a distinct origin; no Console/Proxy deployment or DNS is required.
  await page.locator('#workspace-panel-tab-1 iframe').evaluate((frame: HTMLIFrameElement, url) => { frame.src = url }, `${embeddedOrigin}/interface`)
  const consoleUrl = page.url()
  const popupReady = context.waitForEvent('page')
  await page.frameLocator('#workspace-panel-tab-1 iframe').getByRole('button', { name: 'Open with opener' }).click()
  const popup = await popupReady
  await expect(popup.getByRole('heading', { name: 'Popup with opener' })).toBeVisible()
  expect(await popup.evaluate(() => window.opener !== null)).toBe(true)
  await popup.getByRole('button', { name: 'Try Console access' }).click()
  await expect(popup.locator('output')).toHaveText('read blocked,navigation blocked,read blocked,navigation blocked')
  // Negative security assertion: observe delayed requests as well as exceptions.
  await popup.waitForTimeout(500)
  expect(attacks).toEqual([])
  await expect(page).toHaveURL(consoleUrl)
  await expect(page.getByRole('tab', { name: 'Tab /Alpha', exact: true })).toBeVisible()
  await popup.close()
})

for (const [name, id] of [['Alpha', 'tab-1'], ['Archive', 'tab-7']] as const) {
  test(`embedded ${name === 'Alpha' ? 'Agent' : 'App'} can open a page from a user gesture`, async ({ page, context }) => {
    await fixture(page)
    await page.route('**/api/workspaces/**/ui/**', route => route.fulfill({
      contentType: 'text/html',
      body: `<!doctype html><button onclick="window.open('/embedded-popup-fixture', '_blank', 'noopener,noreferrer')">Open full page</button>`,
    }))
    // Context routing also handles the popup's first navigation.
    await context.route('**/embedded-popup-fixture', route => route.fulfill({
      contentType: 'text/html', body: '<!doctype html><h1>Full page interface</h1>',
    }))
    await gotoWorkspace(page)
    await open(page, name)
    const frame = page.locator(`#workspace-panel-${id} iframe`)
    expect(await frame.evaluate((element: HTMLIFrameElement) => [...element.sandbox].sort())).toEqual([
      'allow-downloads', 'allow-forms', 'allow-modals', 'allow-popups',
      'allow-popups-to-escape-sandbox', 'allow-same-origin', 'allow-scripts',
    ])
    const button = page.frameLocator(`#workspace-panel-${id} iframe`).getByRole('button', { name: 'Open full page' })
    // Exercise both pointer and keyboard activation, not evaluate(window.open).
    const popupReady = context.waitForEvent('page')
    if (name === 'Alpha') await button.click()
    else { await button.focus(); await button.press('Enter') }
    const popup = await popupReady
    await expect(popup.getByRole('heading', { name: 'Full page interface' })).toBeVisible()
    expect(await popup.evaluate(() => window.opener === null)).toBe(true)
    expect(await popup.evaluate(() => document.referrer)).toBe('')
    await expect(frame).toBeVisible()
    await popup.close()
  })
}
