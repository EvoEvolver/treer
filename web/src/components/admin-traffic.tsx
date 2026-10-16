import { useEffect, useState } from "react"
import { api, ApiError, type MachineTrafficRecord } from "@/lib/api"
import { formatBytes } from "@/lib/control-plane"
import { trafficBuckets, trafficRanges, type TrafficHours, type TrafficMetric } from "@/lib/traffic"
import { TrafficChart } from "@/components/traffic-chart"
import { Button } from "@/components/ui/button"
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select"
import { intlLocale, t } from "@/i18n"

type AggregateTraffic = Pick<MachineTrafficRecord, "window_start" | "traffic_class" | "payload_bytes" | "payload_frames" | "billable_bytes">
const classes = ["virtual_network", "service_ingress", "virtual_host", "agent_interface", "direct_network"] as const

// Project only the aggregate contract. Never turn unexpected server fields into
// labels, routes or error details; older Proxy responses cannot become a fallback.
function aggregateResponse(value: unknown, hours: TrafficHours): AggregateTraffic[] {
  if (!value || typeof value !== "object") throw new Error()
  const data = value as Record<string, unknown>
  if (data.hours !== hours || !Array.isArray(data.traffic) || data.traffic.length > 720 * classes.length) throw new Error()
  return data.traffic.map((entry: unknown) => {
    if (!entry || typeof entry !== "object") throw new Error()
    const row = entry as Record<string, unknown>
    if (typeof row.window_start !== "string" || !Number.isFinite(Date.parse(row.window_start)) || !classes.includes(row.traffic_class as typeof classes[number])) throw new Error()
    for (const key of ["payload_bytes", "payload_frames", "billable_bytes"]) if (typeof row[key] !== "number" || !Number.isFinite(row[key]) || row[key] < 0) throw new Error()
    return { window_start: row.window_start, traffic_class: row.traffic_class as AggregateTraffic["traffic_class"], payload_bytes: row.payload_bytes as number, payload_frames: row.payload_frames as number, billable_bytes: row.billable_bytes as number }
  })
}

export function AdminTraffic({ preview, refreshRevision }: { preview: boolean; refreshRevision: number }) {
  const [hours, setHours] = useState<TrafficHours>(24)
  const [metric, setMetric] = useState<TrafficMetric>("bytes")
  const [data, setData] = useState<{ hours: TrafficHours; rows: AggregateTraffic[]; at: number } | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [loading, setLoading] = useState(true)
  const [retry, setRetry] = useState(0)
  useEffect(() => {
    let active = true
    let pending: AbortController | null = null
    setData(null); setError(null)
    async function load() {
      if (pending) return
      const controller = new AbortController(); pending = controller
      setLoading(true)
      try {
        const rows = preview ? [] : aggregateResponse(await api<unknown>(`/api/admin/traffic?hours=${hours}`, { signal: controller.signal, cache: "no-store" }), hours)
        if (active && !controller.signal.aborted) { setData({ hours, rows, at: Date.now() }); setError(null) }
      } catch (reason) {
        if (active && !controller.signal.aborted) {
          setData(null)
          setError(reason instanceof ApiError && [404, 405].includes(reason.status) ? t("admin.traffic_unsupported") : reason instanceof ApiError ? t("traffic.load_failed_http", { status: `HTTP ${reason.status}` }) : t("traffic.load_failed"))
        }
      } finally {
        controller.abort(); pending = null
        if (active) setLoading(false)
      }
    }
    void load()
    const timer = preview ? undefined : window.setInterval(() => void load(), 10_000)
    return () => { active = false; pending?.abort(); window.clearInterval(timer) }
  }, [hours, refreshRevision, retry, preview])
  const current = data?.hours === hours ? data : null
  const rows = current?.rows ?? []
  const bytes = rows.reduce((sum, row) => sum + row.billable_bytes!, 0)
  const frames = rows.filter((row) => row.traffic_class !== "direct_network").reduce((sum, row) => sum + row.payload_frames, 0)
  const range = t(trafficRanges.find((entry) => entry.hours === hours)!.labelKey)
  return <section aria-label={t("admin.platform_traffic")} aria-busy={loading} className="mt-10">
    <div className="mb-3 flex flex-wrap items-center justify-between gap-3"><h2 className="text-sm font-semibold">{t("admin.platform_traffic")}</h2><div className="flex flex-wrap gap-2">
      <Select value={String(hours)} onValueChange={(value) => setHours(Number(value) as TrafficHours)}><SelectTrigger aria-label={t("traffic.traffic_time_range")} className="h-11 w-36 text-xs"><SelectValue /></SelectTrigger><SelectContent>{trafficRanges.map((entry) => <SelectItem key={entry.hours} value={String(entry.hours)}>{t(entry.labelKey)}</SelectItem>)}</SelectContent></Select>
      <Select value={metric} onValueChange={(value) => setMetric(value as TrafficMetric)}><SelectTrigger aria-label={t("traffic.traffic_metric")} className="h-11 w-36 text-xs"><SelectValue /></SelectTrigger><SelectContent><SelectItem value="bytes">{t("traffic.billable_usage")}</SelectItem><SelectItem value="frames">{t("traffic.data_frames")}</SelectItem></SelectContent></Select>
    </div></div>
    <p className="mb-4 text-xs text-muted-foreground">{t("admin.traffic_note", { range })}</p>
    {error ? <div role="alert" className="flex flex-wrap items-center gap-3 rounded-md border p-4 text-xs"><span>{error}</span><Button variant="outline" size="sm" onClick={() => setRetry((value) => value + 1)}>{t("traffic.retry_traffic")}</Button></div>
      : !current ? <p role="status" className="py-8 text-sm text-muted-foreground">{t("traffic.loading_traffic")}</p> : <>
        <div role="group" aria-label={t("traffic.traffic_totals")} className="grid grid-cols-2 border-y"><div className="py-5 pr-3"><div className="text-[10px] text-muted-foreground">{t("traffic.billable_usage")}</div><div className="mt-1 text-xl font-semibold tabular-nums sm:text-2xl">{formatBytes(bytes)}</div></div><div className="border-l py-5 pl-3 sm:pl-6"><div className="text-[10px] text-muted-foreground">{t("traffic.data_frames")}</div><div className="mt-1 text-xl font-semibold tabular-nums sm:text-2xl">{frames.toLocaleString(intlLocale())}</div></div></div>
        <TrafficChart key={`${hours}:${metric}`} buckets={trafficBuckets(rows, hours, current.at)} hours={hours} metric={metric} />
        {rows.length === 0 && <p className="mt-3 text-xs text-muted-foreground">{t("traffic.no_recorded_traffic_in_this")}</p>}
        <p className="mt-3 text-[11px] text-muted-foreground">{t("traffic.unrecorded_note")}</p>
      </>}
  </section>
}
