import { test, expect } from '@playwright/test'
import { createServer } from 'node:http'
import { installPrepareFetchObservation, observedOperationForDocument } from './controller-update-prepare-observer.mjs'
const id = '11111111-1111-4111-8111-111111111111'
const identity = (c: string) => ({ release_sha: c.repeat(40), artifact_source_sha: c.repeat(40), controller_sha256: c.repeat(64), cli_sha256: c.repeat(64) })
const operation = (revision = 1) => ({ operation_id: id, current: identity('a'), target: { target_id: 'new', identity: identity('b'), platform: 'linux-x86_64', version_label: 'new' }, state: 'succeeded', phase: 'complete', observation: 'available', observed_identity: identity('b'), restored_identity: null, health: 'healthy', receipt_revision: revision, created_at: '2026-10-25T01:00:00Z', updated_at: '2026-10-25T01:01:00Z', observed_at: '2026-10-25T01:01:00Z', finished_at: '2026-10-25T01:01:00Z', error_code: null })
const path = '/api/workspaces/ws/machines/m/controller-update/operations'
async function fixture() {
  let mode = 'normal', requests = 0, revision = 1
  const server = createServer((req, res) => {
    if (req.url !== path) { res.setHeader('content-type', 'text/html'); res.end('<!doctype html><title>Owned observer control</title>'); return }
    requests++
    res.writeHead(200, { 'content-type': 'application/json', 'cache-control': 'no-store' })
    const json = JSON.stringify({ ...operation(revision), token: 'DO_NOT_EXPORT', target: { ...operation(revision).target, secret: 'DO_NOT_EXPORT' } })
    if (mode === 'valid-prefix-stall') { res.write(json); setTimeout(() => res.end('   '), 600); return }
    if (mode === 'timeout') { res.write(json.slice(0, 10)); setTimeout(() => res.end(json.slice(10)), 150); return }
    res.end(mode === 'oversize' ? ' '.repeat(65537) + json : mode === 'malformed' ? '{' : json)
  })
  await new Promise<void>(resolve => server.listen(0, '127.0.0.1', resolve))
  const origin = `http://127.0.0.1:${(server.address() as any).port}`
  return { origin, requests: () => requests, set: (next: string, rev = 1) => { mode = next; revision = rev }, close: () => new Promise<void>(resolve => { server.closeAllConnections(); server.close(() => resolve()) }) }
}
for (const mode of ['normal', 'malformed', 'oversize', 'timeout']) test(`actual HTTP clone ${mode}: original response unchanged and one GET`, async ({ page }) => {
  const f = await fixture()
  try {
    f.set(mode)
    const observations: any[] = []
    await page.exposeBinding('__observe', ({ frame }, message) => { if (frame === page.mainFrame() && message.kind === 'operations') observations.push(message) })
    const options = { origin: f.origin, path: path.replace('/operations', '/prepare'), operationsPath: path, targetId: 'new', callback: '__observe', readDeadlineMs: 25 }
    await page.addInitScript(installPrepareFetchObservation, options)
    await page.addInitScript(installPrepareFetchObservation, options)
    await page.goto(f.origin)
    const original = await page.evaluate(async url => { const response = await fetch(url); return { status: response.status, body: await response.text() } }, f.origin + path)
    expect(original.status).toBe(200)
    if (mode === 'malformed') expect(original.body).toBe('{')
    else expect(original.body).toContain('DO_NOT_EXPORT')
    if (mode === 'oversize') expect(original.body.length).toBeGreaterThan(65536)
    await expect.poll(() => observations.length).toBe(1)
    expect(f.requests()).toBe(1)
    const message = observations[0]
    expect(JSON.stringify(message)).not.toContain('DO_NOT_EXPORT')
    if (mode === 'normal') expect(observedOperationForDocument(message, message.documentId, id)).toEqual(operation())
    else expect(message.value).toEqual({ status: 'body_unavailable' })
  } finally { await f.close() }
})
test('delayed pre-refresh bridge cannot replace fresh GET operation', async ({ page }) => {
  const f = await fixture()
  let releaseOld!: () => void
  const oldGate = new Promise<void>(resolve => { releaseOld = resolve })
  let firstSeen = false, accepted: any = null, discarded = 0
  try {
    await page.exposeBinding('__observe', async ({ frame }, message) => {
      if (frame !== page.mainFrame() || message.kind !== 'operations') return
      if (!firstSeen) { firstSeen = true; await oldGate }
      const current = await frame.evaluate(() => (window as any).__treerControllerObservationDocument)
      const op = observedOperationForDocument(message, current, id)
      if (op) accepted = op
      else discarded++
    })
    await page.addInitScript(installPrepareFetchObservation, { origin: f.origin, path: path.replace('/operations', '/prepare'), operationsPath: path, targetId: 'new', callback: '__observe' })
    await page.goto(f.origin)
    await page.evaluate(async url => { await (await fetch(url)).text() }, f.origin + path)
    await expect.poll(() => firstSeen).toBe(true)
    f.set('normal', 2)
    await page.reload()
    await page.evaluate(async url => { await (await fetch(url)).text() }, f.origin + path)
    await expect.poll(() => accepted?.receipt_revision).toBe(2)
    releaseOld()
    await expect.poll(() => discarded).toBe(1)
    expect(accepted).toEqual(operation(2))
    expect(f.requests()).toBe(2)
  } finally { releaseOld(); await f.close() }
})

test('bridge rejection does not reject or delay the original fetch', async ({ page }) => {
  const f = await fixture()
  try {
    let rejected = false
    const errors: string[] = []
    page.on('pageerror', error => errors.push(error.message))
    await page.exposeBinding('__observe', (_source, message) => { if (message.kind === 'operations') { rejected = true; throw Error('closed bridge refusal') } })
    await page.addInitScript(installPrepareFetchObservation, { origin: f.origin, path: path.replace('/operations', '/prepare'), operationsPath: path, targetId: 'new', callback: '__observe' })
    await page.goto(f.origin)
    const original = await page.evaluate(async url => (await fetch(url)).text(), f.origin + path)
    expect(original).toContain('DO_NOT_EXPORT')
    await expect.poll(() => rejected).toBe(true)
    expect(errors).toEqual([])
    expect(f.requests()).toBe(1)
  } finally { await f.close() }
})

test('complete valid JSON prefix stalled before EOF refuses by deadline before original settles', async ({ page }) => {
  const f = await fixture()
  try {
    f.set('valid-prefix-stall')
    const observations: any[] = []
    await page.exposeBinding('__observe', ({ frame }, message) => { if (frame === page.mainFrame() && message.kind === 'operations') observations.push(message) })
    await page.addInitScript(installPrepareFetchObservation, { origin: f.origin, path: path.replace('/operations', '/prepare'), operationsPath: path, targetId: 'new', callback: '__observe', readDeadlineMs: 25 })
    await page.goto(f.origin)
    await page.evaluate(url => { (window as any).originalRead = fetch(url).then(r => r.text()).then(body => { (window as any).originalBody = body }); (window as any).originalBody = null }, f.origin + path)
    await expect.poll(() => observations.length).toBe(1)
    expect(observations[0].value).toEqual({ status: 'body_unavailable' })
    expect(await page.evaluate(() => (window as any).originalBody)).toBeNull()
    const original = await page.evaluate(async () => { await (window as any).originalRead; return (window as any).originalBody })
    expect(original).toContain('DO_NOT_EXPORT')
    expect(original.endsWith('   ')).toBe(true)
    expect(observations).toHaveLength(1)
    expect(f.requests()).toBe(1)
  } finally { await f.close() }
})
