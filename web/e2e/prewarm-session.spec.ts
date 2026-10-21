import { test, expect, type Page } from '@playwright/test'

type Controls = { enter: (id: string, intent: 'pointer') => void; cancel: () => void; replaceSession: () => void; replaceWorkspace: () => void; disable: () => void }
declare global { interface Window { prewarmSession: Controls } }
const id = (i: number) => `ag_${i.toString(16).padStart(32, '0')}`
async function setup(page: Page, heldMint = false) {
  await page.clock.install()
  let mints = 0, deliveries = 0, release!: () => void
  const held = new Promise<void>(resolve => { release = resolve })
  await page.route('**/interface/embed-authorization', async route => {
    mints++
    if (heldMint) await held
    await route.fulfill({ json: { code: 'iec_v1_' + 'b'.repeat(64), origin: 'https://ag-00000000000000000000000000000001.agents.example.test', expires_in: 30 } }).catch(() => {})
  })
  await page.route('https://ag-*.agents.example.test/**', route => route.fulfill({ contentType: 'text/html', body: `<script>
    const hash='${'a'.repeat(64)}';
    addEventListener('message', e=>{
      if(e.source!==parent)return;
      if(e.data.treer_embed==='probe')parent.postMessage({treer_embed:'mint',version:2,nonce_hash:hash},e.origin);
      if(e.data.treer_embed==='code')fetch('/delivered',{method:'POST'});
    });
    parent.postMessage({treer_embed:'ready',version:2,nonce_hash:hash},'*');
  </script>` }))
  await page.route('**/delivered', route => { deliveries++; return route.fulfill({ status: 204 }) })
  await page.route('**/prewarm-session-harness', route => route.fulfill({ contentType: 'text/html', body: `<div id="root"></div><script type="module">
    import RefreshRuntime from '/@react-refresh';
    RefreshRuntime.injectIntoGlobalHook(window);
    window.$RefreshReg$ = () => {}; window.$RefreshSig$ = () => type => type;
    window.__vite_plugin_react_preamble_installed__ = true;
    const { mountPrewarmSession } = await import('/e2e/fixtures/prewarm-session.ts');
    mountPrewarmSession();
  </script>` }))
  await page.goto('/prewarm-session-harness')
  await expect(page.getByText('Synthetic prewarm session harness')).toBeVisible()
  await page.clock.pauseAt(new Date(Date.now() + 1000))
  return { mints: () => mints, deliveries: () => deliveries, release }
}
async function start(page: Page, i = 1) {
  await page.evaluate(id => window.prewarmSession.enter(id, 'pointer'), id(i))
  await page.clock.runFor(401)
}
test('same-user replacement session cancels a pending dwell', async ({ page }) => {
  const s = await setup(page)
  await page.evaluate(id => window.prewarmSession.enter(id, 'pointer'), id(1))
  await page.clock.runFor(200)
  await page.evaluate(() => window.prewarmSession.replaceSession())
  await page.clock.runFor(500)
  await expect(page.locator('iframe')).toHaveCount(0)
  expect(s.mints()).toBe(0)
})
test('same-user replacement session discards a late mint result', async ({ page }) => {
  const s = await setup(page, true)
  await start(page)
  await expect.poll(s.mints).toBe(1)
  await page.evaluate(() => window.prewarmSession.replaceSession())
  await expect(page.locator('iframe')).toHaveCount(0)
  s.release()
  await page.clock.runFor(200)
  expect(s.deliveries()).toBe(0)
})
test('logout cancellation persists across inventory renders but a new session can prewarm', async ({ page }) => {
  const s = await setup(page)
  await page.evaluate(() => window.prewarmSession.cancel())
  await page.evaluate(() => window.prewarmSession.replaceWorkspace())
  await start(page)
  expect(s.mints()).toBe(0)
  await page.evaluate(() => window.prewarmSession.replaceSession())
  await start(page)
  await expect.poll(s.mints).toBe(1)
})
test('capability removal cancels pending work and prevents new frames', async ({ page }) => {
  const s = await setup(page)
  await page.evaluate(id => window.prewarmSession.enter(id, 'pointer'), id(1))
  await page.evaluate(() => window.prewarmSession.disable())
  await page.clock.runFor(500)
  await start(page, 2)
  await expect(page.locator('iframe')).toHaveCount(0)
  expect(s.mints()).toBe(0)
})
