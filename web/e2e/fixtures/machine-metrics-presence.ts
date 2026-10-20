import { parseMachineMetrics, type MetricCurrent, type MetricKind } from '../../src/lib/machine-metrics'
import { metricsFixture } from './machine-metrics'

type GpuKind = Extract<MetricKind, `gpu_${string}`>
export interface GpuPresenceVectors {
  schema: 1
  vectors: { name: string; present: boolean; statuses: Record<GpuKind, 'warming_up' | 'device_absent'>; accepted: boolean }[]
}

// Exercise the full response parser, keeping historical buckets valid and unchanged.
export function exerciseGpuPresence(data: GpuPresenceVectors) {
  const request = { serverId: 'local', from: '2026-10-20T12:25:00Z', to: '2026-10-20T12:40:00Z', maxPoints: 360 }
  const current = (status: MetricCurrent['status']): MetricCurrent => ({ status, value: null, sampled_at: null, age_ms: null })
  const results: { name: string; accepted: boolean }[] = []
  for (const vector of data.vectors) {
    const body = metricsFixture(request)
    const gpu = body.gpus[0]; gpu.present = vector.present
    const series = body.series.filter(s => s.gpu_id === gpu.id)
    for (const s of series) s.current = current(vector.statuses[s.kind as GpuKind])
    try { parseMachineMetrics(body, request, Date.parse(body.generated_at)); results.push({ name: vector.name, accepted: true }) }
    catch (error) {
      if (!(error instanceof Error) || error.message !== 'invalid_response') throw error
      results.push({ name: vector.name, accepted: false })
    }
  }
  return results
}
