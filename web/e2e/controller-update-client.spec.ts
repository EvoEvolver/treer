import { test, expect, type Page } from '@playwright/test'
const identity = (c: string) => ({ release_sha: c.repeat(40), artifact_source_sha: c.repeat(40), controller_sha256: c.repeat(64), cli_sha256: c.repeat(64) })
const current = identity('a'), target = { target_id: 'target-b', identity: identity('b'), platform: 'darwin-aarch64', version_label: '1.1.0' }
const operationId = '11111111-1111-4111-8111-111111111111'
const token = 'CONFIRMATION_SECRET_NOT_DISPLAYED_123456'
const catalog = () => ({ support: 'supported', availability: 'ready', current, observed_at: '2026-10-20T23:00:00Z', platform: target.platform, catalog_revision: 1, targets: [target] })
const operation = (state = 'accepted') => ({ operation_id: operationId, current, target, state, phase: state === 'succeeded' ? 'complete' : null, observation: 'available', observed_identity: state === 'succeeded' ? target.identity : null, restored_identity: null, health: state === 'succeeded' ? 'healthy' : 'unknown', receipt_revision: state === 'succeeded' ? 1 : null, created_at: '2026-10-20T23:00:00Z', updated_at: '2026-10-20T23:01:00Z', observed_at: state === 'succeeded' ? '2026-10-20T23:01:00Z' : null, finished_at: state === 'succeeded' ? '2026-10-20T23:01:00Z' : null, error_code: null })
async function harness(page: Page, mode = 'normal') {
  let stored: any = ['resolved', 'offline-resolved', 'unsupported-resolved', 'manual-resolved', 'late-read'].includes(mode) ? operation('succeeded') : null
  const calls: { method: string; path: string; body: any }[] = []
  let releasePrepare: (() => void) | undefined
  await page.route('**/api/**', async route => {
    const request = route.request(), path = new URL(request.url()).pathname
    if (!path.includes('controller-update') && path !== '/api/auth/config') return route.fulfill({ status: 401, json: {} })
    calls.push({ method: request.method(), path, body: request.postDataJSON() })
    if (path === '/api/auth/config') return route.fulfill({ json: { capabilities: ['machine.controller-update.v1'] } })
    if (path.endsWith('/operations')) {
      const captured = stored
      if (mode === 'late-read' && calls.filter(c => c.path.endsWith('/operations')).length === 1) await new Promise<void>(resolve => { releasePrepare = resolve })
      return route.fulfill({ json: { operations: captured ? [captured] : [], blocked_by_other_operation: false } })
    }
    if (path.endsWith('/controller-update') && mode === 'oversize') return route.fulfill({ contentType: 'application/json', body: ' '.repeat(65536) + JSON.stringify(catalog()) })
    if (path.endsWith('/controller-update')) return ['offline-resolved', 'unsupported-resolved'].includes(mode) ? route.fulfill({ status: mode === 'unsupported-resolved' ? 501 : 503, json: {} }) : route.fulfill({ json: { ...catalog(), ...(mode === 'manual-resolved' ? { support: 'manual_update_required', availability: 'catalog_unavailable', targets: [] } : {}), ...(mode === 'bad-null' ? { current: undefined } : {}), ...(mode === 'extra' ? { command: 'forbidden' } : {}) } })
    if (path.endsWith('/prepare')) {
      if (mode === 'late-prepare') await new Promise<void>(resolve => { releasePrepare = resolve })
      return route.fulfill({ json: { operation_id: operationId, current, target, plan_digest: 'd'.repeat(64), steps: ['verify_artifacts', 'stage', 'replace_controller_and_cli', 'verify_health', 'reconcile_launcher', 'finalize'], confirmation_token: token, expires_at: new Date(Date.now() + 115000).toISOString() } })
    }
    if (path.endsWith('/confirm')) {
      stored = operation()
      if (mode === 'lost') return route.abort('failed')
      return route.fulfill({ json: stored })
    }
    if (path.endsWith('/' + operationId)) return route.fulfill({ json: stored })
    return route.fulfill({ status: 404, json: {} })
  })
  await page.goto('/')
  await expect(page.getByRole('heading', { name: 'Sign in to Treer', exact: true })).toBeVisible()
  await page.evaluate(async () => { const path = '/e2e/fixtures/controller-update.tsx'; const module = await import(/* @vite-ignore */ path); (window as any).updateHarness = await module.mountControllerUpdateEntry() })
  await page.getByRole('button', { name: 'Update Controller', exact: true }).click()
  return { calls, resolve: () => releasePrepare?.(), store: (value: any) => { stored = value } }
}
const dialog = (page: Page) => page.getByRole('dialog')
for (const mode of ['normal', 'lost']) test(`real HTTP client ${mode}: prepare/confirm once and GET recovery`, async ({ page }) => {
  const h = await harness(page, mode)
  await dialog(page).getByRole('button', { name: 'Preview 1.1.0' }).click()
  await dialog(page).getByRole('button', { name: 'Update Controller', exact: true }).click()
  await expect(dialog(page)).toContainText('Update queued.')
  await dialog(page).getByRole('button', { name: 'Check update status' }).click()
  expect(h.calls.filter(c => c.method === 'POST').map(c => c.body)).toEqual([{ target_id: 'target-b' }, { confirmation_token: token }])
  expect(h.calls.some(c => c.method === 'GET' && c.path.endsWith('/controller-update'))).toBe(true)
  expect(h.calls.some(c => c.path.endsWith('/catalog'))).toBe(false)
  expect(h.calls.some(c => c.path.endsWith('/operations/' + operationId))).toBe(true)
  await expect(page.locator('body')).not.toContainText(token)
  h.store(operation('succeeded'))
  await dialog(page).getByRole('button', { name: 'Check update status' }).click()
  await expect(dialog(page)).toContainText('Target version and health verified')
  await expect(dialog(page).getByRole('button', { name: 'Preview 1.1.0' })).toBeVisible()
  expect(h.calls.filter(c => c.path.endsWith('/confirm'))).toHaveLength(1)
})
for (const mode of ['bad-null', 'extra', 'oversize']) test(`strict HTTP decoder refuses ${mode}`, async ({ page }) => {
  const h = await harness(page, mode)
  await expect(dialog(page)).toContainText('could not be verified')
  await expect(dialog(page).getByRole('button', { name: /Preview / })).toHaveCount(0)
  expect(h.calls.filter(c => c.method === 'POST')).toHaveLength(0)
})
test('resolved retained history permits explicit new preview', async ({ page }) => {
  const h = await harness(page, 'resolved')
  await expect(dialog(page)).toContainText('Target version and health verified')
  await dialog(page).getByRole('button', { name: 'Preview 1.1.0' }).click()
  await expect(dialog(page)).toContainText('Review Controller update')
  expect(h.calls.filter(c => c.path.endsWith('/confirm'))).toHaveLength(0)
})
test('same machine session change discards preview and late old prepare result', async ({ page }) => {
  const h = await harness(page, 'late-prepare')
  await dialog(page).getByRole('button', { name: 'Preview 1.1.0' }).click()
  await expect.poll(() => h.calls.filter(c => c.path.endsWith('/prepare')).length).toBe(1)
  await page.evaluate(() => (window as any).updateHarness.changeSession())
  h.resolve()
  await expect(page.getByRole('dialog')).toHaveCount(0)
  await page.getByRole('button', { name: 'Update Controller', exact: true }).click()
  await expect(dialog(page).getByRole('button', { name: 'Preview 1.1.0' })).toBeVisible()
  await expect(dialog(page)).not.toContainText('Review Controller update')
  expect(h.calls.filter(c => c.path.endsWith('/confirm'))).toHaveLength(0)
})
test('unresolved history remains blocked even with ready catalog', async ({ page }) => {
  const h = await harness(page)
  h.store({ ...operation(), state: 'unconfirmed', error_code: 'result_unconfirmed' })
  await dialog(page).getByRole('button', { name: 'Check update status' }).click()
  await expect(dialog(page)).toContainText('Result unconfirmed')
  await expect(dialog(page).getByRole('button', { name: /Preview / })).toHaveCount(0)
  expect(h.calls.filter(c => c.method === 'POST')).toHaveLength(0)
})

for (const mode of ['offline-resolved', 'unsupported-resolved', 'manual-resolved']) test(`durable result survives ${mode} catalog without prepare`, async ({ page }) => {
  const h = await harness(page, mode)
  await expect(dialog(page)).toContainText('Target version and health verified')
  if (['unsupported-resolved', 'manual-resolved'].includes(mode)) await expect(dialog(page)).toContainText('Update it manually first')
  await expect(dialog(page).getByRole('button', { name: /Preview / })).toHaveCount(0)
  expect(h.calls.filter(c => c.method === 'POST')).toHaveLength(0)
})
test('same machine session change ignores a late previous-user discovery', async ({ page }) => {
  const h = await harness(page, 'late-read')
  await expect.poll(() => h.calls.filter(c => c.path.endsWith('/operations')).length).toBe(1)
  h.store(null)
  await page.evaluate(() => (window as any).updateHarness.changeSession())
  h.resolve()
  await expect(page.getByRole('dialog')).toHaveCount(0)
  await page.getByRole('button', { name: 'Update Controller', exact: true }).click()
  await expect(dialog(page).getByRole('button', { name: 'Preview 1.1.0' })).toBeVisible()
  await expect(dialog(page)).not.toContainText('Target version and health verified')
  expect(h.calls.filter(c => c.method === 'POST')).toHaveLength(0)
})
