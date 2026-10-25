// Passive browser observation only: no request, retry, or response-body output.
export async function observePrepareBinding(response, expected) {
  const url = new URL(response.url())
  if (url.origin !== expected.origin || url.pathname !== expected.path || response.request().method() !== 'POST' || !response.ok()) return { status: 'ignored' }
  let value
  try { value = await response.json() } catch { return { status: 'body_unavailable' } }
  if (value?.target?.target_id !== expected.targetId) return { status: 'target_mismatch' }
  if (typeof value.operation_id !== 'string' || !/^[a-f0-9]{8}-[a-f0-9]{4}-4[a-f0-9]{3}-[89ab][a-f0-9]{3}-[a-f0-9]{12}$/.test(value.operation_id)) return { status: 'operation_invalid' }
  return { status: 'bound', operationId: value.operation_id }
}


// Self-contained for Playwright's pre-navigation serialization. Project mode
// reuses exactly the same allowlist/validation in the Node bridge receiver.
export function installPrepareFetchObservation(expected) {
  const uuid = v => typeof v === 'string' && /^[a-f0-9]{8}-[a-f0-9]{4}-4[a-f0-9]{3}-[89ab][a-f0-9]{3}-[a-f0-9]{12}$/.test(v)
  const assert = v => { if (!v) throw Error('invalid observation') }
  const object = (v, keys) => { assert(v && typeof v === 'object' && !Array.isArray(v) && keys.every(k => Object.hasOwn(v, k))); return Object.fromEntries(keys.map(k => [k, v[k]])) }
  const identity = v => {
    const p = object(v, ['release_sha', 'artifact_source_sha', 'controller_sha256', 'cli_sha256'])
    for (const [k, x] of Object.entries(p)) assert(typeof x === 'string' && new RegExp(`^[a-f0-9]{${k.endsWith('sha256') ? 64 : 40}}$`).test(x))
    return p
  }
  const time = v => { assert(typeof v === 'string' && /^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d(?:\.\d{1,9})?Z$/.test(v)); const ms = Date.parse(v.slice(0, 19) + 'Z'); assert(Number.isFinite(ms) && new Date(ms).toISOString().slice(0, 19) === v.slice(0, 19)); return BigInt(ms) * 1000000n + BigInt((v.match(/\.(\d+)Z$/)?.[1] ?? '').padEnd(9, '0')) }
  const same = (a, b) => a && b && Object.keys(a).every(k => a[k] === b[k])
  const project = value => {
    const v = object(value, ['operation_id', 'current', 'target', 'state', 'phase', 'observation', 'observed_identity', 'restored_identity', 'health', 'receipt_revision', 'created_at', 'updated_at', 'observed_at', 'finished_at', 'error_code'])
    assert(uuid(v.operation_id) && ['accepted', 'updating', 'succeeded', 'failed', 'rolled_back', 'rollback_failed', 'partial', 'unconfirmed'].includes(v.state))
    assert(v.phase === null || ['preparing', 'prepared', 'activating', 'core_healthy', 'launcher_installing', 'rolling_back', 'complete', 'failed', 'rolled_back', 'rollback_failed', 'partial'].includes(v.phase))
    assert(['available', 'waiting_for_reconnect', 'unavailable'].includes(v.observation) && ['unknown', 'healthy', 'unhealthy'].includes(v.health))
    assert(v.error_code === null || ['unsupported', 'offline', 'busy', 'catalog_unavailable', 'target_unavailable', 'target_not_upgrade', 'preview_expired', 'preview_changed', 'confirmation_invalid', 'rate_limited', 'capacity_exhausted', 'verification_failed', 'health_failed', 'receipt_invalid', 'recovery_required', 'result_unconfirmed'].includes(v.error_code))
    v.current = identity(v.current)
    v.target = object(v.target, ['target_id', 'identity', 'platform', 'version_label'])
    assert(typeof v.target.target_id === 'string' && /^[a-zA-Z0-9_-]{1,128}$/.test(v.target.target_id) && ['darwin-aarch64', 'darwin-x86_64', 'linux-aarch64', 'linux-x86_64', 'windows-x86_64'].includes(v.target.platform) && typeof v.target.version_label === 'string' && /^[\x20-\x7e]{1,128}$/.test(v.target.version_label))
    v.target.identity = identity(v.target.identity)
    v.observed_identity = v.observed_identity === null ? null : identity(v.observed_identity)
    v.restored_identity = v.restored_identity === null ? null : identity(v.restored_identity)
    const created = time(v.created_at), updated = time(v.updated_at)
    assert(created <= updated && (v.phase === null) === (v.receipt_revision === null) && (v.receipt_revision === null || Number.isSafeInteger(v.receipt_revision) && v.receipt_revision >= 0))
    for (const t of [v.observed_at, v.finished_at]) if (t !== null) assert(time(t) >= created && time(t) <= updated)
    if (v.state === 'succeeded') assert(v.phase === 'complete' && same(v.observed_identity, v.target.identity) && v.observed_at !== null && v.finished_at !== null && v.health === 'healthy' && v.restored_identity === null && v.error_code === null)
    if (v.state === 'rolled_back') assert(v.phase === 'rolled_back' && same(v.restored_identity, v.current) && same(v.observed_identity, v.current) && v.observed_at !== null && v.finished_at !== null && v.health === 'healthy')
    if (v.state === 'failed') assert((v.phase === null || v.phase === 'failed') && v.finished_at !== null && v.error_code !== null)
    if (['partial', 'rollback_failed'].includes(v.state)) assert(v.phase === v.state && v.finished_at !== null && v.health !== 'healthy' && v.error_code !== null)
    if (v.state === 'accepted') assert(v.phase === null && v.finished_at === null && v.error_code === null && v.health === 'unknown' && v.observed_identity === null && v.restored_identity === null && v.observed_at === null)
    if (v.state === 'updating') assert(['preparing', 'prepared', 'activating', 'core_healthy', 'launcher_installing', 'rolling_back'].includes(v.phase) && v.finished_at === null && v.error_code === null)
    if (v.state === 'unconfirmed') assert(v.finished_at === null && v.error_code !== null && v.health !== 'healthy')
    return v
  }
  if (expected.mode === 'project') return project(expected.value)
  const marker = '__treerControllerObservationDocument'
  if (Object.hasOwn(window, marker)) return
  const documentId = crypto.randomUUID()
  Object.defineProperty(window, marker, { value: documentId }) // nonce only, never raw response cache
  const publish = (kind, value) => { try { void Promise.resolve(window[expected.callback]({ documentId, kind, value })).catch(() => {}) } catch { /* bridge cannot affect fetch */ } }
  publish('document', null)
  const original = window.fetch
  window.fetch = function (...args) {
    const issuedDocument = documentId // captured before invoking/awaiting fetch
    const result = Reflect.apply(original, this, args)
    let kind
    try {
      const input = args[0], options = args[1]
      const url = new URL(input instanceof Request ? input.url : String(input), location.href)
      const method = String(options?.method ?? (input instanceof Request ? input.method : 'GET')).toUpperCase()
      if (url.origin === expected.origin && !url.search && !url.hash) {
        if (url.pathname === expected.path && method === 'POST') kind = 'prepare'
        if (expected.operationsPath && method === 'GET' && (url.pathname === expected.operationsPath || url.pathname.startsWith(expected.operationsPath + '/') && uuid(url.pathname.slice(expected.operationsPath.length + 1)))) kind = 'operations'
      }
    } catch { return result }
    if (!kind) return result
    void result.then(response => {
      if (!response.ok) return
      let reader
      try { reader = response.clone().body?.getReader() } catch { /* fixed refusal below */ }
      if (!reader) { publish(kind, { status: 'body_unavailable' }); return }
      let expired = false, published = false
      const finish = value => { if (!published) { published = true; publish(kind, value) } }
      const deadline = Number.isInteger(expected.readDeadlineMs) && expected.readDeadlineMs > 0 && expected.readDeadlineMs <= 15000 ? expected.readDeadlineMs : 15000
      const timer = setTimeout(() => {
        expired = true
        finish({ status: 'body_unavailable' }) // deadline never waits for tee cancellation
        try { void reader.cancel().catch(() => {}) } catch { /* original stream untouched */ }
      }, deadline)
      void (async () => {
        try {
          let length = 0
          const chunks = []
          for (;;) {
            const { done, value } = await reader.read()
            if (expired) throw Error('observation timeout')
            if (done) break
            length += value.length
            if (length > 65536) { void reader.cancel().catch(() => {}); throw Error('bounded observation refused') }
            chunks.push(value)
          }
          const bytes = new Uint8Array(length)
          let offset = 0
          for (const chunk of chunks) { bytes.set(chunk, offset); offset += chunk.length }
          if (expired) throw Error('observation timeout')
          const value = JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(bytes))
          if (expired || issuedDocument !== documentId) return
          if (kind === 'prepare') {
            if (value?.target?.target_id !== expected.targetId) return finish({ status: 'target_mismatch' })
            if (!uuid(value.operation_id)) return finish({ status: 'operation_invalid' })
            return finish({ status: 'bound', operationId: value.operation_id })
          }
          const candidates = Array.isArray(value?.operations) ? value.operations : [value]
          assert(candidates.length <= 16)
          if (!expired) finish({ status: 'bound', operations: candidates.map(project) })
        } catch { finish({ status: 'body_unavailable' }) }
        finally { clearTimeout(timer) }
      })().catch(() => {})
    }, () => {}).catch(() => {})
    return result
  }
}
export function projectObservedOperation(value) { return installPrepareFetchObservation({ mode: 'project', value }) }

// Receiver-side freshness check; caller additionally verifies exact main frame
// and reads the nonce from that frame's current document at callback time.
export function observedOperationForDocument(message, currentDocument, operationId) {
  if (message?.documentId !== currentDocument || message?.kind !== 'operations' || message.value?.status !== 'bound' || !Array.isArray(message.value.operations) || message.value.operations.length > 16) return null
  const operations = message.value.operations.map(projectObservedOperation)
  return operations.find(op => op.operation_id === operationId) ?? null
}
