import { useEffect, useState } from "react"
import { api, ApiError, machineName, type Agent, type AgentTrafficRecord, type Machine, type MachineTrafficRecord } from "@/lib/api"
import { agentPath } from "@/lib/workload-path"
import { formatBytes } from "@/lib/control-plane"
import { trafficBuckets, trafficRanges, type TrafficHours, type TrafficMetric } from "@/lib/traffic"
import { TrafficChart } from "@/components/traffic-chart"
import { Button } from "@/components/ui/button"
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select"
import { ArrowRight, RotateCw } from "lucide-react"
import { t, intlLocale } from "@/i18n"

interface TrafficData { hours: TrafficHours; traffic: MachineTrafficRecord[]; agentTraffic: AgentTrafficRecord[]; at: number; agentDetailAvailable: boolean }

export function TrafficOverview({ workspaceId, agents, machines, refreshRevision }: { workspaceId: string; agents: Agent[]; machines: Machine[]; refreshRevision: number }) {
  const [hours, setHours] = useState<TrafficHours>(24)
  const [metric, setMetric] = useState<TrafficMetric>("bytes")
  const [data, setData] = useState<TrafficData | null>(null)
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)
  const [retry, setRetry] = useState(0)
  useEffect(() => {
    let active = true
    let pending: AbortController | null = null
    setData(null)
    setError(null)
    async function load() {
      if (pending) return
      const controller = new AbortController()
      pending = controller
      setLoading(true)
      try {
        const base = `/api/workspaces/${encodeURIComponent(workspaceId)}/traffic`
        const [usage, detail] = await Promise.all([
          api<{ traffic: MachineTrafficRecord[] }>(`${base}?hours=${hours}`, { signal: controller.signal }),
          api<{ traffic: AgentTrafficRecord[] }>(`${base}/agents?hours=${hours}`, { signal: controller.signal }).then((value) => ({ ...value, available: true })).catch((error: unknown) => {
            if (error instanceof ApiError && error.status === 404) return { traffic: [], available: false }
            throw error
          }),
        ])
        if (active && !controller.signal.aborted) {
          setData({ hours, traffic: usage.traffic, agentTraffic: detail.traffic, at: Date.now(), agentDetailAvailable: detail.available })
          setError(null)
        }
      } catch (error: unknown) {
        if (active && !controller.signal.aborted) {
          setData(null)
          setError(error instanceof ApiError ? t("traffic.load_failed_http", { status: `HTTP ${error.status}` }) : t("traffic.load_failed"))
        }
      } finally {
        controller.abort()
        pending = null
        if (active) setLoading(false)
      }
    }
    void load()
    const timer = window.setInterval(() => void load(), 10_000)
    return () => { active = false; pending?.abort(); window.clearInterval(timer) }
  }, [workspaceId, hours, refreshRevision, retry])
  const current = data?.hours === hours ? data : null
  const traffic = current?.traffic ?? []
  const agentTraffic = current?.agentTraffic ?? []
  const rangeLabel = t(trafficRanges.find((range) => range.hours === hours)!.labelKey)
  const agentTotals = new Map<string, { sent: number; received: number }>()
  for (const row of agentTraffic) {
    if (row.source_type === "agent") {
      const value = agentTotals.get(row.source_id) ?? { sent: 0, received: 0 }
      value.sent += row.payload_bytes
      agentTotals.set(row.source_id, value)
    }
    if (row.destination_type === "agent") {
      const value = agentTotals.get(row.destination_id) ?? { sent: 0, received: 0 }
      value.received += row.payload_bytes
      agentTotals.set(row.destination_id, value)
    }
  }
  const totalBytes = traffic.reduce((sum, item) => sum + (item.billable_bytes ?? item.payload_bytes), 0)
  const totalFrames = traffic.filter((item) => item.traffic_class !== "direct_network").reduce((sum, item) => sum + item.payload_frames, 0)
  const routes = Array.from(traffic.reduce((items, item) => {
    const trafficClass = item.traffic_class ?? "virtual_network"
    const sourceType = item.source_type ?? "machine"
    const destinationType = item.destination_type ?? "machine"
    const key = `${trafficClass}\u0000${sourceType}\u0000${item.source_server_id}\u0000${destinationType}\u0000${item.destination_server_id}`
    const current = items.get(key) ?? { trafficClass, sourceType, source: item.source_server_id, destinationType, destination: item.destination_server_id, bytes: 0, frames: 0 }
    current.bytes += item.payload_bytes
    current.frames += item.payload_frames
    items.set(key, current)
    return items
  }, new Map<string, { trafficClass: NonNullable<MachineTrafficRecord["traffic_class"]>; sourceType: NonNullable<MachineTrafficRecord["source_type"]>; source: string; destinationType: NonNullable<MachineTrafficRecord["destination_type"]>; destination: string; bytes: number; frames: number }>()).values()).sort((left, right) => right.bytes - left.bytes)
  const resolveEndpoint = (type: NonNullable<MachineTrafficRecord["source_type"]>, serverId: string) => type === "client" || serverId === "browser"
    ? t("traffic.browser_ingress")
    : type === "internet" ? serverId : machineName(machines.find((machine) => machine.server_id === serverId), serverId)
  const trafficClassLabel: Record<NonNullable<MachineTrafficRecord["traffic_class"]>, string> = { virtual_network: t("traffic.class_machine"), service_ingress: t("traffic.class_ingress"), virtual_host: t("traffic.class_virtual_host"), agent_interface: t("traffic.class_agent_ui"), direct_network: t("traffic.class_direct") }

  return <>
    <section aria-label={t("traffic.workspace_traffic")} aria-busy={loading} className="mb-11">
      <div className="mb-4 flex flex-wrap items-center justify-between gap-3"><h2 className="text-sm font-semibold">{t("traffic.traffic")}</h2><div className="flex flex-wrap gap-2">
        <Select value={String(hours)} onValueChange={(value) => setHours(Number(value) as TrafficHours)}><SelectTrigger aria-label={t("traffic.traffic_time_range")} className="h-10 w-36 text-xs"><SelectValue /></SelectTrigger><SelectContent>{trafficRanges.map((range) => <SelectItem key={range.hours} value={String(range.hours)}>{t(range.labelKey)}</SelectItem>)}</SelectContent></Select>
        <Select value={metric} onValueChange={(value) => setMetric(value as TrafficMetric)}><SelectTrigger aria-label={t("traffic.traffic_metric")} className="h-10 w-36 text-xs"><SelectValue /></SelectTrigger><SelectContent><SelectItem value="bytes">{t("traffic.billable_usage")}</SelectItem><SelectItem value="frames">{t("traffic.data_frames")}</SelectItem></SelectContent></Select>
      </div></div>
      <p className="mb-4 text-xs text-muted-foreground">{t(hours > 24 ? "traffic.usage_note_days" : "traffic.usage_note_hours", { range: rangeLabel })}</p>
      {error ? <div role="alert" className="flex flex-wrap items-center gap-3 rounded-md border p-4 text-xs"><span>{error}</span><Button size="sm" variant="outline" onClick={() => setRetry((value) => value + 1)}>{t("traffic.retry_traffic")}</Button></div> : !current ? <p role="status" className="flex items-center gap-2 py-8 text-sm text-muted-foreground"><RotateCw aria-hidden="true" className="size-4 animate-spin" />{t("traffic.loading_traffic")}</p> : <>
      <div role="group" aria-label={t("traffic.traffic_totals")} className="grid grid-cols-3 border-y">
      <div className="py-5 pr-3"><div className="text-[10px] text-muted-foreground">{t("traffic.billable_usage")}</div><div className="mt-1 text-xl font-semibold tabular-nums sm:text-2xl">{formatBytes(totalBytes)}</div></div>
      <div className="border-x px-3 py-5 sm:px-6"><div className="text-[10px] text-muted-foreground">{t("traffic.data_frames")}</div><div className="mt-1 text-xl font-semibold tabular-nums sm:text-2xl">{totalFrames.toLocaleString(intlLocale())}</div></div>
      <div className="py-5 pl-3 sm:pl-6"><div className="text-[10px] text-muted-foreground">{t("traffic.routes")}</div><div className="mt-1 text-xl font-semibold tabular-nums sm:text-2xl">{routes.length}</div></div>
    </div>
    <TrafficChart key={`${hours}:${metric}`} buckets={trafficBuckets(traffic, hours, current.at)} hours={hours} metric={metric} />
    {traffic.length === 0 && <p className="mt-3 text-xs text-muted-foreground">{t("traffic.no_recorded_traffic_in_this")}</p>}
    <p className="mt-3 text-[11px] text-muted-foreground">{t("traffic.unrecorded_note")}</p>
    {routes.length > 0 && <div className="mt-3 divide-y border-b">{routes.slice(0, 5).map((route) => <div key={`${route.trafficClass}:${route.source}:${route.destination}`} className="grid min-h-10 grid-cols-[minmax(0,1fr)_auto] items-center gap-4 text-xs"><span className="flex min-w-0 items-center gap-2"><span className="w-16 shrink-0 text-[9px] uppercase text-muted-foreground">{trafficClassLabel[route.trafficClass]}</span><span className="truncate">{resolveEndpoint(route.sourceType, route.source)}</span><ArrowRight className="size-3 shrink-0 text-muted-foreground" /><span className="truncate">{resolveEndpoint(route.destinationType, route.destination)}</span></span><span className="font-mono text-[10px] text-muted-foreground">{formatBytes(route.bytes)}</span></div>)}</div>}</>}</section>
    {current && !current.agentDetailAvailable && <p className="mb-8 text-xs text-muted-foreground">{t("traffic.agent_traffic_detail_is_unavailable")}</p>}
    {agentTotals.size > 0 && <section className="mb-11"><h2 className="mb-3 text-sm font-semibold">{t("traffic.agent_traffic_range", { range: rangeLabel.toLowerCase() })}</h2><p className="mb-3 text-xs text-muted-foreground">{t("traffic.agent_detail_is_included_in")}</p><div className="divide-y border-y">{Array.from(agentTotals).sort((a, b) => b[1].sent + b[1].received - a[1].sent - a[1].received).map(([id, usage]) => <div key={id} className="flex items-center justify-between gap-3 py-3 text-xs"><span className="truncate">{(() => { const agent = agents.find((entry) => entry.agent_id === id); return agent ? agentPath(agent) : id })()}</span><span className="shrink-0 font-mono">↑ {formatBytes(usage.sent)} · ↓ {formatBytes(usage.received)}</span></div>)}</div></section>}
  </>
}
