// Real local Core+Controller+App chain. Supplied human session remains fixture-owned.
import assert from 'node:assert/strict'
import { readFile, stat, mkdir, writeFile } from 'node:fs/promises'
import { createServer } from 'node:http'
import { createHash } from 'node:crypto'
import { chromium, firefox, webkit } from 'playwright'

const [bindingPath, outputPath] = process.argv.slice(2)
assert.ok(bindingPath && outputPath, 'usage: node e2e/app-tree-direct.mjs PRIVATE_BINDING OUTPUT_DIRECTORY')
assert.equal((await stat(bindingPath)).mode & 0o077, 0)
const binding = JSON.parse(await readFile(bindingPath, 'utf8'))
const proxy = new URL(binding.proxy_origin)
assert.equal(proxy.origin, binding.proxy_origin)
assert.equal(proxy.protocol, 'http:'); assert.equal(proxy.hostname, '127.0.0.1')
assert.match(binding.source_sha, /^[a-f0-9]{40}$/)
for (const key of ['workspace_id', 'organization_id', 'app_id', 'owner_id']) assert.match(binding[key], /^[A-Za-z0-9_-]{1,128}$/)
assert.match(binding.cookie, /^[^=;\s]+=[^;\r\n]+$/)
const uiOrigin = 'http://127.0.0.1:54979'
const html = await readFile(new URL('../dist/index.html', import.meta.url))
const artifactHash = createHash('sha256').update(html).digest('hex')
await mkdir(outputPath, { recursive: true, mode: 0o700 })
const server = createServer((request, response) => {
  response.setHeader('cache-control', 'no-store')
  response.setHeader('content-type', request.url === '/config.json' ? 'application/json' : 'text/html; charset=utf-8')
  response.end(request.url === '/config.json' ? JSON.stringify({ proxy_url: proxy.origin }) : html)
})
await new Promise((resolve, reject) => { server.once('error', reject); server.listen(54979, '127.0.0.1', resolve) })
const results = []
try {
  for (const [engine, launcher] of Object.entries({ chromium, firefox, webkit })) {
    let stage = 'launch', browser, context, deadline, completedSends = 0
    const observedPaths = new Set(), messageStatuses = [], forbidden = [], sends = []
    try {
      await Promise.race([(async () => {
        browser = await launcher.launch({ headless: true })
        context = await browser.newContext({ viewport: { width: 1280, height: 900 }, locale: 'en-US' })
        await context.route('**/*', route => {
          const request = route.request(), url = new URL(request.url())
          const origin = [uiOrigin, proxy.origin].includes(url.origin)
          const method = ['GET', 'HEAD', 'OPTIONS'].includes(request.method()) || (url.origin === proxy.origin && request.method() === 'POST' && url.pathname === `/api/workspaces/${binding.workspace_id}/messages`)
          if (origin && method) return route.continue()
          forbidden.push(request.method()); return route.abort()
        })
        const split = binding.cookie.indexOf('=')
        await context.addCookies([{ name: binding.cookie.slice(0, split), value: binding.cookie.slice(split + 1), url: proxy.origin, httpOnly: true, sameSite: 'Lax' }])
        const page = await context.newPage(); page.setDefaultTimeout(15000)
        page.on('request', request => {
          if (request.method() === 'POST' && new URL(request.url()).pathname === `/api/workspaces/${binding.workspace_id}/messages`) sends.push(request.postDataJSON())
        })
        page.on('response', response => {
          const url = new URL(response.url())
          if (url.pathname === `/api/workspaces/${binding.workspace_id}/apps/${binding.app_id}/ui/api/tree` && response.status() === 200) observedPaths.add(url.searchParams.get('path'))
          if (url.pathname === `/api/workspaces/${binding.workspace_id}/messages`) messageStatuses.push(response.status())
        })
        stage = 'workspace'
        await page.goto(`${uiOrigin}/orgs/${binding.organization_id}/workspaces/${binding.workspace_id}`, { waitUntil: 'domcontentloaded' })
        const owner = page.locator(`[role=treeitem][data-owner-app="${binding.app_id}"]`)
        stage = 'root_tree'; await owner.locator('[data-app-subtree=""]').getByRole('button', { name: 'App contents', exact: true }).first().click()
        stage = 'nested_team'; await owner.locator('[data-app-subtree="team"]').getByRole('button', { name: 'App contents', exact: true }).first().click()
        stage = 'nested_tools'; await owner.locator('[data-app-subtree="team/tools"]').getByRole('button', { name: 'App contents', exact: true }).first().click()
        stage = 'leaf'; await owner.getByRole('link', { name: 'Echo App-managed Agent', exact: true }).click()
        await page.getByRole('heading', { name: 'Echo', exact: true }).waitFor()
        assert.ok(['', 'team', 'team/tools'].every(path => observedPaths.has(path)))
        const replies = []
        for (const turn of [1, 2]) {
          stage = `send_${turn}`
          const body = `treer230-local-${engine}-${Date.now()}-${turn}`
          await page.getByLabel('Message', { exact: true }).fill(body)
          await page.getByRole('button', { name: 'Send message', exact: true }).click()
          stage = `actual_reply_${turn}`
          const reply = `[app-hub:reply:v1] [team/tools/agent1] ${body}`
          await page.getByText(reply, { exact: true }).waitFor({ timeout: 30000 })
          replies.push(reply); completedSends++
        }
        assert.equal(sends.length, 2)
        assert.ok(Array.isArray(sends[1].context_ids) && sends[1].context_ids.length > 0)
        assert.notEqual(sends[0].idempotency_key, sends[1].idempotency_key)
        assert.ok(await page.locator('[data-app-message-sender]').filter({ hasText: ' / team/tools/agent1' }).count() > 0)
        stage = 'refresh_recovery'; await page.reload()
        for (const reply of replies) await page.getByText(reply, { exact: true }).waitFor()
        assert.equal(await page.locator('iframe,.xterm').count(), 0)
        stage = 'mobile'; await page.setViewportSize({ width: 390, height: 844 })
        await page.getByRole('heading', { name: 'Echo', exact: true }).waitFor()
        assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth))
        assert.ok(messageStatuses.length >= 3 && messageStatuses.every(status => status === 200))
        assert.equal(forbidden.length, 0)
        stage = 'complete'
      })(), new Promise((_, reject) => { deadline = setTimeout(() => reject(new Error('deadline')), 120000) })])
    } catch { /* Fixed stages only: do not copy private DOM, URLs or session errors. */ }
    finally { clearTimeout(deadline); await context?.close().catch(() => {}); await browser?.close().catch(() => {}) }
    results.push({ engine, passed: stage === 'complete', stage, consecutive_replies: completedSends, contextual_followup: sends.length === 2 && Array.isArray(sends[1].context_ids) && sends[1].context_ids.length > 0, two_nested_apps: observedPaths.has('team') && observedPaths.has('team/tools'), message_statuses: messageStatuses, forbidden_count: forbidden.length, session_cleanup_owner: 'fixture' })
    await writeFile(`${outputPath}/${engine}.json`, JSON.stringify(results.at(-1), null, 2) + '\n', { mode: 0o600, flag: 'wx' })
  }
} finally { await new Promise(resolve => server.close(resolve)) }
await writeFile(`${outputPath}/result.json`, JSON.stringify({ source_sha: binding.source_sha, ui_artifact_sha256: artifactHash, api_mocking: false, production_changed: false, results }, null, 2) + '\n', { mode: 0o600, flag: 'wx' })
console.log(JSON.stringify({ passed: results.filter(result => result.passed).length, total: 3, output_written: true }))
if (results.some(result => !result.passed)) process.exitCode = 1
