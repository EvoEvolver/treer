// Browser projection of treer-protocol machine_metrics.rs, checkpoint 3f1b84d8.
// This module keeps no cache and never persists samples.
import { proxyUrl } from './api'
export const metricStatuses = ['ok', 'warming_up', 'no_device', 'driver_unavailable', 'unsupported_platform', 'unsupported_metric', 'collection_failed', 'timed_out', 'stale', 'device_absent', 'disabled'] as const
export type MetricStatus = typeof metricStatuses[number]
export const metricKinds = ['cpu_utilization', 'ram_used', 'ram_available', 'ram_total', 'gpu_utilization', 'gpu_memory_used', 'gpu_memory_total'] as const
export type MetricKind = typeof metricKinds[number]
export type MetricsRange = 15 | 60 | 360 | 1440
export interface MetricCurrent { status: MetricStatus; value: number | null; sampled_at: string | null; age_ms: number | null }
export interface MetricBucket { from: string; to: string; expected_count: number; valid_count: number; status_counts: { status: MetricStatus; count: number }[]; first_sample_at: string | null; last_sample_at: string | null; mean: number | null; min: number | null; max: number | null; partial: boolean }
export interface MetricSeries { kind: MetricKind; gpu_id: string | null; unit: 'percent' | 'bytes'; current: MetricCurrent; points: MetricBucket[] }
export interface MachineMetrics { schema: 'machine.metrics.v1'; server_id: string; generated_at: string; from: string; to: string; retained_from: string | null; truncated: boolean; sample_interval_ms: number; bucket_width_ms: number; stale_after_ms: number; logical_cpus: number | null; memory_method: 'linux_mem_available' | 'macos_available_estimate' | 'windows_physical_available' | 'unavailable'; scope: 'os_visible_machine'; gpus: { id: string; name: string; present: boolean }[]; inventory_truncated: boolean; gpu_status: MetricStatus; series: MetricSeries[] }
export type MetricsFailure = 'forbidden' | 'offline' | 'unsupported' | 'disabled' | 'busy' | 'timeout' | 'invalid_response' | 'unavailable'
export class MetricsError extends Error { constructor(public readonly reason: MetricsFailure) { super(reason) } }
const fail = (): never => { throw new MetricsError('invalid_response') }
const object = (v: unknown): Record<string, unknown> => v !== null && typeof v === 'object' && !Array.isArray(v) ? v as Record<string, unknown> : fail()
const array = (v: unknown, cap: number): unknown[] => Array.isArray(v) && v.length <= cap ? v : fail()
const bool = (v: unknown): boolean => typeof v === 'boolean' ? v : fail()
const text = (v: unknown, cap: number): string => typeof v === 'string' && new TextEncoder().encode(v).length <= cap && !/[\u0000-\u001f\u007f-\u009f\u200b-\u200f\u202a-\u202e\u2066-\u2069\ufeff]/.test(v) ? v : fail()
const number = (v: unknown, max = Number.MAX_SAFE_INTEGER): number => typeof v === 'number' && Number.isFinite(v) && v >= 0 && v <= max ? v : fail()
const integer = (v: unknown, max = Number.MAX_SAFE_INTEGER): number => Number.isSafeInteger(number(v, max)) ? v as number : fail()
const enumeration = <T extends string>(v: unknown, values: readonly T[]): T => typeof v === 'string' && values.includes(v as T) ? v as T : fail()
const stamp = (v: unknown): string => { const s = text(v, 40); return /^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d(?:\.\d{1,9})?Z$/.test(s) && Number.isFinite(Date.parse(s)) ? s : fail() }
const optionalStamp = (v: unknown) => v === null ? null : stamp(v)
const status = (v: unknown) => enumeration(v, metricStatuses)
export interface MetricsRequest { serverId: string; from: string; to: string; maxPoints: number }

export function parseMachineMetrics(input: unknown, request: MetricsRequest, receivedAt = Date.now()): MachineMetrics {
  const r = object(input), from = stamp(r.from), to = stamp(r.to), generated = stamp(r.generated_at)
  if (r.schema !== 'machine.metrics.v1' || r.server_id !== request.serverId || Date.parse(from) !== Date.parse(request.from) || Date.parse(to) !== Date.parse(request.to) || Date.parse(to) <= Date.parse(from) || Date.parse(to) - Date.parse(from) > 86400000 || Date.parse(generated) < Date.parse(to) || Date.parse(generated) > receivedAt + 15000 || r.scope !== 'os_visible_machine') fail()
  const sampleInterval = integer(r.sample_interval_ms), bucketWidth = integer(r.bucket_width_ms), staleAfter = integer(r.stale_after_ms)
  if (sampleInterval !== 5000 || bucketWidth < sampleInterval || bucketWidth > 86400000 || staleAfter !== 15000) fail()
  const gpus = array(r.gpus, 32).map(v => { const g = object(v), id = text(g.id, 128); if (!/^[a-f0-9]{64}$/.test(id)) fail(); return { id, name: text(g.name, 128) || fail(), present: bool(g.present) } })
  const ids = new Set(gpus.map(g => g.id)); if (ids.size !== gpus.length) fail()
  const keys = new Set<string>(); let total = 0; let bounds: string[] | undefined
  const series = array(r.series, 100).map(v => {
    const s = object(v), kind = enumeration(s.kind, metricKinds), gpu = kind.startsWith('gpu_'), gpuId = s.gpu_id === null ? null : text(s.gpu_id, 128)
    if (gpu ? !gpuId || !ids.has(gpuId) : gpuId !== null) fail()
    const key = kind + '/' + gpuId; if (keys.has(key)) fail(); keys.add(key)
    const unit = enumeration(s.unit, ['percent', 'bytes'] as const); if ((unit === 'percent') !== kind.endsWith('utilization')) fail()
    const value = (v: unknown) => v === null ? null : number(v, unit === 'percent' ? 100 : Number.MAX_SAFE_INTEGER)
    const c = object(s.current), state = status(c.status), sampled = optionalStamp(c.sampled_at), age = c.age_ms === null ? null : integer(c.age_ms), currentValue = value(c.value)
    if ((sampled === null) !== (age === null) || (sampled !== null && Date.parse(sampled) > Date.parse(generated)) || (state === 'ok' ? currentValue === null || age === null || age > staleAfter : currentValue !== null)) fail()
    if ((sampled !== null && age !== Date.parse(generated) - Date.parse(sampled)) || (state === 'stale' && (age === null || age <= staleAfter)) || (currentValue !== null && unit === 'bytes' && !Number.isSafeInteger(currentValue)) || (gpuId && !gpus.find(g => g.id === gpuId)!.present && state !== 'device_absent')) fail()
    const points = array(s.points, Math.min(720, request.maxPoints)).map(v => {
      const b = object(v), a = stamp(b.from), z = stamp(b.to), expected = integer(b.expected_count), valid = integer(b.valid_count), partial = bool(b.partial)
      if (Date.parse(a) < Date.parse(from) || Date.parse(z) > Date.parse(to) || Date.parse(a) >= Date.parse(z) || valid > expected || expected !== Math.ceil((Date.parse(z) - Date.parse(a)) / sampleInterval) || Date.parse(z) !== Math.min(Date.parse(a) + bucketWidth, Date.parse(to))) fail()
      const seen = new Set<MetricStatus>(); let observed = 0, ok = 0
      const counts = array(b.status_counts, metricStatuses.length).map(v => { const c = object(v), st = status(c.status), n = integer(c.count); if (!n || seen.has(st)) fail(); seen.add(st); observed += n; if (st === 'ok') ok = n; return { status: st, count: n } })
      if (observed > expected || ok !== valid || (!partial && (valid !== expected || !valid))) fail()
      const first = optionalStamp(b.first_sample_at), last = optionalStamp(b.last_sample_at), mean = value(b.mean), min = value(b.min), max = value(b.max)
      if (valid ? first === null || last === null || mean === null || min === null || max === null || min > mean || mean > max || Date.parse(first) < Date.parse(a) || Date.parse(last) >= Date.parse(z) || Date.parse(first) > Date.parse(last) : first !== null || last !== null || mean !== null || min !== null || max !== null) fail()
      return { from: a, to: z, expected_count: expected, valid_count: valid, status_counts: counts, first_sample_at: first, last_sample_at: last, mean, min, max, partial }
    })
    total += points.length; if (total > 7200) fail()
    const ownBounds = points.map((p, i) => { if (i && Date.parse(points[i - 1].to) !== Date.parse(p.from)) fail(); return p.from + '/' + p.to })
    if (points.length && (Date.parse(points[0].from) !== Date.parse(from) || Date.parse(points.at(-1)!.to) !== Date.parse(to))) fail()
    if (bounds && (bounds.length !== ownBounds.length || bounds.some((b, i) => b !== ownBounds[i]))) fail(); bounds = ownBounds
    return { kind, gpu_id: gpuId, unit, current: { status: state, value: currentValue, sampled_at: sampled, age_ms: age }, points }
  })
  if (keys.size !== 4 + gpus.length * 3) fail()
  for (const kind of metricKinds.slice(0, 4)) if (!keys.has(kind + '/null')) fail()
  for (const gpu of gpus) for (const kind of metricKinds.slice(4)) if (!keys.has(kind + '/' + gpu.id)) fail()
  // Paired current values must describe the same acquisition, never today's capacity with old use.
  for (const gpuId of [null, ...ids]) {
    const paired = series.filter(s => s.gpu_id === gpuId && (gpuId ? s.kind !== 'gpu_utilization' : s.kind !== 'cpu_utilization'))
    const total = paired.find(s => s.kind.endsWith('total'))!, used = paired.find(s => s.kind.endsWith('used'))!
    if (total.current.value !== null && used.current.value !== null && (total.current.sampled_at !== used.current.sampled_at || used.current.value > total.current.value)) fail()
    const available = paired.find(s => s.kind === 'ram_available')
    if (available?.current.value !== null && available?.current.value !== undefined && total.current.value !== null && (available.current.sampled_at !== total.current.sampled_at || available.current.value > total.current.value || (used.current.value !== null && used.current.value !== total.current.value - available.current.value))) fail()
  }
  const retained = optionalStamp(r.retained_from);
  for (const s of series) for (const p of s.points) if (p.first_sample_at !== null && (retained === null || Date.parse(p.first_sample_at) < Date.parse(retained) || Date.parse(generated) - Date.parse(p.first_sample_at) > 86400000)) fail(); if (retained !== null && (Date.parse(retained) > Date.parse(generated) || Date.parse(generated) - Date.parse(retained) > 86400000)) fail()
  return { schema: 'machine.metrics.v1', server_id: request.serverId, from, to, generated_at: generated, retained_from: retained, truncated: bool(r.truncated), sample_interval_ms: sampleInterval, bucket_width_ms: bucketWidth, stale_after_ms: staleAfter, logical_cpus: r.logical_cpus === null ? null : integer(r.logical_cpus, 65535) || fail(), memory_method: enumeration(r.memory_method, ['linux_mem_available', 'macos_available_estimate', 'windows_physical_available', 'unavailable'] as const), scope: 'os_visible_machine', gpus, inventory_truncated: bool(r.inventory_truncated), gpu_status: status(r.gpu_status), series }
}

export async function fetchMachineMetrics(workspaceId: string, request: MetricsRequest, signal: AbortSignal): Promise<MachineMetrics> {
  const q = new URLSearchParams({ from: request.from, to: request.to, max_points: String(request.maxPoints) })
  const response = await fetch(proxyUrl(`/api/workspaces/${encodeURIComponent(workspaceId)}/machines/${encodeURIComponent(request.serverId)}/metrics?${q}`), { credentials: 'include', cache: 'no-store', redirect: 'error', signal })
  if (!response.headers.get('cache-control')?.split(',').some(v => v.trim().toLowerCase() === 'no-store')) throw new MetricsError('invalid_response')
  const reader = response.body?.getReader(); if (!reader) throw new MetricsError('invalid_response')
  let size = 0; const chunks: Uint8Array[] = []
  try { while (true) { const { done, value } = await reader.read(); if (done) break; size += value.byteLength; if (size > 1048576) fail(); chunks.push(value) } } finally { await reader.cancel() }
  const bytes = new Uint8Array(size); let offset = 0; for (const chunk of chunks) { bytes.set(chunk, offset); offset += chunk.length }
  let body: unknown; try { body = JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(bytes)) } catch { return fail() }
  if (!response.ok) {
    const error = object(object(body).error), code = error.code
    const reasons: Record<string, MetricsFailure> = { machine_metrics_forbidden: 'forbidden', machine_metrics_offline: 'offline', machine_metrics_unsupported: 'unsupported', machine_metrics_disabled: 'disabled', machine_metrics_busy: 'busy', machine_metrics_timeout: 'timeout', machine_metrics_invalid_response: 'invalid_response' }
    throw new MetricsError(response.status === 401 || response.status === 403 ? 'forbidden' : typeof code === 'string' ? reasons[code] ?? 'unavailable' : 'unavailable')
  }
  if (response.status !== 200) fail()
  return parseMachineMetrics(body, request)
}
