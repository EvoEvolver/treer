import { useEffect, useRef, useState } from "react"
import { api, machineName, type Agent, type AppDeployment, type Machine, type WorkloadFolder as FolderEntry, type WorkloadTreeState as Tree } from "@/lib/api"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle, DialogFooter } from "@/components/ui/dialog"
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger } from "@/components/ui/dropdown-menu"
import { ChevronDown, ChevronRight, Folder, Maximize2, MoreVertical, Plus } from "lucide-react"
import { agentPath, workloadKind, folderPath as resolveFolderPath } from "@/lib/workload-path"
import { readFolderExpansion, writeFolderExpansion, type FolderExpansion } from "@/lib/folder-expansion"
import { AgentStartupDialog } from "@/components/agent-startup-dialog"

import { WorkloadIcon } from "@/components/workload-icon"

import TreeMap from "@/components/workload-treemap"

export interface NewAgentFolder { folderId: string; path: string }
type Item = { kind: "folder" | "agent"; id: string; name: string }

export function WorkloadTree({ workspaceId, agents, apps, machines, selectedId, onOpen, onSettings, onRename, onDelete, onNew, preview, placementRevision = 0, onTreeChange }: {
  workspaceId: string | null; agents: Agent[]; apps: AppDeployment[]; machines: Machine[]; selectedId: string | null
  onOpen: (id: string, logs?: boolean) => void; onSettings: (app: AppDeployment) => void; onRename: (agent: Agent) => void; onDelete: (agent: Agent) => void; onNew: (folder?: NewAgentFolder) => void; preview: boolean; placementRevision?: number; onTreeChange?: (tree: Tree) => void
}) {
  const [mapOpen, setMapOpen] = useState(false)
  const isOpeningAgent = useRef(false)
  const mapTrigger = useRef<HTMLButtonElement>(null)
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
  const mutation = useRef(false)
  const readGeneration = useRef(0)
  const base = `/api/workspaces/${encodeURIComponent(workspaceId ?? "")}/workload-tree`
  useEffect(() => {
    setMapOpen(false); setTree({ folders: [], placements: [] }); setError(""); setEditor(null); setTreeLoaded(false); expansion.current = readFolderExpansion(workspaceId); setExpanded(expansion.current); previousSelection.current = null; pendingReveal.current = null; setCopyNotice(""); setManualCopyPath(null)
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
        if (active && generation === readGeneration.current) setError(reason instanceof Error ? reason.message : "Unable to update workload tree")
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
    const changedSelection = previousSelection.current !== null && previousSelection.current !== selectedId
    if (changedSelection) pendingReveal.current = selectedId
    const next = { ...expansion.current }
    let changed = false
    const chain = JSON.parse(selectionChain) as string[]
    for (const id of chain) {
      // Initial/default selection preserves collapsed preferences; changing Agent reveals only its chain.
      if (next[id] !== true && pendingReveal.current === selectedId && selectedId !== null) { next[id] = true; changed = true }
    }
    if (chain.length) pendingReveal.current = null
    previousSelection.current = selectedId
    if (changed) saveExpansion(next)
  }, [selectedId, selectionChain, treeLoaded])
  const workloads = [...agents]
  for (const app of apps) {
    const id = app.runtime_agent_id ?? app.app_id
    const existing = workloads.findIndex((agent) => agent.agent_id === id || agent.agent_id === app.app_id)
    if (existing >= 0) {
      workloads[existing] = { ...workloads[existing], kind: "app", name: workloads[existing].name === `app:${app.name}` ? app.name : workloads[existing].name }
    } else workloads.push({ agent_id: id, name: app.name, server_id: app.server_id, kind: "app", status: app.status })
  }
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
      if (!navigator.clipboard?.writeText) throw new Error("Clipboard unavailable")
      await navigator.clipboard.writeText(path)
      setCopyNotice(`Copied path: ${path}`)
    } catch {
      setCopyNotice("Unable to copy automatically. Select and copy the path below.")
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
    } catch (reason) { setError(reason instanceof Error ? reason.message : "Unable to update workload tree") } finally { mutation.current = false; setBusy(false) }
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
    } catch (reason) { setError(reason instanceof Error ? reason.message : "Unable to update workload tree") }
    finally {
      // Also read after a partial failure: initialization may have succeeded.
      try { setTree(await api<Tree>(base)); setTreeLoaded(true) } catch (reason) { setError(reason instanceof Error ? reason.message : "Unable to update workload tree") }
      mutation.current = false; setBusy(false)
    }
  }
  async function lifecycle(agent: Agent, action: string) {
    setBusy(true); setError("")
    try { await api(`/api/workspaces/${encodeURIComponent(workspaceId ?? "")}/agents/${encodeURIComponent(agent.agent_id)}/${action}`, { method: "POST", body: "{}" }) } catch (reason) { setError(reason instanceof Error ? reason.message : "Unable to update workload tree") } finally { setBusy(false) }
  }
  function edit(mode: "create" | "rename" | "move", parent: string | null, item?: Item) { setName(mode === "rename" ? item?.name ?? "" : ""); setDestination(parent ?? ""); setError(""); setEditor({ mode, parent, item }) }
  function menu(item: Item, parent: string | null, agent?: Agent) {
    const app = agent && apps.find((entry) => entry.runtime_agent_id === agent.agent_id || entry.app_id === agent.agent_id)
    const siblings = orderedRows(parent)
    const index = agent ? siblings.findIndex((entry) => entry.agent_id === agent.agent_id) : -1
    return <DropdownMenu><DropdownMenuTrigger asChild><Button size="icon" variant="ghost" className="size-7 shrink-0" aria-label={`Actions for ${item.name}`}><MoreVertical className="size-3.5" /></Button></DropdownMenuTrigger><DropdownMenuContent align="end">
      {agent && !app && agent.kind !== "app" && <DropdownMenuItem disabled={busy || preview || !machines.some((machine) => machine.server_id === agent.server_id && machine.status === "online")} onSelect={() => setStartupAgent(agent)}>Edit startup command</DropdownMenuItem>}
      {agent ? <><DropdownMenuItem disabled={busy || !machines.some((machine) => machine.server_id === agent.server_id && machine.status === "online")} onSelect={() => void lifecycle(agent, "start")}>Start</DropdownMenuItem><DropdownMenuItem disabled={busy} onSelect={() => void lifecycle(agent, "stop")}>Stop</DropdownMenuItem><DropdownMenuItem disabled={busy} onSelect={() => void lifecycle(agent, "restart")}>Restart</DropdownMenuItem><DropdownMenuItem onSelect={() => onOpen(agent.agent_id, true)}>Logs</DropdownMenuItem>{app && <DropdownMenuItem onSelect={() => onSettings(app)}>App settings</DropdownMenuItem>}<DropdownMenuItem onSelect={() => onRename(agent)}>Rename</DropdownMenuItem></> : <><DropdownMenuItem disabled={!workspaceId || busy || (!preview && machines.length > 0 && !machines.some((machine) => machine.status === "online"))} onSelect={() => onNew({ folderId: item.id, path: itemPath(item, parent) })}>New Agent</DropdownMenuItem><DropdownMenuItem onSelect={() => edit("create", item.id)}>New folder</DropdownMenuItem><DropdownMenuItem onSelect={() => edit("rename", parent, item)}>Rename</DropdownMenuItem></>}
      <DropdownMenuItem onSelect={() => void copyPath(itemPath(item, parent))}>Copy path</DropdownMenuItem>
      <DropdownMenuItem onSelect={() => edit("move", parent, item)}>Move to folder</DropdownMenuItem>
      {agent && <><DropdownMenuItem disabled={busy || index <= 0} onSelect={() => void reorder(agent.agent_id, siblings[index - 1].agent_id, false, parent)}>Move up</DropdownMenuItem><DropdownMenuItem disabled={busy || index < 0 || index >= siblings.length - 1} onSelect={() => void reorder(agent.agent_id, siblings[index + 1].agent_id, true, parent)}>Move down</DropdownMenuItem></>}
      <DropdownMenuItem disabled={busy} onSelect={() => agent ? onDelete(agent) : void mutate(`/folders/${encodeURIComponent(item.id)}`, "DELETE")}>Delete</DropdownMenuItem>
    </DropdownMenuContent></DropdownMenu>
  }
  function render(parent: string | null, ancestors: Set<string> = new Set()): React.ReactNode {
    const folders = tree.folders.filter((folder) => folder.parent_id === parent && !ancestors.has(folder.folder_id)).sort((a, b) => a.name.localeCompare(b.name, undefined, { numeric: true, sensitivity: "base" }) || a.folder_id.localeCompare(b.folder_id))
    const rows = orderedRows(parent)
    return <>{folders.map((folder) => { const item: Item = { kind: "folder", id: folder.folder_id, name: folder.name }; return <div key={folder.folder_id} role="treeitem" aria-expanded={expanded[folder.folder_id] === true} aria-label={folder.name}>
      <div title={folderPath(folder)} className="flex min-h-10 items-center rounded px-1 hover:bg-accent" draggable={!busy} onDragStart={(event) => { event.stopPropagation(); setDragged(item); event.dataTransfer.setData("text/plain", JSON.stringify(item)) }} onDragEnd={() => setDragged(null)} onDragOver={(event) => { event.preventDefault(); event.stopPropagation() }} onDrop={(event) => { event.preventDefault(); event.stopPropagation(); if (dragged) move(dragged, folder.folder_id); setDragged(null) }}>
        <button className="flex min-w-0 flex-1 items-center gap-1 text-left text-xs" onClick={() => { pendingReveal.current = null; saveExpansion({ ...expansion.current, [folder.folder_id]: expansion.current[folder.folder_id] !== true }) }}>{expanded[folder.folder_id] !== true ? <ChevronRight className="size-3" /> : <ChevronDown className="size-3" />}<Folder className="size-3.5" /><span className="truncate">{folder.name}</span></button>{menu(item, parent)}
      </div>{expanded[folder.folder_id] === true && <div role="group" className="ml-3 border-l pl-1">{render(folder.folder_id, new Set([...ancestors, folder.folder_id]))}</div>}
    </div> })}{rows.map((agent) => { const item: Item = { kind: "agent", id: agent.agent_id, name: agent.name }; return <div role="treeitem" aria-label={agent.name} title={agentPath(agent, tree)} data-agent-id={agent.agent_id} aria-selected={selectedId === agent.agent_id} key={agent.agent_id} draggable={!busy} onDragStart={(event) => { event.stopPropagation(); setDragged(item); event.dataTransfer.setData("text/plain", JSON.stringify(item)) }} onDragEnd={() => { setDragged(null); setDropTarget(null) }} onDragOver={(event) => {
      if (busy || dragged?.kind !== "agent" || dragged.id === agent.agent_id) return
      event.preventDefault(); event.stopPropagation()
      event.dataTransfer.dropEffect = "move"
      const rect = event.currentTarget.getBoundingClientRect()
      setDropTarget({ id: agent.agent_id, after: event.clientY >= rect.top + rect.height / 2 })
    }} onDragLeave={(event) => { if (!event.currentTarget.contains(event.relatedTarget as Node | null)) setDropTarget(null) }} onDrop={(event) => {
      if (dragged?.kind !== "agent") return
      event.preventDefault(); event.stopPropagation()
      if (dragged.id !== agent.agent_id) {
        const rect = event.currentTarget.getBoundingClientRect()
        void reorder(dragged.id, agent.agent_id, event.clientY >= rect.top + rect.height / 2, parent)
      }
      setDragged(null); setDropTarget(null)
    }} className={`flex min-h-12 items-center gap-1 rounded px-2 py-1 hover:bg-accent ${selectedId === agent.agent_id ? "bg-accent" : ""} ${dropTarget?.id === agent.agent_id ? dropTarget.after ? "shadow-[inset_0_-2px_0_hsl(var(--primary))]" : "shadow-[inset_0_2px_0_hsl(var(--primary))]" : ""}`}>
      <button className="min-w-0 flex-1 text-left" title={agentPath(agent, tree)} onClick={() => onOpen(agent.agent_id)}><span className="flex min-w-0 items-center gap-1.5 text-xs font-medium"><WorkloadIcon kind={workloadKind(agent)} className="size-3.5 shrink-0 text-muted-foreground" /><span className="truncate">{agent.name}</span></span><span className="block truncate text-[9px] text-muted-foreground">{workloadKind(agent)} · {machineName(machines.find((machine) => machine.server_id === agent.server_id), agent.server_id)} · {machines.some((machine) => machine.server_id === agent.server_id && machine.status === "online") ? agent.status : "offline"}{agent.run_generation !== undefined && ` · run ${agent.run_generation}`}</span></button>{menu(item, parent, agent)}
    </div> })}</>
  }
  return <div className="flex min-h-0 flex-1 flex-col overflow-hidden" data-tour="agents-tab">
    {startupAgent && workspaceId && <AgentStartupDialog key={`${workspaceId}/${startupAgent.agent_id}`} workspaceId={workspaceId} agent={{ ...startupAgent, path: agentPath(startupAgent, tree) }} onClose={() => setStartupAgent(null)} onSaved={() => setCopyNotice(`Saved startup command and restarted ${agentPath(startupAgent, tree)}`)} />}
    <div className="flex h-10 shrink-0 items-center justify-between px-3 text-[11px] font-medium text-muted-foreground"><span>Tree</span><div className="flex items-center gap-1">
      <DropdownMenu><DropdownMenuTrigger asChild><Button data-tour="create-agent" variant="ghost" size="icon" className="size-7" aria-label="Add to tree" disabled={!workspaceId}><Plus className="size-4" /></Button></DropdownMenuTrigger><DropdownMenuContent align="end">
        <DropdownMenuItem disabled={!workspaceId || busy || loading} onSelect={() => edit("create", null)}><Folder className="mr-2 size-3.5" />Add folder</DropdownMenuItem>
        <DropdownMenuItem aria-label={!preview && !machines.length ? "Set up a machine before creating an agent" : undefined} disabled={!workspaceId || (!preview && machines.length > 0 && !machines.some((machine) => machine.status === "online"))} onSelect={() => onNew()}><Plus className="mr-2 size-3.5" />Add agent{!preview && !machines.length && <span className="ml-2 text-[10px] text-muted-foreground">Set up a machine first</span>}</DropdownMenuItem>
      </DropdownMenuContent></DropdownMenu>
      <Button variant="ghost" size="icon" className="size-7" ref={mapTrigger} aria-label="Expand tree map" disabled={!workspaceId || loading} onClick={() => setMapOpen(true)}><Maximize2 className="size-3.5" /></Button>
    </div></div>
    <Dialog open={mapOpen} onOpenChange={setMapOpen}><DialogContent className="flex h-[min(820px,calc(100dvh-2rem))] w-[calc(100%-1rem)] max-w-6xl flex-col gap-0 overflow-hidden p-0 sm:w-[calc(100%-3rem)]" onCloseAutoFocus={(event) => { event.preventDefault(); if (!isOpeningAgent.current) mapTrigger.current?.focus(); isOpeningAgent.current = false }}>
      <DialogHeader className="shrink-0 border-b px-5 py-4 pr-12"><DialogTitle>Workspace map</DialogTitle><DialogDescription className="text-xs">Explore folders and open agents.</DialogDescription></DialogHeader>
      {mapOpen && <TreeMap tree={tree} agents={workloads} machines={machines} selectedId={selectedId} onOpen={(id) => { isOpeningAgent.current = true; onOpen(id); setMapOpen(false) }} />}
    </DialogContent></Dialog>

    {copyNotice && <p role="status" className="px-3 py-2 text-xs break-all">{copyNotice}</p>}
    {loading && <p role="status" className="px-3 text-xs">Loading folders…</p>}{error && !editor && <div role="alert" className="px-3 py-2 text-xs text-destructive">{error}<Button variant="ghost" size="sm" className="mt-1 h-7" disabled={busy} onClick={() => setReload((value) => value + 1)}>Reload folders</Button></div>}
    <div role="tree" aria-label="Workspace workloads" aria-busy={busy || loading} className="min-h-0 flex-1 overflow-auto px-2 pb-3" onDragOver={(event) => event.preventDefault()} onDrop={(event) => { event.preventDefault(); if (dragged) move(dragged, null); setDragged(null) }}>{render(null)}{!workloads.length && !tree.folders.length && !loading && <p className="px-2 py-4 text-xs text-muted-foreground">No workloads in this workspace</p>}<div aria-label="Move to workspace root" className="mt-2 min-h-8 border-t px-2 py-2 text-[10px] text-muted-foreground">Workspace root</div></div>
    <Dialog open={manualCopyPath !== null} onOpenChange={(open) => !open && setManualCopyPath(null)}><DialogContent onOpenAutoFocus={(event) => { event.preventDefault(); copyInput.current?.focus(); copyInput.current?.select() }}><DialogHeader><DialogTitle>Copy path</DialogTitle><DialogDescription>Automatic copying is unavailable. Use your device's copy action on the selected path.</DialogDescription></DialogHeader><Input ref={copyInput} aria-label="Tree path" value={manualCopyPath ?? ""} readOnly className="font-mono" onFocus={(event) => event.target.select()} /><DialogFooter><Button onClick={() => setManualCopyPath(null)}>Done</Button></DialogFooter></DialogContent></Dialog>
    <Dialog open={Boolean(editor)} onOpenChange={(open) => !open && setEditor(null)}><DialogContent><DialogHeader><DialogTitle>{editor?.mode === "move" ? `Move ${editor.item?.name}` : editor?.mode === "rename" ? "Rename folder" : "New folder"}</DialogTitle></DialogHeader>
      {editor?.mode === "move" ? <label className="text-sm">Destination<select aria-label="Destination folder" className="mt-2 w-full rounded border bg-background p-2" value={destination} onChange={(event) => setDestination(event.target.value)}><option value="">Workspace root</option>{tree.folders.filter((folder) => editor.item?.kind !== "folder" || !descendants(editor.item.id).has(folder.folder_id)).map((folder) => <option key={folder.folder_id} value={folder.folder_id}>{folderPath(folder)}</option>)}</select></label> : <Input aria-label="Folder name" autoFocus value={name} onChange={(event) => setName(event.target.value)} />}
      {error && <p role="alert" className="break-words text-sm text-destructive">{error}</p>}
      <DialogFooter><Button variant="outline" onClick={() => setEditor(null)}>Cancel</Button><Button disabled={busy || (editor?.mode !== "move" && !name.trim())} onClick={() => { if (!editor) return; if (editor.mode === "move" && editor.item) move(editor.item, destination || null); else if (editor.mode === "rename" && editor.item) void mutate(`/folders/${encodeURIComponent(editor.item.id)}`, "PATCH", { name: name.trim() }); else void mutate("/folders", "POST", { name: name.trim(), parent_id: editor.parent }) }}>{busy ? "Saving…" : "Save"}</Button></DialogFooter>
    </DialogContent></Dialog>
  </div>
}
