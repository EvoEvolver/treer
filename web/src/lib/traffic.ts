import type { MachineTrafficRecord } from "@/lib/api"

export const trafficRanges = [
  { hours: 6, label: "Last 6 hours" },
  { hours: 24, label: "Last 24 hours" },
  { hours: 168, label: "Last 7 days" },
  { hours: 720, label: "Last 30 days" },
] as const
export type TrafficHours = typeof trafficRanges[number]["hours"]
export type TrafficMetric = "bytes" | "frames"
export interface TrafficBucket { start: number; bytes: number; frames: number }
const HOUR = 3_600_000
const DAY = 24 * HOUR

// Core includes the current, possibly incomplete hour. Longer views group
// these same records into UTC days; the first/last day may be partial.
export function trafficBuckets(records: MachineTrafficRecord[], hours: TrafficHours, now: number): TrafficBucket[] {
  const end = Math.floor(now / HOUR) * HOUR
  const cutoff = end - (hours - 1) * HOUR
  const step = hours <= 24 ? HOUR : DAY
  const first = Math.floor(cutoff / step) * step
  const buckets = Array.from({ length: Math.floor((end - first) / step) + 1 }, (_, index) => ({ start: first + index * step, bytes: 0, frames: 0 }))
  for (const record of records) {
    const timestamp = Date.parse(record.window_start)
    if (!Number.isFinite(timestamp) || timestamp < cutoff || timestamp >= end + HOUR) continue
    const bucket = buckets[Math.floor((timestamp - first) / step)]
    bucket.bytes += record.billable_bytes ?? record.payload_bytes
    if (record.traffic_class !== "direct_network") bucket.frames += record.payload_frames
  }
  return buckets
}

export function trafficTime(start: number, hours: TrafficHours, full = false) {
  return new Intl.DateTimeFormat(undefined, {
    timeZone: "UTC", month: "short", day: "numeric",
    ...(hours <= 24 || full ? { hour: "2-digit", minute: "2-digit", hourCycle: "h23" as const } : {}),
  }).format(start)
}
