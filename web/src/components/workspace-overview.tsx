import { WorkspacePinAction, WorkspacePinsNotice } from "@/components/workspace-pin-action"
import type { PinsController } from "@/components/use-workspace-pins"
import { workloadPin, type WorkspacePin } from "@/lib/workspace-pins"
import { DropdownMenu, DropdownMenuContent, DropdownMenuTrigger } from "@/components/ui/dropdown-menu"
import { MoreVertical, Pin } from "lucide-react"
import { useMemo, useRef, useState } from "react"
import { ChevronDown, ChevronRight, CircleDot, Folder, LayoutDashboard, Maximize2, Plus } from "lucide-react"
import { machineName, type Agent, type AppDeployment, type Member, type Machine, type Workspace, type WorkloadTreeState } from "@/lib/api"
import { attentionGroups } from "@/lib/attention"
import { overviewForest, visibleOverviewRows, type AttentionFilter } from "@/lib/overview-tree"
import { workspaceWorkloads } from "@/lib/workspace-workloads"
import { workloadKind } from "@/lib/workload-path"
import { WorkloadIcon } from "@/components/workload-icon"
import { Button } from "@/components/ui/button"
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "@/components/ui/dialog"
import TreeMap from "@/components/workload-treemap"
import { cn } from "@/lib/utils"
import { statusLabel, t } from "@/i18n"
import { workloadLinkProps } from "@/lib/workload-link"

const filters = [
  { key: "attention" as const, labelKey: "overview.f_attention" as const, color: "text-status-attention" },
  { key: "working" as const, labelKey: "overview.f_working" as const, color: "text-status-working" },
  { key: "idle" as const, labelKey: "overview.f_idle" as const, color: "text-muted-foreground" },
]

export function WorkspaceOverview({ pins, members, workspace, agents, apps, machines, tree, loading, onOpen, workloadHref, onNewAgent, onWorkspaceSettings }: {
  pins: PinsController; members: Member[]; workspace?: Workspace; agents: Agent[]; apps: AppDeployment[]; machines: Machine[]; tree: WorkloadTreeState; loading: boolean
  onOpen: (id: string) => void; workloadHref: (id: string) => string; onNewAgent: () => void; onWorkspaceSettings: () => void
}) {
  const [mapOpen, setMapOpen] = useState(false)
  const isOpeningAgent = useRef(false)
  const mapTrigger = useRef<HTMLButtonElement>(null)
  const [pinExpanded, setPinExpanded] = useState<Record<string, boolean>>({})
  const [filter, setFilter] = useState<AttentionFilter | null>(null)
  const [expanded, setExpanded] = useState<Record<string, boolean>>({})
  const [filteredExpanded, setFilteredExpanded] = useState<Record<string, boolean>>({})
  const workloads = useMemo(() => workspaceWorkloads(agents, apps), [agents, apps])
  const groups = useMemo(() => attentionGroups(workloads, machines), [workloads, machines])
  const forest = useMemo(() => overviewForest(tree, workloads, groups, filter), [tree, workloads, groups, filter])
  const rows = visibleOverviewRows(forest.roots, filter, filter ? filteredExpanded : expanded)
  const attention = new Set(groups.attention.map((agent) => agent.agent_id))
  const working = new Set(groups.working.map((agent) => agent.agent_id))
  const inventory = new Map(machines.map((machine) => [machine.server_id, machine]))
  function toggleFilter(key: AttentionFilter) {
    setFilter((current) => current === key ? null : key)
    setFilteredExpanded({})
  }
  const unfiltered = useMemo(() => overviewForest(tree, workloads, groups, null), [tree, workloads, groups])
  const allNodes = [...unfiltered.roots]
  for (let i = 0; i < allNodes.length; i++) allNodes.push(...allNodes[i].children)
  const pinnedRoots = pins.pins.flatMap((pin) => {
    const node = pin.kind === 'folder' ? unfiltered.folders.get(pin.id) : allNodes.find((node) => {
      if (!node.agent) return false
      const target = workloadPin(node.agent, apps)
      return target?.kind === pin.kind && target.id === pin.id
    })
    return node ? [node] : []
  })
  const pinnedRows = pinnedRoots.flatMap((node) => visibleOverviewRows([node], null, {...Object.fromEntries(tree.folders.map((folder) => [folder.folder_id,false])), ...pinExpanded}))
  function rowMenu(target: Pick<WorkspacePin,'kind'|'id'> | null, path: string, pinned: boolean) {
    if (!target || !pins.supported || !pins.can_pin) return null
    const pin = pinned && pins.pins.find((pin) => pin.kind === target.kind && pin.id === target.id)
    const name = pin && members.find((member) => member.user_id === pin.pinned_by)?.preferred_name
    return <DropdownMenu><DropdownMenuTrigger asChild><Button variant="ghost" size="icon" className="size-11 shrink-0" aria-label={t('pins.actions',{path})} title={pin ? t('pins.by',{name:name || t('pins.unknown_member')}) : undefined}><MoreVertical aria-hidden="true" className="size-4" /></Button></DropdownMenuTrigger><DropdownMenuContent align="end"><WorkspacePinAction pins={pins} target={target} /></DropdownMenuContent></DropdownMenu>
  }
  function renderRows(rendered: ReturnType<typeof visibleOverviewRows>, pinned = false) {
    return rendered.map(({ node, depth, open }, index) => {
          const key = `${node.agent ? "agent" : "folder"}:${node.id}:${index}`
          const indent = { paddingInlineStart: `${Math.min(depth, 8) * 14}px` }
          if (!node.agent) return <li key={key} style={indent} className="flex min-w-0 items-center"><button type="button" title={node.path} aria-label={t("overview.folder", { path: node.path })} aria-expanded={open} onClick={() => (pinned ? setPinExpanded : filter ? setFilteredExpanded : setExpanded)((current) => ({ ...current, [node.id]: !open }))} className="flex min-h-11 w-full min-w-0 flex-wrap items-center gap-x-2 gap-y-1 rounded-md px-2 py-2 text-left outline-none hover:bg-accent focus-visible:ring-2 focus-visible:ring-ring">
            {open ? <ChevronDown aria-hidden="true" className="size-3.5 shrink-0 text-muted-foreground" /> : <ChevronRight aria-hidden="true" className="size-3.5 shrink-0 text-muted-foreground" />}<Folder aria-hidden="true" className="size-4 shrink-0 text-muted-foreground" /><span className="min-w-0 flex-1 truncate text-xs font-semibold">{node.name}</span>
            <span className="flex max-w-full flex-wrap justify-end gap-x-2 text-[10px] sm:text-xs">{filters.map(({ key: group, labelKey, color }) => node.counts[group] > 0 && (group !== "idle" || (!node.counts.attention && !node.counts.working)) && <span key={group} className={cn("whitespace-nowrap", color, group === "idle" && "hidden sm:inline")}>{node.counts[group]} {t(labelKey)}</span>)}</span>
          </button>{rowMenu({kind:"folder",id:node.id},node.path,pinned)}</li>
          const agent = node.agent, machine = inventory.get(agent.server_id), needsAttention = attention.has(agent.agent_id), isWorking = working.has(agent.agent_id)
          const status = machine?.status === "online" || ["blocked", "failed", "exited", "stopped"].includes(agent.status) ? agent.status : "machine offline"
          const statusText = status === "machine offline" ? t("overview.machine_offline") : statusLabel(status)
          return <li key={key} style={indent} className="flex min-w-0 items-center"><a {...workloadLinkProps(workloadHref(agent.agent_id), () => onOpen(agent.agent_id))} aria-label={t("overview.open", { path: node.path })} title={t("overview.row_title", { path: node.path, kind: t(`kind.${workloadKind(agent)}`), machine: machineName(machine, agent.server_id), status: statusText })} className={cn("flex min-h-11 w-full min-w-0 items-center gap-2 rounded-md px-2 py-2 text-left outline-none hover:bg-accent focus-visible:ring-2 focus-visible:ring-ring", needsAttention && "bg-destructive/[0.07]")}>
            <span className="w-3.5 shrink-0" aria-hidden="true" /><WorkloadIcon kind={agent.kind} className="size-4 shrink-0 text-muted-foreground" />
            <span className="min-w-0 flex-1 sm:flex sm:items-center sm:gap-3"><span className="block truncate text-xs font-medium">{node.name}</span><span className="mt-1 block truncate text-[10px] text-muted-foreground sm:mt-0 sm:text-xs">{t(`kind.${workloadKind(agent)}`)} · {machineName(machine, agent.server_id)}</span></span>
            <span className={cn("max-w-[112px] shrink-0 rounded border px-1.5 py-1 text-[10px]", needsAttention ? "border-destructive/40 text-status-attention" : isWorking ? "border-primary/40 text-status-working" : "text-muted-foreground")}>{statusText}</span>
          </a>{rowMenu(workloadPin(agent,apps),node.path,pinned)}</li>
        })
  }
  return <div className="min-h-0 overflow-auto"><div className="mx-auto w-full max-w-[1120px] px-5 py-8 sm:px-8 sm:py-12 lg:px-14">
    <div className="mb-5 flex flex-wrap items-start justify-between gap-4"><div><LayoutDashboard aria-hidden="true" className="mb-4 size-5 text-muted-foreground" /><h1 className="text-2xl font-semibold">{t("app.workspace_overview")}</h1><p className="mt-2 text-xs text-muted-foreground">{workspace?.name ?? t("overview.choose_a_workspace")}</p></div><div className="flex flex-wrap gap-2"><Button ref={mapTrigger} variant="outline" className="max-md:h-11" aria-label={t("overview.expand_tree_map")} disabled={!workspace || loading} onClick={() => setMapOpen(true)}><Maximize2 />{t("overview.workspace_map")}</Button>{workspace && <Button variant="outline" className="max-md:h-11" onClick={onNewAgent}><Plus />{t("overview.add_agent")}</Button>}</div></div>
    {!workspace ? <p className="rounded-md border p-5 text-sm text-muted-foreground">{t("overview.choose_or_create_a_workspace")}</p> : loading ? <p role="status" className="py-8 text-sm text-muted-foreground">{t("overview.loading_workspace")}</p> : <>
      <WorkspacePinsNotice pins={pins} />
      {pins.supported && pinnedRows.length > 0 && <section aria-label={t('pins.pinned')} className="mb-6 border-b pb-5"><h2 className="mb-2 flex items-center gap-2 text-sm font-semibold"><Pin aria-hidden="true" className="size-4 text-muted-foreground" />{t('pins.pinned')}<span className="text-xs font-normal text-muted-foreground">{t('pins.shared')}</span></h2><ul className="space-y-1">{renderRows(pinnedRows,true)}</ul></section>}
      <div role="group" aria-label={t("overview.attention_filters")} className="mb-4 flex flex-wrap gap-2">{filters.map(({ key, labelKey, color }) => <button key={key} type="button" aria-pressed={filter === key} aria-label={t("overview.filter", { label: t(labelKey) })} onClick={() => toggleFilter(key)} className={cn("flex min-h-10 items-center gap-2 rounded-full border px-3 text-xs max-md:min-h-11 outline-none hover:bg-accent focus-visible:ring-2 focus-visible:ring-ring", filter === key && "border-primary bg-primary/10 ring-1 ring-primary")}><CircleDot aria-hidden="true" className={cn("size-3", color)} /><span className="tabular-nums">{groups[key].length}</span>{t(labelKey)}</button>)}<span className="flex min-h-10 cursor-default items-center px-1 text-xs text-muted-foreground">{t("overview.machines_online", { online: machines.filter((machine) => machine.status === "online").length, total: machines.length })}</span></div>
      <p role="status" className="mb-5 text-xs leading-5 text-muted-foreground">{filter ? t("overview.showing_matching_agents_and_their") : t("overview.active_folders_expand_by_default")}</p>
      {workloads.length === 0 && <div className="mb-6 rounded-md border bg-muted/20 p-5"><h2 className="text-sm font-medium">{t("overview.your_workspace_is_ready")}</h2><p className="mt-2 text-xs text-muted-foreground">{machines.length ? t("overview.add_your_first_agent_to") : t("overview.connect_a_machine_then_add")}</p>{!machines.length && <Button variant="outline" className="mt-4" onClick={onWorkspaceSettings}>{t("wsettings.set_up_a_machine")}</Button>}</div>}
      <section aria-label={t("overview.workspace_overview_tree")} className="min-w-0">
        {rows.length ? <ul className="space-y-1">{renderRows(rows)}</ul> : filter ? <p className="rounded-md border p-5 text-xs text-muted-foreground">{t("overview.no_agents_match_this_filter")}</p> : null}
      </section>
    </>}
  </div>
    <Dialog open={mapOpen} onOpenChange={setMapOpen}><DialogContent className="flex h-[min(820px,calc(100dvh-2rem))] w-[calc(100%-1rem)] max-w-6xl flex-col gap-0 overflow-hidden p-0 sm:w-[calc(100%-3rem)]" onCloseAutoFocus={(event) => { event.preventDefault(); if (!isOpeningAgent.current) mapTrigger.current?.focus(); isOpeningAgent.current = false }}>
      <DialogHeader className="shrink-0 border-b px-5 py-4 pr-12"><DialogTitle>{t("overview.workspace_map")}</DialogTitle><DialogDescription className="text-xs">{t("overview.explore_folders_and_open_agents")}</DialogDescription></DialogHeader>
      {mapOpen && <TreeMap tree={tree} agents={workloads} machines={machines} selectedId={null} workloadHref={workloadHref} onOpen={(id) => { isOpeningAgent.current = true; onOpen(id); setMapOpen(false) }} />}
    </DialogContent></Dialog>
  </div>
}
