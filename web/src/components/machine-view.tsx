import { ControllerUpdater } from "@/components/controller-updater"
import { MachineMetricsPanel } from "@/components/machine-metrics-panel"
import { agentPath } from "@/lib/workload-path"
import { EmptyState, MachineRecovery, Status } from "@/components/control-plane-ui"
import { Button } from "@/components/ui/button"
import { api, machineName, type Agent, type Machine, type MachineService, type MachineTrafficRecord, type VirtualNetworkHost } from "@/lib/api"
import { agentDisplayStatus, buildLabel, formatBytes, machineDisplayName, machineListen, supervisionLabel } from "@/lib/control-plane"
import { cn } from "@/lib/utils"
import {
  ExternalLink,
  Network,
  Pencil,
  Server,
  TerminalSquare,
  Trash2,
  TriangleAlert
} from "lucide-react"
import { useState } from "react"
import { statusLabel, t, intlLocale } from "@/i18n"

export function MachineOverviewView({ machine, agents, services, virtualHosts, traffic, machines, workspaceId, onOpenAgent, onClose, onCopy, onRename, onDelete }: { machine?: Machine; agents: Agent[]; services: MachineService[]; virtualHosts: VirtualNetworkHost[]; traffic: MachineTrafficRecord[]; machines: Machine[]; workspaceId?: string | null; onOpenAgent: (agentId: string) => void; onClose: () => void; onCopy: (value: string) => void; onRename: (machine: Machine) => void; onDelete: (machine: Machine) => void }) {
  const [localHealth, setLocalHealth] = useState<Record<string, "healthy" | "unreachable">>({})
  const outBytes = traffic.filter((t) => (t.source_type ?? "machine") === "machine" && t.source_server_id === machine?.server_id).reduce((sum, t) => sum + t.payload_bytes, 0)
  const inBytes = traffic.filter((t) => (t.destination_type ?? "machine") === "machine" && t.destination_server_id === machine?.server_id).reduce((sum, t) => sum + t.payload_bytes, 0)
  const peers = Array.from(new Set(
    traffic
      .filter((t) => t.source_server_id === machine?.server_id || t.destination_server_id === machine?.server_id)
      .map((t) => (t.source_server_id === machine?.server_id ? t.destination_server_id : t.source_server_id)),
  )).map((id) => machines.find((item) => item.server_id === id)).filter((item): item is Machine => Boolean(item))

  async function probe(serviceId: string) {
    if (!workspaceId) return
    try {
      await api(`/api/workspaces/${encodeURIComponent(workspaceId)}/services/${encodeURIComponent(serviceId)}/probe`, { method: "POST", body: "{}" })
      setLocalHealth((current) => ({ ...current, [serviceId]: "healthy" }))
    } catch {
      setLocalHealth((current) => ({ ...current, [serviceId]: "unreachable" }))
    }
  }

  if (!machine) return <div className="grid min-h-0 flex-1 place-items-center p-8 text-sm text-muted-foreground">{t("machine.not_found")} <Button variant="outline" className="mt-3" onClick={onClose}>{t("org.back")}</Button></div>
  const controller = buildLabel(machine.controller_build)
  const host = buildLabel(machine.host_build)
  const supervision = machine.supervision ? supervisionLabel(machine.supervision.mode) : t("machine.unknown")

  return <div className="min-h-0 overflow-auto"><div className="mx-auto w-full max-w-[1120px] px-5 py-8 sm:px-8 sm:py-12 lg:px-14">
    <div className="mb-8 flex items-start justify-between gap-4">
      <div>
        <div className="mb-3 flex items-center gap-3">
          <span className={cn("inline-flex size-2.5 rounded-full", machine.status === "online" ? "bg-emerald-500" : "bg-zinc-400")} />
          <h1 className="truncate text-2xl font-semibold">{machineDisplayName(machine, machines)}</h1>
          <span className="text-xs uppercase text-muted-foreground">{statusLabel(machine.status)}</span>
        </div>
        <p className="font-mono text-xs text-muted-foreground">{machine.server_id}</p>
        {machine.hostname && <p className="mt-1 font-mono text-xs text-muted-foreground">{machine.hostname}{machineListen(machine) ? ` · ${machineListen(machine)}` : ""}</p>}
        <p className="mt-2 max-w-2xl break-all font-mono text-[11px] text-muted-foreground" title={machine.root}>{machine.root}</p>
        {machine.status !== "online" && workspaceId && <MachineRecovery workspaceId={workspaceId} onCopy={onCopy} reason={t("machine.disconnected")} />}
      </div>
      <div className="flex shrink-0 flex-wrap justify-end gap-2">
        <Button variant="outline" className="max-md:h-11" onClick={() => onRename(machine)}><Pencil />{t("machine.rename")}</Button>
        <Button variant="outline" className="text-destructive max-md:h-11" onClick={() => onDelete(machine)}><Trash2 />{t("hub.remove_machine")}</Button>
        <Button variant="outline" className="max-md:hidden" onClick={onClose}>{t("org.close")}</Button>
      </div>
    </div>

    {workspaceId && <div className="mb-6 flex flex-wrap items-start gap-3"><ControllerUpdater key={`${workspaceId}/${machine.server_id}`} workspaceId={workspaceId} machine={machine} /></div>}
    {workspaceId && <MachineMetricsPanel key={`metrics/${workspaceId}/${machine.server_id}`} machine={machine} workspaceId={workspaceId} />}
    <div className="grid gap-10 md:grid-cols-2">
      <section className="space-y-3 rounded-md border p-4">
        <h2 className="text-sm font-semibold">{t("machine.build")}</h2>
        <dl className="grid gap-2 text-xs">
          <div className="flex justify-between gap-3"><dt className="text-muted-foreground">{t("machine.controller")}</dt><dd className="truncate font-mono">{controller}</dd></div>
          <div className="flex justify-between gap-3"><dt className="text-muted-foreground">{t("machine.host")}</dt><dd className="truncate font-mono">{host}</dd></div>
          <div className="flex justify-between gap-3"><dt className="text-muted-foreground">{t("machine.supervision")}</dt><dd className="truncate font-mono">{supervision}</dd></div>
          <div className="flex justify-between gap-3"><dt className="text-muted-foreground">{t("machine.controller_commit")}</dt><dd className="truncate font-mono">{machine.controller_build.git_commit.slice(0, 10)}</dd></div>
          <div className="flex justify-between gap-3"><dt className="text-muted-foreground">{t("machine.host_commit")}</dt><dd className="truncate font-mono">{machine.host_build.git_commit.slice(0, 10)}</dd></div>
        </dl>
        {machine.supervision?.fallback_reason && <div className="flex gap-2 border-t pt-3 text-[11px] leading-5 text-amber-700"><TriangleAlert className="mt-0.5 size-3.5 shrink-0" /><p><span className="font-medium">{supervisionLabel(machine.supervision.mode)} fallback.</span> {machine.supervision.fallback_reason}</p></div>}
      </section>

      <section className="space-y-3 rounded-md border p-4">
        <h2 className="text-sm font-semibold">{t("machine.network_last_24h")}</h2>
        <dl className="grid gap-2 text-xs">
          <div className="flex justify-between gap-3"><dt className="text-muted-foreground">{t("machine.routed_out")}</dt><dd className="font-mono">{formatBytes(outBytes)}</dd></div>
          <div className="flex justify-between gap-3"><dt className="text-muted-foreground">{t("machine.routed_in")}</dt><dd className="font-mono">{formatBytes(inBytes)}</dd></div>
          <div className="flex justify-between gap-3"><dt className="text-muted-foreground">{t("machine.peers")}</dt><dd className="font-mono">{peers.length}</dd></div>
        </dl>
        {peers.length > 0 && <div className="flex flex-wrap gap-1.5 pt-2">{peers.map((peer) => <span key={peer.server_id} className="rounded-full bg-accent px-2 py-1 text-[10px] font-medium">{machineName(peer)}</span>)}</div>}
      </section>
    </div>

    <section className="mt-10">
      <h2 className="mb-3 text-sm font-semibold">{t("machine.agents_on_this_machine")}</h2>
      {agents.length ? <div className="border-y">{agents.map((agent) => <div key={agent.agent_id} className="grid min-h-14 grid-cols-[minmax(0,1fr)_auto] items-center gap-3 border-b py-3 last:border-b-0">
        <div className="min-w-0">
          <div className="truncate text-xs font-medium">{agentPath(agent)}</div>
          <div className="mt-1 truncate font-mono text-[9px] text-muted-foreground">{agent.kind} · {agent.agent_id}</div>
        </div>
        <div className="flex items-center gap-2"><Status value={agentDisplayStatus(agent, machine)} /><Button size="sm" variant="outline" onClick={() => onOpenAgent(agent.agent_id)}>{t("machine.terminal")}</Button></div>
      </div>)}</div> : <EmptyState icon={<TerminalSquare />} label={t("machine.no_agents_running_on_this")} />}
    </section>

    <section className="mt-10">
      <div className="mb-3 flex items-center justify-between"><h2 className="text-sm font-semibold">{t("machine.registered_services")}</h2><span className="text-[10px] text-muted-foreground">{t("machine.http_tcp_endpoints")}</span></div>
      {services.length ? <div className="border-y">{services.map((service) => { const health = localHealth[service.service_id]; return <div key={service.service_id} className="grid min-h-14 grid-cols-[minmax(0,1fr)_auto] items-center gap-3 border-b py-3 last:border-b-0">
        <div className="min-w-0">
          <div className="truncate text-xs font-medium">{service.name} <span className="ml-2 font-mono text-[9px] uppercase text-muted-foreground">{service.protocol}</span>{health && <span className={cn("ml-2 text-[9px]", health === "healthy" ? "text-emerald-700" : "text-red-600")}>{health}</span>}</div>
          <div className="mt-1 truncate font-mono text-[10px] text-muted-foreground">{service.target_host}:{service.target_port} · {t("machine.updated", { time: new Date(service.updated_at).toLocaleString(intlLocale()) })}</div>
        </div>
        <Button size="sm" variant="ghost" onClick={() => probe(service.service_id)} disabled={machine.status !== "online"}>{t("machine.probe")}</Button>
      </div>})}</div> : <EmptyState icon={<Server />} label={t("machine.no_services_registered_for_this")} />}
    </section>

    <section className="mt-10">
      <div className="mb-3 flex items-center justify-between"><h2 className="text-sm font-semibold">{t("machine.virtual_hosts")}</h2><span className="text-[10px] text-muted-foreground">{t("machine.public_hostnames_targeting_this_machine")}</span></div>
      {virtualHosts.length ? <div className="border-y">{virtualHosts.map((host) => { const service = services.find((item) => item.service_id === host.service_id); return <div key={host.hostname} className="grid min-h-14 grid-cols-[minmax(0,1fr)_auto] items-center gap-3 border-b py-3 last:border-b-0">
        <div className="min-w-0">
          <div className="truncate font-mono text-xs font-medium">{host.hostname}</div>
          <div className="mt-1 truncate font-mono text-[10px] text-muted-foreground">{service?.name ?? host.service_id} · {host.target_host}{host.target_port ? `:${host.target_port}` : ""}</div>
        </div>
        <Button size="icon" variant="ghost" aria-label={t("machine.open_host", { host: host.hostname })} onClick={() => window.open(`https://${host.hostname}`, "_blank", "noopener")} disabled={service?.protocol !== "http"}><ExternalLink /></Button>
      </div>})}</div> : <EmptyState icon={<Network />} label={t("machine.no_virtual_hosts_routed_to")} />}
    </section>
  </div></div>
}
