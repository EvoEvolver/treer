import { readFileSync } from 'node:fs'
import { createHash } from 'node:crypto'
import { test, expect, type Page } from '@playwright/test'
import { securityHeaders } from '../worker/security-headers'
import { fixture, gotoWorkspace, open, switchTab } from './fixtures/workspace-tabs'
const id = 'ag_0123456789abcdef0123456789abcdef'
const origin = 'https://ag-0123456789abcdef0123456789abcdef.agents.example.test'
const hash = 'a'.repeat(64), code = 'iec_v1_' + 'b'.repeat(64)
async function setup(page: Page, mode: 'success' | 'blocked' | 'manual' = 'success', strict = false, realBootstrap = false, mintWait?: Promise<void>, enforceHeaders = false, navigationWait?: Promise<void>) {
  const live = await fixture(page, { 'tab-1': origin+'/' }, { 'tab-1': id })
  await page.route('**/api/auth/config', r => r.fulfill({ json: { capabilities: ['agent.interface-origin.v1', 'agent.interface-embed.v1'] } }))
  let mints = 0, probes = 0, delivered = 0, mintedHash = ''
  await page.route('**/interface/embed-authorization', async r => {
    mints++; mintedHash = r.request().postDataJSON().browser_nonce_hash
    if (!realBootstrap) expect(mintedHash).toBe(hash)
    if (mintWait) await mintWait
    return r.fulfill({ json: { code, origin, expires_in: 30 } }).catch(() => {})
  })
  await page.route('**/api/workspaces/**/interface/ui/**', r => {
    probes++
    return strict ? r.fulfill({ status: 503, json: { error: { code: 'embedded_ui_requires_isolated_origin' } } }) : r.fulfill({ contentType: 'text/html', body: '<h1>Compatibility interface</h1>' })
  })
  await page.route(origin+'/**', async r => {
    if (navigationWait && new URL(r.request().url()).pathname === '/.treer/embed') await navigationWait
    const target = JSON.stringify(new URL(page.url()).origin)
    if (realBootstrap && new URL(r.request().url()).pathname === '/.treer/embed') {
      // Execute the actual Core bootstrap, not a second implementation of it.
      const source = readFileSync('../crates/treer-proxy/src/api/network/embed.rs', 'utf8')
      const template = source.match(/r#"(<!doctype html>[\s\S]*?)"#/i)![1]
      const body = template.replaceAll('{console_js}', target).replaceAll('{script_nonce}', 'synthetic-nonce').replaceAll('{{', '{').replaceAll('}}', '}')
      return r.fulfill({ contentType: 'text/html', body })
    }
    if (new URL(r.request().url()).pathname !== '/.treer/embed') return r.fulfill({ contentType: 'text/html', body: `<h1>Isolated interface</h1><textarea aria-label="Draft"></textarea><script>
      parent.postMessage({treer_embed:'cookies_blocked'},${target});
      parent.postMessage({treer_embed:'ready',nonce_hash:'${'c'.repeat(64)}'},${target});
      parent.postMessage({treer_embed:'cookies_blocked',nonce_hash:'${'c'.repeat(64)}'},${target});</script>` })
    return r.fulfill({ contentType: 'text/html', body: `<script>
      window.addEventListener('message',event=>{
        if(event.origin!==${target}||event.source!==parent||event.data.treer_embed!=='code')return;
        fetch('/delivered',{method:'POST',body:event.data.code}).then(()=>{
        ${mode === 'success' ? "location.replace('/'+location.search)" : mode === 'blocked' ? `parent.postMessage({treer_embed:'cookies_blocked',nonce_hash:'${hash}'},${target})` : ''}
        });
      });
      ${mode === 'manual' ? '' : `parent.postMessage({treer_embed:'ready',nonce_hash:'${hash}'},${target})`}
      </script>` })
  })
  await page.route(origin+'/delivered', r => { delivered++; expect(r.request().postData()).toBe(code); return r.fulfill({ status: 204 }) })
  if (realBootstrap) {
    await page.route(origin+'/.treer/embed/session', r => {
      const body = r.request().postDataJSON()
      expect(body.code).toBe(code)
      expect(createHash('sha256').update(body.nonce).digest('hex')).toBe(mintedHash)
      delivered++
      return r.fulfill({ status: 204 })
    })
    await page.route(origin+'/.treer/embed/check', r => r.fulfill({ status: mode === 'blocked' ? 401 : 204 }))
  }
  if (enforceHeaders) {
    await page.route('**/config.json', r => r.fulfill({ json: { proxy_url: 'http://127.0.0.1:' + (process.env.PW_PORT ?? '4173') } }))
    const nonce = 'embed-header-regression'
    const headers = Object.fromEntries(securityHeaders('http://127.0.0.1:' + (process.env.PW_PORT ?? '4173'), nonce, 'https://agents.example.test/'))
    // Built single-file shell with the exact production header generator and
    // nonce rewriting, rather than Vite's header-free development document.
    const body = await page.evaluate(({ html, nonce }) => {
      const doc = new DOMParser().parseFromString(html, 'text/html')
      for (const script of doc.scripts) script.setAttribute('nonce', nonce)
      return '<!doctype html>' + doc.documentElement.outerHTML
    }, { html: readFileSync('dist/index.html', 'utf8'), nonce })
    await page.route('**/*', r => r.request().isNavigationRequest() && r.request().frame() === page.mainFrame()
      ? r.fulfill({ contentType: 'text/html', headers, body }) : r.fallback())
  }
  await gotoWorkspace(page); await open(page, 'Alpha')
  await expect(page.locator('iframe[title="/Alpha interface"]')).toHaveAttribute('src', /\.treer\/embed/)
  return { live, mints: () => mints, probes: () => probes, delivered: () => delivered }
}
async function send(page: Page, data: unknown) {
  await expect.poll(() => page.frames().some(f => f.url().startsWith(origin))).toBe(true)
  const frame = page.frames().find(f => f.url().startsWith(origin))!
  await frame.evaluate(({ data, target }) => parent.postMessage(data, target), { data, target: new URL(page.url()).origin })
}

test('v2 reuse probes once, skips mint and ignores Agent controls after navigation', async ({ page }) => {
  const s = await setup(page, 'manual')
  const child = page.frames().find(f => f.url().startsWith(origin))!
  await child.evaluate(() => {
    document.body.innerHTML = '<output aria-label="probe"></output>'
    addEventListener('message', event => {
      if (event.data?.treer_embed === 'probe') document.querySelector('output')!.textContent = JSON.stringify(event.data)
    })
  })
  await send(page, { treer_embed: 'ready', version: 2, nonce_hash: hash })
  await expect(child.getByLabel('probe')).toHaveText(JSON.stringify({ treer_embed: 'probe', version: 2, nonce_hash: hash }))
  expect(s.mints()).toBe(0)
  await send(page, { treer_embed: 'reused', version: 2, nonce_hash: hash })
  await child.goto(origin + '/')
  await expect(page.frameLocator('iframe').getByRole('heading')).toHaveText('Isolated interface')
  await expect(page.getByText('Checking interface isolation…', { exact: true })).toHaveCount(0)
  await send(page, { treer_embed: 'mint', version: 2, nonce_hash: hash })
  await send(page, { treer_embed: 'ready', version: 2, nonce_hash: hash })
  await send(page, { treer_embed: 'cookies_blocked', nonce_hash: hash })
  await expect(page.getByRole('alert')).toHaveCount(0)
  expect(s.mints()).toBe(0); expect(s.probes()).toBe(0)
})

test('v2 outcomes require the first ready hash, exact shape and assigned source', async ({ page }) => {
  const s = await setup(page, 'manual')
  await send(page, { treer_embed: 'mint', version: 2, nonce_hash: hash }) // unsolicited
  await send(page, { treer_embed: 'ready', version: 3, nonce_hash: hash })
  await send(page, { treer_embed: 'ready', version: 2, nonce_hash: hash })
  await page.evaluate(({ origin, hash }) => {
    window.dispatchEvent(new MessageEvent('message', { origin, source: window, data: { treer_embed: 'mint', version: 2, nonce_hash: hash } }))
  }, { origin, hash })
  await send(page, { treer_embed: 'mint', version: 2, nonce_hash: 'c'.repeat(64) })
  await send(page, { treer_embed: 'mint', version: 2, nonce_hash: hash, extra: true })
  await send(page, { treer_embed: 'ready', version: 2, nonce_hash: 'c'.repeat(64) })
  expect(s.mints()).toBe(0)
  await send(page, { treer_embed: 'mint', version: 2, nonce_hash: hash })
  await expect.poll(s.delivered).toBe(1)
  await send(page, { treer_embed: 'mint', version: 2, nonce_hash: hash })
  await send(page, { treer_embed: 'reused', version: 2, nonce_hash: hash })
  await send(page, { treer_embed: 'mint', version: 2, nonce_hash: hash })
  expect(s.mints()).toBe(1)
})

test('v2 failed reuse mints once and retains bound cookie-failure compatibility choice', async ({ page }) => {
  const s = await setup(page, 'manual')
  await send(page, { treer_embed: 'ready', version: 2, nonce_hash: hash })
  await send(page, { treer_embed: 'mint', version: 2, nonce_hash: hash })
  await expect.poll(s.delivered).toBe(1)
  await send(page, { treer_embed: 'cookies_blocked', nonce_hash: hash })
  await expect(page.getByRole('button', { name: 'Load compatibility view (shared origin)' })).toBeVisible()
  expect(s.mints()).toBe(1); expect(s.probes()).toBe(1)
})

test('v2 outcome after timeout cannot mint or reopen the frame', async ({ page }) => {
  await page.clock.install()
  const s = await setup(page, 'manual')
  await send(page, { treer_embed: 'ready', version: 2, nonce_hash: hash })
  const child = page.frames().find(f => f.url().startsWith(origin))!
  await page.clock.fastForward(21_000)
  await expect(page.getByRole('alert')).toContainText('did not load in time')
  await page.evaluate(({ origin, hash }) => {
    window.dispatchEvent(new MessageEvent('message', { origin, data: { treer_embed: 'mint', version: 2, nonce_hash: hash } }))
  }, { origin, hash })
  expect(child.isDetached()).toBe(true)
  expect(s.mints()).toBe(0); expect(s.delivered()).toBe(0)
})
test('isolated Agent retains draft and cannot force a shared-origin downgrade', async ({ page }) => {
  await page.clock.install()
  const s = await setup(page), ui = page.frameLocator('iframe[title="/Alpha interface"]')
  await expect(ui.getByRole('heading')).toHaveText('Isolated interface')
  await ui.getByRole('textbox').fill('retained draft')
  const isolated = page.frames().find(f => f.url().startsWith(origin))!
  expect(await isolated.evaluate(() => { try { return Boolean(parent.document) } catch { return false } })).toBe(false)
  await expect(page.getByText('Checking interface isolation…', { exact: true })).toHaveCount(0)
  await send(page, { treer_embed: 'ready', nonce_hash: hash })
  await send(page, { treer_embed: 'cookies_blocked', nonce_hash: hash })
  await page.clock.fastForward(21_000)
  await send(page, { treer_embed: 'cookies_blocked' })
  await switchTab(page, 'Overview'); await switchTab(page, 'Alpha')
  await expect(ui.getByRole('textbox')).toHaveValue('retained draft')
  await expect(page.locator('iframe')).toHaveAttribute('sandbox', 'allow-scripts allow-forms allow-same-origin allow-modals allow-downloads allow-popups allow-popups-to-escape-sandbox')
  expect(s.mints()).toBe(1); expect(s.delivered()).toBe(1); expect(s.probes()).toBe(0)
  expect(await page.locator('iframe').getAttribute('src')).not.toContain(code)
  expect(await page.evaluate(() => JSON.stringify({ ...localStorage, ...sessionStorage }))).not.toContain(code)
})
for (const strict of [false, true]) test(`cookie failure requires explicit choice; strict=${strict}`, async ({ page }) => {
  const s = await setup(page, 'blocked', strict), choice = page.getByRole('button', { name: 'Load compatibility view (shared origin)' })
  if (strict) {
    await expect(page.getByRole('alert')).toContainText('requires opening'); await expect(choice).toHaveCount(0)
  } else {
    await expect(choice).toBeVisible(); await expect(page.getByRole('alert')).toContainText('would run this interface on the control-plane origin shared with the Console')
    await expect(page.locator('iframe')).toHaveCount(0); expect(s.probes()).toBe(1)
    await choice.focus(); await page.keyboard.press('Enter')
    await expect(page.frameLocator('iframe').getByRole('heading')).toHaveText('Compatibility interface'); expect(s.probes()).toBe(2)
    await expect(page.getByRole('alert')).toBeFocused()
    await expect(page.getByRole('alert')).toContainText('Compatibility view: this interface shares the control-plane origin')
  }
  await expect(page.getByRole('link', { name: 'Open in new browser tab' })).toHaveAttribute('href', origin+'/')
  await expect(page.getByRole('link', { name: 'Open in new browser tab' })).toHaveAttribute('rel', 'noopener noreferrer')
})
test('ready checks source, origin, shape and replay; unbound failure ignored', async ({ page }) => {
  const s = await setup(page, 'manual')
  await page.evaluate(({ origin, hash }) => {
    window.dispatchEvent(new MessageEvent('message', { origin, source: window, data: { treer_embed: 'ready', nonce_hash: hash } }))
    window.dispatchEvent(new MessageEvent('message', { origin: 'https://evil.example.test', source: window, data: { treer_embed: 'ready', nonce_hash: hash } }))
  }, { origin, hash })
  for (const data of [{ treer_embed: 'ready', nonce_hash: 'invalid' }, { treer_embed: 'ready', nonce_hash: hash, extra: true }]) await send(page, data)
  expect(s.mints()).toBe(0)
  await send(page, { treer_embed: 'ready', nonce_hash: hash }); await expect.poll(s.delivered).toBe(1)
  await send(page, { treer_embed: 'ready', nonce_hash: hash })
  await send(page, { treer_embed: 'cookies_blocked' }); await send(page, { treer_embed: 'cookies_blocked', nonce_hash: 'c'.repeat(64) })
  await page.waitForTimeout(100); expect(s.mints()).toBe(1); expect(s.probes()).toBe(0)
})
test('mint permission failure never downgrades', async ({ page }) => {
  const s = await setup(page, 'manual')
  await page.route('**/interface/embed-authorization', r => r.fulfill({ status: 403, json: { error: { code: 'denied', message: 'denied' } } }))
  await send(page, { treer_embed: 'ready', nonce_hash: hash })
  await expect(page.getByRole('alert')).toContainText('Cannot open an isolated'); await expect(page.locator('iframe')).toHaveCount(0); expect(s.probes()).toBe(0)
})
test('late mint after timeout is dropped without credentials or fallback', async ({ page }) => {
  await page.clock.install(); const s = await setup(page, 'manual')
  let release!: () => void, started = false
  const held = new Promise<void>(resolve => { release = resolve })
  await page.route('**/interface/embed-authorization', async r => { started = true; await held; await r.fulfill({ json: { code, origin, expires_in: 30 } }).catch(() => {}) })
  await send(page, { treer_embed: 'ready', nonce_hash: hash }); await expect.poll(() => started).toBe(true)
  await page.clock.fastForward(21_000); await expect(page.getByRole('alert')).toContainText('did not load in time')
  release(); await expect(page.locator('iframe')).toHaveCount(0); expect(s.delivered()).toBe(0); expect(s.probes()).toBe(0)
})

for (const mode of ['success', 'blocked'] as const) test(`actual Core bootstrap: ${mode}`, async ({ page }) => {
  const s = await setup(page, mode, false, true)
  if (mode === 'success') await expect(page.frameLocator('iframe').getByRole('heading')).toHaveText('Isolated interface')
  else {
    await expect(page.getByRole('button', { name: 'Load compatibility view (shared origin)' })).toBeVisible()
    await expect(page.locator('iframe')).toHaveCount(0)
  }
  expect(s.mints()).toBe(1); expect(s.delivered()).toBe(1)
})

test('mint reply with another origin fails closed', async ({ page }) => {
  const s = await setup(page, 'manual')
  await page.route('**/interface/embed-authorization', r => r.fulfill({ json: { code, origin: 'https://evil.example.test', expires_in: 30 } }))
  await send(page, { treer_embed: 'ready', nonce_hash: hash })
  await expect(page.getByRole('alert')).toContainText('Cannot open an isolated')
  expect(s.delivered()).toBe(0); expect(s.probes()).toBe(0)
})

test('authoritative origin revocation unmounts the frame and drops a pending code', async ({ page }) => {
  await page.clock.install()
  const s = await setup(page, 'manual')
  let release!: () => void, started = false
  const held = new Promise<void>(resolve => { release = resolve })
  await page.route('**/interface/embed-authorization', async r => { started = true; await held; await r.fulfill({ json: { code, origin, expires_in: 30 } }).catch(() => {}) })
  await send(page, { treer_embed: 'ready', nonce_hash: hash }); await expect.poll(() => started).toBe(true)
  const old = s.live.snapshot()
  const snapshot = { ...old, revision: old.revision + 1, agents: old.agents.map(a => a.agent_id === id ? { ...a, interface: { ...a.interface!, origin_url: undefined } } : a) }
  await page.route('**/snapshot', r => r.fulfill({ json: snapshot }))
  s.live.event('workspace.snapshot', snapshot)
  await page.evaluate(() => window.dispatchEvent(new Event('focus')))
  await page.clock.fastForward(5100)
  await expect(page.locator('iframe')).toHaveCount(0)
  release(); expect(s.delivered()).toBe(0); expect(s.probes()).toBe(0)
})

for (const discovery of ['legacy', 'unavailable'] as const) test(`capability discovery ${discovery} never assumes isolation`, async ({ page }) => {
  const s = await setup(page, 'manual')
  await page.route('**/api/auth/config', r => discovery === 'legacy' ? r.fulfill({ status: 404, json: {} }) : r.fulfill({ status: 500, json: {} }))
  await page.reload(); await gotoWorkspace(page); await open(page, 'Alpha')
  if (discovery === 'legacy') await expect(page.frameLocator('iframe').getByRole('heading')).toHaveText('Compatibility interface')
  else { await expect(page.getByRole('alert')).toContainText('Cannot check interface isolation'); await expect(page.locator('iframe')).toHaveCount(0) }
  expect(s.mints()).toBe(0)
})

test('compatibility choice fits desktop and touch, English/Chinese and light/dark', async ({ page, browserName }) => {
  await setup(page, 'blocked')
  await expect(page.getByRole('button', { name: 'Load compatibility view (shared origin)' })).toBeVisible()
  for (const width of [1360, 390]) for (const theme of ['light', 'dark'] as const) for (const lang of ['en', 'zh'] as const) {
    await page.setViewportSize({ width, height: 900 })
    await page.evaluate(async ({ theme, lang }) => {
      const themeModule = await import('/src/lib/theme.ts')
      const i18nModule = await import('/src/i18n/index.ts')
      themeModule.persistTheme(theme); i18nModule.setLocale(lang)
    }, { theme, lang })
    const alert = page.getByRole('alert')
    await expect(alert).toContainText(lang === 'zh' ? '兼容视图会使此界面与 Console 共享来源' : 'Compatibility view would run this interface on the control-plane origin shared with the Console')
    expect(await alert.evaluate(el => el.scrollWidth <= el.clientWidth)).toBe(true)
    const button = alert.getByRole('button'), link = alert.getByRole('link')
    expect((await button.boundingBox())!.height).toBeGreaterThanOrEqual(44)
    expect((await link.boundingBox())!.height).toBeGreaterThanOrEqual(44)
    await button.focus(); await expect(button).toBeFocused()
    await page.screenshot({ path: `/tmp/treer-embed156-${browserName}-${width}-${theme}-${lang}.png` })
  }
  await page.getByRole('alert').getByRole('button').click()
  await expect(page.getByRole('alert')).toContainText('兼容视图：此界面与控制面共享来源')
})

test('the assigned WindowProxy at a different origin cannot mint or request fallback', async ({ page }) => {
  const s = await setup(page, 'manual')
  await expect.poll(() => page.frames().some(f => f.url().startsWith(origin))).toBe(true)
  const frame = page.frames().find(f => f.url().startsWith(origin))!
  await page.route('https://evil.example.test/**', r => r.fulfill({ contentType: 'text/html', body: '<h1>Other origin</h1>' }))
  await frame.goto('https://evil.example.test/')
  await frame.evaluate(({ target, hash }) => {
    parent.postMessage({ treer_embed: 'ready', nonce_hash: hash }, target)
    parent.postMessage({ treer_embed: 'cookies_blocked' }, target)
  }, { target: new URL(page.url()).origin, hash })
  await page.waitForTimeout(100)
  expect(s.mints()).toBe(0); expect(s.probes()).toBe(0)
})

test('the actual bootstrap 15 s timeout can offer compatibility before the Console deadline', async ({ page }) => {
  await page.clock.install()
  let release!: () => void
  const held = new Promise<void>(resolve => { release = resolve })
  const s = await setup(page, 'success', false, true, held)
  await expect.poll(s.mints).toBe(1)
  await page.clock.fastForward(16_000)
  await expect(page.getByRole('button', { name: 'Load compatibility view (shared origin)' })).toBeVisible()
  release()
  expect(s.probes()).toBe(1)
})

for (const strict of [false, true]) test(`built Console under production CSP loads isolated Agent; strict=${strict}`, async ({ page }) => {
  const violations: string[] = []
  page.on('console', m => { if (m.type() === 'error' && m.text().includes('Content Security Policy')) violations.push(m.text()) })
  const s = await setup(page, strict ? 'blocked' : 'success', strict, true, undefined, true)
  if (strict) await expect(page.getByRole('alert')).toContainText('requires opening')
  else await expect(page.frameLocator('iframe[title="/Alpha interface"]').getByRole('heading')).toHaveText('Isolated interface')
  expect(s.mints()).toBe(1); expect(s.delivered()).toBe(1)
  expect(violations).toEqual([])
})

for (const badOrigin of [
  origin.replace('https:', 'http:'),
  origin.replace('https://', 'https://x.'),
  origin.replace('0123456789abcdef0123456789abcdef', 'ffffffffffffffffffffffffffffffff'),
]) test(`invalid authenticated Agent origin never creates an embed frame: ${badOrigin}`, async ({ page }) => {
  await fixture(page, { 'tab-1': badOrigin + '/' }, { 'tab-1': id })
  await page.route('**/api/auth/config', r => r.fulfill({ json: { capabilities: ['agent.interface-origin.v1', 'agent.interface-embed.v1'] } }))
  await gotoWorkspace(page); await open(page, 'Alpha')
  await expect(page.getByRole('alert')).toContainText('Cannot')
  await expect(page.locator('iframe')).toHaveCount(0)
})


test('theme messages wait for iframe navigation and still follow later theme changes', async ({ page }) => {
  const errors: string[] = []
  page.on('console', message => { if (message.type() === 'error' || /postMessage|target origin/i.test(message.text())) errors.push(message.text()) })
  page.on('pageerror', error => errors.push(error.message))
  let release!: () => void
  const navigation = new Promise<void>(resolve => { release = resolve })
  try {
    await setup(page, 'manual', false, false, undefined, false, navigation)
    // Keep the iframe at its initial Console-origin about:blank through effects
    // and a theme change. Neither may post to the future Agent origin yet.
    const setTheme = async (value: 'dark' | 'light') => page.evaluate(async theme => {
      const module = await import('/src/lib/theme.ts')
      module.persistTheme(theme)
    }, value)
    await setTheme('dark')
    await page.waitForTimeout(200)
    expect(errors).toEqual([])
    release()
    await expect.poll(() => page.frames().some(frame => frame.url().startsWith(origin))).toBe(true)
    const child = page.frames().find(frame => frame.url().startsWith(origin))!
    await child.evaluate(() => {
      document.body.innerHTML = '<output></output>'
      addEventListener('message', event => {
        if (event.data?.type === 'treer:embed-theme') document.querySelector('output')!.textContent = event.data.theme
      })
    })
    await setTheme('light')
    await expect(child.locator('output')).toHaveText('light')
    expect(errors).toEqual([])
  } finally { release() }
})
