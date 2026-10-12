import { useEffect, useRef, useState } from "react"
import { api, machineName, type Agent, type AppDeployment, type Machine, type WorkloadFolder as FolderEntry, type WorkloadTreeState as Tree } from "@/lib/api"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle, DialogFooter } from "@/components/ui/dialog"
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger } from "@/components/ui/dropdown-menu"
import { ChevronDown, ChevronRight, Folder, MoreVertical, Plus } from "lucide-react"

export interface NewAgentFolder { folderId: string; path: string }
type Item = { kind: "folder" | "agent"; id: string; name: string }

export function WorkloadTree({ workspaceId, agents, apps, machines, selectedId, onOpen, onSettings, onRename, onDelete, onNew, preview, placementRevision = 0 }: {
  workspaceId: string | null; agents: Agent[]; apps: AppDeployment[]; machines: Machine[]; selectedId: string | null
  onOpen: (id: string, logs?: boolean) => void; onSettings: (app: AppDeployment) => void; onRename: (agent: Agent) => void; onDelete: (agent: Agent) => void; onNew: (folder?: NewAgentFolder) => void; preview: boolean; placementRevision?: number
}) {
  const [tree, setTree] = useState<Tree>({ folders: [], placements: [] })
  const [loading, setLoading] = useState(false)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState("")
  const [collapsed, setCollapsed] = useState<Set<string>>(new Set())
  const [dragged, setDragged] = useState<Item | null>(null)
  const [dropTarget, setDropTarget] = useState<{ id: string; after: boolean } | null>(null)
  const [editor, setEditor] = useState<{ mode: "create" | "rename" | "move"; item?: Item; parent: string | null } | null>(null)
  const [name, setName] = useState("")
  const [destination, setDestination] = useState("")
  const [reload, setReload] = useState(0)
  const [copyNotice, setCopyNotice] = useState("")
  const [manualCopyPath, setManualCopyPath] = useState<string | null>(null)
  const copyInput = useRef<HTMLInputElement>(null)
  const mutation = useRef(false)
  const readGeneration = useRef(0)
  const base = `/api/workspaces/${encodeURIComponent(workspaceId ?? "")}/workload-tree`
  useEffect(() => {
    setTree({ folders: [], placements: [] }); setError(""); setEditor(null); setCollapsed(new Set()); setCopyNotice(""); setManualCopyPath(null)
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
        if (active && generation === readGeneration.current) setTree(data)
      } catch (reason) {
        if (active && generation === readGeneration.current) setError(String(reason))
      } finally { reading = false; if (active) setLoading(false) }
    }
    void refresh()
    const timer = window.setInterval(() => { if (!document.hidden) void refresh() }, 5000)
    const focus = () => void refresh()
    window.addEventListener("focus", focus)
    return () => { active = false; window.clearInterval(timer); window.removeEventListener("focus", focus) }
  }, [base, workspaceId, preview, reload, placementRevision])
  const workloads = [...agents]
  for (const app of apps) {
    const id = app.runtime_agent_id ?? app.app_id
    const existing = workloads.findIndex((agent) => agent.agent_id === id || agent.agent_id === app.app_id)
    if (existing >= 0) {
      if (workloads[existing].name === `app:${app.name}`) workloads[existing] = { ...workloads[existing], name: app.name }
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
  function folderPath(folder: FolderEntry): string {
    const names = [folder.name]
    const visited = new Set([folder.folder_id])
    let parentId = folder.parent_id
    while (parentId && !visited.has(parentId)) {
      visited.add(parentId)
      const parent = tree.folders.find((entry) => entry.folder_id === parentId)
      if (!parent) break
      names.unshift(parent.name); parentId = parent.parent_id
    }
    return `/${names.join("/")}`
  }
  function itemPath(item: Item, parent: string | null) {
    const folder = tree.folders.find((entry) => entry.folder_id === (item.kind === "folder" ? item.id : parent))
    if (item.kind === "folder") return folder ? folderPath(folder) : `/${item.name}`
    // App rows can have a friendlier label than their Agent name. CLI paths
    // resolve the stored workload name, so copying must preserve that name.
    const agentName = agents.find((entry) => entry.agent_id === item.id)?.name ?? item.name
    return `${folder ? folderPath(folder) : ""}/${agentName}`
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
      setTree(updated); setEditor(null)
    } catch (reason) { setError(String(reason)) } finally { mutation.current = false; setBusy(false) }
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
    } catch (reason) { setError(String(reason)) }
    finally {
      // Also read after a partial failure: initialization may have succeeded.
      try { setTree(await api<Tree>(base)) } catch (reason) { setError(String(reason)) }
      mutation.current = false; setBusy(false)
    }
  }
  async function lifecycle(agent: Agent, action: string) {
    setBusy(true); setError("")
    try { await api(`/api/workspaces/${encodeURIComponent(workspaceId ?? "")}/agents/${encodeURIComponent(agent.agent_id)}/${action}`, { method: "POST", body: "{}" }) } catch (reason) { setError(String(reason)) } finally { setBusy(false) }
  }
  function edit(mode: "create" | "rename" | "move", parent: string | null, item?: Item) { setName(mode === "rename" ? item?.name ?? "" : ""); setDestination(parent ?? ""); setEditor({ mode, parent, item }) }
  function menu(item: Item, parent: string | null, agent?: Agent) {
    const app = agent && apps.find((entry) => entry.runtime_agent_id === agent.agent_id || entry.app_id === agent.agent_id)
    const siblings = orderedRows(parent)
    const index = agent ? siblings.findIndex((entry) => entry.agent_id === agent.agent_id) : -1
    return <DropdownMenu><DropdownMenuTrigger asChild><Button size="icon" variant="ghost" className="size-7 shrink-0" aria-label={`Actions for ${item.name}`}><MoreVertical className="size-3.5" /></Button></DropdownMenuTrigger><DropdownMenuContent align="end">
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
    return <>{folders.map((folder) => { const item: Item = { kind: "folder", id: folder.folder_id, name: folder.name }; return <div key={folder.folder_id} role="treeitem" aria-expanded={!collapsed.has(folder.folder_id)} aria-label={folder.name}>
      <div className="flex min-h-10 items-center rounded px-1 hover:bg-accent" draggable={!busy} onDragStart={(event) => { event.stopPropagation(); setDragged(item); event.dataTransfer.setData("text/plain", JSON.stringify(item)) }} onDragEnd={() => setDragged(null)} onDragOver={(event) => { event.preventDefault(); event.stopPropagation() }} onDrop={(event) => { event.preventDefault(); event.stopPropagation(); if (dragged) move(dragged, folder.folder_id); setDragged(null) }}>
        <button className="flex min-w-0 flex-1 items-center gap-1 text-left text-xs" onClick={() => setCollapsed((current) => { const next = new Set(current); if (next.has(folder.folder_id)) next.delete(folder.folder_id); else next.add(folder.folder_id); return next })}>{collapsed.has(folder.folder_id) ? <ChevronRight className="size-3" /> : <ChevronDown className="size-3" />}<Folder className="size-3.5" /><span className="truncate">{folder.name}</span></button>{menu(item, parent)}
      </div>{!collapsed.has(folder.folder_id) && <div role="group" className="ml-3 border-l pl-1">{render(folder.folder_id, new Set([...ancestors, folder.folder_id]))}</div>}
    </div> })}{rows.map((agent) => { const item: Item = { kind: "agent", id: agent.agent_id, name: agent.name }; return <div role="treeitem" aria-label={agent.name} aria-selected={selectedId === agent.agent_id} key={agent.agent_id} draggable={!busy} onDragStart={(event) => { event.stopPropagation(); setDragged(item); event.dataTransfer.setData("text/plain", JSON.stringify(item)) }} onDragEnd={() => { setDragged(null); setDropTarget(null) }} onDragOver={(event) => {
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
      <button className="min-w-0 flex-1 text-left" onClick={() => onOpen(agent.agent_id)}><span className="block truncate text-xs font-medium">{agent.name}</span><span className="block truncate text-[9px] text-muted-foreground">{agent.kind} · {machineName(machines.find((machine) => machine.server_id === agent.server_id), agent.server_id)} · {machines.some((machine) => machine.server_id === agent.server_id && machine.status === "online") ? agent.status : "offline"}{agent.run_generation !== undefined && ` · run ${agent.run_generation}`}</span></button>{menu(item, parent, agent)}
    </div> })}</>
  }
  return <div className="flex min-h-0 flex-1 flex-col overflow-hidden" data-tour="agents-tab">
    <div className="flex h-10 shrink-0 items-center justify-between px-3 text-[11px] font-medium text-muted-foreground"><span>Tree</span><div className="flex"><Button variant="ghost" size="sm" className="h-7 px-2" disabled={!workspaceId || busy || loading} onClick={() => edit("create", null)} aria-label="New folder"><Folder className="size-3.5" /><Plus className="size-3" /></Button><span data-tour="create-agent"><Button variant="ghost" size="sm" className="h-7 px-2" aria-label={!preview && !machines.length ? "Set up a machine before creating an agent" : undefined} disabled={!workspaceId || (!preview && machines.length > 0 && !machines.some((machine) => machine.status === "online"))} onClick={() => onNew()}><Plus className="size-3.5" />New</Button></span></div></div>
    {copyNotice && <p role="status" className="px-3 py-2 text-xs break-all">{copyNotice}</p>}
    {loading && <p role="status" className="px-3 text-xs">Loading folders…</p>}{error && <div role="alert" className="px-3 py-2 text-xs text-destructive">{error}<Button variant="ghost" size="sm" className="mt-1 h-7" disabled={busy} onClick={() => setReload((value) => value + 1)}>Reload folders</Button></div>}
    <div role="tree" aria-label="Workspace workloads" aria-busy={busy || loading} className="min-h-0 flex-1 overflow-auto px-2 pb-3" onDragOver={(event) => event.preventDefault()} onDrop={(event) => { event.preventDefault(); if (dragged) move(dragged, null); setDragged(null) }}>{render(null)}{!workloads.length && !tree.folders.length && !loading && <p className="px-2 py-4 text-xs text-muted-foreground">No workloads in this workspace</p>}<div aria-label="Move to workspace root" className="mt-2 min-h-8 border-t px-2 py-2 text-[10px] text-muted-foreground">Workspace root</div></div>
    <Dialog open={manualCopyPath !== null} onOpenChange={(open) => !open && setManualCopyPath(null)}><DialogContent onOpenAutoFocus={(event) => { event.preventDefault(); copyInput.current?.focus(); copyInput.current?.select() }}><DialogHeader><DialogTitle>Copy path</DialogTitle><DialogDescription>Automatic copying is unavailable. Use your device's copy action on the selected path.</DialogDescription></DialogHeader><Input ref={copyInput} aria-label="Tree path" value={manualCopyPath ?? ""} readOnly className="font-mono" onFocus={(event) => event.target.select()} /><DialogFooter><Button onClick={() => setManualCopyPath(null)}>Done</Button></DialogFooter></DialogContent></Dialog>
    <Dialog open={Boolean(editor)} onOpenChange={(open) => !open && setEditor(null)}><DialogContent><DialogHeader><DialogTitle>{editor?.mode === "move" ? `Move ${editor.item?.name}` : editor?.mode === "rename" ? "Rename folder" : "New folder"}</DialogTitle></DialogHeader>
      {editor?.mode === "move" ? <label className="text-sm">Destination<select aria-label="Destination folder" className="mt-2 w-full rounded border bg-background p-2" value={destination} onChange={(event) => setDestination(event.target.value)}><option value="">Workspace root</option>{tree.folders.filter((folder) => editor.item?.kind !== "folder" || !descendants(editor.item.id).has(folder.folder_id)).map((folder) => <option key={folder.folder_id} value={folder.folder_id}>{folderPath(folder)}</option>)}</select></label> : <Input aria-label="Folder name" autoFocus value={name} onChange={(event) => setName(event.target.value)} />}
      <DialogFooter><Button variant="outline" onClick={() => setEditor(null)}>Cancel</Button><Button disabled={busy || (editor?.mode !== "move" && !name.trim())} onClick={() => { if (!editor) return; if (editor.mode === "move" && editor.item) move(editor.item, destination || null); else if (editor.mode === "rename" && editor.item) void mutate(`/folders/${encodeURIComponent(editor.item.id)}`, "PATCH", { name: name.trim() }); else void mutate("/folders", "POST", { name: name.trim(), parent_id: editor.parent }) }}>{busy ? "Saving…" : "Save"}</Button></DialogFooter>
    </DialogContent></Dialog>
  </div>
}
