import { test, expect } from '@playwright/test'
import { readFile, stat, writeFile } from 'node:fs/promises'
import { join } from 'node:path'
import { recordBrowserPhase } from './controller-update-diagnostics.mjs'
import { installPrepareFetchObservation, observedOperationForDocument } from './controller-update-prepare-observer.mjs'

// No request context, route interception, injected cookies or identity headers.
// Credentials stay in the Core-owned 0600 input; disable traces/screenshots.
test('actual Machine update: one confirmation, refresh observes same native operation', async ({ page }) => {
  const root = process.env.TREER_CONTROLLER_BROWSER_ROOT
  if (!root) throw Error('Core private fixture root required')
  recordBrowserPhase(root, 'waiting_for_ready')
  const readyPath = join(root, 'browser-ready.json')
  await expect.poll(async () => stat(readyPath).then(() => true, () => false), { timeout: 120_000 }).toBe(true)
  recordBrowserPhase(root, 'validating_binding')
  const metadata = await stat(readyPath)
  if (!metadata.isFile() || metadata.mode & 0o077) throw Error('private ready input required')
  const b = JSON.parse(await readFile(readyPath, 'utf8'))
  const origin = new URL(b.origin)
  if (origin.protocol !== 'http:' || origin.hostname !== '127.0.0.1' || origin.pathname !== '/' || origin.search || origin.hash || origin.username || origin.password) throw Error('owned loopback origin required')
  for (const field of ['email', 'password', 'workspace_id', 'organization_id', 'machine_id', 'target_id']) if (typeof b[field] !== 'string' || !b[field]) throw Error('invalid fixture binding')
  const base = `/api/workspaces/${encodeURIComponent(b.workspace_id)}/machines/${encodeURIComponent(b.machine_id)}/controller-update`
  let prepare = 0, confirm = 0, otherWrites = 0, afterRefreshWrites = 0, refreshed = false
  let currentDocument: string | null = null
  const retiredDocuments = new Set<string>()
  let prepareBinding: { status: string, operationId?: string } | null = null
  let operationId: string | undefined
  let finalOperation: any = null
  let loggedIn = false
  page.on('request', req => {
    const url = new URL(req.url())
    if (url.origin !== origin.origin || !url.pathname.startsWith(base) || ['GET', 'HEAD', 'OPTIONS'].includes(req.method())) return
    if (refreshed) afterRefreshWrites++
    if (req.method() === 'POST' && url.pathname === `${base}/prepare`) prepare++
    else if (req.method() === 'POST' && url.pathname === `${base}/confirm`) confirm++
    else otherWrites++
  })
  page.on('response', response => {
    const url = new URL(response.url())
    if (url.origin !== origin.origin || !response.ok()) return
    if (url.pathname === '/api/auth/login' && response.request().method() === 'POST') loggedIn = true
  })
  // Observe only the same product response; never issue a second prepare.
  await page.exposeBinding('__controllerPrepareObservation', async ({ frame }, message) => {
    if (frame !== page.mainFrame() || !message || typeof message !== 'object' || typeof message.documentId !== 'string' || retiredDocuments.has(message.documentId)) return
    try { if (new URL(frame.url()).origin !== origin.origin) return } catch { return }
    let documentNow: unknown
    try { documentNow = await frame.evaluate(() => (window as any).__treerControllerObservationDocument) } catch { return }
    if (message.documentId !== documentNow || retiredDocuments.has(message.documentId)) return
    try { if (new URL(frame.url()).origin !== origin.origin) return } catch { return }
    currentDocument = message.documentId
    if (message.kind === 'document') return
    const result = message.value
    if (message.kind === 'prepare' && result && typeof result === 'object') {
      if (['body_unavailable', 'target_mismatch', 'operation_invalid'].includes(result.status) && Object.keys(result).length === 1) prepareBinding = { status: result.status }
      else if (result.status === 'bound' && Object.keys(result).length === 2 && typeof result.operationId === 'string' && /^[a-f0-9]{8}-[a-f0-9]{4}-4[a-f0-9]{3}-[89ab][a-f0-9]{3}-[a-f0-9]{12}$/.test(result.operationId)) {
        prepareBinding = { status: 'bound', operationId: result.operationId }; operationId = result.operationId
      }
    }
    if (message.kind === 'operations' && operationId && currentDocument === message.documentId) {
      try { const op = observedOperationForDocument(message, currentDocument, operationId); if (op) finalOperation = op } catch { /* invalid projection never supplies final evidence */ }
    }
  })
  await page.addInitScript(installPrepareFetchObservation, { origin: origin.origin, path: `${base}/prepare`, operationsPath: `${base}/operations`, targetId: b.target_id, callback: '__controllerPrepareObservation' })
  const workspace = `${origin.origin}/orgs/${encodeURIComponent(b.organization_id)}/workspaces/${encodeURIComponent(b.workspace_id)}`
  recordBrowserPhase(root, 'opening_login')
  await page.goto(workspace)
  recordBrowserPhase(root, 'normal_login')
  await page.locator('#email').fill(b.email)
  await page.locator('#password').fill(b.password)
  await page.getByRole('button', { name: 'Sign in', exact: true }).click()
  await expect.poll(() => loggedIn).toBe(true)
  async function openMachine() {
    await page.getByRole('button', { name: 'Workspace settings', exact: true }).click()
    const rows = page.locator('[data-tour="workspace-machines"] .group > button:first-child')
    await expect(rows).toHaveCount(1)
    await rows.first().click()
    await expect(page.getByText(b.machine_id, { exact: true })).toBeVisible()
    await page.getByRole('button', { name: 'Update Controller', exact: true }).click()
  }
  recordBrowserPhase(root, 'opening_machine')
  await openMachine()
  const dialog = page.getByRole('dialog')
  recordBrowserPhase(root, 'waiting_for_preview_choice')
  const target = dialog.getByRole('button', { name: /^Preview / })
  // Fresh inspect can perform three bounded 15s reads before the 15s prepare.
  // This observer budget covers that existing client contract, not a retry.
  await expect(target).toHaveCount(1, { timeout: 65_000 })
  await target.click()
  recordBrowserPhase(root, 'waiting_for_preview_review')
  await expect(dialog.getByRole('heading', { name: 'Review Controller update', exact: true })).toBeVisible({ timeout: 65_000 })
  recordBrowserPhase(root, 'waiting_for_preview_binding')
  await expect.poll(() => prepareBinding !== null, { timeout: 65_000 }).toBe(true)
  if (!operationId) {
    const status = prepareBinding?.status
    recordBrowserPhase(root, status === 'body_unavailable' ? 'preview_binding_body_unavailable' : status === 'target_mismatch' ? 'preview_binding_target_mismatch' : status === 'operation_invalid' ? 'preview_binding_operation_invalid' : 'waiting_for_preview_binding')
    throw Error('prepare observation could not bind operation')
  }
  recordBrowserPhase(root, 'confirming_update')
  await dialog.getByRole('button', { name: 'Update Controller', exact: true }).click()
  await expect.poll(() => confirm).toBe(1)
  recordBrowserPhase(root, 'waiting_for_reconnect')
  await expect.poll(async () => stat(join(root, 'browser-reconnected.json')).then(() => true, () => false), { timeout: 90_000 }).toBe(true)
  const reconnect = JSON.parse(await readFile(join(root, 'browser-reconnected.json'), 'utf8'))
  expect(reconnect.operation_id).toBe(operationId)
  refreshed = true
  const retiringDocument = await page.evaluate(() => (window as any).__treerControllerObservationDocument)
  if (typeof retiringDocument === 'string') retiredDocuments.add(retiringDocument)
  currentDocument = null
  finalOperation = null
  recordBrowserPhase(root, 'refreshing')
  await page.reload()
  await openMachine()
  recordBrowserPhase(root, 'observing_final_operation')
  await expect.poll(() => finalOperation?.state, { timeout: 450_000 }).toBe('succeeded')
  await expect(dialog.getByText('Target version and health verified. Update complete.', { exact: true })).toBeVisible()
  expect(prepare).toBe(1)
  expect(confirm).toBe(1)
  expect(otherWrites + afterRefreshWrites).toBe(0)
  expect(finalOperation.operation_id).toBe(operationId)
  expect(finalOperation.target.target_id).toBe(b.target_id)
  // Core compares this exact wire operation with its PG row and native receipt.
  recordBrowserPhase(root, 'writing_result')
  await writeFile(join(root, 'browser-result.json'), JSON.stringify({ operation: finalOperation, prepare_requests: prepare, confirm_requests: confirm, duplicate_write_requests: otherWrites + afterRefreshWrites, normal_login: loggedIn, refreshed }) + '\n', { mode: 0o600, flag: 'wx' })
  // Keep authenticated context alive until Core has collected the result.
  recordBrowserPhase(root, 'waiting_for_core_completion')
  await expect.poll(async () => stat(join(root, 'browser-complete.json')).then(() => true, () => false), { timeout: 60_000 }).toBe(true)
  const complete = JSON.parse(await readFile(join(root, 'browser-complete.json'), 'utf8'))
  expect(complete.operation_id).toBe(operationId)
})
