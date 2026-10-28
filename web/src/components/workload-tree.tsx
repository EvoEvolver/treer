import { AppOwnedTree, useAppTreeSupport, type AppTreeNavigation } from "@/components/app-owned-tree"
import { WorkspacePinAction, WorkspacePinsNotice } from "@/components/workspace-pin-action"
import type { PinsController } from "@/components/use-workspace-pins"
import { workloadPin } from "@/lib/workspace-pins"
import { workspaceWorkloads } from "@/lib/workspace-workloads"
import { useEffect, useRef, useState } from "react"
import { api, type Agent, type AppDeployment, type Machine, type WorkloadFolder as FolderEntry, type WorkloadTreeState as Tree } from "@/lib/api"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle, DialogFooter } from "@/components/ui/dialog"
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuSeparator, DropdownMenuTrigger } from "@/components/ui/dropdown-menu"
import { ArrowDown, ArrowUp, Copy, ExternalLink, Folder, FolderInput, FolderOpen, FolderPlus, MoreHorizontal, Monitor, Pencil, Play, Plus, RotateCcw, ScrollText, Settings2, Square, SquareTerminal, Trash2 } from "lucide-react"
import { agentPath, workloadKind, folderPath as resolveFolderPath } from "@/lib/workload-path"
import { readFolderExpansion, writeFolderExpansion, type FolderExpansion } from "@/lib/folder-expansion"
import { AgentStartupDialog } from "@/components/agent-startup-dialog"

import { WorkloadIcon } from "@/components/workload-icon"
import { matches } from "@/components/sidebar-sections"
import { t } from "@/i18n"
import { TreeCount, TreeGroup, TreeRow, TreeStatus, isAttention, isInactive, treeActionsClass, treeIndent, treeStatus } from "@/components/tree-row"
import { workloadLinkProps } from "@/lib/workload-link"

// Marker in a render ancestor set: an enclosing folder matched the search, so show all of its contents.
const MATCHED = "\u0000matched"
export interface NewAgentFolder { folderId: string; path: string }
type Item = { kind: "folder" | "agent"; id: string; name: string }

export function WorkloadTree({ pins, workspaceId, agents, apps, machines, selectedId, onOpen, workloadHref, onOpenInNewTab, onSettings, onRename, onDelete, onNew, onAddMachine, preview, placementRevision = 0, onTreeChange, prewarm, appTree, query = "" }: {
  /** Sidebar search: shows matching rows and the folders that contain them. */
  query?: string
  appTree?: AppTreeNavigation
  prewarm?: { enter: (id: string, intent: 'pointer' | 'focus') => void; leave: (id: string) => void }
  pins: PinsController; workspaceId: string | null; agents: Agent[]; apps: AppDeployment[]; machines: Machine[]; selectedId: string | null
  onOpen: (id: string, logs?: boolean) => void; workloadHref: (id: string) => string; onOpenInNewTab: (id: string) => void; onSettings: (app: AppDeployment) => void; onRename: (agent: Agent) => void; onDelete: (agent: Agent) => void; onNew: (folder?: NewAgentFolder) => void; onAddMachine: () => void; preview: boolean; placementRevision?: number; onTreeChange?: (tree: Tree) => void
}) {
  const [tree, setTree] = useState<Tree>({ folders: [], placements: [] })
  const [loading, setLoading] = useState(false)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState("")
  const [expanded, setExpanded] = useState<FolderExpansion>(() => readFolderExpansion(workspaceId))
  const expansion = useRef(expanded)
  const [treeLoaded, setTreeLoaded] = useState(false)
  const previousSelection = useRef<string | null>(null)
  const pendingReveal = useRef<string | null>(null)
  function saveExpansion(next: FolderExpansion) {
    expansion.current = next; setExpanded(next); writeFolderExpansion(workspaceId, next)
  }
  const [dragged, setDragged] = useState<Item | null>(null)
  const [dropTarget, setDropTarget] = useState<{ id: string; after: boolean } | null>(null)
  const [editor, setEditor] = useState<{ mode: "create" | "rename" | "move"; item?: Item; parent: string | null } | null>(null)
  const [name, setName] = useState("")
  const [destination, setDestination] = useState("")
  const [reload, setReload] = useState(0)
  const [copyNotice, setCopyNotice] = useState("")
  const [manualCopyPath, setManualCopyPath] = useState<string | null>(null)
  const [startupAgent, setStartupAgent] = useState<Agent | null>(null)
  const copyInput = useRef<HTMLInputElement>(null)
  useEffect(() => { if (!copyNotice) return; const timer = window.setTimeout(() => setCopyNotice(""), 6000); return () => window.clearTimeout(timer) }, [copyNotice])
  const mutation = useRef(false)
  const readGeneration = useRef(0)
  const base = `/api/workspaces/${encodeURIComponent(workspaceId ?? "")}/workload-tree`
  useEffect(() => {
    setTree({ folders: [], placements: [] }); setError(""); setEditor(null); setTreeLoaded(false); expansion.current = readFolderExpansion(workspaceId); setExpanded(expansion.current); previousSelection.current = null; pendingReveal.current = null; setCopyNotice(""); setManualCopyPath(null)
  }, [workspaceId, preview])
  useEffect(() => {
    let active = true
    setError("")
    if (!workspaceId || preview) return
    setLoading(true)
    let reading = false
    async function refresh() {
      if (reading || mutation.current) return
      reading = true
      const generation = ++readGeneration.current
      try {
        const data = await api<Tree>(base)
        if (active && generation === readGeneration.current) { setTree(data); setTreeLoaded(true) }
      } catch (reason) {
        if (active && generation === readGeneration.current) setError(reason instanceof Error ? reason.message : t("tree.unable_to_update_workload_tree"))
      } finally { reading = false; if (active) setLoading(false) }
    }
    void refresh()
    const timer = window.setInterval(() => { if (!document.hidden) void refresh() }, 5000)
    const focus = () => void refresh()
    window.addEventListener("focus", focus)
    return () => { active = false; window.clearInterval(timer); window.removeEventListener("focus", focus) }
  }, [base, workspaceId, preview, reload, placementRevision])
  // Reconcile only an authoritative successful tree read, never the empty loading state.
  useEffect(() => {
    if (!treeLoaded) return
    const valid = new Set(tree.folders.map((folder) => folder.folder_id))
    const next = Object.fromEntries(Object.entries(expansion.current).filter(([id]) => valid.has(id)))
    if (Object.keys(next).length !== Object.keys(expansion.current).length) saveExpansion(next)
  }, [tree, treeLoaded])
  const selectedAncestors: string[] = []
  let selectedFolder = tree.placements.find((entry) => entry.agent_id === selectedId)?.folder_id
  const visited = new Set<string>()
  while (selectedFolder && !visited.has(selectedFolder)) {
    visited.add(selectedFolder)
    const folder = tree.folders.find((entry) => entry.folder_id === selectedFolder)
    if (!folder) break
    selectedAncestors.push(folder.folder_id); selectedFolder = folder.parent_id
  }
  const selectionChain = JSON.stringify(selectedAncestors)
  useEffect(() => {
    if (!treeLoaded) return
    const changedSelection = previousSelection.current !== selectedId
    if (changedSelection) pendingReveal.current = selectedId
    const next = { ...expansion.current }
    let changed = false
    const chain = JSON.parse(selectionChain) as string[]
    for (const id of chain) {
      // Overview starts with no selection; explicitly choosing an Agent reveals only its chain.
      if (next[id] !== true && pendingReveal.current === selectedId && selectedId !== null) { next[id] = true; changed = true }
    }
    if (chain.length) pendingReveal.current = null
    previousSelection.current = selectedId
    if (changed) saveExpansion(next)
  }, [selectedId, selectionChain, treeLoaded])
  const workloads = workspaceWorkloads(agents, apps)
  const placements = new Map(tree.placements.map((entry) => [entry.agent_id, entry]))
  function orderedRows(parent: string | null): Agent[] {
    return workloads.filter((agent) => (placements.get(agent.agent_id)?.folder_id ?? null) === parent)
      .sort((a, b) => {
        const left = placements.get(a.agent_id)
        const right = placements.get(b.agent_id)
        if (left && !right) return -1
        if (!left && right) return 1
        return (left?.position ?? 0) - (right?.position ?? 0)
          || a.name.localeCompare(b.name, undefined, { numeric: true, sensitivity: "base" })
          || a.agent_id.localeCompare(b.agent_id)
      })
  }
  function descendants(id: string): Set<string> {
    const result = new Set([id])
    let changed = true
    while (changed) { changed = false; for (const folder of tree.folders) if (folder.parent_id && result.has(folder.parent_id) && !result.has(folder.folder_id)) { result.add(folder.folder_id); changed = true } }
    return result
  }
  useEffect(() => { onTreeChange?.(tree) }, [tree, onTreeChange])
  function folderPath(folder: FolderEntry) { return resolveFolderPath(folder, tree.folders) }
  function itemPath(item: Item, parent: string | null) {
    const folder = tree.folders.find((entry) => entry.folder_id === (item.kind === "folder" ? item.id : parent))
    if (item.kind === "folder") return folder ? folderPath(folder) : `/${item.name}`
    // App deployment labels are the canonical workload labels; legacy runtime
    // inventory may still contain the retired app: display prefix.
    return agentPath(workloads.find((agent) => agent.agent_id === item.id)!, tree)
  }
  async function copyPath(path: string) {
    setCopyNotice("")
    try {
      if (!navigator.clipboard?.writeText) throw new Error(t("tree.clipboard_unavailable"))
      await navigator.clipboard.writeText(path)
      setCopyNotice(t("tree.copied", { path }))
    } catch {
      setCopyNotice(t("tree.unable_to_copy_automatically_select"))
      setManualCopyPath(path)
    }
  }
  async function mutate(path: string, method: string, body?: unknown) {
    if (!workspaceId || busy || mutation.current) return
    mutation.current = true; readGeneration.current += 1
    setBusy(true); setError("")
    try {
      await api(`${base}${path}`, { method, ...(body === undefined ? {} : { body: JSON.stringify(body) }) })
      const updated = await api<Tree>(base)
      setTree(updated); setTreeLoaded(true); setEditor(null)
    } catch (reason) { setError(reason instanceof Error ? reason.message : t("tree.unable_to_update_workload_tree")) } finally { mutation.current = false; setBusy(false) }
  }
  function move(item: Item, folderId: string | null) {
    if (item.kind === "folder") {
      if (folderId && descendants(item.id).has(folderId)) { setError("A folder cannot contain itself or its descendants."); return }
      void mutate(`/folders/${encodeURIComponent(item.id)}`, "PATCH", { parent_id: folderId })
    } else void mutate(`/placements/${encodeURIComponent(item.id)}`, "PUT", { folder_id: folderId, position: Math.max(-1, ...tree.placements.filter((placement) => placement.folder_id === folderId).map((placement) => placement.position)) + 1 })
  }
  async function reorder(agentId: string, targetId: string, after: boolean, parent: string | null) {
    if (!workspaceId || busy || mutation.current || agentId === targetId) return
    const siblings = orderedRows(parent)
    const remaining = siblings.filter((agent) => agent.agent_id !== agentId)
    const targetIndex = remaining.findIndex((agent) => agent.agent_id === targetId)
    if (targetIndex < 0) return
    const position = targetIndex + (after ? 1 : 0)
    if (siblings[position]?.agent_id === agentId) return
    mutation.current = true; readGeneration.current += 1
    setBusy(true); setError("")
    try {
      // The placement API indexes persisted siblings. First append unplaced
      // siblings in their displayed order so the insertion index is meaningful.
      let next = Math.max(-1, ...tree.placements.filter((entry) => entry.folder_id === parent).map((entry) => entry.position)) + 1
      for (const agent of siblings) {
        if (!placements.has(agent.agent_id)) {
          await api(`${base}/placements/${encodeURIComponent(agent.agent_id)}`, {
            method: "PUT", body: JSON.stringify({ folder_id: parent, position: next++ }),
          })
        }
      }
      await api(`${base}/placements/${encodeURIComponent(agentId)}`, {
        method: "PUT", body: JSON.stringify({ folder_id: parent, position }),
      })
    } catch (reason) { setError(reason instanceof Error ? reason.message : t("tree.unable_to_update_workload_tree")) }
    finally {
      // Also read after a partial failure: initialization may have succeeded.
      try { setTree(await api<Tree>(base)); setTreeLoaded(true) } catch (reason) { setError(reason instanceof Error ? reason.message : t("tree.unable_to_update_workload_tree")) }
      mutation.current = false; setBusy(false)
    }
  }
  async function lifecycle(agent: Agent, action: string) {
    setBusy(true); setError("")
    try { await api(`/api/workspaces/${encodeURIComponent(workspaceId ?? "")}/agents/${encodeURIComponent(agent.agent_id)}/${action}`, { method: "POST", body: "{}" }) } catch (reason) { setError(reason instanceof Error ? reason.message : t("tree.unable_to_update_workload_tree")) } finally { setBusy(false) }
  }
  function edit(mode: "create" | "rename" | "move", parent: string | null, item?: Item) { setName(mode === "rename" ? item?.name ?? "" : ""); setDestination(parent ?? ""); setError(""); setEditor({ mode, parent, item }) }
  function menuItems(item: Item, parent: string | null, agent?: Agent) {
    const app = agent && apps.find((entry) => entry.runtime_agent_id === agent.agent_id || entry.app_id === agent.agent_id)
    const siblings = orderedRows(parent)
    const index = agent ? siblings.findIndex((entry) => entry.agent_id === agent.agent_id) : -1
    const onlineMachine = agent && online.has(agent.server_id)
    const anyOnline = preview || machines.length === 0 || machines.some((machine) => machine.status === "online")
    // Grouped: open, run, organise, then the destructive pair after the last separator.
    return <>
      {agent ? <>
        <DropdownMenuItem onSelect={() => onOpenInNewTab(agent.agent_id)}><ExternalLink />{t("app.open_in_new_tab")}</DropdownMenuItem>
        <DropdownMenuItem onSelect={() => onOpen(agent.agent_id, true)}><ScrollText />{t("tree.logs")}</DropdownMenuItem>
        {app && <DropdownMenuItem onSelect={() => onSettings(app)}><Settings2 />{t("tree.app_settings")}</DropdownMenuItem>}
        <DropdownMenuSeparator />
        <DropdownMenuItem disabled={busy || !onlineMachine} onSelect={() => void lifecycle(agent, "start")}><Play />{t("apps.start")}</DropdownMenuItem>
        <DropdownMenuItem disabled={busy} onSelect={() => void lifecycle(agent, "restart")}><RotateCcw />{t("apps.restart")}</DropdownMenuItem>
        {!app && agent.kind !== "app" && <DropdownMenuItem disabled={busy || preview || !onlineMachine} onSelect={() => setStartupAgent(agent)}><SquareTerminal />{t("startup.edit_startup_command")}</DropdownMenuItem>}
      </> : <>
        <DropdownMenuItem disabled={!workspaceId || busy || !anyOnline} onSelect={() => onNew({ folderId: item.id, path: itemPath(item, parent) })}><Plus />{t("tree.new_agent")}</DropdownMenuItem>
        <DropdownMenuItem onSelect={() => edit("create", item.id)}><FolderPlus />{t("tree.new_folder")}</DropdownMenuItem>
      </>}
      <DropdownMenuSeparator />
      <DropdownMenuItem onSelect={() => agent ? onRename(agent) : edit("rename", parent, item)}><Pencil />{t("machine.rename")}</DropdownMenuItem>
      <DropdownMenuItem onSelect={() => void copyPath(itemPath(item, parent))}><Copy />{t("tree.copy_path")}</DropdownMenuItem>
      <DropdownMenuItem onSelect={() => edit("move", parent, item)}><FolderInput />{t("tree.move_to_folder")}</DropdownMenuItem>
      {agent && <>
        <DropdownMenuItem disabled={busy || index <= 0} onSelect={() => void reorder(agent.agent_id, siblings[index - 1].agent_id, false, parent)}><ArrowUp />{t("tree.move_up")}</DropdownMenuItem>
        <DropdownMenuItem disabled={busy || index < 0 || index >= siblings.length - 1} onSelect={() => void reorder(agent.agent_id, siblings[index + 1].agent_id, true, parent)}><ArrowDown />{t("tree.move_down")}</DropdownMenuItem>
      </>}
      <WorkspacePinAction pins={pins} target={agent ? workloadPin(agent, apps) : { kind: "folder", id: item.id }} />
      <DropdownMenuSeparator data-tree-danger="" />
      {agent && <DropdownMenuItem className="text-status-attention focus:text-status-attention" disabled={busy} onSelect={() => setStopTarget(agent)}><Square />{t("apps.stop")}</DropdownMenuItem>}
      <DropdownMenuItem className="text-status-attention focus:text-status-attention" disabled={busy} onSelect={() => agent ? onDelete(agent) : void mutate(`/folders/${encodeURIComponent(item.id)}`, "DELETE")}><Trash2 />{t("machine.delete")}</DropdownMenuItem>
    </>
  }
  function menu(item: Item, parent: string | null, agent?: Agent) {
    return <DropdownMenu><DropdownMenuTrigger asChild><button type="button" className={`grid place-items-center outline-none focus-visible:ring-2 focus-visible:ring-ring ${treeActionsClass}`} aria-label={t("tree.actions_for", { name: item.name })} tabIndex={-1} data-tree-actions=""><MoreHorizontal aria-hidden="true" className="size-4" /></button></DropdownMenuTrigger><DropdownMenuContent align="end" className="max-h-[80dvh] overflow-y-auto">{menuItems(item, parent, agent)}</DropdownMenuContent></DropdownMenu>
  }
  // Keyboard model (WAI-ARIA tree): one Tab stop on the row buttons (roving
  // tabindex), arrows move, Right/Left expand, collapse or enter/leave a row's
  // actions menu button. Menu buttons are reachable by ArrowRight, not Tab.
  // Right-click, Shift+F10 and the Menu key open the same actions as the row's ⋯ button, anchored at the pointer
  // (or under the focused row). One shared menu serves every row; the actions come from menuItems.
  type ContextTarget = { item: Item; parent: string | null; agent?: Agent; x: number; y: number }
  // Stop is a destructive action: it always asks first, like Delete.
  const [stopTarget, setStopTarget] = useState<Agent | null>(null)
  const [contextTarget, setContextTarget] = useState<ContextTarget | null>(null)
  const contextReturn = useRef<HTMLElement | null>(null)
  function openContext(item: Item, parent: string | null, agent: Agent | undefined, origin: HTMLElement, x?: number, y?: number) {
    const rect = origin.getBoundingClientRect()
    contextReturn.current = origin.closest<HTMLElement>('[role="treeitem"]')?.querySelector<HTMLElement>("[data-tree-primary]") ?? origin
    setContextTarget({ item, parent, agent, x: x ?? rect.left + 12, y: y ?? rect.bottom })
  }
  const treeRoot = useRef<HTMLDivElement>(null)
  const treeActive = useRef<string | null>(null)
  function syncTreeTabStop() {
    const primaries = Array.from(treeRoot.current?.querySelectorAll<HTMLElement>("[data-tree-primary]") ?? [])
    const wanted = primaries.find((el) => el.dataset.treeKey === treeActive.current)
      ?? primaries.find((el) => el.dataset.treeKey === selectedId) ?? primaries[0]
    for (const el of primaries) el.tabIndex = el === wanted ? 0 : -1
  }
  useEffect(syncTreeTabStop)
  function trackTreeFocus(event: React.FocusEvent<HTMLDivElement>) {
    const key = (event.target as HTMLElement).closest<HTMLElement>("[data-tree-primary]")?.dataset.treeKey
    if (key && key !== treeActive.current) { treeActive.current = key; syncTreeTabStop() }
  }
  function treeKeyDown(event: React.KeyboardEvent<HTMLDivElement>) {
    if ((event.key === "ContextMenu" || (event.shiftKey && event.key === "F10")) && (event.target as HTMLElement).matches("[data-tree-primary]")) {
      const key = (event.target as HTMLElement).dataset.treeKey ?? ""
      const agent = workloads.find((entry) => entry.agent_id === key)
      const folder = agent ? undefined : tree.folders.find((entry) => entry.folder_id === key)
      if (agent || folder) {
        event.preventDefault()
        if (agent) openContext({ kind: "agent", id: agent.agent_id, name: agent.name }, placements.get(agent.agent_id)?.folder_id ?? null, agent, event.target as HTMLElement)
        else if (folder) openContext({ kind: "folder", id: folder.folder_id, name: folder.name }, folder.parent_id, undefined, event.target as HTMLElement)
      }
      return
    }
    if (event.altKey || event.ctrlKey || event.metaKey || event.shiftKey) return
    const target = event.target as HTMLElement
    const root = event.currentTarget
    if (!root.contains(target) || !(target.matches("[data-tree-primary]") || target.matches("[data-tree-actions]"))) return
    const onActions = target.matches("[data-tree-actions]")
    const item = target.closest<HTMLElement>('[role="treeitem"]')
    const primary = item?.querySelector<HTMLElement>("[data-tree-primary]")
    if (!item || !primary) return
    const all = Array.from(root.querySelectorAll<HTMLElement>("[data-tree-primary]"))
    const at = all.indexOf(primary)
    const isFolder = item.hasAttribute("aria-expanded")
    const open = item.getAttribute("aria-expanded") === "true"
    // Folders toggle with their row button; Apps and App nodes have a separate disclosure button.
    const toggle = item.querySelector<HTMLElement>(":scope > [data-tree-row] [data-tree-toggle]") ?? primary
    let next: HTMLElement | undefined
    switch (event.key) {
      case "ArrowDown": next = all[Math.min(at + 1, all.length - 1)]; break
      case "ArrowUp": next = all[Math.max(at - 1, 0)]; break
      case "Home": next = all[0]; break
      case "End": next = all[all.length - 1]; break
      case "ArrowRight":
        if (onActions) return
        if (isFolder && !open) { toggle.click(); event.preventDefault(); return }
        if (isFolder) { const child = all[at + 1]; next = child && item.contains(child) ? child : undefined } else next = item.querySelector<HTMLElement>("[data-tree-actions]") ?? undefined
        break
      case "ArrowLeft":
        if (onActions) next = primary
        else if (isFolder && open) { toggle.click(); event.preventDefault(); return }
        else next = item.parentElement?.closest<HTMLElement>('[role="treeitem"]')?.querySelector<HTMLElement>("[data-tree-primary]") ?? undefined
        break
      default: return
    }
    event.preventDefault()
    next?.focus()
  }
  const online = new Set(machines.filter((machine) => machine.status === "online").map((machine) => machine.server_id))
  const statusOf = (agent: Agent) => treeStatus(agent.status, online.has(agent.server_id))
  const childFolders = (parent: string | null, ancestors: Set<string>) => tree.folders.filter((folder) => folder.parent_id === parent && !ancestors.has(folder.folder_id)).sort((a, b) => a.name.localeCompare(b.name, undefined, { numeric: true, sensitivity: "base" }) || a.folder_id.localeCompare(b.folder_id))
  // What a collapsed folder hides: Agents needing attention, else working ones.
  function hidden(folderId: string, seen = new Set<string>()): { attention: number; working: number } {
    seen.add(folderId)
    const total = { attention: 0, working: 0 }
    for (const agent of orderedRows(folderId)) {
      if (agent.kind === "app") continue
      const key = statusOf(agent)
      if (isAttention(key)) total.attention++
      else if (key === "working" || key === "starting") total.working++
    }
    for (const folder of childFolders(folderId, seen)) { const inner = hidden(folder.folder_id, seen); total.attention += inner.attention; total.working += inner.working }
    return total
  }
  const [appOpen, setAppOpen] = useState<Record<string, boolean>>({})
  // An App row discloses contents only when that App declared a directory by answering the tree contract.
  const treeApps = useAppTreeSupport(appTree && !preview ? workspaceId : null, apps)
  const [dropFolder, setDropFolder] = useState<string | null>(null)
  function dragProps(item: Item) {
    return {
      draggable: !busy,
      onDragStart: (event: React.DragEvent) => { event.stopPropagation(); setDragged(item); event.dataTransfer.effectAllowed = "move"; event.dataTransfer.setData("text/plain", JSON.stringify(item)) },
      onDragEnd: () => { setDragged(null); setDropTarget(null); setDropFolder(null) },
    }
  }
  const searching = query.trim() !== ""
  const agentMatches = (agent: Agent) => matches(query, agent.name, agentPath(agent, tree))
  function folderMatches(folderId: string, seen: Set<string>): boolean {
    const folder = tree.folders.find((entry) => entry.folder_id === folderId)
    if (!folder || seen.has(folderId)) return false
    seen.add(folderId)
    return matches(query, folder.name) || orderedRows(folderId).some(agentMatches) || childFolders(folderId, seen).some((child) => folderMatches(child.folder_id, seen))
  }
  function render(parent: string | null, ancestors: Set<string> = new Set()): React.ReactNode {
    const depth = ancestors.size
    // While searching, a folder whose name matches shows everything inside it.
    const showAll = !searching || (parent !== null && ancestors.has(MATCHED))
    return <>{childFolders(parent, ancestors).filter((folder) => showAll || folderMatches(folder.folder_id, new Set(ancestors))).map((folder) => {
      const named = searching && !showAll && matches(query, folder.name)
      const item: Item = { kind: "folder", id: folder.folder_id, name: folder.name }
      const open = searching || expanded[folder.folder_id] === true
      const summary = open ? null : hidden(folder.folder_id, new Set(ancestors))
      const summaryText = summary && [summary.attention && t("tree.hidden_attention", { count: summary.attention }), summary.working && t("tree.hidden_working", { count: summary.working })].filter(Boolean).join(", ")
      return <div key={folder.folder_id} role="treeitem" aria-level={depth + 1} aria-expanded={open} aria-label={folder.name} aria-description={summaryText || undefined}>
        <TreeRow depth={depth} expanded={open} treeKey={folder.folder_id} label={folder.name} title={summaryText ? `${folder.name} · ${summaryText}` : folder.name}
          icon={open ? <FolderOpen /> : <Folder />}
          primary={{ kind: "button", toggles: true, onClick: () => { pendingReveal.current = null; saveExpansion({ ...expansion.current, [folder.folder_id]: !open }) } }}
          trailing={summary && (summary.attention ? <TreeCount tone="attention" count={summary.attention} label={summaryText ?? ""} /> : summary.working ? <TreeCount tone="working" count={summary.working} label={summaryText ?? ""} /> : null)}
          actions={menu(item, parent)}
          highlight={dropFolder === folder.folder_id ? "drop" : null}
          rowProps={{
            ...dragProps(item),
            onContextMenu: (event) => { event.preventDefault(); openContext(item, parent, undefined, event.currentTarget, event.clientX, event.clientY) },
            onDragOver: (event) => { event.preventDefault(); event.stopPropagation(); if (dragged && dragged.id !== folder.folder_id) setDropFolder(folder.folder_id) },
            onDragLeave: (event) => { if (!event.currentTarget.contains(event.relatedTarget as Node | null)) setDropFolder(null) },
            onDrop: (event) => { event.preventDefault(); event.stopPropagation(); if (dragged) move(dragged, folder.folder_id); setDragged(null); setDropFolder(null) },
          }} />
        {open && <TreeGroup depth={depth + 1}>{tree.folders.some((entry) => entry.parent_id === folder.folder_id) || orderedRows(folder.folder_id).length
          ? render(folder.folder_id, new Set([...ancestors, folder.folder_id, ...(named || (searching && showAll) ? [MATCHED] : [])]))
          : <p className="flex h-7 items-center text-xs text-muted-foreground max-md:h-9" style={{ paddingLeft: treeIndent(depth + 1) + 22 }}>{t("tree.empty_folder")}</p>}</TreeGroup>}
      </div>
    })}{orderedRows(parent).filter((agent) => showAll || agentMatches(agent)).map((agent) => {
      const item: Item = { kind: "agent", id: agent.agent_id, name: agent.name }
      const status = statusOf(agent)
      const ownedApps = appTree && workspaceId && !preview ? apps.filter((app) => (app.runtime_agent_id ?? app.app_id) === agent.agent_id && treeApps.has(app.app_id)) : []
      const expandable = ownedApps.length > 0
      const appExpanded = expandable ? appOpen[agent.agent_id] === true : undefined
      const selected = selectedId === agent.agent_id
      const machine = machines.find((entry) => entry.server_id === agent.server_id)?.name
      return <div key={agent.agent_id} role="treeitem" aria-level={depth + 1} aria-label={agent.name} aria-selected={selected} aria-expanded={appExpanded} data-agent-id={agent.agent_id} data-owner-app={apps.find((app) => (app.runtime_agent_id ?? app.app_id) === agent.agent_id)?.app_id}>
        <TreeRow depth={depth} expanded={appExpanded} treeKey={agent.agent_id} label={agent.name} selected={selected} muted={isInactive(status)}
          title={[agentPath(agent, tree), machine].filter(Boolean).join(" · ")}
          icon={<WorkloadIcon kind={workloadKind(agent)} />}
          toggle={expandable ? { label: t("app_tree.contents"), onClick: () => setAppOpen((value) => ({ ...value, [agent.agent_id]: !value[agent.agent_id] })) } : undefined}
          primary={{ kind: "link", props: workloadLinkProps(workloadHref(agent.agent_id), () => onOpen(agent.agent_id)) }}
          trailing={status && <TreeStatus status={status} />}
          actions={menu(item, parent, agent)}
          highlight={dropTarget?.id === agent.agent_id ? dropTarget.after ? "after" : "before" : null}
          rowProps={{
            ...dragProps(item),
            onPointerEnter: (event) => { if (event.pointerType === "mouse" && event.buttons === 0) prewarm?.enter(agent.agent_id, "pointer") },
            onPointerLeave: () => prewarm?.leave(agent.agent_id),
            onFocus: (event) => { if ((event.target as Element).matches("[data-tree-primary]")) prewarm?.enter(agent.agent_id, "focus") },
            onBlur: () => prewarm?.leave(agent.agent_id),
            onContextMenu: (event) => { event.preventDefault(); openContext(item, parent, agent, event.currentTarget, event.clientX, event.clientY) },
            onDragOver: (event) => {
              if (busy || dragged?.kind !== "agent" || dragged.id === agent.agent_id) return
              event.preventDefault(); event.stopPropagation()
              event.dataTransfer.dropEffect = "move"
              const rect = event.currentTarget.getBoundingClientRect()
              setDropFolder(null); setDropTarget({ id: agent.agent_id, after: event.clientY >= rect.top + rect.height / 2 })
            },
            onDragLeave: (event) => { if (!event.currentTarget.contains(event.relatedTarget as Node | null)) setDropTarget(null) },
            onDrop: (event) => {
              if (dragged?.kind !== "agent") return
              event.preventDefault(); event.stopPropagation()
              if (dragged.id !== agent.agent_id) {
                const rect = event.currentTarget.getBoundingClientRect()
                void reorder(dragged.id, agent.agent_id, event.clientY >= rect.top + rect.height / 2, parent)
              }
              setDragged(null); setDropTarget(null)
            },
          }} />
        {appExpanded && <TreeGroup depth={depth + 1}>{ownedApps.map((app) => <AppOwnedTree key={app.app_id} workspaceId={workspaceId!} appId={app.app_id} ownerId={agent.agent_id} navigation={appTree!} depth={depth + 1} />)}</TreeGroup>}
      </div>
    })}</>
  }
  const empty = treeLoaded && !tree.folders.length && !workloads.length
  const canAddAgent = Boolean(workspaceId) && (preview || machines.length === 0 || machines.some((machine) => machine.status === "online"))
  const nothingFound = searching && treeLoaded && !tree.folders.some((folder) => folder.parent_id === null && folderMatches(folder.folder_id, new Set())) && !orderedRows(null).some(agentMatches)
  return <div className="flex flex-col" data-tour="agents-tab">
    {startupAgent && workspaceId && <AgentStartupDialog key={`${workspaceId}/${startupAgent.agent_id}`} workspaceId={workspaceId} agent={{ ...startupAgent, path: agentPath(startupAgent, tree) }} onClose={() => setStartupAgent(null)} onSaved={() => setCopyNotice(`Saved startup command and restarted ${agentPath(startupAgent, tree)}`)} />}
    <div className="sticky top-0 z-[3] flex h-8 shrink-0 items-center justify-between bg-sidebar pl-4 pr-2 max-md:h-11">
      <h2 className="text-[11px] font-semibold uppercase tracking-wider text-muted-foreground">{t("tree.tree")}</h2>
      <DropdownMenu><DropdownMenuTrigger asChild><Button data-tour="create-agent" variant="ghost" size="icon" className="size-6 text-muted-foreground hover:text-foreground max-md:size-11 [@media(pointer:coarse)]:size-11" aria-label={t("tree.add_to_tree")} disabled={!workspaceId}><Plus className="size-4" /></Button></DropdownMenuTrigger><DropdownMenuContent align="end">
        <DropdownMenuItem disabled={!canAddAgent} aria-label={!preview && !machines.length ? t("tree.set_up_a_machine_before") : undefined} onSelect={() => onNew()}><Plus />{t("overview.add_agent")}{!preview && !machines.length && <span className="ml-2 text-[10px] text-muted-foreground">{t("tree.set_up_a_machine_first")}</span>}</DropdownMenuItem>
        <DropdownMenuItem disabled={!workspaceId || busy || loading} onSelect={() => edit("create", null)}><FolderPlus />{t("tree.add_folder")}</DropdownMenuItem>
        <DropdownMenuSeparator />
        <DropdownMenuItem onSelect={onAddMachine}><Monitor />{t("dialogs.add_machine")}</DropdownMenuItem>
      </DropdownMenuContent></DropdownMenu>
    </div>
    {workspaceId && !preview && !machines.length && <div className="mx-2 mb-2 rounded-md border bg-background p-3"><p className="mb-2 text-xs text-muted-foreground">{t("tree.no_machines")}</p><Button variant="outline" size="sm" className="h-auto min-h-9 w-full whitespace-normal max-md:min-h-11" onClick={onAddMachine}><Monitor />{t("wsettings.set_up_a_machine")}</Button></div>}
    <WorkspacePinsNotice pins={pins} />
    {copyNotice && <p role="status" className="mx-2 mb-1 rounded-md bg-background px-2 py-1.5 text-xs text-muted-foreground break-all">{copyNotice}</p>}
    {error && !editor && <div role="alert" className="mx-2 mb-2 rounded-md border border-destructive/40 bg-destructive/10 px-2 py-1.5 text-xs text-foreground">{error}<Button variant="ghost" size="sm" className="ml-1 h-6 px-2 max-md:h-11" disabled={busy} onClick={() => setReload((value) => value + 1)}>{t("tree.reload_folders")}</Button></div>}
    <div ref={treeRoot} role="tree" aria-label={t("tree.workspace_workloads")} aria-busy={busy || loading} onKeyDown={treeKeyDown} onFocus={trackTreeFocus} className="flex flex-col px-2 pb-3" onDragOver={(event) => event.preventDefault()} onDrop={(event) => { event.preventDefault(); if (dragged) move(dragged, null); setDragged(null); setDropFolder(null) }}>
      {loading && !treeLoaded && <p role="status" className="px-2 py-1 text-xs text-muted-foreground">{t("tree.loading_folders")}</p>}
      {render(null)}
      {nothingFound && <p role="status" className="px-2 py-2 text-xs text-muted-foreground">{t("sidebar.no_matches", { query: query.trim() })}</p>}
      {empty && machines.length > 0 && <div className="px-2 py-3 text-xs text-muted-foreground"><p>{t("tree.no_workloads_in_this_workspace")}</p>{canAddAgent && <Button variant="outline" size="sm" className="mt-2 h-7 max-md:h-11" onClick={() => onNew()}><Plus />{t("overview.add_agent")}</Button>}</div>}
      {/* The whole tree background accepts a drop to the root; while dragging, say so. */}
      {dragged && <div aria-hidden="true" className="mt-1 grid min-h-9 flex-1 place-items-center rounded-md border border-dashed border-muted-foreground/50 text-[11px] text-muted-foreground">{t("tree.move_to_workspace_root")}</div>}
    </div>
    <DropdownMenu modal={false} open={Boolean(contextTarget)} onOpenChange={(open) => { if (!open) setContextTarget(null) }}><DropdownMenuTrigger asChild><button tabIndex={-1} aria-hidden="true" className="pointer-events-none fixed size-px opacity-0" style={{ left: contextTarget?.x ?? 0, top: contextTarget?.y ?? 0 }} /></DropdownMenuTrigger><DropdownMenuContent align="start" aria-label={t("tree.actions_for", { name: contextTarget?.item.name ?? "" })} className="max-h-[80dvh] overflow-y-auto" onCloseAutoFocus={(event) => { event.preventDefault(); const row = contextReturn.current; /* an action may open a dialog; let that dialog keep focus */ requestAnimationFrame(() => { if (!document.querySelector('[role="dialog"]') && row?.isConnected) row.focus() }) }}>{contextTarget && menuItems(contextTarget.item, contextTarget.parent, contextTarget.agent)}</DropdownMenuContent></DropdownMenu>

    <Dialog open={stopTarget !== null} onOpenChange={(open) => { if (!open) setStopTarget(null) }}><DialogContent><DialogHeader><DialogTitle>{t("tree.stop_confirm", { path: stopTarget ? agentPath(stopTarget, tree) : "" })}</DialogTitle><DialogDescription>{t("tree.stop_detail")}</DialogDescription></DialogHeader><DialogFooter><Button variant="outline" onClick={() => setStopTarget(null)}>{t("common.cancel")}</Button><Button variant="destructive" disabled={busy} onClick={() => { const target = stopTarget; setStopTarget(null); if (target) void lifecycle(target, "stop") }}>{t("apps.stop")}</Button></DialogFooter></DialogContent></Dialog>
    <Dialog open={manualCopyPath !== null} onOpenChange={(open) => !open && setManualCopyPath(null)}><DialogContent onOpenAutoFocus={(event) => { event.preventDefault(); copyInput.current?.focus(); copyInput.current?.select() }}><DialogHeader><DialogTitle>{t("tree.copy_path")}</DialogTitle><DialogDescription>{t("tree.automatic_copying_is_unavailable_use")}</DialogDescription></DialogHeader><Input ref={copyInput} aria-label={t("tree.tree_path")} value={manualCopyPath ?? ""} readOnly className="font-mono" onFocus={(event) => event.target.select()} /><DialogFooter><Button onClick={() => setManualCopyPath(null)}>{t("tabs.done")}</Button></DialogFooter></DialogContent></Dialog>
    <Dialog open={Boolean(editor)} onOpenChange={(open) => !open && setEditor(null)}><DialogContent><DialogHeader><DialogTitle>{editor?.mode === "move" ? t("tree.move", { name: editor.item?.name ?? "" }) : editor?.mode === "rename" ? t("tree.rename_folder") : t("tree.new_folder")}</DialogTitle></DialogHeader>
      {editor?.mode === "move" ? <label className="text-sm">{t("tree.destination")}<select aria-label={t("tree.destination_folder")} className="mt-2 w-full rounded border bg-background p-2" value={destination} onChange={(event) => setDestination(event.target.value)}><option value="">{t("tree.workspace_root")}</option>{tree.folders.filter((folder) => editor.item?.kind !== "folder" || !descendants(editor.item.id).has(folder.folder_id)).map((folder) => <option key={folder.folder_id} value={folder.folder_id}>{folderPath(folder)}</option>)}</select></label> : <Input aria-label={t("tree.folder_name")} autoFocus value={name} onChange={(event) => setName(event.target.value)} />}
      {error && <p role="alert" className="break-words text-sm text-destructive">{error}</p>}
      <DialogFooter><Button variant="outline" onClick={() => setEditor(null)}>{t("common.cancel")}</Button><Button disabled={busy || (editor?.mode !== "move" && !name.trim())} onClick={() => { if (!editor) return; if (editor.mode === "move" && editor.item) move(editor.item, destination || null); else if (editor.mode === "rename" && editor.item) void mutate(`/folders/${encodeURIComponent(editor.item.id)}`, "PATCH", { name: name.trim() }); else void mutate("/folders", "POST", { name: name.trim(), parent_id: editor.parent }) }}>{busy ? t("policy.saving") : t("common.save")}</Button></DialogFooter>
    </DialogContent></Dialog>
  </div>
}
