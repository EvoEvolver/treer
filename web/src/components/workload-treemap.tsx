import { useEffect, useMemo, useRef, useState } from "react"
import { ArrowLeft, ChevronRight, Folder, Layers3 } from "lucide-react"
import { WorkloadIcon } from "@/components/workload-icon"
import { Button } from "@/components/ui/button"
import { machineName, type Agent, type Machine, type WorkloadTreeState } from "@/lib/api"
import { agentPath, folderPath, workloadKind } from "@/lib/workload-path"
import "./workload-treemap.css"
import { statusLabel, t } from "@/i18n"

type Branch = { id: string; label: string; path: string; agent?: Agent; children: Branch[]; count: number; nodes: number; parent?: Branch }

function forest(tree: WorkloadTreeState, agents: Agent[]) {
  const folders = new Map<string, Branch>(tree.folders.map((folder) => [folder.folder_id, { id: folder.folder_id, label: folder.name, path: folderPath(folder, tree.folders), children: [], count: 0, nodes: 1 }]))
  const parents = new Map(tree.folders.map((folder) => [folder.folder_id, folder.parent_id]))
  const roots: Branch[] = []
  for (const folder of tree.folders) {
    const branch = folders.get(folder.folder_id)!
    const seen = new Set([folder.folder_id])
    let parentId = folder.parent_id
    let cyclic = false
    while (parentId && folders.has(parentId)) {
      if (seen.has(parentId)) { cyclic = true; break }
      seen.add(parentId); parentId = parents.get(parentId) ?? null
    }
    branch.parent = !cyclic && folder.parent_id ? folders.get(folder.parent_id) : undefined
    if (branch.parent) branch.parent.children.push(branch)
    else roots.push(branch)
  }
  const placements = new Map(tree.placements.map((entry) => [entry.agent_id, entry.folder_id]))
  for (const agent of agents) {
    const branch: Branch = { id: agent.agent_id, label: agent.name, path: agentPath(agent, tree), agent, children: [], count: 1, nodes: 1, parent: folders.get(placements.get(agent.agent_id) ?? "") }
    if (branch.parent) branch.parent.children.push(branch)
    else roots.push(branch)
  }
  // Iterative traversal handles deep folder chains without recursive stack growth.
  const all = [...roots]
  for (let index = 0; index < all.length; index++) all.push(...all[index].children)
  for (let index = all.length - 1; index >= 0; index--) {
    all[index].children.sort((a, b) => Number(Boolean(a.agent)) - Number(Boolean(b.agent)) || a.label.localeCompare(b.label, undefined, { numeric: true, sensitivity: "base" }) || a.id.localeCompare(b.id))
    all[index].nodes = 1 + all[index].children.reduce((sum, child) => sum + child.nodes, 0)
    if (!all[index].agent) all[index].count = all[index].children.reduce((sum, child) => sum + child.count, 0)
  }
  roots.sort((a, b) => Number(Boolean(a.agent)) - Number(Boolean(b.agent)) || a.label.localeCompare(b.label))
  return { roots, folders }
}

export default function WorkloadTreeMap({ tree, agents, machines, selectedId, onOpen }: {
  tree: WorkloadTreeState; agents: Agent[]; machines: Machine[]; selectedId: string | null; onOpen: (id: string) => void
}) {
  const { roots, folders } = useMemo(() => forest(tree, agents), [tree, agents])
  const [folderId, setFolderId] = useState<string | null>(null)
  const current = folderId ? folders.get(folderId) : undefined
  const [detail, setDetail] = useState("")
  const container = useRef<HTMLDivElement>(null)
  const breadcrumbs = useRef<HTMLElement>(null)
  const [size, setSize] = useState({ width: 0 })
  useEffect(() => {
    const observer = new ResizeObserver(([entry]) => setSize({ width: entry.contentRect.width }))
    if (container.current) observer.observe(container.current)
    return () => observer.disconnect()
  }, [])
  useEffect(() => { if (folderId && !folders.has(folderId)) setFolderId(null) }, [folderId, folders])
  const chain: Branch[] = []
  for (let branch = current; branch; branch = branch.parent) chain.unshift(branch)
  const children = current?.children ?? roots
  // Bound nested previews, not explicit drill-down. Very large branches remain
  // counted folder tiles rather than allocating an enormous empty canvas.
  const LEAF_BUDGET = 48
  const NODE_BUDGET = 96
  const narrow = size.width < 600
  let remainingLeaves = Math.max(0, LEAF_BUDGET - children.filter((child) => child.agent).length)
  let remainingNodes = Math.max(0, NODE_BUDGET - children.length)
  function summary(item: Branch) {
    const counts = new Map<string, number>()
    const queue = [item]
    for (let index = 0; index < queue.length; index++) {
      const entry = queue[index]
      if (entry.agent) {
        const state = machines.find((machine) => machine.server_id === entry.agent!.server_id)?.status === "online" ? entry.agent.status : "offline"
        counts.set(state, (counts.get(state) ?? 0) + 1)
      } else queue.push(...entry.children)
    }
    return { states: [...counts].sort(([a], [b]) => a.localeCompare(b)).map(([state, count]) => `${state} ${count}`).join(" · "),
      tags: [...new Map([...item.children.slice(0, 2), ...queue.filter((entry) => entry.agent).slice(0, 3)].map((entry) => [entry.id, entry])).values()].slice(0, 4) }
  }
  function navigate(id: string | null) {
    setFolderId(id); setDetail(""); container.current?.scrollTo({ top: 0 })
    breadcrumbs.current?.focus()
  }
  function render(items: Branch[], depth: number): React.ReactNode {
    return <div className="grid grid-cols-[repeat(auto-fill,minmax(min(100%,180px),1fr))] content-start items-start gap-2">{items.map((item) => {
      if (item.agent) {
        const machine = machines.find((entry) => entry.server_id === item.agent!.server_id)
        const online = machine?.status === "online"
        const status = online ? item.agent.status : "offline"
        const running = online && ["running", "idle", "working", "starting"].includes(status)
        const description = t("overview.row_title", { path: item.path, kind: t(`kind.${workloadKind(item.agent)}`), machine: machineName(machine, item.agent.server_id), status: statusLabel(status) })
        return <button key={`agent:${item.id}`} title={description} aria-label={t("treemap.open_agent", { description })} aria-current={item.id === selectedId ? "true" : undefined} className="treemap-agent flex h-[72px] min-w-0 flex-col justify-start gap-1 overflow-hidden rounded-md border bg-card p-2 text-left transition-colors hover:border-primary focus-visible:z-10 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring" onMouseEnter={() => setDetail(description)} onMouseLeave={() => setDetail("")} onFocus={() => setDetail(description)} onBlur={() => setDetail("")} onClick={() => onOpen(item.id)}>
          <span className="flex w-full min-w-0 items-center gap-2 text-xs font-medium"><WorkloadIcon kind={workloadKind(item.agent)} className="size-3.5 shrink-0 text-muted-foreground" /><span className="truncate">{item.label}</span></span>
          <span className="block w-full min-w-0 text-[10px] leading-4 text-muted-foreground"><span className="block truncate">{t(`kind.${workloadKind(item.agent)}`)} · {machineName(machine, item.agent.server_id)}</span><span className="flex items-center gap-1.5"><span aria-hidden="true" className={`size-1.5 shrink-0 rounded-full ${running ? "bg-[hsl(var(--status-online))]" : "bg-muted-foreground"}`} />{statusLabel(status)}</span></span>
        </button>
      }
      const preview = item.children.length > 0 && item.count <= remainingLeaves && item.nodes - 1 <= remainingNodes
        && size.width - depth * (narrow ? 12 : 20) >= 180
      if (preview) {
        remainingNodes -= item.children.length
        remainingLeaves -= item.children.filter((child) => child.agent).length
      }
      const folded = !preview ? summary(item) : null
      return <section key={`folder:${item.id}`} aria-label={`Folder ${item.path}`} className={`treemap-folder col-span-full min-w-0 overflow-hidden rounded-lg border ${depth % 2 ? "bg-background" : "bg-muted/60"}`}>
        <button title={item.path} aria-label={t("treemap.explore", { path: item.path })} className="flex min-h-11 w-full min-w-0 items-center gap-2 px-2 text-left text-xs font-medium hover:bg-accent focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring" onClick={() => navigate(item.id)} onMouseEnter={() => setDetail(item.path)} onMouseLeave={() => setDetail("")} onFocus={() => setDetail(item.path)} onBlur={() => setDetail("")}><Folder aria-hidden="true" className="size-3.5 shrink-0 text-muted-foreground" /><span className="min-w-0 flex-1 truncate">{item.label}</span><span className="text-[10px] text-muted-foreground">{item.count} {item.count === 1 ? "agent" : "agents"}</span><ChevronRight aria-hidden="true" className="size-3" /></button>
        {preview ? <div className="border-t p-1.5 sm:p-2">{render(item.children, depth + 1)}</div> : <div className="space-y-2 border-t p-2">
          {item.children.length ? <>
            <p className="break-words text-[10px] leading-4 text-muted-foreground">{folded?.states}</p>
            <div className="flex min-w-0 flex-wrap gap-1" aria-label={t("treemap.preview", { path: item.path })}>{folded?.tags.map((child) => <span key={child.id} title={child.path} className="flex max-w-full items-center gap-1 rounded border bg-background px-1.5 py-1 text-[10px] text-muted-foreground">{child.agent ? <WorkloadIcon kind={workloadKind(child.agent)} className="size-3 shrink-0" /> : <Folder aria-hidden="true" className="size-3 shrink-0" />}<span className="max-w-40 truncate">{child.label}</span></span>)}</div>
          </> : <p className="text-xs text-muted-foreground">{t("treemap.empty_folder")}</p>}
          <Button variant="outline" size="sm" aria-label={t("treemap.view_contents", { path: item.path })} onClick={() => navigate(item.id)}><Layers3 aria-hidden="true" className="size-3.5" />{item.children.length ? t(item.count === 1 ? "treemap.expand_one" : "treemap.expand_many", { count: item.count }) : t("treemap.open_folder")}</Button>
        </div>}
      </section>
    })}</div>
  }
  return <div className="flex min-h-0 flex-1 flex-col">
    <div className="flex shrink-0 items-center gap-2 border-b px-3 py-2 sm:px-5">
      <Button size="icon" variant="ghost" aria-label={t("treemap.back_to_parent_folder")} disabled={!current} onClick={() => navigate(current?.parent?.id ?? null)}><ArrowLeft className="size-3.5" /></Button>
      <nav ref={breadcrumbs} tabIndex={-1} aria-label={t("treemap.map_breadcrumbs")} className="flex min-w-0 flex-1 items-center gap-1 overflow-x-auto rounded text-xs focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"><button className="shrink-0 rounded px-1 py-2 hover:bg-accent focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring" onClick={() => navigate(null)} aria-current={!current ? "location" : undefined}>{t("app.workspace")}</button>{chain.map((branch) => <span key={branch.id} className="flex shrink-0 items-center gap-1"><ChevronRight aria-hidden="true" className="size-3 text-muted-foreground" /><button className="max-w-40 truncate rounded px-1 py-2 hover:bg-accent focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring" title={branch.path} aria-current={branch.id === current?.id ? "location" : undefined} onClick={() => navigate(branch.id)}>{branch.label}</button></span>)}</nav>
      <span className="shrink-0 text-[10px] tabular-nums text-muted-foreground">{agents.length} agents</span>
    </div>
    <div ref={container} className="min-h-0 flex-1 overflow-y-auto overflow-x-hidden bg-background p-2 sm:p-3" role="group" aria-label={t("treemap.workspace_treemap")}>
      {children.length ? size.width > 0 && <div className="w-full">{render(children, 0)}</div> : <p role="status" className="flex h-full items-center justify-center text-sm text-muted-foreground">{current ? t("treemap.this_folder_is_empty") : t("tree.no_workloads_in_this_workspace")}</p>}
    </div>
    <p className="min-h-10 shrink-0 truncate border-t px-5 py-3 text-[10px] text-muted-foreground" title={detail}>{detail || t("treemap.compact_tiles_folder_previews_adapt")}</p>
  </div>
}
