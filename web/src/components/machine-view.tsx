import { agentPath } from "@/lib/workload-path"
import { EmptyState, MachineRecovery, Status } from "@/components/control-plane-ui"
import { Button } from "@/components/ui/button"
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuSeparator, DropdownMenuTrigger } from "@/components/ui/dropdown-menu"
import { api, machineName, type Agent, type Machine, type MachineService, type MachineTrafficRecord, type VirtualNetworkHost } from "@/lib/api"
import { agentDisplayStatus, buildLabel, formatBytes, machineDisplayName, machineListen, machineRecoveryCommands, supervisionLabel } from "@/lib/control-plane"
import { cn } from "@/lib/utils"
import {
  Copy,
  ExternalLink,
  MoreHorizontal,
  Network,
  Pencil,
  Server,
  TerminalSquare,
  Trash2,
  TriangleAlert
} from "lucide-react"
import { useState } from "react"

export function MachineOverviewView({ machine, agents, services, virtualHosts, traffic, machines, workspaceId, onOpenAgent, onClose, onCopy }: { machine?: Machine; agents: Agent[]; services: MachineService[]; virtualHosts: VirtualNetworkHost[]; traffic: MachineTrafficRecord[]; machines: Machine[]; workspaceId?: string | null; onOpenAgent: (agentId: string) => void; onClose: () => void; onCopy: (value: string) => void }) {
  const [localHealth, setLocalHealth] = useState<Record<string, "healthy" | "unreachable">>({})
  const outBytes = traffic.filter((t) => (t.source_type ?? "machine") === "machine" && t.source_server_id === machine?.server_id).reduce((sum, t) => sum + t.payload_bytes, 0)
  const inBytes = traffic.filter((t) => (t.destination_type ?? "machine") === "machine" && t.destination_server_id === machine?.server_id).reduce((sum, t) => sum + t.payload_bytes, 0)
  const peers = Array.from(new Set(
    traffic
      .filter((t) => t.source_server_id === machine?.server_id || t.destination_server_id === machine?.server_id)
      .map((t) => (t.source_server_id === machine?.server_id ? t.destination_server_id : t.source_server_id)),
  )).map((id) => machines.find((item) => item.server_id === id)).filter((item): item is Machine => Boolean(item))

  async function probe(serviceId: string) {
    try {
      await api(`/api/services/${encodeURIComponent(serviceId)}/probe`, { method: "POST", body: "{}" })
      setLocalHealth((current) => ({ ...current, [serviceId]: "healthy" }))
    } catch {
      setLocalHealth((current) => ({ ...current, [serviceId]: "unreachable" }))
    }
  }

  if (!machine) return <div className="grid min-h-0 flex-1 place-items-center p-8 text-sm text-muted-foreground">Machine not found (or disconnected). <Button variant="outline" className="mt-3" onClick={onClose}>Back</Button></div>
  const controller = buildLabel(machine.controller_build)
  const host = buildLabel(machine.host_build)
  const supervision = machine.supervision ? supervisionLabel(machine.supervision.mode) : "Unknown"

  return <div className="min-h-0 overflow-auto"><div className="mx-auto w-full max-w-[1120px] px-5 py-8 sm:px-8 sm:py-12 lg:px-14">
    <div className="mb-8 flex items-start justify-between gap-4">
      <div>
        <div className="mb-3 flex items-center gap-3">
          <span className={cn("inline-flex size-2.5 rounded-full", machine.status === "online" ? "bg-emerald-500" : "bg-zinc-400")} />
          <h1 className="truncate text-2xl font-semibold">{machineDisplayName(machine, machines)}</h1>
          <span className="text-xs uppercase text-muted-foreground">{machine.status}</span>
        </div>
        <p className="font-mono text-xs text-muted-foreground">{machine.server_id}</p>
        {machine.hostname && <p className="mt-1 font-mono text-xs text-muted-foreground">{machine.hostname}{machineListen(machine) ? ` · ${machineListen(machine)}` : ""}</p>}
        <p className="mt-2 max-w-2xl break-all font-mono text-[11px] text-muted-foreground" title={machine.root}>{machine.root}</p>
        {machine.status !== "online" && workspaceId && <MachineRecovery workspaceId={workspaceId} onCopy={onCopy} reason="This machine is not connected to the control plane. It may be stopped, waking from sleep, or fenced as a duplicate." />}
      </div>
      <Button variant="outline" className="shrink-0" onClick={onClose}>Close</Button>
    </div>

    <div className="grid gap-10 md:grid-cols-2">
      <section className="space-y-3 rounded-md border p-4">
        <h2 className="text-sm font-semibold">Build</h2>
        <dl className="grid gap-2 text-xs">
          <div className="flex justify-between gap-3"><dt className="text-muted-foreground">Controller</dt><dd className="truncate font-mono">{controller}</dd></div>
          <div className="flex justify-between gap-3"><dt className="text-muted-foreground">Host</dt><dd className="truncate font-mono">{host}</dd></div>
          <div className="flex justify-between gap-3"><dt className="text-muted-foreground">Supervision</dt><dd className="truncate font-mono">{supervision}</dd></div>
          <div className="flex justify-between gap-3"><dt className="text-muted-foreground">Controller commit</dt><dd className="truncate font-mono">{machine.controller_build.git_commit.slice(0, 10)}</dd></div>
          <div className="flex justify-between gap-3"><dt className="text-muted-foreground">Host commit</dt><dd className="truncate font-mono">{machine.host_build.git_commit.slice(0, 10)}</dd></div>
        </dl>
        {machine.supervision?.fallback_reason && <div className="flex gap-2 border-t pt-3 text-[11px] leading-5 text-amber-700"><TriangleAlert className="mt-0.5 size-3.5 shrink-0" /><p><span className="font-medium">{supervisionLabel(machine.supervision.mode)} fallback.</span> {machine.supervision.fallback_reason}</p></div>}
      </section>

      <section className="space-y-3 rounded-md border p-4">
        <h2 className="text-sm font-semibold">Network (last 24h)</h2>
        <dl className="grid gap-2 text-xs">
          <div className="flex justify-between gap-3"><dt className="text-muted-foreground">Routed out</dt><dd className="font-mono">{formatBytes(outBytes)}</dd></div>
          <div className="flex justify-between gap-3"><dt className="text-muted-foreground">Routed in</dt><dd className="font-mono">{formatBytes(inBytes)}</dd></div>
          <div className="flex justify-between gap-3"><dt className="text-muted-foreground">Peers</dt><dd className="font-mono">{peers.length}</dd></div>
        </dl>
        {peers.length > 0 && <div className="flex flex-wrap gap-1.5 pt-2">{peers.map((peer) => <span key={peer.server_id} className="rounded-full bg-accent px-2 py-1 text-[10px] font-medium">{machineName(peer)}</span>)}</div>}
      </section>
    </div>

    <section className="mt-10">
      <h2 className="mb-3 text-sm font-semibold">Agents on this machine</h2>
      {agents.length ? <div className="border-y">{agents.map((agent) => <div key={agent.agent_id} className="grid min-h-14 grid-cols-[minmax(0,1fr)_auto] items-center gap-3 border-b py-3 last:border-b-0">
        <div className="min-w-0">
          <div className="truncate text-xs font-medium">{agentPath(agent)}</div>
          <div className="mt-1 truncate font-mono text-[9px] text-muted-foreground">{agent.kind} · {agent.agent_id}</div>
        </div>
        <div className="flex items-center gap-2"><Status value={agentDisplayStatus(agent, machine)} /><Button size="sm" variant="outline" onClick={() => onOpenAgent(agent.agent_id)}>Terminal</Button></div>
      </div>)}</div> : <EmptyState icon={<TerminalSquare />} label="No agents running on this machine" />}
    </section>

    <section className="mt-10">
      <div className="mb-3 flex items-center justify-between"><h2 className="text-sm font-semibold">Registered services</h2><span className="text-[10px] text-muted-foreground">HTTP / TCP endpoints</span></div>
      {services.length ? <div className="border-y">{services.map((service) => { const health = localHealth[service.service_id]; return <div key={service.service_id} className="grid min-h-14 grid-cols-[minmax(0,1fr)_auto] items-center gap-3 border-b py-3 last:border-b-0">
        <div className="min-w-0">
          <div className="truncate text-xs font-medium">{service.name} <span className="ml-2 font-mono text-[9px] uppercase text-muted-foreground">{service.protocol}</span>{health && <span className={cn("ml-2 text-[9px]", health === "healthy" ? "text-emerald-700" : "text-red-600")}>{health}</span>}</div>
          <div className="mt-1 truncate font-mono text-[10px] text-muted-foreground">{service.target_host}:{service.target_port} · updated {new Date(service.updated_at).toLocaleString()}</div>
        </div>
        <Button size="sm" variant="ghost" onClick={() => probe(service.service_id)} disabled={machine.status !== "online"}>Probe</Button>
      </div>})}</div> : <EmptyState icon={<Server />} label="No services registered for this machine" />}
    </section>

    <section className="mt-10">
      <div className="mb-3 flex items-center justify-between"><h2 className="text-sm font-semibold">Virtual hosts</h2><span className="text-[10px] text-muted-foreground">Public hostnames targeting this machine</span></div>
      {virtualHosts.length ? <div className="border-y">{virtualHosts.map((host) => { const service = services.find((item) => item.service_id === host.service_id); return <div key={host.hostname} className="grid min-h-14 grid-cols-[minmax(0,1fr)_auto] items-center gap-3 border-b py-3 last:border-b-0">
        <div className="min-w-0">
          <div className="truncate font-mono text-xs font-medium">{host.hostname}</div>
          <div className="mt-1 truncate font-mono text-[10px] text-muted-foreground">{service?.name ?? host.service_id} · {host.target_host}{host.target_port ? `:${host.target_port}` : ""}</div>
        </div>
        <Button size="icon" variant="ghost" aria-label={`Open ${host.hostname}`} onClick={() => window.open(`https://${host.hostname}`, "_blank", "noopener")} disabled={service?.protocol !== "http"}><ExternalLink /></Button>
      </div>})}</div> : <EmptyState icon={<Network />} label="No virtual hosts routed to this machine" />}
    </section>
  </div></div>
}

export function MachineItem({ machine, machines = [], workspaceId, selected, onClick, onRename, onDelete, onCopy }: { machine: Machine; machines?: Machine[]; workspaceId?: string | null; selected?: boolean; onClick?: () => void; onRename: () => void; onDelete: () => void; onCopy: (value: string) => void }) {
  const builds = `Controller ${buildLabel(machine.controller_build)} · Host ${buildLabel(machine.host_build)}`
  const buildTitle = `Controller ${machine.controller_build.version} (${machine.controller_build.git_commit})\nHost ${machine.host_build.version} (${machine.host_build.git_commit})`
  const commands = workspaceId ? machineRecoveryCommands(workspaceId) : null
  return <div className={cn("group flex min-h-[68px] items-start gap-2 rounded-[5px] px-2.5 py-2 hover:bg-accent", selected && "bg-accent hover:bg-accent")}><button type="button" onClick={onClick} className="flex min-w-0 flex-1 items-start gap-2 text-left"><span className={cn("mt-1.5 size-1.5 shrink-0 rounded-full bg-zinc-400", machine.status === "online" && "bg-emerald-500")} /><div className="min-w-0 flex-1"><div className="truncate text-xs font-medium">{machineDisplayName(machine, machines)}</div><div className="mt-1 truncate font-mono text-[9px] text-muted-foreground">{machine.root}</div><div className="mt-1 truncate font-mono text-[9px] text-muted-foreground" title={buildTitle}>{builds}</div>{machine.status !== "online" && <div className="mt-1 text-[9px] uppercase text-red-600">offline</div>}</div></button><DropdownMenu><DropdownMenuTrigger asChild><Button size="icon" variant="ghost" className="size-7 shrink-0 opacity-0 group-hover:opacity-100 group-focus-within:opacity-100 data-[state=open]:opacity-100 max-md:opacity-100" aria-label={`Actions for ${machineName(machine)}`}><MoreHorizontal /></Button></DropdownMenuTrigger><DropdownMenuContent align="end"><DropdownMenuItem onSelect={onRename}><Pencil />Rename</DropdownMenuItem>{commands && machine.status !== "online" && <><DropdownMenuItem onSelect={() => onCopy(commands.restartController)}><Copy />Copy restart-controller</DropdownMenuItem><DropdownMenuItem onSelect={() => onCopy(commands.start)}><Copy />Copy restore</DropdownMenuItem><DropdownMenuSeparator /></>}<DropdownMenuItem className="text-destructive focus:text-destructive" onSelect={onDelete}><Trash2 />Delete</DropdownMenuItem></DropdownMenuContent></DropdownMenu></div>
}
