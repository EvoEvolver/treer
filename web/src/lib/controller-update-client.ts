import { ApiError, proxyUrl, type Machine } from '@/lib/api'
import { sameControllerIdentity, type ControllerIdentity, type ControllerRelease, type ControllerUpdateClient, type ControllerUpdateOperation, type ControllerUpdateSnapshot } from './controller-update'

export const CONTROLLER_UPDATE_CAPABILITY = 'machine.controller-update.v1'
const steps = ['verify_artifacts', 'stage', 'replace_controller_and_cli', 'verify_health', 'reconcile_launcher', 'finalize'] as const
const platforms = ['darwin-aarch64', 'darwin-x86_64', 'linux-aarch64', 'linux-x86_64', 'windows-x86_64']
const states = ['accepted', 'updating', 'succeeded', 'failed', 'rolled_back', 'rollback_failed', 'partial', 'unconfirmed']
const phases = ['preparing', 'prepared', 'activating', 'core_healthy', 'launcher_installing', 'rolling_back', 'complete', 'failed', 'rolled_back', 'rollback_failed', 'partial']
const errors = ['unsupported', 'offline', 'busy', 'catalog_unavailable', 'target_unavailable', 'target_not_upgrade', 'preview_expired', 'preview_changed', 'confirmation_invalid', 'rate_limited', 'capacity_exhausted', 'verification_failed', 'health_failed', 'receipt_invalid', 'recovery_required', 'result_unconfirmed']
const unresolved = new Set(['accepted', 'updating', 'unconfirmed', 'partial', 'rollback_failed'])
function require(ok: unknown): asserts ok { if (!ok) throw Error('invalid_response') }
function object(value: unknown, fields: string[]): Record<string, any> {
  require(value && typeof value === 'object' && !Array.isArray(value))
  const v = value as Record<string, any>
  require(Object.keys(v).length === fields.length && fields.every(k => Object.hasOwn(v, k)))
  return v
}
const hex = (v: unknown, n: number) => typeof v === 'string' && new RegExp(`^[a-f0-9]{${n}}$`).test(v)
const opaque = (v: unknown, max: number) => typeof v === 'string' && v.length > 0 && v.length <= max && /^[a-zA-Z0-9_-]+$/.test(v)
const job = (v: unknown) => typeof v === 'string' && /^[a-f0-9]{8}-[a-f0-9]{4}-4[a-f0-9]{3}-[89ab][a-f0-9]{3}-[a-f0-9]{12}$/.test(v)
const integer = (v: unknown) => typeof v === 'number' && Number.isSafeInteger(v) && v >= 0
// Compare UTC timestamps without discarding sub-millisecond ordering.
function time(v: unknown): bigint {
  require(typeof v === 'string' && /^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d(?:\.\d{1,9})?Z$/.test(v))
  const main = v.slice(0, 19), ms = Date.parse(main + 'Z')
  require(Number.isFinite(ms) && new Date(ms).toISOString().slice(0, 19) === main)
  return BigInt(ms) * 1000000n + BigInt((v.match(/\.(\d+)Z$/)?.[1] ?? '').padEnd(9, '0'))
}
function identity(value: unknown): ControllerIdentity {
  const v = object(value, ['release_sha', 'artifact_source_sha', 'controller_sha256', 'cli_sha256'])
  require(hex(v.release_sha, 40) && hex(v.artifact_source_sha, 40) && hex(v.controller_sha256, 64) && hex(v.cli_sha256, 64))
  return v as unknown as ControllerIdentity
}
function release(value: unknown): ControllerRelease {
  const v = object(value, ['target_id', 'identity', 'platform', 'version_label'])
  require(opaque(v.target_id, 128) && platforms.includes(v.platform) && typeof v.version_label === 'string' && /^[\x20-\x7e]{1,128}$/.test(v.version_label))
  const id = identity(v.identity)
  return { id: v.target_id, identity: id, platform: v.platform, version: v.version_label, commit: id.release_sha }
}
function currentRelease(id: ControllerIdentity, platform: string): ControllerRelease {
  return { id: id.release_sha, identity: id, platform, version: `SHA ${id.release_sha.slice(0, 12)}`, commit: id.release_sha }
}
export function decodeControllerCatalog(value: unknown) {
  const v = object(value, ['support', 'availability', 'current', 'observed_at', 'platform', 'catalog_revision', 'targets'])
  require(['supported', 'manual_update_required'].includes(v.support) && ['ready', 'offline', 'busy', 'catalog_unavailable'].includes(v.availability) && platforms.includes(v.platform) && integer(v.catalog_revision) && v.catalog_revision > 0)
  require((v.current === null) === (v.observed_at === null))
  const current = v.current === null ? null : currentRelease(identity(v.current), v.platform)
  if (v.observed_at !== null) time(v.observed_at)
  require(v.availability !== 'ready' || v.support === 'supported' && current)
  require(Array.isArray(v.targets) && v.targets.length <= 16)
  const targets = v.targets.map(release)
  require(new Set(targets.map(t => t.id)).size === targets.length && targets.every(t => t.platform === v.platform))
  require(v.support === 'supported' && v.availability === 'ready' || targets.length === 0)
  return { supported: v.support === 'supported', availability: v.availability as string, current, targets }
}
export function decodeControllerOperation(value: unknown, machineId: string) {
  const v = object(value, ['operation_id', 'current', 'target', 'state', 'phase', 'observation', 'observed_identity', 'restored_identity', 'health', 'receipt_revision', 'created_at', 'updated_at', 'observed_at', 'finished_at', 'error_code'])
  require(job(v.operation_id) && states.includes(v.state) && (v.phase === null || phases.includes(v.phase)) && ['available', 'waiting_for_reconnect', 'unavailable'].includes(v.observation) && ['unknown', 'healthy', 'unhealthy'].includes(v.health) && (v.error_code === null || errors.includes(v.error_code)))
  const target = release(v.target), current = currentRelease(identity(v.current), target.platform)
  const observed = v.observed_identity === null ? undefined : identity(v.observed_identity)
  const restored = v.restored_identity === null ? null : currentRelease(identity(v.restored_identity), target.platform)
  const created = time(v.created_at), updated = time(v.updated_at)
  require(created <= updated && (v.phase === null) === (v.receipt_revision === null) && (v.receipt_revision === null || integer(v.receipt_revision)))
  for (const t of [v.observed_at, v.finished_at]) if (t !== null) require(time(t) >= created && time(t) <= updated)
  if (v.state === 'succeeded') require(v.phase === 'complete' && sameControllerIdentity(observed, target.identity) && v.observed_at !== null && v.finished_at !== null && v.health === 'healthy' && restored === null && v.error_code === null)
  if (v.state === 'rolled_back') require(v.phase === 'rolled_back' && sameControllerIdentity(restored?.identity, current.identity) && sameControllerIdentity(observed, current.identity) && v.observed_at !== null && v.finished_at !== null && v.health === 'healthy')
  if (v.state === 'failed') require((v.phase === null || v.phase === 'failed') && v.finished_at !== null && v.error_code !== null)
  if (['partial', 'rollback_failed'].includes(v.state)) require(v.phase === v.state && v.finished_at !== null && v.health !== 'healthy' && v.error_code !== null)
  if (v.state === 'accepted') require(v.phase === null && v.finished_at === null && v.error_code === null && v.health === 'unknown' && !observed && !restored && v.observed_at === null)
  if (v.state === 'updating') require(['preparing', 'prepared', 'activating', 'core_healthy', 'launcher_installing', 'rolling_back'].includes(v.phase) && v.finished_at === null && v.error_code === null)
  if (v.state === 'unconfirmed') require(v.finished_at === null && v.error_code !== null && v.health !== 'healthy')
  const operation: ControllerUpdateOperation = { id: v.operation_id, machineId, current, target, status: v.state === 'accepted' ? 'queued' : v.state, phase: v.phase, observation: v.observation, observedIdentity: observed, restored, health: v.health === 'healthy' ? 'passed' : v.health === 'unhealthy' ? 'failed' : 'unknown', receiptVerified: v.receipt_revision !== null, observedAt: v.observed_at }
  // Receipt authenticity is enforced by the server adapter; this decoder checks
  // the authenticated projection, never treats reconnect as receipt evidence.
  return { operation, state: v.state as string, created, finished: v.finished_at === null ? null : time(v.finished_at) }
}
export function decodeControllerDiscovery(value: unknown, machineId: string) {
  const v = object(value, ['operations', 'blocked_by_other_operation'])
  require(Array.isArray(v.operations) && v.operations.length <= 16 && typeof v.blocked_by_other_operation === 'boolean')
  const operations = v.operations.map((op: unknown) => decodeControllerOperation(op, machineId))
  require(new Set(operations.map(o => o.operation.id)).size === operations.length && operations.filter(o => unresolved.has(o.state)).length <= 1)
  operations.forEach((o, i) => { if (i) { const prev = operations[i - 1]; require(prev.created > o.created || prev.created === o.created && prev.operation.id >= o.operation.id) } })
  return { operations, blocked: v.blocked_by_other_operation as boolean }
}

// Requests never accept arbitrary URL, path, argv or native updater options.
export function createControllerUpdateClient(workspaceId: string, machine: Pick<Machine, 'server_id' | 'capabilities'>, lifetime: AbortSignal): ControllerUpdateClient {
  const machineId = machine.server_id
  const base = `/api/workspaces/${encodeURIComponent(workspaceId)}/machines/${encodeURIComponent(machineId)}/controller-update`
  let preview: { token: string; operationId: string; current: ControllerIdentity; target: ControllerRelease } | null = null
  async function request(path: string, signal?: AbortSignal, body?: unknown): Promise<unknown> {
    const controller = new AbortController()
    const abort = () => controller.abort()
    lifetime.addEventListener('abort', abort, { once: true }); signal?.addEventListener('abort', abort, { once: true })
    if (lifetime.aborted || signal?.aborted) abort()
    const timer = window.setTimeout(abort, 15000)
    try {
      const response = await fetch(proxyUrl(path), { method: body === undefined ? 'GET' : 'POST', credentials: 'include', cache: 'no-store', redirect: 'error', signal: controller.signal, headers: { 'content-type': 'application/json' }, ...(body === undefined ? {} : { body: JSON.stringify(body) }) })
      if (!response.ok) throw new ApiError('controller_update_unavailable', response.status)
      require(response.body)
      const reader = response.body.getReader(), chunks: Uint8Array[] = []; let length = 0
      for (;;) { const { value, done } = await reader.read(); if (done) break; length += value.length; if (length > 262144) { await reader.cancel(); throw Error('invalid_response') }; chunks.push(value) }
      const bytes = new Uint8Array(length); let offset = 0
      for (const chunk of chunks) { bytes.set(chunk, offset); offset += chunk.length }
      return JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(bytes))
    } finally { window.clearTimeout(timer); lifetime.removeEventListener('abort', abort); signal?.removeEventListener('abort', abort) }
  }
  lifetime.addEventListener('abort', () => { preview = null }, { once: true })
  const client: ControllerUpdateClient = {
    async inspect(signal, operationId) {
      // Recovery is GET-only and remains useful without advertised runtime support.
      let discovery: ReturnType<typeof decodeControllerDiscovery>
      try { discovery = decodeControllerDiscovery(await request(`${base}/operations`, signal), machineId) }
      catch (error) {
        if (error instanceof ApiError && ([404, 501].includes(error.status) && !operationId || error.status === 403)) return { machineId, supported: false, unavailableReason: error.status === 403 ? 'forbidden' : 'unsupported', current: null, targets: [], operation: null, blockedByOtherOperation: false, prepareAllowed: false }
        throw error
      }
      let selected = discovery.operations.find(o => unresolved.has(o.state)) ?? discovery.operations[0]
      if (operationId) {
        require(job(operationId))
        selected = decodeControllerOperation(await request(`${base}/operations/${operationId}`, signal), machineId)
        require(selected.operation.id === operationId)
      }
      const unavailable = (reason: ControllerUpdateSnapshot['unavailableReason']): ControllerUpdateSnapshot => ({ machineId, supported: false, unavailableReason: reason, current: null, targets: [], operation: selected?.operation ?? null, blockedByOtherOperation: discovery.blocked, prepareAllowed: false })
      let config: unknown
      try { config = await request('/api/auth/config', signal) } catch { if (signal.aborted || lifetime.aborted) throw Error('aborted'); return unavailable('unavailable') }
      const capabilities = (config as { capabilities?: unknown })?.capabilities
      if (!Array.isArray(capabilities) || !capabilities.includes(CONTROLLER_UPDATE_CAPABILITY) || !machine.capabilities?.includes(CONTROLLER_UPDATE_CAPABILITY)) return unavailable('unsupported')
      let catalog: ReturnType<typeof decodeControllerCatalog>
      try { catalog = decodeControllerCatalog(await request(`${base}/catalog`, signal)) } catch (error) { if (signal.aborted || lifetime.aborted) throw Error('aborted'); return unavailable(error instanceof ApiError && error.status === 501 ? 'unsupported' : error instanceof ApiError && error.status === 403 ? 'forbidden' : 'unavailable') }
      const ready = catalog.supported && catalog.availability === 'ready'
      const cooldownElapsed = !selected || selected.state === 'succeeded' || selected.finished !== null && BigInt(Date.now()) * 1000000n >= selected.finished + 60000000000n
      return { machineId, supported: ready, unavailableReason: catalog.availability === 'offline' ? 'offline' : !ready ? 'unavailable' : undefined, current: catalog.current, targets: catalog.targets, operation: selected?.operation ?? null, blockedByOtherOperation: discovery.blocked || catalog.availability === 'busy', prepareAllowed: ready && !discovery.blocked && !discovery.operations.some(o => unresolved.has(o.state)) && cooldownElapsed }
    },
    async prepare(targetId, signal) {
      preview = null
      const snapshot = await client.inspect(signal)
      require(snapshot.prepareAllowed && snapshot.current)
      const target = snapshot.targets.find(t => t.id === targetId); require(target)
      const value = object(await request(`${base}/prepare`, signal, { target_id: targetId }), ['operation_id', 'current', 'target', 'plan_digest', 'steps', 'confirmation_token', 'expires_at'])
      const current = identity(value.current), chosen = release(value.target)
      require(job(value.operation_id) && hex(value.plan_digest, 64) && JSON.stringify(value.steps) === JSON.stringify(steps) && opaque(value.confirmation_token, 256) && value.confirmation_token.length >= 32 && sameControllerIdentity(current, snapshot.current.identity) && chosen.id === target.id && sameControllerIdentity(chosen.identity, target.identity) && chosen.version === target.version && chosen.platform === target.platform && !sameControllerIdentity(current, chosen.identity))
      const expiresInMs = Number((time(value.expires_at) - BigInt(Date.now()) * 1000000n) / 1000000n)
      require(expiresInMs > 0 && expiresInMs <= 120000 && !signal.aborted && !lifetime.aborted)
      preview = { token: value.confirmation_token, operationId: value.operation_id, current, target: chosen }
      return { machineId, current: snapshot.current, target: chosen, operationId: value.operation_id, confirmationToken: value.confirmation_token, expiresInMs, steps: [...steps] }
    },
    async dispatch(token) {
      const expected = preview; preview = null
      require(expected && expected.token === token && !lifetime.aborted)
      const result = decodeControllerOperation(await request(`${base}/confirm`, undefined, { confirmation_token: token }), machineId).operation
      require(result.id === expected.operationId && sameControllerIdentity(result.current.identity, expected.current) && result.target.id === expected.target.id && sameControllerIdentity(result.target.identity, expected.target.identity))
    },
  }
  return client
}
