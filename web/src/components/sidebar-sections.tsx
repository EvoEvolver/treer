import { useState, type ReactNode } from "react"
import { ChevronRight, Monitor, Plus } from "lucide-react"
import { machineName, type Agent, type Machine } from "@/lib/api"
import { TreeRow, TreeStatus } from "@/components/tree-row"
import { cn } from "@/lib/utils"
import { t } from "@/i18n"

// A collapsible sidebar section with a sticky heading; open state is remembered per section.
export function SidebarSection({ id, title, count, action, children, defaultOpen = true }: {
  id: string; title: string; count?: number | string; action?: ReactNode; children: ReactNode; defaultOpen?: boolean
}) {
  const key = `treer:sidebar-section:v1:${id}`
  const [open, setOpen] = useState(() => { try { const value = localStorage.getItem(key); return value === null ? defaultOpen : value === "true" } catch { return defaultOpen } })
  function toggle() { setOpen((value) => { try { localStorage.setItem(key, String(!value)) } catch { /* optional */ } return !value }) }
  return <section aria-labelledby={`sidebar-section-${id}`} data-sidebar-section={id}>
    <div className="sticky top-0 z-[3] flex h-8 items-center justify-between bg-sidebar pl-3 pr-2 max-md:h-11">
      <button id={`sidebar-section-${id}`} type="button" aria-expanded={open} onClick={toggle} className="group/section flex h-full min-w-0 items-center gap-1.5 rounded-md px-1 text-[11px] font-semibold uppercase tracking-wider text-muted-foreground outline-none hover:text-foreground focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring">
        <span className="truncate">{title}</span>
        {count !== undefined && <span className="rounded-full bg-accent px-1.5 py-px text-[10px] font-semibold tabular-nums normal-case tracking-normal text-muted-foreground">{count}</span>}
        <ChevronRight aria-hidden="true" className={cn("size-3 opacity-60 transition-transform group-hover/section:opacity-100 motion-reduce:transition-none", open && "rotate-90")} />
      </button>
      {action}
    </div>
    {open && <div className="px-2 pb-2">{children}</div>}
  </section>
}

// Machines are first-class: health at a glance, one click to the machine page, enrollment one click away.
export function MachinesSection({ machines, agents, selectedId, onOpen, onAdd }: {
  machines: Machine[]; agents: Agent[]; selectedId: string | null; onOpen: (id: string) => void; onAdd: () => void
}) {
  const shown = [...machines].sort((a, b) => Number(b.status === "online") - Number(a.status === "online") || machineName(a).localeCompare(machineName(b)))
  // Collapsed by default: machines are managed in Workspace settings.
  return <SidebarSection id="machines" title={t("wsettings.machines")} count={machines.length} defaultOpen={false}
    action={<button type="button" aria-label={t("dialogs.add_machine")} title={t("dialogs.add_machine")} onClick={onAdd} className="grid size-6 place-items-center rounded-md text-muted-foreground outline-none hover:bg-accent hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring max-md:size-11"><Plus aria-hidden="true" className="size-4" /></button>}>
    <ul aria-label={t("wsettings.machines")}>{shown.map((machine) => {
      const isOnline = machine.status === "online"
      const count = agents.filter((agent) => agent.server_id === machine.server_id).length
      const name = machineName(machine)
      return <li key={machine.server_id}>
        <TreeRow depth={0} treeKey={`machine:${machine.server_id}`} label={name} muted={!isOnline} selected={selectedId === machine.server_id} title={[name, machine.hostname, t(isOnline ? "status.online" : "machine.offline")].filter(Boolean).join(" · ")}
          icon={<Monitor />}
          trailing={<>
            <span className="text-[11px] tabular-nums text-muted-foreground" title={t("sidebar.machine_agents", { count })}><span aria-hidden="true">{count}</span><span className="sr-only">{t("sidebar.machine_agents", { count })}</span></span>
            {isOnline ? <span role="img" aria-label={t("status.online")} className="mx-1 size-2 rounded-full bg-status-online" /> : <TreeStatus status="offline" marker={false} />}
          </>}
          primary={{ kind: "button", onClick: () => onOpen(machine.server_id) }} />
      </li>
    })}</ul>
  </SidebarSection>
}
