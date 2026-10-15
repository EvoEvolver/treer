import { useEffect, useState } from "react"
import { api, ApiError, machineName, type Agent, type AgentTrafficRecord, type Machine, type MachineTrafficRecord } from "@/lib/api"
import { agentPath } from "@/lib/workload-path"
import { formatBytes } from "@/lib/control-plane"
import { trafficBuckets, trafficRanges, type TrafficHours, type TrafficMetric } from "@/lib/traffic"
import { TrafficChart } from "@/components/traffic-chart"
import { Button } from "@/components/ui/button"
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select"
import { ArrowRight, RotateCw } from "lucide-react"

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
          setError(`Could not load traffic${error instanceof ApiError ? ` (HTTP ${error.status})` : ""}. Try again.`)
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
  const rangeLabel = trafficRanges.find((range) => range.hours === hours)!.label
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
    ? "Browser / ingress"
    : type === "internet" ? serverId : machineName(machines.find((machine) => machine.server_id === serverId), serverId)
  const trafficClassLabel: Record<NonNullable<MachineTrafficRecord["traffic_class"]>, string> = { virtual_network: "Machine", service_ingress: "Ingress", virtual_host: "Virtual host", agent_interface: "Agent UI", direct_network: "Direct (reported)" }

  return <>
    <section aria-label="Workspace traffic" aria-busy={loading} className="mb-11">
      <div className="mb-4 flex flex-wrap items-center justify-between gap-3"><h2 className="text-sm font-semibold">Traffic</h2><div className="flex flex-wrap gap-2">
        <Select value={String(hours)} onValueChange={(value) => setHours(Number(value) as TrafficHours)}><SelectTrigger aria-label="Traffic time range" className="h-10 w-36 text-xs"><SelectValue /></SelectTrigger><SelectContent>{trafficRanges.map((range) => <SelectItem key={range.hours} value={String(range.hours)}>{range.label}</SelectItem>)}</SelectContent></Select>
        <Select value={metric} onValueChange={(value) => setMetric(value as TrafficMetric)}><SelectTrigger aria-label="Traffic metric" className="h-10 w-36 text-xs"><SelectValue /></SelectTrigger><SelectContent><SelectItem value="bytes">Billable usage</SelectItem><SelectItem value="frames">Data frames</SelectItem></SelectContent></Select>
      </div></div>
      <p className="mb-4 text-xs text-muted-foreground">{rangeLabel} · Workspace usage. Current hour{hours > 24 ? " and edge days are" : " is"} incomplete. Times are UTC.</p>
      {error ? <div role="alert" className="flex flex-wrap items-center gap-3 rounded-md border p-4 text-xs"><span>{error}</span><Button size="sm" variant="outline" onClick={() => setRetry((value) => value + 1)}>Retry traffic</Button></div> : !current ? <p role="status" className="flex items-center gap-2 py-8 text-sm text-muted-foreground"><RotateCw aria-hidden="true" className="size-4 animate-spin" />Loading traffic…</p> : <>
      <div role="group" aria-label="Traffic totals" className="grid grid-cols-3 border-y">
      <div className="py-5 pr-3"><div className="text-[10px] text-muted-foreground">Billable usage</div><div className="mt-1 text-xl font-semibold tabular-nums sm:text-2xl">{formatBytes(totalBytes)}</div></div>
      <div className="border-x px-3 py-5 sm:px-6"><div className="text-[10px] text-muted-foreground">Data frames</div><div className="mt-1 text-xl font-semibold tabular-nums sm:text-2xl">{totalFrames.toLocaleString()}</div></div>
      <div className="py-5 pl-3 sm:pl-6"><div className="text-[10px] text-muted-foreground">Routes</div><div className="mt-1 text-xl font-semibold tabular-nums sm:text-2xl">{routes.length}</div></div>
    </div>
    <TrafficChart key={`${hours}:${metric}`} buckets={trafficBuckets(traffic, hours, current.at)} hours={hours} metric={metric} />
    {traffic.length === 0 && <p className="mt-3 text-xs text-muted-foreground">No recorded traffic in this range.</p>}
    <p className="mt-3 text-[11px] text-muted-foreground">Unrecorded windows appear as zero; this does not prove there was no traffic. Direct traffic is machine-reported; its frames are excluded.</p>
    {routes.length > 0 && <div className="mt-3 divide-y border-b">{routes.slice(0, 5).map((route) => <div key={`${route.trafficClass}:${route.source}:${route.destination}`} className="grid min-h-10 grid-cols-[minmax(0,1fr)_auto] items-center gap-4 text-xs"><span className="flex min-w-0 items-center gap-2"><span className="w-16 shrink-0 text-[9px] uppercase text-muted-foreground">{trafficClassLabel[route.trafficClass]}</span><span className="truncate">{resolveEndpoint(route.sourceType, route.source)}</span><ArrowRight className="size-3 shrink-0 text-muted-foreground" /><span className="truncate">{resolveEndpoint(route.destinationType, route.destination)}</span></span><span className="font-mono text-[10px] text-muted-foreground">{formatBytes(route.bytes)}</span></div>)}</div>}</>}</section>
    {current && !current.agentDetailAvailable && <p className="mb-8 text-xs text-muted-foreground">Agent traffic detail is unavailable on this Proxy.</p>}
    {agentTotals.size > 0 && <section className="mb-11"><h2 className="mb-3 text-sm font-semibold">Agent traffic · {rangeLabel.toLowerCase()}</h2><p className="mb-3 text-xs text-muted-foreground">Agent detail is included in machine usage, not added to it. Direct traffic is reported by the machine.</p><div className="divide-y border-y">{Array.from(agentTotals).sort((a, b) => b[1].sent + b[1].received - a[1].sent - a[1].received).map(([id, usage]) => <div key={id} className="flex items-center justify-between gap-3 py-3 text-xs"><span className="truncate">{(() => { const agent = agents.find((entry) => entry.agent_id === id); return agent ? agentPath(agent) : id })()}</span><span className="shrink-0 font-mono">↑ {formatBytes(usage.sent)} · ↓ {formatBytes(usage.received)}</span></div>)}</div></section>}
  </>
}
