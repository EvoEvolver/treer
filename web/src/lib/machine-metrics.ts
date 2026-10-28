// Machine hardware metrics (machine.metrics.v1). The Controller keeps the
// history in memory; the Console asks for a window and keeps nothing.
import { api, type Machine } from "@/lib/api"

export const MACHINE_METRICS_CAPABILITY = "machine.metrics.v1"

export interface GpuSample {
  index: number
  utilization_percent?: number
  memory_used_bytes?: number
  memory_total_bytes?: number
}

export interface MachineMetricsSample {
  at: string
  cpu_percent: number
  memory_used_bytes: number
  memory_total_bytes: number
  gpus?: GpuSample[]
}

export interface MachineMetrics {
  cpu_count: number
  gpu_status: "available" | "not_detected" | "unsupported"
  gpus: { index: number; name: string }[]
  sample_seconds: number
  resolution_seconds: number
  window_seconds: number
  samples: MachineMetricsSample[]
  latest?: MachineMetricsSample
}

export const METRICS_RANGES = [
  { key: "15m", seconds: 15 * 60 },
  { key: "1h", seconds: 60 * 60 },
  { key: "6h", seconds: 6 * 60 * 60 },
  { key: "24h", seconds: 24 * 60 * 60 },
] as const
export type MetricsRange = (typeof METRICS_RANGES)[number]["key"]

export function supportsMachineMetrics(machine: Machine) {
  return Boolean(machine.capabilities?.includes(MACHINE_METRICS_CAPABILITY))
}

export function fetchMachineMetrics(workspaceId: string, serverId: string, windowSeconds: number) {
  return api<MachineMetrics>(`/api/workspaces/${encodeURIComponent(workspaceId)}/machines/${encodeURIComponent(serverId)}/metrics?window_seconds=${windowSeconds}`, { cache: "no-store" })
}

/** One plotted point: time (ms) and a 0..100 percentage. */
export interface MetricPoint { at: number; value: number }

/** Percent series; a missing value leaves a gap instead of a zero. */
export function series(metrics: MachineMetrics, pick: (sample: MachineMetricsSample) => number | undefined): MetricPoint[] {
  const points: MetricPoint[] = []
  for (const sample of metrics.samples) {
    const value = pick(sample)
    if (value !== undefined && Number.isFinite(value)) points.push({ at: Date.parse(sample.at), value: Math.max(0, Math.min(100, value)) })
  }
  return points
}

export const memoryPercent = (used?: number, total?: number) => used === undefined || !total ? undefined : used / total * 100
export const gpuOf = (sample: MachineMetricsSample | undefined, index: number) => sample?.gpus?.find((gpu) => gpu.index === index)

/** Splits a series where consecutive points are further apart than `gapMs`. */
export function segments(points: MetricPoint[], gapMs: number): MetricPoint[][] {
  const result: MetricPoint[][] = []
  for (const point of points) {
    const current = result.at(-1)
    if (current && point.at - current[current.length - 1].at <= gapMs) current.push(point)
    else result.push([point])
  }
  return result
}
