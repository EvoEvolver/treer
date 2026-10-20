import { parseMachineMetrics } from '../../src/lib/machine-metrics'
export interface SpanVectors { from: string; sample_interval_ms: number; vectors: { name: string; span_ns: string; max_points: number; bucket_width_ms: number; expected_counts: number[] }[] }
export function exerciseSpanVectors(data: SpanVectors) {
    const base = BigInt(Date.parse(data.from.slice(0, 19) + 'Z')) * 1000000n + BigInt(data.from.split('.')[1].slice(0, -1))
    const stamp = (ns: bigint) => new Date(Number(ns / 1000000n)).toISOString().slice(0, 19) + '.' + (ns % 1000000000n).toString().padStart(9, '0') + 'Z'
    return data.vectors.map(v => {
      const to = stamp(base + BigInt(v.span_ns))
      const request = { serverId: 'fixture-machine', from: data.from, to, maxPoints: v.max_points }
      const points = v.expected_counts.map((expected, i) => ({
        from: stamp(base + BigInt(i * v.bucket_width_ms) * 1000000n),
        to: i + 1 === v.expected_counts.length ? to : stamp(base + BigInt((i + 1) * v.bucket_width_ms) * 1000000n),
        expected_count: expected, valid_count: 0, status_counts: [], first_sample_at: null, last_sample_at: null,
        mean: null, min: null, max: null, partial: true,
      }))
      const body = {
        schema: 'machine.metrics.v1', server_id: request.serverId, generated_at: to, from: data.from, to,
        retained_from: null, truncated: false, sample_interval_ms: data.sample_interval_ms,
        bucket_width_ms: v.bucket_width_ms, stale_after_ms: 15000, logical_cpus: null,
        memory_method: 'unavailable', scope: 'os_visible_machine', gpus: [], inventory_truncated: false, gpu_status: 'no_device',
        series: ['cpu_utilization', 'ram_used', 'ram_available', 'ram_total'].map(kind => ({
          kind, gpu_id: null, unit: kind === 'cpu_utilization' ? 'percent' : 'bytes',
          current: { status: 'warming_up', value: null, sampled_at: null, age_ms: null }, points: structuredClone(points),
        })),
      }
      const received = Date.parse(to)
      const parsed = parseMachineMetrics(body, request, received)
      const counts = parsed.series[0].points.map(p => p.expected_count)
      body.series[0].points.at(-1)!.expected_count--
      let decrementedRefused = false
      try { parseMachineMetrics(body, request, received) } catch { decrementedRefused = true }
      return { name: v.name, counts, decrementedRefused }
    })
}
