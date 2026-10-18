import { ApiError, proxyUrl, type MachineFileUpload } from '@/lib/api'

export const UPLOAD_CAPABILITY = 'machine.upload.v3'
export const MIB = 1024 * 1024
export const MAX_UPLOAD_BYTES = 128 * MIB
export type UploadPhase = 'idle' | 'preparing' | 'uploading' | 'waiting' | 'finishing' | 'cancelling' | 'paused' | 'succeeded' | 'cancelled' | 'cleanup_pending'
export interface UploadStatus {
  upload_id: string
  size: number
  committed_offset: number
  part_bytes_max: number
  state: 'pending' | 'active' | 'committing' | 'committed' | 'aborted' | 'expired' | 'failed' | 'unknown'
  failure_code?: string
  result?: MachineFileUpload & { sha256: string; committed_at: string }
  reconciled: boolean
  cleanup_pending?: boolean
}
export interface UploadView {
  phase: UploadPhase
  loaded: number
  total: number
  error?: string
  result?: MachineFileUpload
  resumable?: boolean
  uncertain?: boolean
}
class UploadError extends ApiError {
  constructor(status: number, code?: string, readonly retryAfter = 0) { super(code ?? 'upload_failed', status, code) }
}
function aborted() { return new DOMException('Aborted', 'AbortError') }
function isAbort(error: unknown) { return error instanceof DOMException && error.name === 'AbortError' }
function transient(error: unknown) {
  return error instanceof UploadError && ([0, 408, 429, 502, 503, 504].includes(error.status) || ['upload_busy', 'upload_committing'].includes(error.code ?? ''))
}
function wait(ms: number, signal: AbortSignal) {
  return new Promise<void>((resolve, reject) => {
    if (signal.aborted) return reject(aborted())
    const cancel = () => { clearTimeout(timer); reject(aborted()) }
    const timer = window.setTimeout(() => { signal.removeEventListener('abort', cancel); resolve() }, ms)
    signal.addEventListener('abort', cancel, { once: true })
  })
}
function request<T>(path: string, method: string, signal: AbortSignal, body?: Blob | string, headers: Record<string, string> = {}, progress?: (loaded: number) => void): Promise<T> {
  return new Promise((resolve, reject) => {
    if (signal.aborted) return reject(aborted())
    const xhr = new XMLHttpRequest()
    xhr.open(method, proxyUrl(path))
    xhr.withCredentials = true
    xhr.timeout = body instanceof Blob ? 130_000 : 15_000
    if (typeof body === 'string') xhr.setRequestHeader('Content-Type', 'application/json')
    for (const [name, value] of Object.entries(headers)) xhr.setRequestHeader(name, value)
    const cancel = () => xhr.abort()
    const finish = (action: () => void) => { signal.removeEventListener('abort', cancel); action() }
    if (progress) xhr.upload.onprogress = (event) => progress(event.loaded)
    xhr.onload = () => finish(() => {
      let value: { error?: { code?: string } }
      try { value = JSON.parse(xhr.responseText) } catch { return reject(new UploadError(xhr.status)) }
      if (xhr.status >= 200 && xhr.status < 300) resolve(value as T)
      else {
        const raw = xhr.getResponseHeader('Retry-After')
        const seconds = raw ? (/^\d+$/.test(raw) ? Number(raw) : Math.max(0, (Date.parse(raw) - Date.now()) / 1000)) : 0
        reject(new UploadError(xhr.status, value.error?.code, Number.isFinite(seconds) ? seconds : 0))
      }
    })
    xhr.onerror = xhr.ontimeout = () => finish(() => reject(new UploadError(0)))
    xhr.onabort = () => finish(() => reject(aborted()))
    signal.addEventListener('abort', cancel, { once: true })
    xhr.send(body ?? null)
  })
}

// One in-memory upload, bound by the owning dialog to an account/workspace/machine.
// Dispose only aborts local work: it must not send a cleanup request with a new
// account's cookies after the scope changed. Server expiry is the backstop.
export class MachineUpload {
  private control = new AbortController()
  private disposed = false
  private cancelling = false
  private running: Promise<void> | null = null
  private id?: string
  private key?: string
  private file?: File
  private parameters?: { directory: string; file_name: string; overwrite: boolean; size: number }
  private committed = 0
  private partSize = MIB
  private partMax = 8 * MIB
  private commitStarted = false
  private legacy = false
  private view: UploadView = { phase: 'idle', loaded: 0, total: 0 }
  constructor(private base: string, private emit: (view: UploadView) => void) {}
  private update(change: Partial<UploadView>) {
    this.view = { ...this.view, ...change }
    if (!this.disposed) this.emit(this.view)
  }
  dispose() { this.disposed = true; this.control.abort() }
  get snapshot() { return this.view }
  private async backoff(error: UploadError, attempt: number, signal: AbortSignal) {
    if (error.retryAfter > 60) throw error // Do not retry sooner than the server permits.
    this.update({ phase: this.cancelling ? 'cancelling' : this.commitStarted ? 'finishing' : 'waiting', loaded: this.committed })
    await wait(Math.max(error.retryAfter * 1000, Math.min(8000, 500 * 2 ** attempt)) + Math.random() * 200, signal)
  }
  // Rate limiting has its own finite budget; server throttling must
  // not exhaust the three retries reserved for an ambiguous transport failure.
  private async retry<T>(operation: () => Promise<T>, signal: AbortSignal): Promise<T> {
    let failures = 0, throttles = 0
    for (;;) {
      try { return await operation() } catch (error) {
        if (!(error instanceof UploadError) || !transient(error)) throw error
        if (error.status === 429 ? ++throttles > 10 : ++failures > 3) throw error
        await this.backoff(error, error.status === 429 ? throttles : failures, signal)
      }
    }
  }
  private check(status: UploadStatus) {
    if (!this.file || status.size !== this.file.size || !Number.isSafeInteger(status.committed_offset) || status.committed_offset < 0 || status.committed_offset > status.size || status.upload_id !== this.id) throw new UploadError(400, 'upload_invalid_response')
    // A forgotten receipt cannot prove that publication failed. Keep this
    // distinct from a definitive Controller refusal, including on later polls.
    if (status.state === 'unknown' || status.failure_code === 'outcome_unknown') {
      this.commitStarted = true
      throw new UploadError(409, 'outcome_unknown')
    }
    if (status.state === 'committed') {
      if (!status.result || status.result.bytes_written !== this.file.size || typeof status.result.path !== 'string') throw new UploadError(400, 'upload_invalid_response')
      this.committed = this.file.size
      this.update({ phase: 'succeeded', loaded: this.file.size, result: status.result, error: undefined, resumable: false, uncertain: false })
      return true
    }
    if (['aborted', 'expired', 'failed'].includes(status.state)) {
      this.commitStarted = false // An authoritative terminal refusal resolves uncertainty.
      throw new UploadError(400, status.failure_code ?? `upload_${status.state}`)
    }
    if (!status.reconciled) throw new UploadError(503, 'controller_unavailable')
    this.committed = status.committed_offset
    if (status.state === 'committing') this.commitStarted = true
    if (!Number.isSafeInteger(status.part_bytes_max) || status.part_bytes_max < MIB) throw new UploadError(400, 'upload_invalid_response')
    this.partMax = Math.min(status.part_bytes_max, 8 * MIB)
    return false
  }
  private async status(signal: AbortSignal) {
    return this.retry(() => request<UploadStatus>(`${this.base}/uploads/${encodeURIComponent(this.id!)}`, 'GET', signal), signal)
  }
  start(file: File, directory: string, overwrite: boolean, legacy: boolean) {
    if (this.running) return this.running
    this.file = file
    this.parameters = { directory, file_name: file.name, overwrite, size: file.size }
    this.legacy = legacy
    this.key = legacy ? undefined : Array.from(crypto.getRandomValues(new Uint8Array(16)), (value) => value.toString(16).padStart(2, '0')).join('')
    return this.resume()
  }
  resume() {
    if (this.running || !this.file || this.disposed) return this.running ?? Promise.resolve()
    this.control = new AbortController()
    this.update({ phase: this.commitStarted ? 'finishing' : 'preparing', error: undefined, uncertain: this.commitStarted, resumable: false })
    this.running = this.run(this.control.signal).catch((error: unknown) => {
      if (isAbort(error) || this.disposed) return
      if (error instanceof ApiError && error.code === 'outcome_unknown') this.commitStarted = true
      this.update({ phase: 'paused', loaded: this.committed, error: error instanceof ApiError ? error.code ?? 'upload_failed' : 'upload_failed', resumable: !this.legacy && (this.commitStarted || transient(error) || error instanceof UploadError && ['upload_busy', 'upload_committing', 'controller_unavailable'].includes(error.code ?? '')), uncertain: this.commitStarted || this.legacy })
    }).finally(() => { this.running = null })
    return this.running
  }
  private async run(signal: AbortSignal) {
    const file = this.file!
    this.update({ total: file.size })
    if (this.legacy) {
      // Compatibility only: this path is capped at 16 MiB before start.
      const data = new Uint8Array(await file.arrayBuffer())
      if (signal.aborted) throw aborted()
      let text = ''
      for (let i = 0; i < data.length; i += 8192) text += String.fromCharCode(...data.subarray(i, i + 8192))
      this.update({ phase: 'uploading' })
      const result = await request<MachineFileUpload>(`${this.base}/files`, 'POST', signal, JSON.stringify({ directory: this.parameters!.directory, file_name: file.name, overwrite: this.parameters!.overwrite, content_base64: btoa(text) }), {}, () => {})
      this.update({ phase: 'succeeded', loaded: file.size, result, uncertain: false })
      return
    }
    if (!this.id) {
      const begun = await this.retry(() => request<UploadStatus>(`${this.base}/uploads`, 'POST', signal, JSON.stringify(this.parameters), { 'Idempotency-Key': this.key! }), signal)
      if (typeof begun.upload_id !== 'string' || !/^upl_[a-f0-9]{32,64}$/.test(begun.upload_id)) {
        // Begin may have created staging before returning an incompatible id.
        // Only address a bounded Controller handle, never a path or URL, and
        // use this dialog's original scope and cancellation signal.
        if (typeof begun.upload_id === 'string' && /^[A-Za-z0-9_]{8,80}$/.test(begun.upload_id)) {
          this.id = begun.upload_id
          try { await request(`${this.base}/uploads/${encodeURIComponent(this.id)}`, 'DELETE', signal) } catch { /* Server expiry remains the backstop. */ }
        }
        throw new UploadError(400, 'upload_invalid_response')
      }
      this.id = begun.upload_id
      // Begin's compact response does not contain state/reconciled; GET is the
      // authority for a replayed begin that may already have committed.
    }
    if (signal.aborted) throw aborted()
    if (this.check(await this.status(signal))) return
    if (this.commitStarted) return this.finish(signal)
    let failures = 0, throttles = 0
    while (this.committed < file.size) {
      const offset = this.committed
      const length = Math.min(this.partSize, this.partMax, file.size - offset)
      const part = file.slice(offset, offset + length)
      const bytes = await part.arrayBuffer()
      const digest = await crypto.subtle.digest('SHA-256', bytes)
      const hash = Array.from(new Uint8Array(digest), (value) => value.toString(16).padStart(2, '0')).join('')
      if (signal.aborted) throw aborted()
      this.update({ phase: 'uploading', loaded: offset })
      const started = performance.now()
      try {
        const reply = await request<{ committed_offset: number; size: number }>(`${this.base}/uploads/${encodeURIComponent(this.id!)}/parts?offset=${offset}`, 'PUT', signal, part, { 'Content-Type': 'application/octet-stream', 'X-Treer-Part-Sha256': hash }, (loaded) => this.update({ loaded: Math.min(offset + loaded, file.size) }))
        if (reply.size !== file.size || !Number.isSafeInteger(reply.committed_offset) || reply.committed_offset < offset + length || reply.committed_offset > file.size) throw new UploadError(400, 'upload_invalid_response')
        this.committed = reply.committed_offset
        failures = 0; throttles = 0
        // Target roughly 15 seconds per part, within the 120 s server limit.
        const target = length / Math.max(1, (performance.now() - started) / 1000) * 15
        this.partSize = Math.max(MIB, Math.min(this.partMax, Math.floor(target / MIB) * MIB))
      } catch (error) {
        const offsetMismatch = error instanceof UploadError && ['upload_offset_gap', 'upload_busy'].includes(error.code ?? '')
        if (!(error instanceof UploadError) || !transient(error) && !offsetMismatch) throw error
        if (error.status === 429 ? ++throttles > 10 : ++failures > 3) throw error
        await this.backoff(error, error.status === 429 ? throttles : failures, signal)
        if (this.check(await this.status(signal))) return
        this.partSize = Math.max(MIB, Math.floor(this.partSize / 2 / MIB) * MIB)
        if (this.commitStarted) return this.finish(signal)
      }
    }
    await this.finish(signal)
  }
  private async finish(signal: AbortSignal) {
    this.commitStarted = true
    this.update({ phase: 'finishing', loaded: this.file!.size, uncertain: true })
    // The result may have been published even when POST failed. Only a terminal
    // receipt can claim success; never infer it from machine_file_exists.
    for (let attempt = 0; attempt <= 3; attempt++) {
      try {
        const result = await request<UploadStatus | (MachineFileUpload & { sha256: string; committed_at: string })>(`${this.base}/uploads/${encodeURIComponent(this.id!)}/commit`, 'POST', signal)
        if ('state' in result) { if (this.check(result)) return }
        else if (result.bytes_written === this.file!.size && typeof result.path === 'string' && typeof result.committed_at === 'string' && typeof result.sha256 === 'string') {
          this.update({ phase: 'succeeded', loaded: this.file!.size, result, uncertain: false, resumable: false }); return
        } else throw new UploadError(400, 'upload_invalid_response')
      } catch (error) {
        if (!(error instanceof UploadError) || !transient(error) && !['upload_busy', 'upload_committing'].includes(error.code ?? '')) throw error
        await this.backoff(error, attempt, signal)
      }
      if (this.check(await this.status(signal))) return
      if (attempt === 3) throw new UploadError(503, 'upload_outcome_unknown')
    }
  }
  async cancel() {
    if (this.commitStarted || this.disposed || this.cancelling) return
    this.cancelling = true
    this.update({ phase: 'cancelling', resumable: false, error: undefined })
    this.control.abort()
    await this.running
    if (this.disposed) return
    this.control = new AbortController()
    const signal = this.control.signal
    this.update({ phase: 'cancelling', resumable: false, error: undefined })
    try {
      if (this.legacy) { this.update({ phase: 'paused', uncertain: true, error: 'upload_outcome_unknown' }); return }
      if (!this.id && this.key) {
        const list = await request<{ uploads: UploadStatus[] }>(`${this.base}/uploads?idempotency_key=${encodeURIComponent(this.key)}`, 'GET', signal)
        this.id = list.uploads[0]?.upload_id
      }
      if (!this.id) { this.update({ phase: 'cleanup_pending' }); return }
      for (let attempt = 0; attempt <= 3; attempt++) {
        try {
          const result = await request<UploadStatus>(`${this.base}/uploads/${encodeURIComponent(this.id)}`, 'DELETE', signal)
          if (result.state === 'committed') { this.check(result); return }
          if (result.state !== 'aborted' || result.cleanup_pending === undefined) throw new UploadError(400, 'upload_invalid_response')
          this.update({ phase: result.cleanup_pending ? 'cleanup_pending' : 'cancelled', uncertain: false, resumable: false }); return
        } catch (error) {
          if (error instanceof UploadError && ['upload_committing', 'upload_committed'].includes(error.code ?? '')) {
            this.commitStarted = true
            if (!this.check(await this.status(signal))) this.update({ phase: 'paused', uncertain: true, resumable: true, error: 'upload_outcome_unknown' })
            return
          }
          if (attempt === 3 || !(error instanceof UploadError) || !transient(error) && error.code !== 'upload_busy') throw error
          await this.backoff(error, attempt, signal)
          this.update({ phase: 'cancelling' })
        }
      }
    } catch (error) {
      if (!isAbort(error)) this.update({ phase: 'cleanup_pending', error: 'upload_cleanup_pending' })
    } finally { this.cancelling = false }
  }
}
