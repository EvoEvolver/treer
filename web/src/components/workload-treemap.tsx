import { useEffect, useMemo, useRef, useState } from "react"
import { ArrowLeft, ChevronRight, Folder, Layers3 } from "lucide-react"
import { WorkloadIcon } from "@/components/workload-icon"
import { Button } from "@/components/ui/button"
import { machineName, type Agent, type Machine, type WorkloadTreeState } from "@/lib/api"
import { agentPath, folderPath, workloadKind } from "@/lib/workload-path"
import { treemap, type Rectangle } from "@/lib/treemap"
import "./workload-treemap.css"

type Branch = { id: string; label: string; path: string; agent?: Agent; children: Branch[]; weight: number; count: number; nodes: number; levels: number; parent?: Branch }

function forest(tree: WorkloadTreeState, agents: Agent[]) {
  const folders = new Map<string, Branch>(tree.folders.map((folder) => [folder.folder_id, { id: folder.folder_id, label: folder.name, path: folderPath(folder, tree.folders), children: [], weight: 1, count: 0, nodes: 1, levels: 1 }]))
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
    const branch: Branch = { id: agent.agent_id, label: agent.name, path: agentPath(agent, tree), agent, children: [], weight: 1, count: 1, nodes: 1, levels: 0, parent: folders.get(placements.get(agent.agent_id) ?? "") }
    if (branch.parent) branch.parent.children.push(branch)
    else roots.push(branch)
  }
  // Iterative traversal handles deep folder chains without recursive stack growth.
  const all = [...roots]
  for (let index = 0; index < all.length; index++) all.push(...all[index].children)
  for (let index = all.length - 1; index >= 0; index--) {
    all[index].children.sort((a, b) => Number(Boolean(a.agent)) - Number(Boolean(b.agent)) || a.label.localeCompare(b.label, undefined, { numeric: true, sensitivity: "base" }) || a.id.localeCompare(b.id))
    all[index].weight = Math.max(1, all[index].children.reduce((sum, child) => sum + child.weight, 0))
    all[index].nodes = 1 + all[index].children.reduce((sum, child) => sum + child.nodes, 0)
    all[index].levels = all[index].children.reduce((max, child) => Math.max(max, child.levels + 1), 0)
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
  const [size, setSize] = useState({ width: 0, height: 0 })
  useEffect(() => {
    const observer = new ResizeObserver(([entry]) => setSize({ width: entry.contentRect.width, height: entry.contentRect.height }))
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
  const layoutWeight = (item: Branch) => item.count > LEAF_BUDGET || item.nodes > NODE_BUDGET ? 2 : item.weight + (narrow ? (item.nodes - item.count) * 0.875 : 0)
  const count = children.reduce((sum, child) => sum + layoutWeight(child), 0)
  const levels = children.reduce((max, child) => Math.max(max, child.count <= LEAF_BUDGET && child.nodes <= NODE_BUDGET ? child.levels : 1), 1)
  let remainingLeaves = Math.max(0, LEAF_BUDGET - children.filter((child) => child.agent).length)
  let remainingNodes = Math.max(0, NODE_BUDGET - children.length)
  // Keep touch targets and three-line labels readable. Large inventories scroll
  // vertically rather than reducing every agent to an unusable sliver.
  const height = Math.max(size.height, narrow ? count * 88 : Math.ceil(count * 18000 / Math.max(1, size.width)) + levels * 44)
  function navigate(id: string | null) {
    setFolderId(id); setDetail(""); container.current?.scrollTo({ top: 0 })
    breadcrumbs.current?.focus()
  }
  function render(items: Branch[], bounds: Rectangle, depth: number): React.ReactNode {
    let offset = 0
    const total = Math.max(1, items.reduce((sum, child) => sum + layoutWeight(child), 0))
    const rectangles = narrow ? items.map((item) => {
      const rect = { x: bounds.x, y: bounds.y + offset, width: bounds.width, height: layoutWeight(item) / total * bounds.height }
      offset += rect.height
      return rect
    }) : treemap(items.map(layoutWeight), bounds)
    return items.map((item, index) => {
      const rect = rectangles[index]
      const style = { left: rect.x + 4, top: rect.y + 4, width: Math.max(0, rect.width - 8), height: Math.max(0, rect.height - 8) }
      if (item.agent) {
        const machine = machines.find((entry) => entry.server_id === item.agent!.server_id)
        const online = machine?.status === "online"
        const status = online ? item.agent.status : "offline"
        const running = online && ["running", "idle", "working", "starting"].includes(status)
        const description = `${item.path} · ${workloadKind(item.agent)} · ${machineName(machine, item.agent.server_id)} · ${status}`
        return <button key={`agent:${item.id}`} style={style} title={description} aria-label={`Open ${description}`} aria-current={item.id === selectedId ? "true" : undefined} className="treemap-agent absolute flex min-w-0 flex-col justify-start gap-1 overflow-hidden rounded-md border bg-card p-2 text-left transition-colors hover:border-primary focus-visible:z-10 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring" onMouseEnter={() => setDetail(description)} onMouseLeave={() => setDetail("")} onFocus={() => setDetail(description)} onBlur={() => setDetail("")} onClick={() => onOpen(item.id)}>
          <span className="flex w-full min-w-0 items-center gap-2 text-xs font-medium"><WorkloadIcon kind={workloadKind(item.agent)} className="size-3.5 shrink-0 text-muted-foreground" /><span className="truncate">{item.label}</span></span>
          <span className="block w-full min-w-0 text-[10px] leading-4 text-muted-foreground"><span className="block truncate">{workloadKind(item.agent)} · {machineName(machine, item.agent.server_id)}</span><span className="flex items-center gap-1.5"><span aria-hidden="true" className={`size-1.5 shrink-0 rounded-full ${running ? "bg-[hsl(var(--status-online))]" : "bg-muted-foreground"}`} />{status}</span></span>
        </button>
      }
      const headerHeight = narrow ? 44 : 32
      const childBounds = { x: 2, y: headerHeight + 4, width: Math.max(0, rect.width - 20), height: Math.max(0, rect.height - headerHeight - 20) }
      const childTotal = Math.max(1, item.children.reduce((sum, child) => sum + layoutWeight(child), 0))
      const childRects = narrow
        ? item.children.map((child) => ({ width: childBounds.width, height: layoutWeight(child) / childTotal * childBounds.height }))
        : treemap(item.children.map(layoutWeight), childBounds)
      const preview = item.children.length > 0 && item.count <= remainingLeaves && item.nodes - 1 <= remainingNodes
        && childRects.every((cell) => cell.width >= 112 && cell.height >= 76)
      if (preview) {
        remainingNodes -= item.children.length
        remainingLeaves -= item.children.filter((child) => child.agent).length
      }
      return <section key={`folder:${item.id}`} aria-label={`Folder ${item.path}`} className={`treemap-folder absolute overflow-hidden rounded-lg border ${depth % 2 ? "bg-background" : "bg-muted/60"}`} style={style}>
        <button title={item.path} aria-label={`Explore folder ${item.path}`} style={{ height: headerHeight }} className="flex w-full min-w-0 items-center gap-2 border-b px-2 text-left text-xs font-medium hover:bg-accent focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring" onClick={() => navigate(item.id)} onMouseEnter={() => setDetail(item.path)} onMouseLeave={() => setDetail("")} onFocus={() => setDetail(item.path)} onBlur={() => setDetail("")}><Folder aria-hidden="true" className="size-3.5 shrink-0 text-muted-foreground" /><span className="min-w-0 flex-1 truncate">{item.label}</span><span className="text-[10px] text-muted-foreground">{item.count}</span><ChevronRight aria-hidden="true" className="size-3" /></button>
        {preview ? render(item.children, childBounds, depth + 1) : <button style={{ height: `calc(100% - ${headerHeight}px)` }} className="flex w-full flex-col items-center justify-center gap-2 p-2 text-xs text-muted-foreground hover:bg-accent focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring" aria-label={`View contents of ${item.path}`} onClick={() => navigate(item.id)}><Layers3 aria-hidden="true" className="size-5 opacity-50" /><span>{item.children.length ? `${item.count} ${item.count === 1 ? "agent" : "agents"} · Explore` : "Empty folder"}</span></button>}
      </section>
    })
  }
  return <div className="flex min-h-0 flex-1 flex-col">
    <div className="flex shrink-0 items-center gap-2 border-b px-3 py-2 sm:px-5">
      <Button size="icon" variant="ghost" aria-label="Back to parent folder" disabled={!current} onClick={() => navigate(current?.parent?.id ?? null)}><ArrowLeft className="size-3.5" /></Button>
      <nav ref={breadcrumbs} tabIndex={-1} aria-label="Map breadcrumbs" className="flex min-w-0 flex-1 items-center gap-1 overflow-x-auto rounded text-xs focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"><button className="shrink-0 rounded px-1 py-2 hover:bg-accent focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring" onClick={() => navigate(null)} aria-current={!current ? "location" : undefined}>Workspace</button>{chain.map((branch) => <span key={branch.id} className="flex shrink-0 items-center gap-1"><ChevronRight aria-hidden="true" className="size-3 text-muted-foreground" /><button className="max-w-40 truncate rounded px-1 py-2 hover:bg-accent focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring" title={branch.path} aria-current={branch.id === current?.id ? "location" : undefined} onClick={() => navigate(branch.id)}>{branch.label}</button></span>)}</nav>
      <span className="shrink-0 text-[10px] tabular-nums text-muted-foreground">{agents.length} agents</span>
    </div>
    <div ref={container} className="min-h-0 flex-1 overflow-y-auto overflow-x-hidden bg-background p-2 sm:p-3" role="group" aria-label="Workspace treemap">
      {children.length ? size.width > 0 && <div className="relative" style={{ width: "100%", height }}>{render(children, { x: 0, y: 0, width: Math.max(0, size.width), height }, 0)}</div> : <p role="status" className="flex h-full items-center justify-center text-sm text-muted-foreground">{current ? "This folder is empty" : "No workloads in this workspace"}</p>}
    </div>
    <p className="min-h-10 shrink-0 truncate border-t px-5 py-3 text-[10px] text-muted-foreground" title={detail}>{detail || "Equal-weight agents · Compact previews adapt to space"}</p>
  </div>
}
