// Opt-in local native integration. No mocked API responses or saved browser state.
import assert from 'node:assert/strict'
import { readFile, stat, mkdir, writeFile } from 'node:fs/promises'
import { createServer } from 'node:http'
import { createHash } from 'node:crypto'
import { chromium, firefox, webkit } from 'playwright'

const [bindingPath, outputPath] = process.argv.slice(2)
assert.ok(bindingPath && outputPath, 'usage: node e2e/native-metrics-direct.mjs PRIVATE_BINDING OUTPUT_DIRECTORY')
assert.equal((await stat(bindingPath)).mode & 0o077, 0, 'binding must be private')
const supplied = JSON.parse(await readFile(bindingPath, 'utf8'))
const binding = { ...supplied, proxy_origin: supplied.proxy_origin ?? supplied.base,
  workspace_id: supplied.workspace_id ?? supplied.workspace,
  server_id: supplied.server_id ?? supplied.server,
  cookie: supplied.cookie ?? supplied.test_cookie }

const proxy = new URL(binding.proxy_origin)
assert.equal(proxy.protocol, 'http:')
assert.equal(proxy.hostname, '127.0.0.1')
assert.equal(proxy.origin, binding.proxy_origin)
assert.match(binding.source_sha, /^[a-f0-9]{40}$/)
for (const key of ['workspace_id', 'server_id']) assert.match(binding[key], /^[a-zA-Z0-9_-]{1,128}$/)
const suppliedSession = typeof binding.cookie === 'string'
if (suppliedSession) {
  assert.ok(/^[^=;\s]+=[^;\r\n]+$/.test(binding.cookie), 'invalid supplied session')
  // Read real discovery only when the fixture omitted presentation metadata.
  const discover = async path => {
    const response=await fetch(proxy.origin+path,{headers:{Cookie:binding.cookie},redirect:'error',signal:AbortSignal.timeout(10000)});
    assert.equal(response.status,200);return response.json();
  }
  if (!binding.organization_id) {
    const data=await discover('/api/organizations');
    for (const organization of data.organizations) {
      const workspaces=await discover('/api/workspaces?'+new URLSearchParams({organization_id:organization.organization_id}));
      if (workspaces.workspaces.some(w=>w.workspace_id===binding.workspace_id)) { binding.organization_id=organization.organization_id; break; }
    }
  }
  if (!binding.machine_name) {
    const data=await discover(`/api/workspaces/${binding.workspace_id}/snapshot`);
    const machine=data.servers.find(m=>m.server_id===binding.server_id);
    binding.machine_name=machine?.name ?? machine?.hostname;
  }
} else for (const key of ['email','password']) assert.equal(typeof binding[key],'string')
assert.match(binding.organization_id, /^[a-zA-Z0-9_-]{1,128}$/)
assert.equal(typeof binding.machine_name,'string')
const uiOrigin = 'http://127.0.0.1:19485'
if (suppliedSession) {
  const check=await fetch(proxy.origin+'/api/auth/me',{headers:{Cookie:binding.cookie,Origin:uiOrigin},redirect:'error',signal:AbortSignal.timeout(10000)});
  assert.equal(check.status,200);
  assert.ok(check.headers.get('access-control-allow-origin')===uiOrigin && check.headers.get('access-control-allow-credentials')==='true','native fixture must allow the exact static UI origin with credentials');
}
const html = await readFile(new URL('../dist/index.html', import.meta.url))
const artifactHash = createHash('sha256').update(html).digest('hex')
await mkdir(outputPath, { recursive: true, mode: 0o700 })
const server = createServer((request, response) => {
  response.setHeader('cache-control', 'no-store')
  if (request.url === '/config.json') {
    response.setHeader('content-type', 'application/json')
    response.end(JSON.stringify({ proxy_url: proxy.origin }))
  } else {
    response.setHeader('content-type', 'text/html; charset=utf-8')
    response.end(html)
  }
})
await new Promise((resolve, reject) => { server.once('error', reject); server.listen(19485, '127.0.0.1', resolve) })
const results = []
try {
  for (const [engine, launcher] of Object.entries({ chromium, firefox, webkit })) {
    let stage = 'launch', browser, context, deadline, loggedIn = false, cleanup401 = false
    const ranges = [], forbidden = []
    const run = async () => {
      browser = await launcher.launch({ headless: true })
      context = await browser.newContext({ viewport: { width: 1360, height: 900 }, locale: 'en-US' })
      // Transport guard only: every allowed request still reaches the actual server.
      await context.route('**/*', route => {
        const request = route.request(), url = new URL(request.url())
        const allowedOrigin = [uiOrigin, proxy.origin].includes(url.origin)
        const allowedMethod = ['GET', 'HEAD', 'OPTIONS'].includes(request.method()) ||
          (url.origin === proxy.origin && request.method() === 'POST' && ['/api/auth/login', '/api/auth/logout'].includes(url.pathname))
        if (allowedOrigin && allowedMethod) return route.continue()
        forbidden.push({ method: request.method(), approved_origin: allowedOrigin })
        return route.abort()
      })
      const page = await context.newPage()
      page.setDefaultTimeout(15000)
      stage = 'login'
      if (suppliedSession) {
        const split=binding.cookie.indexOf('=');
        await context.addCookies([{name:binding.cookie.slice(0,split),value:binding.cookie.slice(split+1),url:proxy.origin,httpOnly:true,sameSite:'Lax'}]);
        assert.equal((await context.request.get(proxy.origin+'/api/auth/me')).status(),200);
      } else {
      await page.goto(uiOrigin, { waitUntil: 'domcontentloaded' })
      await page.locator('#email').fill(binding.email)
      await page.locator('#password').fill(binding.password)
      const login = page.waitForResponse(r => r.url() === proxy.origin + '/api/auth/login' && r.request().method() === 'POST')
      await page.locator('form button[type=submit]').click()
      assert.equal((await login).status(), 200)
      loggedIn = true
      }
      stage = 'workspace'
      await page.goto(`${uiOrigin}/orgs/${binding.organization_id}/workspaces/${binding.workspace_id}`, { waitUntil: 'domcontentloaded' })
      await page.getByRole('button', { name: 'Workspace settings', exact: true }).click()
      const path = `/api/workspaces/${binding.workspace_id}/machines/${binding.server_id}/metrics`
      const responseFor = minutes => page.waitForResponse(r => {
        const url = new URL(r.url())
        return url.origin === proxy.origin && url.pathname === path && r.request().method() === 'GET' &&
          (Date.parse(url.searchParams.get('to')) - Date.parse(url.searchParams.get('from'))) / 60000 === minutes
      }, { timeout: 20000 })
      let response = responseFor(15)
      await page.getByRole('button').filter({ hasText: binding.machine_name }).first().click()
      const panel = page.getByTestId('machine-metrics')
      for (const [label, minutes] of [['15m', 15], ['1h', 60], ['6h', 360], ['24h', 1440]]) {
        stage = `range_${minutes}`
        if (minutes !== 15) { response = responseFor(minutes); await panel.getByRole('button', { name: label, exact: true }).click() }
        stage = `response_${minutes}`
        const actual = await response
        stage = `status_${minutes}_${actual.status()}`
        assert.equal(actual.status(), 200)
        stage = `cache_${minutes}`
        assert.equal(actual.headers()['cache-control'], 'no-store')
        stage = `body_${minutes}`
        const body = await actual.body()
        stage = `json_${minutes}`
        const data = JSON.parse(body.toString())
        stage = `identity_${minutes}`
        assert.equal(data.server_id, binding.server_id)
        stage = `schema_${minutes}`
        assert.equal(data.schema, 'machine.metrics.v1')
        stage = `max_points_${minutes}`
        assert.equal(new URL(actual.url()).searchParams.get('max_points'), '360')
        stage = `samples_${minutes}`
        assert.ok(data.series.find(s => s.kind === 'cpu_utilization').points.some(p => p.valid_count > 0))
        stage = `render_${minutes}`
        await panel.getByRole('button', { name: 'Refresh', exact: true }).waitFor({ state: 'visible' })
        await page.waitForFunction(() => !document.querySelector('[data-testid=machine-metrics] button:last-child')?.disabled)
        await panel.getByRole('heading', { name: 'CPU', exact: true }).waitFor()
        await panel.getByRole('heading', { name: 'RAM', exact: true }).waitFor()
        assert.equal(await panel.getByRole('alert').count(), 0)
        // Long native ranges are mostly gaps; partial samples render as dots,
        // deliberately not connected lines across unavailable history.
        stage = `chart_${minutes}`
        const cpu = panel.getByRole('figure', { name: 'CPU utilization', exact: true })
        assert.ok(await cpu.locator('circle[data-partial]').count() > 0)
        ranges.push({ minutes, status: 200, no_store: true, response_sha256: createHash('sha256').update(body).digest('hex') })
      }
      assert.equal(forbidden.length, 0)
      stage = 'complete'
    }
    try {
      await Promise.race([run(), new Promise((_, reject) => { deadline = setTimeout(() => reject(new Error('deadline')), 120000) })])
    } catch { /* Report fixed stage only; errors may contain credentials or private DOM. */ }
    finally {
      clearTimeout(deadline)
      if (loggedIn && context) {
        try {
          const logout = await context.request.post(proxy.origin + '/api/auth/logout', { headers: { Origin: uiOrigin }, timeout: 10000 })
          const me = await context.request.get(proxy.origin + '/api/auth/me', { timeout: 10000 })
          cleanup401 = logout.status() === 200 && me.status() === 401
        } catch { /* Unknown cleanup remains false. */ }
      }
      await context?.close().catch(() => {})
      await browser?.close().catch(() => {})
    }
    results.push({ engine, passed: stage === 'complete' && (suppliedSession || cleanup401), stage, ranges, forbidden_count: forbidden.length, session_cleanup_401: suppliedSession ? null : cleanup401, session_cleanup_owner: suppliedSession ? 'fixture owner; supplied session not revoked' : 'browser runner' })
    await writeFile(`${outputPath}/${engine}.json`, JSON.stringify(results.at(-1), null, 2) + '\n', { mode: 0o600, flag: 'wx' })
  }
} finally { await new Promise(resolve => server.close(resolve)) }
const result = { source_sha: binding.source_sha, ui_artifact_sha256: artifactHash, api_mocking: false, production_changed: false, results }
await writeFile(`${outputPath}/result.json`, JSON.stringify(result, null, 2) + '\n', { mode: 0o600, flag: 'wx' })
console.log(JSON.stringify({ passed: results.filter(r => r.passed).length, total: 3, output_written: true }))
if (results.length !== 3 || results.some(r => !r.passed)) process.exitCode = 1
