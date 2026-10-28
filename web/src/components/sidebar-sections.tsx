import { useEffect, useRef, useState, type ReactNode } from "react"
import { ChevronRight, Monitor, Plus, Search, X } from "lucide-react"
import { machineName, type Agent, type AppDeployment, type Machine, type WorkloadTreeState } from "@/lib/api"
import { attentionGroups } from "@/lib/attention"
import { agentPath, workloadKind } from "@/lib/workload-path"
import { workspaceWorkloads } from "@/lib/workspace-workloads"
import { workloadLinkProps } from "@/lib/workload-link"
import { WorkloadIcon } from "@/components/workload-icon"
import { TreeRow, TreeStatus, treeStatus } from "@/components/tree-row"
import { cn } from "@/lib/utils"
import { t } from "@/i18n"

// A collapsible sidebar section with a sticky heading; open state is remembered per section.
export function SidebarSection({ id, title, count, tone, action, children, defaultOpen = true }: {
  id: string; title: string; count?: number; tone?: "attention"; action?: ReactNode; children: ReactNode; defaultOpen?: boolean
}) {
  const key = `treer:sidebar-section:v1:${id}`
  const [open, setOpen] = useState(() => { try { const value = localStorage.getItem(key); return value === null ? defaultOpen : value === "true" } catch { return defaultOpen } })
  function toggle() { setOpen((value) => { try { localStorage.setItem(key, String(!value)) } catch { /* optional */ } return !value }) }
  return <section aria-labelledby={`sidebar-section-${id}`} data-sidebar-section={id}>
    <div className="sticky top-0 z-[3] flex h-8 items-center justify-between bg-sidebar pl-3 pr-2 max-md:h-11">
      <button id={`sidebar-section-${id}`} type="button" aria-expanded={open} onClick={toggle} className="group/section flex h-full min-w-0 items-center gap-1.5 rounded-md px-1 text-[11px] font-semibold uppercase tracking-wider text-muted-foreground outline-none hover:text-foreground focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring">
        <span className="truncate">{title}</span>
        {count !== undefined && <span className={cn("rounded-full px-1.5 py-px text-[10px] font-semibold tabular-nums normal-case tracking-normal", tone === "attention" && count > 0 ? "bg-status-attention text-background" : "bg-accent text-muted-foreground")}>{count}</span>}
        <ChevronRight aria-hidden="true" className={cn("size-3 opacity-60 transition-transform group-hover/section:opacity-100 motion-reduce:transition-none", open && "rotate-90")} />
      </button>
      {action}
    </div>
    {open && <div className="px-2 pb-2">{children}</div>}
  </section>
}

// Finding one Agent among many: filters the Tree, the attention list and machines. Cmd/Ctrl+K focuses it.
export function SidebarSearch({ value, onChange }: { value: string; onChange: (value: string) => void }) {
  const input = useRef<HTMLInputElement>(null)
  useEffect(() => {
    function onKey(event: KeyboardEvent) {
      if (!(event.metaKey || event.ctrlKey) || event.altKey || event.shiftKey || event.key.toLowerCase() !== "k") return
      const target = event.target
      // Terminals and editors keep Ctrl+K (readline kill-line).
      if (target instanceof Element && target.closest(".xterm, textarea, [contenteditable]:not([contenteditable=\"false\"])")) return
      const sidebar = document.getElementById("tree-sidebar")
      if (!sidebar || sidebar.getClientRects().length === 0) return
      event.preventDefault(); input.current?.focus(); input.current?.select()
    }
    window.addEventListener("keydown", onKey)
    return () => window.removeEventListener("keydown", onKey)
  }, [])
  const mac = typeof navigator !== "undefined" && /Mac|iPhone|iPad/.test(navigator.platform)
  return <div className="relative px-2 pb-2">
    <Search aria-hidden="true" className="pointer-events-none absolute left-4 top-1/2 size-3.5 -translate-y-[calc(50%+4px)] text-muted-foreground" />
    <input ref={input} type="search" value={value} onChange={(event) => onChange(event.target.value)} aria-label={t("sidebar.search")} placeholder={t("sidebar.search")}
      onKeyDown={(event) => {
        if (event.key === "Escape" && value) { event.preventDefault(); onChange("") }
        if (event.key === "ArrowDown") { const first = document.querySelector<HTMLElement>("#tree-sidebar [data-sidebar-section] [data-tree-primary]"); if (first) { event.preventDefault(); first.focus() } }
      }}
      className="h-8 w-full rounded-md border border-transparent bg-background pl-8 pr-14 text-[13px] text-foreground shadow-[0_0_0_1px_hsl(var(--border))] outline-none placeholder:text-muted-foreground focus-visible:ring-2 focus-visible:ring-ring max-md:h-11 [&::-webkit-search-cancel-button]:hidden" />
    {value
      ? <button type="button" aria-label={t("sidebar.clear_search")} onClick={() => { onChange(""); input.current?.focus() }} className="absolute right-3 top-1/2 grid size-6 -translate-y-[calc(50%+4px)] place-items-center rounded text-muted-foreground hover:bg-accent hover:text-foreground max-md:size-10"><X aria-hidden="true" className="size-3.5" /></button>
      : <kbd aria-hidden="true" className="pointer-events-none absolute right-4 top-1/2 -translate-y-[calc(50%+4px)] rounded border bg-sidebar px-1 font-sans text-[10px] text-muted-foreground max-md:hidden">{mac ? "⌘K" : "Ctrl K"}</kbd>}
  </div>
}

export const matches = (query: string, ...values: (string | undefined)[]) => {
  const q = query.trim().toLocaleLowerCase()
  return !q || values.some((value) => value?.toLocaleLowerCase().includes(q))
}

// The triage inbox: every Agent that is blocked, failed or on an offline machine, flat, with its path.
export function AttentionSection({ agents, apps, machines, tree, query, selectedId, workloadHref, onOpen }: {
  agents: Agent[]; apps: AppDeployment[]; machines: Machine[]; tree: WorkloadTreeState; query: string; selectedId: string | null
  workloadHref: (id: string) => string; onOpen: (id: string) => void
}) {
  const online = new Set(machines.filter((machine) => machine.status === "online").map((machine) => machine.server_id))
  const items = attentionGroups(workspaceWorkloads(agents, apps), machines).attention
    .map((agent) => ({ agent, path: agentPath(agent, tree) }))
    .filter(({ agent, path }) => matches(query, agent.name, path))
  const [all, setAll] = useState(false)
  if (!items.length) return null
  // A short inbox keeps the Tree in view; searching or "show all" lists everything.
  const limit = all || query.trim() ? items.length : ATTENTION_PREVIEW
  return <SidebarSection id="attention" title={t("sidebar.attention")} count={items.length} tone="attention">
    <ul aria-label={t("sidebar.attention")}>{items.slice(0, limit).map(({ agent, path }) => {
      const status = treeStatus(agent.status, online.has(agent.server_id))
      const folder = path.slice(0, path.lastIndexOf("/")) || "/"
      return <li key={agent.agent_id}>
        <TreeRow depth={0} treeKey={`attention:${agent.agent_id}`} label={agent.name} detail={folder === "/" ? undefined : folder} selected={selectedId === agent.agent_id}
          title={[path, machineName(machines.find((machine) => machine.server_id === agent.server_id), agent.server_id)].join(" · ")}
          icon={<WorkloadIcon kind={workloadKind(agent)} />}
          trailing={status && <TreeStatus status={status} marker={false} />}
          primary={{ kind: "link", props: { ...workloadLinkProps(workloadHref(agent.agent_id), () => onOpen(agent.agent_id)), "aria-label": t("sidebar.open_attention", { path, status: status ? t(`status.${status}`) : "" }) } }} />
      </li>
    })}</ul>
    {items.length > limit && <button type="button" onClick={() => setAll(true)} className="flex h-7 w-full items-center rounded-md pl-[26px] text-left text-xs text-muted-foreground outline-none hover:bg-accent hover:text-foreground focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring max-md:h-11">{t("sidebar.show_more", { count: items.length - limit })}</button>}
  </SidebarSection>
}
const ATTENTION_PREVIEW = 5

// Machines are first-class: health at a glance, one click to the machine page, enrollment one click away.
export function MachinesSection({ machines, agents, query, selectedId, onOpen, onAdd }: {
  machines: Machine[]; agents: Agent[]; query: string; selectedId: string | null; onOpen: (id: string) => void; onAdd: () => void
}) {
  const shown = machines.filter((machine) => matches(query, machineName(machine), machine.hostname))
    .sort((a, b) => Number(b.status === "online") - Number(a.status === "online") || machineName(a).localeCompare(machineName(b)))
  const offline = machines.filter((machine) => machine.status !== "online").length
  if (query.trim() && !shown.length) return null
  return <SidebarSection id="machines" title={t("wsettings.machines")} count={machines.length} defaultOpen={offline > 0 || machines.length <= 4}
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
