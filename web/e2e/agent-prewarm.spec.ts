import { test, expect, type Page } from '@playwright/test'
import { mkdir } from 'node:fs/promises'
import { join } from 'node:path'
import { fixture } from './fixtures/workspace-tabs'

const ids = Array.from({ length: 11 }, (_, i) => `ag_${(i + 1).toString(16).padStart(32, '0')}`)
const origins = ids.map(id => `https://ag-${id.slice(3)}.agents.example.test`)
const hash = 'a'.repeat(64), code = 'iec_v1_' + 'b'.repeat(64)
async function setup(page: Page, options: { capability?: boolean; quiet?: boolean; denied?: boolean; holdMint?: Promise<void>; many?: boolean } = {}) {
  await page.clock.install()
  const live = await fixture(page, Object.fromEntries(origins.slice(0, 6).map((origin, i) => [`tab-${i + 1}`, origin])), Object.fromEntries(ids.slice(0, 6).map((id, i) => [`tab-${i + 1}`, id])))
  for (const [offset, name] of ['Eta', ...(options.many ? ['Theta', 'Iota', 'Kappa', 'Lambda'] : [])].entries()) {
    const index = offset + 6
    live.snapshot().agents.push({ ...live.snapshot().agents[1], agent_id: ids[index], name, path: `/${name}`, interface: { ...live.snapshot().agents[1].interface!, instance_id: `ui-${name}`, origin_url: origins[index] } })
  }
  await page.route('**/api/auth/config', r => r.fulfill({ json: { capabilities: ['agent.interface-origin.v1', 'agent.interface-embed.v1', ...(options.capability === false ? [] : ['agent.interface-embed-prewarm.v1'])] } }))
  const minted: string[] = [], foreground: string[] = [], ui: string[] = [], bootstraps: string[] = [], warmed = new Set<string>()
  await page.route('**/interface/embed-authorization', async r => {
    const data = r.request().postDataJSON(), id = new URL(r.request().url()).pathname.split('/').at(-3)!
    if (data.purpose === 'prewarm') minted.push(id); else foreground.push(id)
    expect(data.browser_nonce_hash).toBe(hash)
    if (options.holdMint) await options.holdMint
    return options.denied ? r.fulfill({ status: 429, json: {} }).catch(() => {}) : r.fulfill({ json: { code, origin: origins[ids.indexOf(id)], expires_in: 30 } }).catch(() => {})
  })
  await page.route('https://ag-*.agents.example.test/**', async r => {
    const url = new URL(r.request().url()), index = origins.indexOf(url.origin), id = ids[index]
    if (url.pathname === '/session') { warmed.add(id); return r.fulfill({ status: 204 }) }
    if (url.pathname !== '/.treer/embed') { ui.push(id); return r.fulfill({ contentType: 'text/html', body: '<h1>Agent interface</h1><textarea aria-label="Draft"></textarea>' }) }
    const prewarm = url.searchParams.get('prewarm') === '1'
    bootstraps.push(id)
    const target = JSON.stringify(new URL(page.url()).origin)
    return r.fulfill({ contentType: 'text/html', body: `<script>
      const target=${target}, hash=${JSON.stringify(hash)}, prewarm=${prewarm};
      const post=(treer_embed)=>parent.postMessage({treer_embed,version:2,nonce_hash:hash},target);
      addEventListener('message',async event=>{
        if(event.source!==parent||event.origin!==target)return;
        if(event.data.treer_embed==='probe') {
          ${options.quiet ? '' : warmed.has(id) ? "post(prewarm?'warmed':'reused');if(!prewarm)location.replace('/');" : "post('mint');"}
        }
        if(event.data.treer_embed==='code') {
          await fetch('/session',{method:'POST'});post(prewarm?'warmed':'reused');if(!prewarm)location.replace('/');
        }
      });
      ${options.quiet ? '' : "post('ready');"}
    </script>` })
  })
  await page.goto('/')
  await expect(page.locator('[data-agent-id="tab-0"]')).toBeAttached()
  await page.clock.pauseAt(new Date(Date.now() + 1000))
  return { live, minted, foreground, ui, bootstraps }
}
const row = (page: Page, name: string) => page.getByRole('treeitem', { name, exact: true })
async function hover(page: Page, name: string) { await row(page, name).hover(); await page.clock.runFor(401) }
async function report(page: Page, index: number, kind: string, nonceHash = hash) {
  await expect.poll(() => page.frames().some(frame => frame.url().startsWith(origins[index]))).toBe(true)
  const frame = page.frames().find(frame => frame.url().startsWith(origins[index]))!
  await frame.evaluate(({ target, kind, nonceHash }) => parent.postMessage({ treer_embed: kind, version: 2, nonce_hash: nonceHash }, target), { target: new URL(page.url()).origin, kind, nonceHash })
}

test('prewarm waits for dwell, stays hidden, never opens Agent HTML; click reuses without another mint', async ({ page }) => {
  const s = await setup(page)
  await row(page, 'Alpha').hover(); await page.clock.runFor(200)
  await page.getByRole('heading', { name: 'Workspace overview' }).hover()
  await page.clock.runFor(500); expect(s.bootstraps).toHaveLength(0)
  await hover(page, 'Alpha')
  await expect.poll(() => s.minted.length).toBe(1)
  await expect(page.locator('iframe[aria-hidden="true"]')).toHaveCount(0)
  expect(s.ui).toHaveLength(0)
  await row(page, 'Alpha').getByRole('link').click()
  await expect(page.frameLocator('iframe[title="/Alpha interface"]').getByRole('heading')).toHaveText('Agent interface')
  expect(s.foreground).toHaveLength(0); expect(s.minted).toHaveLength(1); expect(s.ui).toEqual([ids[0]])
})

test('prewarm is absent on an old Proxy', async ({ page }) => {
  const s = await setup(page, { capability: false })
  await hover(page, 'Alpha'); expect(s.bootstraps).toHaveLength(0)
})

test('prewarm skips Save-Data', async ({ page }) => {
  await page.addInitScript(() => Object.defineProperty(navigator, 'connection', { configurable: true, value: Object.assign(new EventTarget(), { saveData: true }) }))
  const s = await setup(page)
  await hover(page, 'Alpha'); expect(s.bootstraps).toHaveLength(0)
})

test('prewarm skips touch hover emulation', async ({ page }) => {
  const s = await setup(page)
  await page.evaluate(() => {
    const original = window.matchMedia.bind(window)
    window.matchMedia = query => query === '(hover: hover) and (pointer: fine)' ? { matches: false } as MediaQueryList : original(query)
  })
  await hover(page, 'Alpha'); expect(s.bootstraps).toHaveLength(0)
})

test('dragging across Tree rows never starts prewarm', async ({ page }) => {
  const s = await setup(page, { quiet: true })
  for (const name of ['Alpha', 'Beta', 'Gamma']) {
    await row(page, name).dispatchEvent('pointerover', { pointerType: 'mouse', buttons: 1 })
    await page.clock.runFor(500)
    await row(page, name).dispatchEvent('pointerout', { pointerType: 'mouse', buttons: 1 })
  }
  expect(s.bootstraps).toHaveLength(0)
  await expect(page.locator('iframe[aria-hidden="true"]')).toHaveCount(0)
})

test('rapid Tab and Shift+Tab stops do not warm; pointer focus is not keyboard intent', async ({ page }) => {
  const s = await setup(page, { quiet: true })
  for (const [key, name] of [['Tab', 'Alpha'], ['Shift+Tab', 'Beta'], ['Tab', 'Gamma']]) {
    await page.keyboard.press(key)
    await row(page, name).getByRole('link').focus()
    await page.clock.runFor(150)
  }
  await row(page, 'Delta').dispatchEvent('pointerdown', { pointerType: 'mouse', buttons: 1 })
  await row(page, 'Delta').getByRole('link').focus()
  await page.clock.runFor(500)
  expect(s.bootstraps).toHaveLength(0)
  await page.keyboard.press('Tab')
  await row(page, 'Epsilon').getByRole('link').focus()
  await page.clock.runFor(401)
  await expect(page.locator('iframe[aria-hidden="true"]')).toHaveCount(1)
})

test('click during a pending prewarm cancels its code and starts the normal foreground handshake', async ({ page }) => {
  let release!: () => void
  const held = new Promise<void>(resolve => { release = resolve })
  const s = await setup(page, { holdMint: held })
  await hover(page, 'Alpha')
  await expect.poll(() => s.minted.length).toBe(1)
  await row(page, 'Alpha').getByRole('link').click()
  await expect(page.locator('iframe[aria-hidden="true"]')).toHaveCount(0)
  await expect.poll(() => s.foreground.length).toBe(1)
  expect(s.ui).toHaveLength(0)
  release()
  await expect(page.frameLocator('iframe[title="/Alpha interface"]').getByRole('heading')).toHaveText('Agent interface')
  expect(s.ui).toEqual([ids[0]])
  expect(s.minted).toHaveLength(1); expect(s.foreground).toHaveLength(1)
})

test('prewarm skips an already-open tab', async ({ page }) => {
  const s = await setup(page)
  await row(page, 'Alpha').getByRole('link').click()
  await expect(page.frameLocator('iframe[title="/Alpha interface"]').getByRole('heading')).toHaveText('Agent interface')
  await hover(page, 'Alpha')
  expect(s.foreground).toHaveLength(1); expect(s.minted).toHaveLength(0); expect(s.bootstraps).toHaveLength(1)
})

test('hiding the document cancels frames and prevents another attempt', async ({ page }) => {
  const s = await setup(page, { quiet: true })
  await hover(page, 'Alpha'); await expect(page.locator('iframe[aria-hidden="true"]')).toHaveCount(1)
  await page.evaluate(() => {
    Object.defineProperty(document, 'hidden', { configurable: true, get: () => true })
    document.dispatchEvent(new Event('visibilitychange'))
  })
  await expect(page.locator('iframe[aria-hidden="true"]')).toHaveCount(0)
  await hover(page, 'Beta'); expect(s.bootstraps).toHaveLength(1); expect(s.minted).toHaveLength(0)
})

test('keyboard Tab focus can prewarm but Arrow navigation cannot', async ({ page }) => {
  const s = await setup(page, { quiet: true })
  await page.keyboard.press('Tab')
  await row(page, 'Alpha').getByRole('link').focus(); await page.clock.runFor(401)
  await expect(page.locator('iframe[aria-hidden="true"]')).toHaveCount(1)
  await page.keyboard.press('ArrowDown'); await page.clock.runFor(401)
  await expect(page.locator('iframe[aria-hidden="true"]')).toHaveCount(0)
  expect(s.bootstraps).toHaveLength(1)
})

test('timeout, explicit warm failure and authoritative Agent removal are quiet', async ({ page }) => {
  const s = await setup(page, { quiet: true })
  await hover(page, 'Alpha'); await expect(page.locator('iframe[aria-hidden="true"]')).toHaveCount(1)
  await page.clock.fastForward(15_001)
  await expect(page.locator('iframe[aria-hidden="true"]')).toHaveCount(0)
  await hover(page, 'Beta'); await expect(page.locator('iframe[aria-hidden="true"]')).toHaveCount(1)
  await report(page, 1, 'ready'); await report(page, 1, 'warm_failed')
  await expect(page.locator('iframe[aria-hidden="true"]')).toHaveCount(0)
  await hover(page, 'Gamma'); await expect(page.locator('iframe[aria-hidden="true"]')).toHaveCount(1)
  s.live.remove(ids[2])
  await expect(page.locator('iframe[aria-hidden="true"]')).toHaveCount(0)
  expect(s.minted).toHaveLength(0); expect(s.ui).toHaveLength(0)
  await expect(page.getByRole('alert')).toHaveCount(0)
})

test('keyboard logout cancels a pending prewarm mint before the logout response', async ({ page }) => {
  let release!: () => void, releaseLogout!: () => void
  const held = new Promise<void>(resolve => { release = resolve })
  const heldLogout = new Promise<void>(resolve => { releaseLogout = resolve })
  const s = await setup(page, { holdMint: held })
  await page.route('**/api/auth/logout', async r => { await heldLogout; return r.fulfill({ json: {} }).catch(() => {}) })
  await hover(page, 'Alpha'); await expect.poll(() => s.minted.length).toBe(1)
  await page.getByRole('button', { name: 'User menu', exact: true }).focus(); await page.keyboard.press('Enter')
  await page.getByRole('menuitem', { name: 'Log out', exact: true }).press('Enter')
  await expect(page.locator('iframe[aria-hidden="true"]')).toHaveCount(0)
  release(); await page.clock.runFor(100)
  expect(s.ui).toHaveLength(0)
  // An inventory update while logout is pending must not re-enable the hook.
  s.live.remove(ids[6])
  await expect(row(page, 'Eta')).toHaveCount(0)
  await hover(page, 'Beta'); expect(s.minted).toHaveLength(1)
  expect(s.bootstraps).toHaveLength(1)
  releaseLogout()
})

test('prewarm source/hash/state checks isolate two concurrent frames and clear them on cancellation', async ({ page }) => {
  const s = await setup(page, { quiet: true })
  // Dispatch hover intents independently to retain both slots, as pointer + Tab can.
  await row(page, 'Alpha').dispatchEvent('pointerover', { pointerType: 'mouse' }); await page.clock.runFor(401)
  await row(page, 'Beta').dispatchEvent('pointerover', { pointerType: 'mouse' }); await page.clock.runFor(401)
  await expect(page.locator('iframe[aria-hidden="true"]')).toHaveCount(2)
  const frames = page.locator('iframe[aria-hidden="true"]')
  for (const frame of await frames.all()) {
    await expect(frame).toBeHidden(); await expect(frame).toHaveAttribute('tabindex', '-1'); await expect(frame).toHaveAttribute('inert', '')
  }
  await row(page, 'Gamma').dispatchEvent('pointerover', { pointerType: 'mouse' }); await page.clock.runFor(401)
  expect(s.bootstraps).toHaveLength(2)
  await report(page, 0, 'warmed'); await report(page, 0, 'mint'); await report(page, 0, 'reused'); await report(page, 0, 'warm_failed')
  expect(s.minted).toHaveLength(0)
  await expect(frames).toHaveCount(2)
  await report(page, 0, 'ready')
  for (const kind of ['warmed', 'mint', 'warm_failed']) {
    await page.evaluate(({ hash, origin, kind }) => {
      const source = document.querySelectorAll<HTMLIFrameElement>('iframe[aria-hidden="true"]')[1].contentWindow
      const event = new MessageEvent('message', { origin, data: { treer_embed: kind, version: 2, nonce_hash: hash } })
      Object.defineProperty(event, 'source', { value: source }); dispatchEvent(event)
    }, { hash, origin: origins[0], kind })
    const other = page.frames().find(frame => frame.url().startsWith(origins[0]))!
    await other.evaluate(({ target, hash, kind }) => parent.postMessage({ treer_embed: kind, version: 2, nonce_hash: hash, extra: true }, target), { target: new URL(page.url()).origin, hash, kind })
  }
  await expect(frames).toHaveCount(2)
  await page.evaluate(hash => dispatchEvent(new MessageEvent('message', { origin: 'https://ag-00000000000000000000000000000001.agents.example.test', source: window, data: { treer_embed: 'mint', version: 2, nonce_hash: hash } })), hash)
  for (const kind of ['warmed', 'mint', 'warm_failed']) await report(page, 0, kind, 'c'.repeat(64))
  await expect(frames).toHaveCount(2); expect(s.minted).toHaveLength(0)
  await page.evaluate(() => {
    (window as unknown as { removedPrewarmSource: Window | null }).removedPrewarmSource = document.querySelector<HTMLIFrameElement>('iframe[aria-hidden="true"]')!.contentWindow
  })
  await row(page, 'Alpha').dispatchEvent('pointerout')
  await expect(frames).toHaveCount(1)
  await page.evaluate(({ hash, origin }) => {
    const old = window as unknown as { removedPrewarmSource?: Window | null }
    // WebKit rejects detached WindowProxy in the constructor; retain its exact
    // identity on a synthetic late event without crossing Playwright's serializer.
    const event = new MessageEvent('message', { origin, data: { treer_embed: 'mint', version: 2, nonce_hash: hash } })
    Object.defineProperty(event, 'source', { value: old.removedPrewarmSource })
    dispatchEvent(event); delete old.removedPrewarmSource
  }, { hash, origin: origins[0] })
  expect(s.minted).toHaveLength(0)
  await report(page, 1, 'ready'); await report(page, 1, 'mint')
  await expect.poll(() => s.minted.length).toBe(1)
  await expect(frames).toHaveCount(0)
  expect(s.minted).toEqual([ids[1]]); expect(s.ui).toHaveLength(0)
})

test('prewarm quota denial is silent and starts a one-minute cooldown', async ({ page }) => {
  const s = await setup(page, { denied: true })
  await hover(page, 'Alpha'); await expect.poll(() => s.minted.length).toBe(1)
  await expect(page.locator('iframe[aria-hidden="true"]')).toHaveCount(0)
  await row(page, 'Alpha').dispatchEvent('pointerout'); await hover(page, 'Alpha')
  expect(s.minted).toHaveLength(1)
  await expect(page.getByRole('button', { name: 'Load compatibility view (shared origin)' })).toHaveCount(0)
  await expect(page.getByRole('alert')).toHaveCount(0)
})

test('prewarm respects six background mints per minute and never retries automatically', async ({ page }) => {
  const s = await setup(page)
  for (const [index, name] of ['Alpha', 'Beta', 'Gamma', 'Delta', 'Epsilon', 'Zeta'].entries()) {
    await hover(page, name); await expect.poll(() => s.minted.length).toBe(index + 1)
    await expect(page.locator('iframe[aria-hidden="true"]')).toHaveCount(0)
  }
  await hover(page, 'Eta'); await expect.poll(() => s.bootstraps.length).toBe(7)
  await expect(page.locator('iframe[aria-hidden="true"]')).toHaveCount(0)
  expect(s.minted).toHaveLength(6)
  await page.clock.fastForward(60_001); expect(s.minted).toHaveLength(6)
})

test('a hover sweep is capped at ten hidden frame starts per minute independently of minting', async ({ page }) => {
  const s = await setup(page, { quiet: true, many: true })
  for (const [index, name] of ['Alpha', 'Beta', 'Gamma', 'Delta', 'Epsilon', 'Zeta', 'Eta', 'Theta', 'Iota', 'Kappa'].entries()) {
    await row(page, name).dispatchEvent('pointerover', { pointerType: 'mouse' }); await page.clock.runFor(401)
    await expect.poll(() => s.bootstraps.length).toBe(index + 1)
    await row(page, name).dispatchEvent('pointerout')
  }
  await row(page, 'Lambda').dispatchEvent('pointerover', { pointerType: 'mouse' }); await page.clock.runFor(401)
  expect(s.bootstraps).toHaveLength(10); expect(s.minted).toHaveLength(0)
  await expect(page.locator('iframe[aria-hidden="true"]')).toHaveCount(0)
  await row(page, 'Lambda').dispatchEvent('pointerout')
  await page.clock.fastForward(60_001)
  await row(page, 'Lambda').dispatchEvent('pointerover', { pointerType: 'mouse' }); await page.clock.runFor(401)
  await expect.poll(() => s.bootstraps.length).toBe(11)
})

for (const [width, theme, locale] of [[1360, 'light', 'en'], [1360, 'dark', 'zh'], [390, 'light', 'zh'], [390, 'dark', 'en']] as const) {
  test(`hidden prewarm preserves layout and focus: ${width} ${theme} ${locale}`, async ({ page }) => {
    await page.setViewportSize({ width, height: width === 390 ? 844 : 900 })
    await page.addInitScript(({ theme, locale }) => {
      localStorage.setItem('treer-theme', theme)
      localStorage.setItem('treer-lang', locale)
    }, { theme, locale })
    await setup(page, { quiet: true })
    if (width === 390) await page.getByRole('button', { name: locale === 'zh' ? '浏览树' : 'Browse tree', exact: true }).click()
    const before = await row(page, 'Alpha').boundingBox()
    await page.keyboard.press('Tab')
    await row(page, 'Alpha').getByRole('link').focus()
    await page.clock.runFor(401)
    const frame = page.locator('iframe[aria-hidden="true"]')
    await expect(frame).toHaveCount(1)
    await expect(frame).toBeHidden()
    await expect(row(page, 'Alpha').getByRole('link')).toBeFocused()
    expect(await row(page, 'Alpha').boundingBox()).toEqual(before)
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
    const directory = process.env.EMBED_PREWARM_SCREENSHOTS
    if (directory) {
      await mkdir(directory, { recursive: true, mode: 0o700 })
      await page.screenshot({ path: join(directory, `${width}-${theme}-${locale}.png`), fullPage: true })
    }
  })
}
