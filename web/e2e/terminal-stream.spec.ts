import { test, expect, type Page } from '@playwright/test'
import { fixture, gotoWorkspace, open } from './fixtures/workspace-tabs'
import { TerminalBackend, browserBuffer, expectedBuffer, split, workload } from './fixtures/terminal-proxy'

const scope = '#workspace-panel-tab-0 .xterm'

async function attach(page: Page, backend: TerminalBackend, mobile = false) {
  await fixture(page)
  await backend.route(page)
  await gotoWorkspace(page)
  await open(page, 'Terminal', mobile)
  await expect(page.locator(scope)).toBeVisible()
  await expect.poll(() => backend.connections.length).toBe(1)
  const url = backend.connections[0]
  return { cols: Number(url.searchParams.get('cols')), rows: Number(url.searchParams.get('rows')) }
}

/** Busy-wait the page main thread, like a slow phone or a backgrounded tab. */
function jank(page: Page, ms: number) {
  return page.evaluate((ms) => { const end = performance.now() + ms; while (performance.now() < end) { /* busy */ } }, ms)
}

async function settled(page: Page, backend: TerminalBackend) {
  for (const deadline = Date.now() + 30_000; !backend.idle;) {
    if (Date.now() > deadline) throw new Error(`terminal never settled: ${JSON.stringify(backend.stats)}`)
    await page.waitForTimeout(50)
  }
  await page.evaluate(() => new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve))))
}

async function expectScreen(page: Page, backend: TerminalBackend, size: { cols: number; rows: number }) {
  // Geometry must not change mid-stream for a byte-exact comparison.
  expect(backend.resizes.every((resize) => resize.cols === size.cols && resize.rows === size.rows)).toBe(true)
  const expected = await expectedBuffer(Buffer.concat(backend.produced), size.cols, size.rows)
  const actual = await browserBuffer(page, scope, expected.lines.length)
  expect(actual.rows).toBe(size.rows)
  expect(actual.screen).toEqual(expected.screen)
  expect(actual.lines).toEqual(expected.lines)
}

for (const mobile of [false, true]) {
  test.describe(mobile ? 'mobile' : 'desktop', () => {
    test.use(mobile ? { viewport: { width: 390, height: 844 }, hasTouch: true, isMobile: true } : { viewport: { width: 1280, height: 900 } })

    for (const [frames, maxChunk] of [['small', 600], ['large', 8192]] as const) {
      test(`heavy output in ${frames} frames stays byte-exact through backpressure and reconnects`, async ({ page }) => {
        test.setTimeout(120_000)
        // A tiny Proxy queue forces the overflow, detach and resume path.
        const backend = new TerminalBackend(32)
        const chunks = split(workload(mobile ? 2 : 1, 24), 7, maxChunk)
        const history = chunks.length >> 4
        for (const chunk of chunks.slice(0, history)) backend.push(chunk)
        const size = await attach(page, backend, mobile)
        // Long stalls (slow phone, background tab) while output keeps
        // arriving: a burst lands during the first, the rest streams.
        const rest = chunks.slice(history)
        const burst = frames === 'small' ? 1_500 : rest.length
        const stall = jank(page, 1_500)
        await new Promise((resolve) => setTimeout(resolve, 100))
        for (const chunk of rest.slice(0, burst)) backend.push(chunk)
        const streaming = backend.stream(rest.slice(burst), 8, 4)
        await stall
        await page.waitForTimeout(200)
        await jank(page, 800)
        await streaming
        await settled(page, backend)
        // The stall must exercise backpressure: thousands of small frames held
        // at once, or a full flow window plus a Proxy queue overflow that
        // detaches the session and resumes it from the cursor.
        if (frames === 'small') expect(backend.maxUnackedFrames).toBeGreaterThan(1024)
        else expect([backend.windowStalls > 0, backend.detaches > 0]).toEqual([true, true])
        expect(backend.browserCloses).toEqual([])
        await expectScreen(page, backend, size)
      })
    }

    test('attaching to a long history converges while output continues', async ({ page }) => {
      test.setTimeout(90_000)
      const backend = new TerminalBackend()
      // Every attach finds the browser busy for a while (a slow phone parsing
      // the replay): the flow window stays full while live output queues. The
      // Proxy queue must absorb that instead of detaching and replaying again.
      backend.attachStallMs = 800
      const chunks = split(workload(4, 26), 13, 4096)
      const history = Math.floor(chunks.length * 0.8)
      for (const chunk of chunks.slice(0, history)) backend.push(chunk)
      const streaming = (async () => {
        while (backend.connections.length === 0) await new Promise((resolve) => setTimeout(resolve, 5))
        await backend.stream(chunks.slice(history), 1, 8)
      })()
      const size = await attach(page, backend, mobile)
      await streaming
      await settled(page, backend)
      expect(backend.windowStalls).toBeGreaterThan(0)
      expect(backend.detaches).toBe(0)
      await expectScreen(page, backend, size)
    })

    test('mid-session resync resets in order with queued output', async ({ page }) => {
      test.setTimeout(60_000)
      const backend = new TerminalBackend()
      const chunks = split(workload(3, 6), 11)
      const half = chunks.length >> 1
      const size = await attach(page, backend, mobile)
      // While the page is busy, output (ending inside an OSC title) and a gap
      // resync queue up: the reset must apply after the earlier output parses
      // and must leave the parser in the ground state.
      const busy = jank(page, 1_000)
      await new Promise((resolve) => setTimeout(resolve, 100))
      for (const chunk of chunks.slice(0, half)) backend.push(chunk)
      backend.push(Buffer.from('\x1b]0;title cut by the resync'))
      backend.resync()
      for (const chunk of chunks.slice(half)) backend.push(chunk)
      await busy
      await settled(page, backend)
      await expectScreen(page, backend, size)
    })
  })
}
