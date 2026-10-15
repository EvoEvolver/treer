import { useEffect, useMemo, useRef, useState } from "react"
import { ArrowLeft, ChevronRight, Folder } from "lucide-react"
import { WorkloadIcon } from "@/components/workload-icon"
import { Button } from "@/components/ui/button"
import { machineName, type Agent, type Machine, type WorkloadTreeState } from "@/lib/api"
import { agentPath, folderPath, workloadKind } from "@/lib/workload-path"
import { squarify, type MapRect } from "@/lib/treemap-layout"
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
    all[index].children.sort((a, b) => a.path.localeCompare(b.path, "en") || a.id.localeCompare(b.id))
    all[index].nodes = 1 + all[index].children.reduce((sum, child) => sum + child.nodes, 0)
    if (!all[index].agent) all[index].count = all[index].children.reduce((sum, child) => sum + child.count, 0)
  }
  roots.sort((a, b) => a.path.localeCompare(b.path, "en") || a.id.localeCompare(b.id))
  return { roots, folders }
}

export default function WorkloadTreeMap({ tree, agents, machines, selectedId, onOpen }: {
  tree: WorkloadTreeState; agents: Agent[]; machines: Machine[]; selectedId: string | null; onOpen: (id: string) => void
}) {
  const { roots, folders } = useMemo(() => forest(tree, agents), [tree, agents])
  const [folderId, setFolderId] = useState<string | null>(null)
  const current = folderId ? folders.get(folderId) : undefined
  const [detail, setDetail] = useState("")
  const [tip, setTip] = useState<{ x: number; y: number } | null>(null)
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
  const machineById = new Map(machines.map((machine) => [machine.server_id, machine]))
  function description(item: Branch) {
    if (!item.agent) return `${item.path} · ${t("treemap.workload_count", { count: item.count })}`
    const machine = machineById.get(item.agent.server_id)
    return t("overview.row_title", { path: item.path, kind: t(`kind.${workloadKind(item.agent)}`), machine: machineName(machine, item.agent.server_id), status: statusLabel(machine?.status === "online" ? item.agent.status : "offline") })
  }
  function clearDetail() { setDetail(""); setTip(null) }
  function showDetail(info: string, target: HTMLElement) {
    setDetail(info)
    const anchor = target.getBoundingClientRect(), canvas = container.current?.getBoundingClientRect()
    if (!canvas) return
    const width = Math.min(320, Math.max(0, size.width - 16))
    setTip({ x: Math.max(8, Math.min(size.width - width - 8, anchor.left - canvas.left)),
      y: Math.max(8, Math.min(size.height - 84, anchor.top - canvas.top > size.height / 2 ? anchor.top - canvas.top - 80 : anchor.bottom - canvas.top + 6)) })
  }
  function navigate(id: string | null) {
    setFolderId(id); clearDetail(); breadcrumbs.current?.focus()
  }
  // A native picker keeps *every* immediate child reachable at a 44px target,
  // even when hundreds of equal-weight blocks cannot each fit a touch target.
  // Empty folders have zero area, but remain available here for drill-down.
  function choose(id: string) {
    const item = children.find((child) => child.id === id)
    if (item?.agent) onOpen(item.id)
    else if (item) navigate(item.id)
  }
  let nodes = 0
  function render(items: Branch[], bounds: MapRect, depth: number): React.ReactNode {
    return squarify(items, (item) => item.count, bounds).map(({ item, rect }) => {
      const gap = Math.min(2, rect.width / 8, rect.height / 8)
      const box = { ...rect, x: rect.x + gap, y: rect.y + gap, width: Math.max(0, rect.width - gap * 2), height: Math.max(0, rect.height - gap * 2) }
      const style = { left: box.x, top: box.y, width: box.width, height: box.height }
      const hit = box.width >= 44 && box.height >= 44
      const full = box.width >= 230 && box.height >= 120
      const named = box.width >= 96 && box.height >= 64
      const initial = box.width >= 44 && box.height >= 44
      const info = description(item)
      const events = { onMouseEnter: (event: React.MouseEvent<HTMLElement>) => showDetail(info, event.currentTarget), onMouseLeave: clearDetail, onFocus: (event: React.FocusEvent<HTMLElement>) => showDetail(info, event.currentTarget), onBlur: clearDetail }
      nodes++
      if (item.agent) {
        const machine = machineById.get(item.agent.server_id)
        const status = machine?.status === "online" ? item.agent.status : "offline"
        const colour = status === "working" ? "bg-[hsl(var(--status-working-text))]" : ["running", "idle", "starting"].includes(status) ? "bg-[hsl(var(--status-online))]" : "bg-muted-foreground"
        const diameter = Math.max(0, Math.min(6, box.width - 2, box.height - 2))
        const dot = <span aria-hidden="true" style={{ width: diameter, height: diameter }} className={`treemap-dot shrink-0 rounded-full ${colour}`} />
        const contents = !initial ? dot : <>
          <span className={`flex min-w-0 items-center gap-1.5 ${named ? "text-xs" : "justify-center text-[10px]"}`}>
            {named && <WorkloadIcon kind={workloadKind(item.agent)} className="size-3.5 shrink-0 text-muted-foreground" />}
            {initial && <span className="truncate font-medium">{full ? item.path : named ? item.label : Array.from(item.label).slice(0, 2).join("")}</span>}
            {dot}
          </span>
          {named && <span className="mt-1 block truncate text-[10px] text-muted-foreground">{t(`kind.${workloadKind(item.agent)}`)} · {machineName(machine, item.agent.server_id)}</span>}
          {named && <span className="block truncate text-[10px] text-muted-foreground">{statusLabel(status)}</span>}
        </>
        const className = `treemap-agent absolute overflow-hidden rounded-md border bg-card text-left ${named ? "p-2" : initial ? "p-0.5" : "p-0"} ${hit ? "flex flex-col justify-start hover:border-primary focus-visible:z-10 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring" : "flex items-center justify-center"}`
        return hit ? <button key={item.id} data-map-id={item.id} style={style} className={className} title={info} aria-describedby={detail === info ? "treemap-detail" : undefined} aria-label={t("treemap.open_agent", { description: info })} aria-current={item.id === selectedId ? "true" : undefined} {...events} onClick={() => onOpen(item.id)}>{contents}</button>
          : <div key={item.id} data-map-id={item.id} style={style} className={className} role="img" aria-label={info} title={info} aria-describedby={detail === info ? "treemap-detail" : undefined} {...events}>{contents}</div>
      }
      // A folder-only, single-child chain reserves one title bar, not one
      // bar per level. Each segment remains reachable, including narrow/deep
      // cases where older segments move into a native picker.
      const segments = [item]
      let tail = item
      while (tail.children.length === 1 && !tail.children[0].agent) {
        tail = tail.children[0]; segments.push(tail)
      }
      const inner = { x: 2, y: 46, width: Math.max(0, box.width - 4), height: Math.max(0, box.height - 48) }
      const preview = hit && inner.width >= 88 && inner.height >= 44 && depth < 12 && nodes + tail.nodes < 700
        && squarify(tail.children, (child) => child.count, inner).every(({ item: child, rect: childRect }) => child.agent || (childRect.width >= 48 && childRect.height >= 48))
      const capacity = Math.min(6, Math.max(1, Math.floor((box.width - (named ? 60 : 0)) / 56)))
      const visible = segments.length <= capacity ? segments : segments.slice(segments.length - (capacity - 1))
      const hidden = segments.slice(0, segments.length - visible.length)
      return <section key={item.id} data-map-id={item.id} style={style} aria-label={t("treemap.folder_label", { path: item.path })} className={`treemap-folder absolute overflow-hidden rounded-lg border ${depth % 2 ? "bg-background" : "bg-muted/60"}`}>
        {hit ? <div className={`flex w-full min-w-0 items-center text-xs font-medium ${preview ? "h-11 border-b" : "h-full"}`}>
          {named && <Folder aria-hidden="true" className="mx-1 size-3.5 shrink-0 text-muted-foreground" />}
          {hidden.length > 0 && <select aria-label={t("treemap.chain_folders", { path: item.path })} title={segments.map((segment) => segment.label).join(" / ")} className="h-full min-h-11 w-11 shrink-0 rounded bg-transparent text-xs focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring" value="" onChange={(event) => navigate(event.target.value)}><option value="">…</option>{hidden.map((segment) => <option key={segment.id} value={segment.id}>{segment.path}</option>)}</select>}
          {visible.map((segment, index) => <button key={segment.id} title={description(segment)} aria-label={t("treemap.explore", { path: segment.path })} className="h-full min-h-11 min-w-11 flex-1 truncate px-1 text-left hover:bg-accent focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring" onMouseEnter={(event) => showDetail(description(segment), event.currentTarget)} onMouseLeave={clearDetail} onFocus={(event) => showDetail(description(segment), event.currentTarget)} onBlur={clearDetail} onClick={() => navigate(segment.id)}>{index > 0 && <span aria-hidden="true" className="mr-1 text-muted-foreground">/</span>}{named ? segment.label : Array.from(segment.label).slice(0, 2).join("")}</button>)}
          {named && <span className="mx-1 shrink-0 text-[10px] text-muted-foreground">{item.count}</span>}
        </div> : <div role="img" aria-label={info} title={info} className="flex h-full items-center justify-center" {...events}><Folder aria-hidden="true" className="size-3 text-muted-foreground" /></div>}
        {preview && render(tail.children, inner, depth + 1)}
      </section>
    })
  }
  return <div className="flex min-h-0 min-w-0 flex-1 flex-col overflow-hidden">
    <div className="flex shrink-0 items-center gap-1 border-b px-2 py-1 sm:px-4">
      <Button size="icon" variant="ghost" className="size-11 shrink-0" aria-label={t("treemap.back_to_parent_folder")} disabled={!current} onClick={() => navigate(current?.parent?.id ?? null)}><ArrowLeft className="size-3.5" /></Button>
      <nav ref={breadcrumbs} tabIndex={-1} aria-label={t("treemap.map_breadcrumbs")} className="flex min-w-0 flex-1 items-center gap-1 overflow-hidden rounded text-xs focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring">
        <button className="h-11 shrink-0 rounded px-1 hover:bg-accent focus-visible:ring-2 focus-visible:ring-ring" onClick={() => navigate(null)} aria-current={!current ? "location" : undefined}>{t("app.workspace")}</button>
        {chain.length > 2 && <select aria-label={t("treemap.ancestors")} className="h-11 w-11 shrink-0 rounded border bg-background text-xs" value="" onChange={(event) => navigate(event.target.value)}><option value="">…</option>{chain.slice(0, -2).map((branch) => <option key={branch.id} value={branch.id}>{branch.path}</option>)}</select>}
        {chain.slice(-2).map((branch) => <span key={branch.id} className="flex min-w-0 flex-1 items-center gap-1"><ChevronRight aria-hidden="true" className="size-3 shrink-0 text-muted-foreground" /><button className="h-11 min-w-0 flex-1 truncate rounded px-1 hover:bg-accent focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring" title={branch.path} aria-current={branch.id === current?.id ? "location" : undefined} onClick={() => navigate(branch.id)}>{branch.label}</button></span>)}
      </nav>
      <span className="shrink-0 text-[10px] tabular-nums text-muted-foreground">{t("treemap.workload_count", { count: current?.count ?? agents.length })}</span>
    </div>
    <div className="flex shrink-0 items-center gap-2 border-b px-3 py-1">
      <label htmlFor="map-destination" className="shrink-0 text-[10px] text-muted-foreground">{t("treemap.go_to")}</label>
      <select id="map-destination" aria-label={t("treemap.go_to")} value="" onChange={(event) => choose(event.target.value)} className="h-11 min-w-0 flex-1 rounded border bg-background px-2 text-xs focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"><option value="">{t("treemap.choose_child")}</option>{children.map((item) => <option key={item.id} value={item.id}>{description(item)}</option>)}</select>
    </div>
    <div ref={container} className="relative min-h-0 min-w-0 flex-1 overflow-hidden bg-background" role="group" aria-label={t("treemap.workspace_treemap")}>
      {children.some((child) => child.count > 0) ? size.width > 0 && render(children, { x: 2, y: 2, width: Math.max(0, size.width - 4), height: Math.max(0, size.height - 4) }, 0) : <p role="status" className="flex h-full items-center justify-center px-3 text-center text-sm text-muted-foreground">{current ? t("treemap.this_folder_is_empty") : t("tree.no_workloads_in_this_workspace")}</p>}
      {tip && detail && size.height >= 84 && size.width >= 32 && <div id="treemap-detail" role="tooltip" className="pointer-events-none absolute z-20 rounded-md bg-foreground px-3 py-2 text-xs text-background shadow-md" style={{ left: Math.max(8, Math.min(tip.x, size.width - Math.min(320, size.width - 16) - 8)), top: Math.max(8, Math.min(tip.y, size.height - 84)), width: Math.min(320, size.width - 16) }}><span className="line-clamp-3 break-words">{detail}</span></div>}
    </div>
    <div className="shrink-0 border-t px-3 py-2 text-[10px] text-muted-foreground">
      <div role="group" aria-label={t("treemap.status_legend")} className="mb-1 flex items-center gap-3">
        <span className="flex items-center gap-1"><span aria-hidden="true" className="size-1.5 rounded-full bg-[hsl(var(--status-working-text))]" />{statusLabel("working")}</span>
        <span className="flex items-center gap-1"><span aria-hidden="true" className="size-1.5 rounded-full bg-[hsl(var(--status-online))]" />{t("treemap.online_idle")}</span>
        <span className="flex items-center gap-1"><span aria-hidden="true" className="size-1.5 rounded-full bg-muted-foreground" />{t("treemap.other_offline")}</span>
      </div>
      <p className="truncate" title={detail}>{detail || t("treemap.fixed_canvas_hint")}</p>
    </div>
  </div>
}
