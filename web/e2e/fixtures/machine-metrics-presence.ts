import { parseMachineMetrics, type MetricCurrent } from '../../src/lib/machine-metrics'
import { metricsFixture } from './machine-metrics'

// Exercise the full response parser, keeping historical buckets valid and unchanged.
export function exerciseGpuPresence() {
  const request = { serverId: 'local', from: '2026-10-20T12:25:00Z', to: '2026-10-20T12:40:00Z', maxPoints: 360 }
  const current = (status: MetricCurrent['status']): MetricCurrent => ({ status, value: null, sampled_at: null, age_ms: null })
  const results: { name: string; accepted: boolean }[] = []
  for (const present of [true, false]) {
    const body = metricsFixture(request)
    const gpu = body.gpus[0]; gpu.present = present
    const series = body.series.filter(s => s.gpu_id === gpu.id)
    for (const s of series) s.current = current(present ? 'warming_up' : 'device_absent')
    const check = (name: string, candidate = body) => {
      try { parseMachineMetrics(candidate, request, Date.parse(candidate.generated_at)); results.push({ name, accepted: true }) }
      catch (error) {
        if (!(error instanceof Error) || error.message !== 'invalid_response') throw error
        results.push({ name, accepted: false })
      }
    }
    check(present ? 'present/warming_up' : 'absent/device_absent')
    for (const s of series) {
      const candidate = structuredClone(body)
      candidate.series.find(v => v.kind === s.kind && v.gpu_id === gpu.id)!.current = current(present ? 'device_absent' : 'warming_up')
      check(`${present ? 'present/device_absent' : 'absent/warming_up'}/${s.kind}`, candidate)
    }
  }
  return results
}
