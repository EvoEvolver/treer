import { EmptyState } from "@/components/control-plane-ui"
import { machineName, type Agent, type AgentTrafficRecord, type Machine, type MachineTrafficRecord, type OrganizationAuditEvent } from "@/lib/api"
import { formatBytes, initials } from "@/lib/control-plane"
import {
  Activity,
  ArrowRight,
  RotateCw,
  ScrollText
} from "lucide-react"

export const auditActionLabels: Record<string, string> = {
  "organization.created": "created the organization",
  "organization.renamed": "renamed the organization",
  "workspace.created": "created a workspace",
  "workspace.renamed": "renamed a workspace",
  "invitation.created": "created a member invitation",
  "member.role_updated": "changed a member role",
  "member.removed": "removed a member",
  "machine.renamed": "renamed a machine",
  "machine.deleted": "deleted a machine",
  "machine.exec": "ran a machine command",
  "machine.file.uploaded": "uploaded a machine file",
  "agent.created": "created an agent",
  "agent.renamed": "renamed an agent",
  "agent.stopped": "stopped an agent",
  "agent.deleted": "deleted an agent",
  "launch_profile.created": "created a launch profile",
  "launch_profile.updated": "updated a launch profile",
  "launch_profile.deleted": "deleted a launch profile",
}

export function AuditView({ events, traffic, agentTraffic, agents, machines, loading }: { events: OrganizationAuditEvent[]; traffic: MachineTrafficRecord[]; agentTraffic: AgentTrafficRecord[]; agents: Agent[]; machines: Machine[]; loading: boolean }) {
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

  return <div className="min-h-0 overflow-auto"><div className="mx-auto w-full max-w-[1120px] px-5 py-8 sm:px-8 sm:py-12 lg:px-14">
    <div className="mb-8 flex items-end justify-between gap-4"><div><div className="mb-2 grid size-9 place-items-center rounded-md bg-[#f8d9df] text-[#8b4452]"><ScrollText className="size-4" /></div><h1 className="text-2xl font-semibold">Audit</h1></div><span className="text-xs text-muted-foreground">Organization activity</span></div>
    <section className="mb-11"><div className="mb-3 flex items-center justify-between"><h2 className="text-sm font-semibold">Traffic</h2><span className="text-[10px] uppercase text-muted-foreground">Last 24 hours</span></div><div className="grid grid-cols-3 border-y">
      <div className="py-5 pr-3"><div className="text-[10px] text-muted-foreground">Billable usage</div><div className="mt-1 text-xl font-semibold tabular-nums sm:text-2xl">{formatBytes(totalBytes)}</div></div>
      <div className="border-x px-3 py-5 sm:px-6"><div className="text-[10px] text-muted-foreground">Data frames</div><div className="mt-1 text-xl font-semibold tabular-nums sm:text-2xl">{totalFrames.toLocaleString()}</div></div>
      <div className="py-5 pl-3 sm:pl-6"><div className="text-[10px] text-muted-foreground">Routes</div><div className="mt-1 text-xl font-semibold tabular-nums sm:text-2xl">{routes.length}</div></div>
    </div>{routes.length > 0 && <div className="mt-3 divide-y border-b">{routes.slice(0, 5).map((route) => <div key={`${route.trafficClass}:${route.source}:${route.destination}`} className="grid min-h-10 grid-cols-[minmax(0,1fr)_auto] items-center gap-4 text-xs"><span className="flex min-w-0 items-center gap-2"><span className="w-16 shrink-0 text-[9px] uppercase text-muted-foreground">{trafficClassLabel[route.trafficClass]}</span><span className="truncate">{resolveEndpoint(route.sourceType, route.source)}</span><ArrowRight className="size-3 shrink-0 text-muted-foreground" /><span className="truncate">{resolveEndpoint(route.destinationType, route.destination)}</span></span><span className="font-mono text-[10px] text-muted-foreground">{formatBytes(route.bytes)}</span></div>)}</div>}</section>
    {agentTotals.size > 0 && <section className="mb-11"><h2 className="mb-3 text-sm font-semibold">Agent traffic · last 24 hours</h2><p className="mb-3 text-xs text-muted-foreground">Agent detail is included in machine usage, not added to it. Direct traffic is reported by the machine.</p><div className="divide-y border-y">{Array.from(agentTotals).sort((a, b) => b[1].sent + b[1].received - a[1].sent - a[1].received).map(([id, usage]) => <div key={id} className="flex items-center justify-between gap-3 py-3 text-xs"><span className="truncate">{agents.find((agent) => agent.agent_id === id)?.name ?? id}</span><span className="shrink-0 font-mono">↑ {formatBytes(usage.sent)} · ↓ {formatBytes(usage.received)}</span></div>)}</div></section>}
    <section><div className="mb-3 flex items-center justify-between"><h2 className="text-sm font-semibold">Activity</h2><span className="text-[10px] text-muted-foreground">{events.length} events</span></div><div className="border-y divide-y">{events.map((event) => { const actor = event.actor_name ?? event.actor_id ?? event.actor_kind; const resource = event.resource_name ?? event.resource_id; return <div key={event.event_id} className="grid min-h-16 grid-cols-[32px_minmax(0,1fr)] gap-3 py-3 sm:grid-cols-[32px_minmax(0,1fr)_auto] sm:items-center"><span className="grid size-8 place-items-center rounded bg-[#dcebea] text-[10px] font-bold text-[#35645f]">{initials(actor)}</span><div className="min-w-0"><div className="truncate text-xs"><span className="font-medium">{actor}</span> <span className="text-muted-foreground">{auditActionLabels[event.action] ?? event.action}</span></div><div className="mt-1 truncate font-mono text-[9px] text-muted-foreground">{resource}</div></div><time className="col-start-2 text-[10px] text-muted-foreground sm:col-start-auto" dateTime={event.occurred_at}>{new Date(event.occurred_at).toLocaleString()}</time></div>})}{!events.length && <EmptyState icon={loading ? <RotateCw className="animate-spin" /> : <Activity />} label={loading ? "Loading activity" : "No audit activity yet"} />}</div></section>
  </div></div>
}
