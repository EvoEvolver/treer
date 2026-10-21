// Local, opt-in diagnostic for #198. No application telemetry or response mocking.
// Configuration and session files are supplied by the canary fixture operator.
import { createRequire } from 'node:module'
import { readFileSync, writeFileSync } from 'node:fs'
import { createHash } from 'node:crypto'

const need = (key) => process.env[key] || (() => { throw new Error(`Missing ${key}`) })()
const { chromium, firefox, webkit } = createRequire(need('EMBED_PLAYWRIGHT_FROM'))('playwright')
const consoleOrigin = new URL(need('EMBED_CONSOLE_URL')).origin
const proxyOrigin = new URL(need('EMBED_PROXY_URL')).origin
const agentUrl = new URL(need('EMBED_AGENT_URL'))
const agentOrigin = new URL(need('EMBED_AGENT_ORIGIN')).origin
if (agentUrl.origin !== consoleOrigin || !agentOrigin.startsWith('https://')) throw new Error('Invalid fixture origins')
const suffixes = need('EMBED_ALLOWED_HOST_SUFFIXES').split(',')
if (suffixes.some((s) => s.split('.').length < 3 || s.length > 253 || !s.split('.').every((label) => /^[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?$/.test(label)))) throw new Error('Invalid fixture host allowlist')
const agentHost = new URL(agentOrigin).hostname
if (!suffixes.some((s) => agentHost.endsWith(`.${s}`) && /^ag-[0-9a-f]{32}$/.test(agentHost.slice(0, -(s.length + 1))))) throw new Error('Agent host is outside the fixture allowlist')
if (agentHost !== need('EMBED_FIXTURE_AGENT_HOST')) throw new Error('Agent host does not match the disposable fixture')
const deployedSha = need('EMBED_DEPLOYED_SHA')
if (!/^[0-9a-f]{40}$/.test(deployedSha)) throw new Error('Invalid deployed SHA')
const engines = (process.env.EMBED_ENGINES || 'chromium,firefox,webkit').split(',')
const pairs = Number(process.env.EMBED_PAIRS || 3)
if (!Number.isInteger(pairs) || pairs < 1 || pairs > 5) throw new Error('EMBED_PAIRS must be 1..5')
const stages = new Set(['frame-created', 'bootstrap-script', 'bootstrap-loaded', 'ready-received', 'code-received', 'ui-first-paint', 'ui-first-contentful-paint'])
const report = {
  schema: 1, deployed_sha: deployedSha,
  runner_sha256: createHash('sha256').update(readFileSync(new URL(import.meta.url))).digest('hex'),
  caveats: ['Cold means a fresh browser process/context; OS DNS and upstream caches are not flushed.',
    'Warm reload reuses the same context, cookie jar and connection pool.',
    'Injected fixed-label marks are local measurement only; paint entries may be unavailable in an engine.',
    'UI-visible is the fixture Message textbox visibility, not a substitute for an unavailable paint entry.'],
  runs: [],
}

function endpoint(request) {
  const u = new URL(request.url())
  if (u.origin === consoleOrigin && request.isNavigationRequest() && request.frame() === request.frame().page().mainFrame()) return 'console-document'
  if (u.origin === agentOrigin) {
    if (u.pathname === '/.treer/embed') return 'bootstrap'
    if (u.pathname === '/.treer/embed/session') return 'session'
    if (u.pathname === '/.treer/embed/check') return 'check'
    if (u.pathname === '/' && request.isNavigationRequest()) return 'ui-document'
    if (u.pathname === '/api/state') return 'ui-state'
  }
  if (u.origin === proxyOrigin) {
    if (request.method() === 'POST' && /\/interface\/embed-authorization$/.test(u.pathname)) return 'mint'
    if (u.pathname.endsWith('/snapshot')) return 'inventory'
    if (u.pathname.endsWith('/apps')) return 'apps'
    if (u.pathname === '/api/auth/config') return 'capabilities'
  }
  return null
}

async function measure(temperature, pair, context, version, engine) {
  const page = await context.newPage()
  const marks = [], network = [], requests = new Map(), entries = new Map()
  await page.exposeBinding('__treerEmbedTiming', ({ frame }, stage, epoch) => {
    if (!stages.has(stage) || !Number.isFinite(epoch)) return
    const expected = stage === 'frame-created' || stage === 'ready-received' ? consoleOrigin : agentOrigin
    try { if (new URL(frame.url()).origin !== expected) return } catch { return }
    marks.push({ stage, epoch_ms: epoch })
  })
  await page.addInitScript(({ consoleOrigin, agentOrigin }) => {
    const isConsole = location.origin === consoleOrigin
    const isAgent = location.origin === agentOrigin
    if (!isConsole && !isAgent) return
    const mark = (stage, time = performance.now()) => {
      performance.mark(`treer-embed:${stage}`, { startTime: time })
      void window.__treerEmbedTiming(stage, performance.timeOrigin + time).catch(() => {})
    }
    if (isConsole) {
      const observer = new MutationObserver((mutations) => {
        for (const m of mutations) for (const node of m.addedNodes) {
          if (!(node instanceof Element)) continue
          const frames = node.matches('iframe') ? [node] : node.querySelectorAll('iframe')
          for (const frame of frames) {
            try { if (new URL(frame.src).origin === agentOrigin) { mark('frame-created'); observer.disconnect(); return } } catch { /* unrelated frame */ }
          }
        }
      })
      observer.observe(document, { childList: true, subtree: true })
      window.addEventListener('message', (event) => {
        if (event.origin === agentOrigin && event.data?.treer_embed === 'ready') mark('ready-received')
      })
    } else if (location.pathname === '/.treer/embed') {
      mark('bootstrap-script')
      document.addEventListener('DOMContentLoaded', () => mark('bootstrap-loaded'), { once: true })
      window.addEventListener('message', (event) => {
        if (event.origin === consoleOrigin && event.source === parent && event.data?.treer_embed === 'code') mark('code-received')
      })
    } else if (location.pathname === '/') {
      try {
        new PerformanceObserver((list) => {
          for (const entry of list.getEntries()) {
            if (entry.name === 'first-paint') mark('ui-first-paint', entry.startTime)
            if (entry.name === 'first-contentful-paint') mark('ui-first-contentful-paint', entry.startTime)
          }
        }).observe({ type: 'paint', buffered: true })
      } catch { /* report missing marks honestly */ }
    }
  }, { consoleOrigin, agentOrigin })
  page.on('request', (request) => { const name = endpoint(request); if (name) requests.set(request, name) })
  const record = (request, status) => {
    if (!requests.has(request)) return
    const timing = request.timing()
    // Record response headers arrival even when a browser omits the finished
    // event for a bodyless 204; unavailable responseEnd remains -1.
    const value = { endpoint: requests.get(request), status,
      start_epoch_ms: timing.startTime, dns_start_ms: timing.domainLookupStart,
      dns_end_ms: timing.domainLookupEnd, connect_start_ms: timing.connectStart,
      tls_start_ms: timing.secureConnectionStart, connect_end_ms: timing.connectEnd,
      request_start_ms: timing.requestStart, response_start_ms: timing.responseStart,
      response_end_ms: timing.responseEnd }
    if (entries.has(request)) Object.assign(entries.get(request), value)
    else { entries.set(request, value); network.push(value) }
  }
  page.on('response', (response) => record(response.request(), response.status()))
  page.on('requestfinished', (request) => {
    if (entries.has(request)) record(request, entries.get(request).status)
  })
  const run = { engine, version, pair, temperature, marks, network, success: false }
  report.runs.push(run)
  try {
    await page.goto(agentUrl.href, { waitUntil: 'domcontentloaded', timeout: 60000 })
    const frame = page.frameLocator('iframe[title$=" interface"]')
    await frame.getByRole('textbox', { name: /Message/ }).waitFor({ state: 'visible', timeout: 45000 })
    run.ui_visible_epoch_ms = await page.evaluate(() => performance.timeOrigin + performance.now())
    const actual = page.frames().find((f) => { try { return new URL(f.url()).origin === agentOrigin && new URL(f.url()).pathname === '/' } catch { return false } })
    if (!actual) throw new Error('fixture-not-isolated')
    await actual.locator('header.top').waitFor({ state: 'visible', timeout: 10000 })
    run.success = true
  } catch { run.failure = 'fixture-did-not-reach-isolated-ui' }
  const start = network.find((n) => n.endpoint === 'bootstrap')?.start_epoch_ms
  const consoleStart = network.find((n) => n.endpoint === 'console-document')?.start_epoch_ms
  if (Number.isFinite(start)) {
    run.marks = marks.map(({ stage, epoch_ms }) => ({ stage, ms: Math.round((epoch_ms - start) * 10) / 10 }))
    run.ui_visible_ms = Math.round((run.ui_visible_epoch_ms - start) * 10) / 10
    run.network = network.map(({ start_epoch_ms, ...rest }) => ({ ...rest, start_ms: Math.round((start_epoch_ms - start) * 10) / 10 }))
    if (Number.isFinite(consoleStart)) {
      run.console_to_bootstrap_ms = Math.round((start - consoleStart) * 10) / 10
      run.console_to_ui_visible_ms = Math.round((run.ui_visible_epoch_ms - consoleStart) * 10) / 10
    }
  } else { run.marks = []; run.network = []; run.failure = 'bootstrap-not-observed'; run.success = false }
  delete run.ui_visible_epoch_ms
  await page.close()
}

for (const engine of engines) {
  const type = { chromium, firefox, webkit }[engine]
  if (!type) throw new Error('Unsupported engine')
  for (let pair = 1; pair <= pairs; pair++) {
    const browser = await type.launch()
    try {
      const context = await browser.newContext({ storageState: need('EMBED_STORAGE_STATE'), locale: 'en-US', viewport: { width: 1360, height: 900 } })
      await measure('cold-browser', pair, context, browser.version(), engine)
      await measure('warm-context', pair, context, browser.version(), engine)
      await context.close()
    } finally { await browser.close() }
  }
}
writeFileSync(need('EMBED_REPORT_FILE'), JSON.stringify(report, null, 2) + '\n', { mode: 0o600 })
console.log(JSON.stringify({ runs: report.runs.length, passed: report.runs.filter((r) => r.success).length }))
if (report.runs.some((r) => !r.success)) process.exitCode = 1
