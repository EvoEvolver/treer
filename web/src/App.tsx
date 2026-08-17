import { FormEvent, useCallback, useEffect, useState } from "react"
import type * as React from "react"
import {
  ChevronRight,
  CirclePlus,
  Copy,
  FolderKanban,
  LogOut,
  MoreHorizontal,
  Network,
  Pencil,
  Plus,
  RotateCw,
  Server,
  Square,
  TerminalSquare,
  Trash2,
  UserRound,
  Users,
} from "lucide-react"
import { api, ApiError, machineName, websocketUrl, type Agent, type Machine, type Member, type NetworkPolicy, type Organization, type Snapshot, type User, type VirtualNetworkHost, type Workspace } from "@/lib/api"
import { cn } from "@/lib/utils"
import { TerminalPane } from "@/components/terminal-pane"
import { Button } from "@/components/ui/button"
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog"
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuLabel, DropdownMenuSeparator, DropdownMenuTrigger } from "@/components/ui/dropdown-menu"
import { Input } from "@/components/ui/input"
import { Label } from "@/components/ui/label"
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select"
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs"
import { Textarea } from "@/components/ui/textarea"
import { Tooltip, TooltipContent, TooltipProvider, TooltipTrigger } from "@/components/ui/tooltip"

type ConnectionState = "connecting" | "live" | "reconnecting" | "no workspace"
type TerminalState = "not attached" | "connecting" | "live" | "reconnecting" | "closed" | "error"
type RenameTarget = { kind: "machine" | "agent"; id: string; name: string } | null
type DeleteTarget = { kind: "machine" | "agent"; id: string; name: string } | null

const activeStatuses = new Set(["starting", "working", "idle", "blocked"])

function initials(value: string) {
  return value.trim().slice(0, 2).toUpperCase() || "T"
}

function defaultAgentName() {
  const now = new Date()
  const month = String(now.getMonth() + 1).padStart(2, "0")
  const day = String(now.getDate()).padStart(2, "0")
  return `agent-${now.getFullYear()}-${month}-${day}`
}

function Status({ value }: { value: string }) {
  return <span className={cn("inline-flex shrink-0 items-center gap-1.5 text-[10px] font-medium capitalize text-zinc-500", value === "idle" && "text-emerald-700", ["working", "starting"].includes(value) && "text-sky-700", value === "blocked" && "text-amber-700", ["failed", "exited"].includes(value) && "text-red-600")}><span className="size-1.5 rounded-full bg-current opacity-75" />{value}</span>
}

function IconButton({ label, children, ...props }: React.ComponentProps<typeof Button> & { label: string }) {
  return <Tooltip><TooltipTrigger asChild><Button size="icon" variant="ghost" aria-label={label} {...props}>{children}</Button></TooltipTrigger><TooltipContent>{label}</TooltipContent></Tooltip>
}

function AuthScreen({ onAuthenticated }: { onAuthenticated: (user: User) => void }) {
  const invite = new URLSearchParams(window.location.search).get("invite")
  const [registering, setRegistering] = useState(Boolean(invite))
  const [username, setUsername] = useState(registering ? "" : "admin")
  const [password, setPassword] = useState("")
  const [error, setError] = useState("")
  const [submitting, setSubmitting] = useState(false)

  async function submit(event: FormEvent) {
    event.preventDefault()
    setError("")
    setSubmitting(true)
    try {
      const path = registering ? "/api/auth/register" : "/api/auth/login"
      const body = registering ? { invite, username, password } : { username, password }
      const user = await api<User>(path, { method: "POST", body: JSON.stringify(body) })
      window.history.replaceState(null, "", window.location.pathname)
      onAuthenticated(user)
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : "Authentication failed")
    } finally {
      setSubmitting(false)
    }
  }

  return <main className="grid min-h-dvh place-items-center bg-[#f7f7f5] p-4">
    <form onSubmit={submit} className="w-full max-w-[390px] rounded-lg border bg-background p-7 shadow-sm">
      <div className="mb-6 grid size-9 place-items-center rounded-md bg-[#37352f] font-serif text-lg font-bold text-white">T</div>
      <h1 className="text-xl font-semibold">{registering ? "Join Treer" : "Sign in to Treer"}</h1>
      <p className="mt-1 text-sm text-muted-foreground">{registering ? "Create your account to join the workspace." : "Open your agent workspace."}</p>
      <div className="mt-6 space-y-2"><Label htmlFor="username">Username</Label><Input id="username" autoComplete="username" value={username} minLength={registering ? 3 : undefined} maxLength={32} onChange={(event) => setUsername(event.target.value)} required autoFocus /></div>
      <div className="mt-4 space-y-2"><Label htmlFor="password">Password</Label><Input id="password" type="password" autoComplete={registering ? "new-password" : "current-password"} value={password} minLength={registering ? 8 : undefined} onChange={(event) => setPassword(event.target.value)} required /></div>
      <div className="mt-3 min-h-5 text-xs text-destructive">{error}</div>
      <div className="mt-4 flex items-center justify-between gap-3">
        {registering && <Button type="button" variant="ghost" className="px-0 text-primary" onClick={() => setRegistering(false)}>Sign in instead</Button>}
        <Button type="submit" className="ml-auto" disabled={submitting}>{submitting ? "Please wait" : registering ? "Create account" : "Sign in"}</Button>
      </div>
    </form>
  </main>
}

export default function App() {
  const [user, setUser] = useState<User | null | undefined>(undefined)
  const [organizations, setOrganizations] = useState<Organization[]>([])
  const [organizationId, setOrganizationId] = useState<string | null>(null)
  const [workspaces, setWorkspaces] = useState<Workspace[]>([])
  const [workspaceId, setWorkspaceId] = useState<string | null>(null)
  const [snapshot, setSnapshot] = useState<Snapshot | null>(null)
  const [selectedAgentId, setSelectedAgentId] = useState<string | null>(null)
  const [connection, setConnection] = useState<ConnectionState>("connecting")
  const [terminalStatus, setTerminalStatus] = useState<TerminalState>("not attached")
  const [error, setError] = useState<string | null>(null)
  const [createOrganizationOpen, setCreateOrganizationOpen] = useState(false)
  const [createWorkspaceOpen, setCreateWorkspaceOpen] = useState(false)
  const [createAgentOpen, setCreateAgentOpen] = useState(false)
  const [installOpen, setInstallOpen] = useState(false)
  const [membersOpen, setMembersOpen] = useState(false)
  const [networkOpen, setNetworkOpen] = useState(false)
  const [inviteOpen, setInviteOpen] = useState(false)
  const [renameTarget, setRenameTarget] = useState<RenameTarget>(null)
  const [deleteTarget, setDeleteTarget] = useState<DeleteTarget>(null)
  const [organizationName, setOrganizationName] = useState("")
  const [workspaceName, setWorkspaceName] = useState("")
  const [agentName, setAgentName] = useState(defaultAgentName())
  const [agentKind, setAgentKind] = useState("codex")
  const [agentServerId, setAgentServerId] = useState("")
  const [agentCwd, setAgentCwd] = useState(".")
  const [agentArgs, setAgentArgs] = useState("")
  const [renameName, setRenameName] = useState("")
  const [installCommand, setInstallCommand] = useState("")
  const [connectCommand, setConnectCommand] = useState("")
  const [inviteUrl, setInviteUrl] = useState("")
  const [members, setMembers] = useState<Member[]>([])
  const [currentRole, setCurrentRole] = useState<Member["role"]>("member")
  const [networkPolicies, setNetworkPolicies] = useState<NetworkPolicy[]>([])
  const [virtualHosts, setVirtualHosts] = useState<VirtualNetworkHost[]>([])
  const [virtualHostname, setVirtualHostname] = useState("")
  const [virtualDestination, setVirtualDestination] = useState("")
  const [virtualTargetHost, setVirtualTargetHost] = useState("127.0.0.1")
  const [virtualTargetPort, setVirtualTargetPort] = useState("")
  const [networkSource, setNetworkSource] = useState("*")
  const [networkDestination, setNetworkDestination] = useState("")
  const [networkHost, setNetworkHost] = useState("127.0.0.1")
  const [networkPortStart, setNetworkPortStart] = useState("")
  const [networkPortEnd, setNetworkPortEnd] = useState("")

  const showError = useCallback((reason: unknown) => setError(reason instanceof Error ? reason.message : "Something went wrong"), [])

  useEffect(() => {
    api<User>("/api/auth/me").then(setUser).catch((reason) => {
      if (reason instanceof ApiError && reason.status === 401) setUser(null)
      else showError(reason)
    })
  }, [showError])

  const loadOrganizations = useCallback(async (preferred?: string) => {
    const data = await api<{ organizations: Organization[] }>("/api/organizations")
    setOrganizations(data.organizations)
    setOrganizationId((current) => {
      if (preferred && data.organizations.some((item) => item.organization_id === preferred)) return preferred
      if (current && data.organizations.some((item) => item.organization_id === current)) return current
      return data.organizations[0]?.organization_id ?? null
    })
  }, [])

  useEffect(() => { if (user) loadOrganizations().catch(showError) }, [user, loadOrganizations, showError])

  useEffect(() => {
    let cancelled = false
    setWorkspaceId(null)
    setSnapshot(null)
    if (!organizationId) {
      setWorkspaces([])
      setConnection("no workspace")
      return
    }
    api<{ workspaces: Workspace[] }>(`/api/workspaces?organization_id=${encodeURIComponent(organizationId)}`).then((data) => {
      if (cancelled) return
      setWorkspaces(data.workspaces)
      setWorkspaceId(data.workspaces[0]?.workspace_id ?? null)
      if (!data.workspaces.length) setConnection("no workspace")
    }).catch(showError)
    return () => { cancelled = true }
  }, [organizationId, showError])

  const refreshSnapshot = useCallback(async () => {
    if (!workspaceId) return
    const data = await api<Snapshot>(`/api/workspaces/${encodeURIComponent(workspaceId)}/snapshot`)
    setSnapshot(data)
  }, [workspaceId])

  useEffect(() => {
    if (!workspaceId) {
      setSnapshot(null)
      setSelectedAgentId(null)
      setConnection("no workspace")
      return
    }
    let disposed = false
    let socket: WebSocket | null = null
    let timer: number | undefined
    refreshSnapshot().catch(showError)
    const connect = (initial = false) => {
      if (disposed) return
      if (initial) setConnection("connecting")
      socket = new WebSocket(websocketUrl(`/api/workspaces/${encodeURIComponent(workspaceId)}/events`))
      socket.onopen = () => { if (!disposed) setConnection("live") }
      socket.onmessage = (event) => {
        if (disposed) return
        const message = JSON.parse(event.data) as { event: string; data?: Snapshot }
        if (message.event === "workspace.snapshot" && message.data) setSnapshot(message.data)
        else refreshSnapshot().catch(showError)
      }
      socket.onclose = () => {
        if (disposed) return
        setConnection("reconnecting")
        timer = window.setTimeout(() => connect(false), 1200)
      }
    }
    connect(true)
    return () => { disposed = true; window.clearTimeout(timer); socket?.close() }
  }, [workspaceId, refreshSnapshot, showError])

  useEffect(() => {
    const agents = snapshot?.agents ?? []
    setSelectedAgentId((current) => current && agents.some((agent) => agent.agent_id === current) ? current : agents[0]?.agent_id ?? null)
  }, [snapshot])

  const selectedAgent = snapshot?.agents.find((agent) => agent.agent_id === selectedAgentId)
  const onlineMachines = snapshot?.servers.filter((machine) => machine.status === "online") ?? []
  const organization = organizations.find((item) => item.organization_id === organizationId)
  const workspace = workspaces.find((item) => item.workspace_id === workspaceId)
  const terminalActive = Boolean(selectedAgent && activeStatuses.has(selectedAgent.status))
  const setTerminalState = useCallback((value: TerminalState) => setTerminalStatus(value), [])
  const canManageMembers = ["owner", "admin"].includes(currentRole)

  useEffect(() => {
    if (createAgentOpen && !onlineMachines.some((machine) => machine.server_id === agentServerId)) setAgentServerId(onlineMachines[0]?.server_id ?? "")
  }, [createAgentOpen, onlineMachines, agentServerId])

  async function createOrganization(event: FormEvent) {
    event.preventDefault()
    try {
      const data = await api<{ organization: Organization }>("/api/organizations", { method: "POST", body: JSON.stringify({ name: organizationName }) })
      setCreateOrganizationOpen(false); setOrganizationName(""); await loadOrganizations(data.organization.organization_id)
    } catch (reason) { showError(reason) }
  }

  async function createWorkspace(event: FormEvent) {
    event.preventDefault()
    if (!organizationId) return
    try {
      const data = await api<{ workspace: Workspace }>("/api/workspaces", { method: "POST", body: JSON.stringify({ organization_id: organizationId, name: workspaceName }) })
      setCreateWorkspaceOpen(false); setWorkspaceName("")
      const list = await api<{ workspaces: Workspace[] }>(`/api/workspaces?organization_id=${encodeURIComponent(organizationId)}`)
      setWorkspaces(list.workspaces); setWorkspaceId(data.workspace.workspace_id)
    } catch (reason) { showError(reason) }
  }

  async function createAgent(event: FormEvent) {
    event.preventDefault()
    if (!workspaceId) return
    try {
      const agent = await api<Agent>(`/api/workspaces/${encodeURIComponent(workspaceId)}/agents`, { method: "POST", body: JSON.stringify({ server_id: agentServerId, kind: agentKind, name: agentName, cwd: agentCwd, args: agentArgs.split("\n").map((value) => value.trim()).filter(Boolean), cols: 120, rows: 36 }) })
      setCreateAgentOpen(false); setSelectedAgentId(agent.agent_id); await refreshSnapshot()
    } catch (reason) { showError(reason) }
  }

  async function openInstall() {
    if (!workspaceId) return
    try {
      const data = await api<{ install_command: string; connect_command: string }>(`/api/workspaces/${encodeURIComponent(workspaceId)}/bootstrap`, { method: "POST", body: "{}" })
      setInstallCommand(data.install_command); setConnectCommand(data.connect_command); setInstallOpen(true)
    } catch (reason) { showError(reason) }
  }

  function openRename(target: NonNullable<RenameTarget>) { setRenameTarget(target); setRenameName(target.name) }

  async function submitRename(event: FormEvent) {
    event.preventDefault()
    if (!workspaceId || !renameTarget) return
    const resource = renameTarget.kind === "machine" ? "servers" : "agents"
    try {
      await api(`/api/workspaces/${encodeURIComponent(workspaceId)}/${resource}/${encodeURIComponent(renameTarget.id)}`, { method: "PATCH", body: JSON.stringify({ name: renameName }) })
      setRenameTarget(null); await refreshSnapshot()
    } catch (reason) { showError(reason) }
  }

  async function confirmDelete() {
    if (!workspaceId || !deleteTarget) return
    const resource = deleteTarget.kind === "machine" ? "servers" : "agents"
    try {
      await api(`/api/workspaces/${encodeURIComponent(workspaceId)}/${resource}/${encodeURIComponent(deleteTarget.id)}`, { method: "DELETE" })
      setDeleteTarget(null); await refreshSnapshot()
    } catch (reason) { showError(reason) }
  }

  async function stopAgent() {
    if (!workspaceId || !selectedAgentId) return
    try { await api(`/api/workspaces/${encodeURIComponent(workspaceId)}/agents/${encodeURIComponent(selectedAgentId)}/stop`, { method: "POST", body: "{}" }) }
    catch (reason) { showError(reason) }
  }

  async function openMembers() {
    if (!organizationId) return
    try {
      const data = await api<{ members: Member[]; current_role: Member["role"] }>(`/api/organizations/${encodeURIComponent(organizationId)}/members`)
      setMembers(data.members); setCurrentRole(data.current_role); setMembersOpen(true)
    } catch (reason) { showError(reason) }
  }

  async function updateMemberRole(username: string, role: Member["role"]) {
    if (!organizationId) return
    try {
      await api(`/api/organizations/${encodeURIComponent(organizationId)}/members/${encodeURIComponent(username)}`, { method: "PATCH", body: JSON.stringify({ role }) })
      await openMembers()
    } catch (reason) { showError(reason) }
  }

  async function removeMember(username: string) {
    if (!organizationId) return
    try {
      await api(`/api/organizations/${encodeURIComponent(organizationId)}/members/${encodeURIComponent(username)}`, { method: "DELETE" })
      await openMembers()
    } catch (reason) { showError(reason) }
  }

  async function createInvite() {
    if (!organizationId) return
    try {
      const invitation = await api<{ url: string }>(`/api/organizations/${encodeURIComponent(organizationId)}/invitations`, { method: "POST", body: "{}" })
      setInviteUrl(invitation.url); setMembersOpen(false); setInviteOpen(true)
    } catch (reason) { showError(reason) }
  }

  async function loadNetworkPolicies() {
    if (!workspaceId) return
    const data = await api<{ policies: NetworkPolicy[] }>(`/api/workspaces/${encodeURIComponent(workspaceId)}/network-policies`)
    setNetworkPolicies(data.policies)
  }

  async function loadVirtualHosts() {
    if (!workspaceId) return
    const data = await api<{ hosts: VirtualNetworkHost[] }>(`/api/workspaces/${encodeURIComponent(workspaceId)}/virtual-hosts`)
    setVirtualHosts(data.hosts)
  }

  async function openNetwork() {
    try {
      await Promise.all([loadNetworkPolicies(), loadVirtualHosts()])
      const firstMachine = snapshot?.servers[0]?.server_id ?? ""
      setNetworkDestination(firstMachine)
      setVirtualDestination(firstMachine)
      setNetworkOpen(true)
    } catch (reason) { showError(reason) }
  }

  async function createVirtualHost(event: FormEvent) {
    event.preventDefault()
    if (!workspaceId) return
    try {
      await api(`/api/workspaces/${encodeURIComponent(workspaceId)}/virtual-hosts`, {
        method: "POST",
        body: JSON.stringify({
          hostname: virtualHostname,
          destination_server_id: virtualDestination,
          target_host: virtualTargetHost,
          target_port: virtualTargetPort ? Number(virtualTargetPort) : null,
        }),
      })
      setVirtualHostname(""); setVirtualTargetPort("")
      await loadVirtualHosts()
    } catch (reason) { showError(reason) }
  }

  async function deleteVirtualHost(hostname: string) {
    if (!workspaceId) return
    try {
      await api(`/api/workspaces/${encodeURIComponent(workspaceId)}/virtual-hosts/${encodeURIComponent(hostname)}`, { method: "DELETE" })
      await loadVirtualHosts()
    } catch (reason) { showError(reason) }
  }

  async function createNetworkPolicy(event: FormEvent) {
    event.preventDefault()
    if (!workspaceId) return
    try {
      await api(`/api/workspaces/${encodeURIComponent(workspaceId)}/network-policies`, {
        method: "POST",
        body: JSON.stringify({
          source_server_id: networkSource === "*" ? null : networkSource,
          destination_server_id: networkDestination,
          target_host: networkHost,
          port_start: Number(networkPortStart),
          port_end: networkPortEnd ? Number(networkPortEnd) : null,
        }),
      })
      setNetworkPortStart(""); setNetworkPortEnd("")
      await loadNetworkPolicies()
    } catch (reason) { showError(reason) }
  }

  async function deleteNetworkPolicy(policyId: string) {
    if (!workspaceId) return
    try {
      await api(`/api/workspaces/${encodeURIComponent(workspaceId)}/network-policies/${encodeURIComponent(policyId)}`, { method: "DELETE" })
      await loadNetworkPolicies()
    } catch (reason) { showError(reason) }
  }

  async function copy(value: string) {
    try { await navigator.clipboard.writeText(value) }
    catch { showError("Unable to copy to the clipboard") }
  }

  async function logout() {
    try { await api("/api/auth/logout", { method: "POST", body: "{}" }) }
    finally { window.location.href = "/" }
  }

  if (user === undefined) return <div className="grid min-h-dvh place-items-center bg-[#f7f7f5] text-sm text-muted-foreground">Loading Treer...</div>
  if (!user) return <AuthScreen onAuthenticated={setUser} />

  return <TooltipProvider delayDuration={350}>
    <main className="grid h-dvh min-h-0 grid-rows-[310px_minmax(620px,1fr)] overflow-auto bg-background md:grid-cols-[272px_minmax(0,1fr)] md:grid-rows-1 md:overflow-hidden">
      <aside className="flex min-h-0 flex-col border-b bg-[#f7f7f5] md:border-b-0 md:border-r">
        <div className="grid min-h-[58px] grid-cols-[32px_minmax(0,1fr)_32px] items-center gap-2 px-3 py-2">
          <div className="grid size-8 place-items-center rounded-[5px] bg-[#37352f] font-serif font-bold text-white">{initials(organization?.name ?? "Treer")}</div>
          <div className="min-w-0"><div className="mb-0.5 px-1 text-[9px] font-semibold uppercase text-muted-foreground">Organization</div><Select value={organizationId ?? undefined} onValueChange={setOrganizationId}><SelectTrigger className="h-7 border-0 bg-transparent px-1 shadow-none hover:bg-black/[.04]"><SelectValue placeholder="No organization" /></SelectTrigger><SelectContent>{organizations.map((item) => <SelectItem key={item.organization_id} value={item.organization_id}>{item.name}</SelectItem>)}</SelectContent></Select></div>
          <IconButton label="Create organization" onClick={() => setCreateOrganizationOpen(true)}><Plus /></IconButton>
        </div>
        <div className="grid grid-cols-[20px_minmax(0,1fr)_32px] items-center gap-2 px-3 pb-3 pl-5">
          <FolderKanban className="size-3.5 text-muted-foreground" />
          <Select value={workspaceId ?? undefined} onValueChange={setWorkspaceId} disabled={!organizationId}><SelectTrigger className="h-7 border-0 bg-transparent px-1 text-xs shadow-none hover:bg-black/[.04]"><SelectValue placeholder="No workspace" /></SelectTrigger><SelectContent>{workspaces.map((item) => <SelectItem key={item.workspace_id} value={item.workspace_id}>{item.name}</SelectItem>)}</SelectContent></Select>
          <IconButton label="Create workspace" disabled={!organizationId} onClick={() => setCreateWorkspaceOpen(true)}><Plus /></IconButton>
        </div>

        <Tabs defaultValue="agents" className="flex min-h-0 flex-1 flex-col overflow-hidden">
          <TabsList className="mx-2 grid h-auto grid-cols-2 bg-black/[.04] p-0.5">
            <TabsTrigger value="machines" className="h-8 gap-2 text-xs"><Server className="size-3.5" />Machines <span className="rounded-full bg-black/[.06] px-1.5 text-[9px]">{snapshot?.servers.length ?? 0}</span></TabsTrigger>
            <TabsTrigger value="agents" className="h-8 gap-2 text-xs"><TerminalSquare className="size-3.5" />Agents <span className="rounded-full bg-black/[.06] px-1.5 text-[9px]">{snapshot?.agents.length ?? 0}</span></TabsTrigger>
          </TabsList>
          <TabsContent value="machines" className="mt-0 min-h-0 flex-1 overflow-hidden data-[state=inactive]:hidden">
            <div className="flex h-full min-h-0 flex-col">
              <div className="flex h-10 shrink-0 items-center justify-between px-4 text-[11px] font-medium text-muted-foreground"><span>Machines</span><div className="flex items-center"><IconButton label="Network policies" className="size-7" onClick={openNetwork} disabled={!workspaceId}><Network /></IconButton><Button variant="ghost" size="sm" className="h-7 px-2" onClick={openInstall} disabled={!workspaceId}><CirclePlus className="size-3.5" />Add</Button></div></div>
              <div className="min-h-0 flex-1 overflow-auto px-2 pb-2">
                {snapshot?.servers.map((machine) => <MachineItem key={machine.server_id} machine={machine} onRename={() => openRename({ kind: "machine", id: machine.server_id, name: machineName(machine) })} onDelete={() => setDeleteTarget({ kind: "machine", id: machine.server_id, name: machineName(machine) })} />)}
                {snapshot && !snapshot.servers.length && <EmptyState icon={<Server />} label="No machines connected" />}
              </div>
            </div>
          </TabsContent>
          <TabsContent value="agents" className="mt-0 min-h-0 flex-1 overflow-hidden data-[state=inactive]:hidden">
            <div className="flex h-full min-h-0 flex-col">
              <div className="flex h-10 shrink-0 items-center justify-between px-4 text-[11px] font-medium text-muted-foreground"><span>Agents {snapshot && <span className="ml-1 font-mono text-[9px] text-zinc-400">rev {snapshot.revision}</span>}</span><Button variant="ghost" size="sm" className="h-7 px-2" onClick={() => { setAgentName(defaultAgentName()); setCreateAgentOpen(true) }} disabled={!workspaceId || !onlineMachines.length}><Plus className="size-3.5" />New</Button></div>
              <div className="min-h-0 flex-1 overflow-auto px-2 pb-2">
                {snapshot?.agents.map((agent) => <AgentItem key={agent.agent_id} agent={agent} machine={snapshot.servers.find((item) => item.server_id === agent.server_id)} selected={agent.agent_id === selectedAgentId} onClick={() => setSelectedAgentId(agent.agent_id)} />)}
                {snapshot && !snapshot.agents.length && <EmptyState icon={<TerminalSquare />} label="No agents in this workspace" />}
              </div>
            </div>
          </TabsContent>
        </Tabs>

        <div className="shrink-0 border-t p-2">
          <Button variant="ghost" className="h-8 w-full justify-start px-2 text-xs font-normal text-muted-foreground" onClick={openMembers} disabled={!organizationId}><Users className="size-3.5" />Members</Button>
          <DropdownMenu>
            <DropdownMenuTrigger asChild><button className="mt-1 grid h-10 w-full grid-cols-[28px_minmax(0,1fr)_20px] items-center gap-2 rounded-[5px] px-2 text-left hover:bg-black/[.05]"><span className="grid size-7 place-items-center rounded bg-[#e8deee] text-[10px] font-bold text-[#694a73]">{initials(user.username)}</span><span className="min-w-0"><span className="block truncate text-xs font-medium">{user.username}</span><span className="flex items-center gap-1.5 text-[9px] capitalize text-muted-foreground"><span className={cn("size-1.5 rounded-full bg-amber-500", connection === "live" && "bg-emerald-500")} />{connection}</span></span><MoreHorizontal className="size-4 text-muted-foreground" /></button></DropdownMenuTrigger>
            <DropdownMenuContent side="top" align="start" className="w-56"><DropdownMenuLabel>{user.username}</DropdownMenuLabel><DropdownMenuSeparator /><DropdownMenuItem onSelect={openMembers}><Users />Members</DropdownMenuItem><DropdownMenuItem onSelect={logout}><LogOut />Log out</DropdownMenuItem></DropdownMenuContent>
          </DropdownMenu>
        </div>
      </aside>

      <section className="grid min-h-0 min-w-0 grid-rows-[48px_minmax(0,1fr)]">
        <header className="flex min-w-0 items-center justify-between gap-4 border-b px-3 sm:px-5">
          <div className="flex min-w-0 items-center gap-1.5 overflow-hidden text-xs text-muted-foreground"><span className="hidden truncate sm:block">{workspace?.name ?? "Workspace"}</span><ChevronRight className="hidden size-3 shrink-0 sm:block" /><strong className="truncate font-medium text-foreground">{selectedAgent?.name ?? "Terminal"}</strong></div>
          <div className="flex shrink-0 items-center gap-0.5">
            <IconButton label="Rename agent" disabled={!selectedAgent} onClick={() => selectedAgent && openRename({ kind: "agent", id: selectedAgent.agent_id, name: selectedAgent.name })}><Pencil /></IconButton>
            <IconButton label="Reconnect terminal" disabled={!selectedAgent} onClick={() => { setSelectedAgentId(null); requestAnimationFrame(() => setSelectedAgentId(selectedAgent?.agent_id ?? null)) }}><RotateCw /></IconButton>
            <IconButton label="Stop agent" disabled={!selectedAgent || !terminalActive} onClick={stopAgent}><Square /></IconButton>
            <IconButton label="Delete agent" disabled={!selectedAgent} className="text-destructive hover:text-destructive" onClick={() => selectedAgent && setDeleteTarget({ kind: "agent", id: selectedAgent.agent_id, name: selectedAgent.name })}><Trash2 /></IconButton>
          </div>
        </header>
        <div className="flex min-h-0 justify-center overflow-hidden px-3 pb-4 pt-4 sm:px-8 sm:pb-7 sm:pt-6 lg:px-16">
          <div className="grid h-full min-h-0 w-full max-w-[1120px] grid-rows-[42px_minmax(0,1fr)] overflow-hidden rounded-md border border-zinc-800 bg-[#0f1215] shadow-[0_8px_28px_rgba(15,18,21,.14)]">
            <div className="flex min-w-0 items-center justify-between gap-4 border-b border-zinc-800 bg-[#191d20] px-3.5"><div className="flex min-w-0 items-baseline gap-2"><span className="truncate text-xs font-semibold text-zinc-200">{selectedAgent?.name ?? "Terminal"}</span>{selectedAgent && <span className="hidden truncate font-mono text-[9px] text-zinc-500 sm:block">{selectedAgent.agent_id} · {machineName(snapshot?.servers.find((item) => item.server_id === selectedAgent.server_id))}</span>}</div><span className="inline-flex shrink-0 items-center gap-1.5 text-[9px] uppercase text-zinc-500"><span className="size-1.5 rounded-full bg-current" />{terminalStatus}</span></div>
            <div className="min-h-0 min-w-0 overflow-hidden"><TerminalPane key={`${workspaceId}:${selectedAgentId}`} workspaceId={workspaceId} agentId={selectedAgentId} active={terminalActive} onStatusChange={setTerminalState} /></div>
          </div>
        </div>
      </section>
    </main>

    {error && <div className="fixed bottom-4 left-1/2 z-[90] flex max-w-[calc(100vw-2rem)] -translate-x-1/2 items-center gap-3 rounded-md border bg-background px-4 py-3 text-sm shadow-lg"><span className="truncate">{error}</span><Button size="sm" variant="ghost" onClick={() => setError(null)}>Dismiss</Button></div>}

    <SimpleNameDialog open={createOrganizationOpen} onOpenChange={setCreateOrganizationOpen} title="Create organization" description="Organizations contain members and workspaces." label="Organization name" value={organizationName} onValueChange={setOrganizationName} onSubmit={createOrganization} />
    <SimpleNameDialog open={createWorkspaceOpen} onOpenChange={setCreateWorkspaceOpen} title="Create workspace" description={`Add a workspace to ${organization?.name ?? "this organization"}.`} label="Workspace name" value={workspaceName} onValueChange={setWorkspaceName} onSubmit={createWorkspace} />

    <Dialog open={createAgentOpen} onOpenChange={setCreateAgentOpen}><DialogContent><form onSubmit={createAgent} className="space-y-4"><DialogHeader><DialogTitle>Create agent</DialogTitle><DialogDescription>Start an agent on an online machine in this workspace.</DialogDescription></DialogHeader><Field label="Machine"><Select value={agentServerId} onValueChange={setAgentServerId} required><SelectTrigger><SelectValue placeholder="Select a machine" /></SelectTrigger><SelectContent>{onlineMachines.map((machine) => <SelectItem key={machine.server_id} value={machine.server_id}>{machineName(machine)}</SelectItem>)}</SelectContent></Select></Field><Field label="Kind"><Select value={agentKind} onValueChange={setAgentKind}><SelectTrigger><SelectValue /></SelectTrigger><SelectContent><SelectItem value="codex">codex</SelectItem><SelectItem value="claude">claude</SelectItem><SelectItem value="command">command</SelectItem></SelectContent></Select></Field><Field label="Name"><Input value={agentName} onChange={(event) => setAgentName(event.target.value)} required /></Field><Field label="Working directory"><Input value={agentCwd} onChange={(event) => setAgentCwd(event.target.value)} /></Field><Field label="Arguments, one per line"><Textarea rows={3} value={agentArgs} onChange={(event) => setAgentArgs(event.target.value)} /></Field><DialogFooter><Button type="button" variant="outline" onClick={() => setCreateAgentOpen(false)}>Cancel</Button><Button type="submit" disabled={!agentServerId}>Create agent</Button></DialogFooter></form></DialogContent></Dialog>

    <Dialog open={installOpen} onOpenChange={setInstallOpen}><DialogContent className="max-w-xl"><DialogHeader><DialogTitle>Add machine</DialogTitle><DialogDescription>Install Treer, then connect this workspace.</DialogDescription></DialogHeader><div className="space-y-4"><Field label="1. Install Treer"><div className="space-y-2"><Textarea readOnly value={installCommand} className="min-h-20 font-mono text-xs" /><Button size="sm" variant="outline" onClick={() => copy(installCommand)}><Copy />Copy install command</Button></div></Field><Field label="2. Connect workspace"><div className="space-y-2"><Textarea readOnly value={connectCommand} className="min-h-24 font-mono text-xs" /><Button size="sm" onClick={() => copy(connectCommand)}><Copy />Copy connection command</Button></div></Field></div><DialogFooter><Button variant="outline" onClick={() => setInstallOpen(false)}>Close</Button></DialogFooter></DialogContent></Dialog>

    <Dialog open={networkOpen} onOpenChange={setNetworkOpen}><DialogContent className="max-h-[90dvh] max-w-2xl overflow-y-auto"><DialogHeader><DialogTitle>Workspace network</DialogTitle><DialogDescription>{workspace?.name ?? "Workspace"}</DialogDescription></DialogHeader><section className="space-y-3"><h3 className="text-xs font-semibold">Virtual hosts</h3><div className="max-h-40 divide-y overflow-auto border-y">{virtualHosts.map((host) => <div key={host.hostname} className="grid min-h-12 grid-cols-[minmax(0,1fr)_auto] items-center gap-3 py-2"><div className="min-w-0 text-xs"><div className="truncate font-mono font-medium">{host.hostname}</div><div className="mt-1 truncate text-[10px] text-muted-foreground">{machineName(snapshot?.servers.find((item) => item.server_id === host.destination_server_id), host.destination_server_id)} · {host.target_host}:{host.target_port ?? "requested port"}</div></div><IconButton label={`Delete ${host.hostname}`} className="text-destructive" onClick={() => deleteVirtualHost(host.hostname)}><Trash2 /></IconButton></div>)}{!virtualHosts.length && <EmptyState icon={<Network />} label="No virtual hosts" />}</div><form onSubmit={createVirtualHost} className="grid gap-3 sm:grid-cols-2"><Field label="Virtual hostname"><Input className="font-mono" value={virtualHostname} onChange={(event) => setVirtualHostname(event.target.value)} placeholder="api.internal" required /></Field><Field label="Machine"><Select value={virtualDestination} onValueChange={setVirtualDestination} required><SelectTrigger><SelectValue placeholder="Select machine" /></SelectTrigger><SelectContent>{snapshot?.servers.map((machine) => <SelectItem key={machine.server_id} value={machine.server_id}>{machineName(machine)}</SelectItem>)}</SelectContent></Select></Field><Field label="Target host"><Input value={virtualTargetHost} onChange={(event) => setVirtualTargetHost(event.target.value)} required /></Field><Field label="Target port"><Input type="number" min="1" max="65535" placeholder="Keep requested port" value={virtualTargetPort} onChange={(event) => setVirtualTargetPort(event.target.value)} /></Field><DialogFooter className="sm:col-span-2"><Button type="submit" disabled={!virtualHostname || !virtualDestination}><Plus />Add host</Button></DialogFooter></form></section><section className="space-y-3 border-t pt-4"><h3 className="text-xs font-semibold">Policies</h3><div className="max-h-40 divide-y overflow-auto border-y">{networkPolicies.map((policy) => <div key={policy.policy_id} className="grid min-h-12 grid-cols-[minmax(0,1fr)_auto] items-center gap-3 py-2"><div className="min-w-0 text-xs"><div className="truncate font-medium">{policy.source_server_id ? machineName(snapshot?.servers.find((item) => item.server_id === policy.source_server_id), policy.source_server_id) : "Any machine"} → {machineName(snapshot?.servers.find((item) => item.server_id === policy.destination_server_id), policy.destination_server_id)}</div><div className="mt-1 truncate font-mono text-[10px] text-muted-foreground">{policy.target_host}:{policy.port_start}{policy.port_end !== policy.port_start && `-${policy.port_end}`}</div></div><IconButton label="Delete network policy" className="text-destructive" onClick={() => deleteNetworkPolicy(policy.policy_id)}><Trash2 /></IconButton></div>)}{!networkPolicies.length && <EmptyState icon={<Network />} label="No network policies" />}</div><form onSubmit={createNetworkPolicy} className="grid gap-3 sm:grid-cols-2"><Field label="Source"><Select value={networkSource} onValueChange={setNetworkSource}><SelectTrigger><SelectValue /></SelectTrigger><SelectContent><SelectItem value="*">Any machine</SelectItem>{snapshot?.servers.map((machine) => <SelectItem key={machine.server_id} value={machine.server_id}>{machineName(machine)}</SelectItem>)}</SelectContent></Select></Field><Field label="Destination"><Select value={networkDestination} onValueChange={setNetworkDestination} required><SelectTrigger><SelectValue placeholder="Select machine" /></SelectTrigger><SelectContent>{snapshot?.servers.map((machine) => <SelectItem key={machine.server_id} value={machine.server_id}>{machineName(machine)}</SelectItem>)}</SelectContent></Select></Field><Field label="Target host"><Input value={networkHost} onChange={(event) => setNetworkHost(event.target.value)} required /></Field><div className="grid grid-cols-2 gap-2"><Field label="First port"><Input type="number" min="1" max="65535" value={networkPortStart} onChange={(event) => setNetworkPortStart(event.target.value)} required /></Field><Field label="Last port"><Input type="number" min="1" max="65535" placeholder={networkPortStart || "Same"} value={networkPortEnd} onChange={(event) => setNetworkPortEnd(event.target.value)} /></Field></div><DialogFooter className="sm:col-span-2"><Button type="submit" disabled={!networkDestination || !networkPortStart}><Plus />Add policy</Button></DialogFooter></form></section></DialogContent></Dialog>

    <Dialog open={Boolean(renameTarget)} onOpenChange={(open) => !open && setRenameTarget(null)}><DialogContent><form onSubmit={submitRename}><DialogHeader><DialogTitle>Rename {renameTarget?.kind}</DialogTitle><DialogDescription>Choose a clear name for this {renameTarget?.kind}.</DialogDescription></DialogHeader><div className="my-5"><Field label="Name"><Input value={renameName} onChange={(event) => setRenameName(event.target.value)} required autoFocus /></Field></div><DialogFooter><Button type="button" variant="outline" onClick={() => setRenameTarget(null)}>Cancel</Button><Button type="submit">Rename</Button></DialogFooter></form></DialogContent></Dialog>

    <Dialog open={Boolean(deleteTarget)} onOpenChange={(open) => !open && setDeleteTarget(null)}><DialogContent><DialogHeader><DialogTitle>Delete {deleteTarget?.kind}</DialogTitle><DialogDescription>{deleteTarget?.kind === "machine" ? `Remove ${deleteTarget.name} and all of its agents? Its credential will be revoked, but its local service will not be uninstalled.` : `Delete ${deleteTarget?.name} and stop its process? This agent will not return after reconnecting.`}</DialogDescription></DialogHeader><DialogFooter><Button variant="outline" onClick={() => setDeleteTarget(null)}>Cancel</Button><Button variant="destructive" onClick={confirmDelete}>Delete</Button></DialogFooter></DialogContent></Dialog>

    <Dialog open={membersOpen} onOpenChange={setMembersOpen}><DialogContent className="max-w-xl"><DialogHeader><div className="flex items-center justify-between gap-4 pr-7"><DialogTitle>Organization members</DialogTitle>{canManageMembers && <Button size="sm" onClick={createInvite}><UserRound />Invite member</Button>}</div><DialogDescription>Manage access to {organization?.name ?? "this organization"}.</DialogDescription></DialogHeader><div className="max-h-[55vh] divide-y overflow-auto border-y">{members.map((member) => <div key={member.username} className="grid min-h-14 grid-cols-[minmax(0,1fr)_120px_auto] items-center gap-3"><span className="truncate text-sm font-medium">{member.username}</span>{currentRole === "owner" && member.role !== "owner" ? <Select value={member.role} onValueChange={(value: Member["role"]) => updateMemberRole(member.username, value)}><SelectTrigger className="h-8 text-xs"><SelectValue /></SelectTrigger><SelectContent><SelectItem value="member">Member</SelectItem><SelectItem value="admin">Admin</SelectItem></SelectContent></Select> : <span className="text-xs capitalize text-muted-foreground">{member.role}</span>}{canManageMembers && member.role !== "owner" ? <IconButton label={`Remove ${member.username}`} className="text-destructive" onClick={() => removeMember(member.username)}><Trash2 /></IconButton> : <span />}</div>)}</div></DialogContent></Dialog>

    <Dialog open={inviteOpen} onOpenChange={setInviteOpen}><DialogContent><DialogHeader><DialogTitle>Invite member</DialogTitle><DialogDescription>This registration link can be used once.</DialogDescription></DialogHeader><Textarea readOnly value={inviteUrl} className="min-h-24 font-mono text-xs" /><DialogFooter><Button variant="outline" onClick={() => setInviteOpen(false)}>Close</Button><Button onClick={() => copy(inviteUrl)}><Copy />Copy link</Button></DialogFooter></DialogContent></Dialog>
  </TooltipProvider>
}

function Field({ label, children }: { label: string; children: React.ReactNode }) {
  return <div className="space-y-2"><Label>{label}</Label>{children}</div>
}

function SimpleNameDialog({ open, onOpenChange, title, description, label, value, onValueChange, onSubmit }: { open: boolean; onOpenChange: (open: boolean) => void; title: string; description: string; label: string; value: string; onValueChange: (value: string) => void; onSubmit: (event: FormEvent) => void }) {
  return <Dialog open={open} onOpenChange={onOpenChange}><DialogContent><form onSubmit={onSubmit}><DialogHeader><DialogTitle>{title}</DialogTitle><DialogDescription>{description}</DialogDescription></DialogHeader><div className="my-5"><Field label={label}><Input value={value} onChange={(event) => onValueChange(event.target.value)} required autoFocus maxLength={80} /></Field></div><DialogFooter><Button type="button" variant="outline" onClick={() => onOpenChange(false)}>Cancel</Button><Button type="submit">Create</Button></DialogFooter></form></DialogContent></Dialog>
}

function EmptyState({ icon, label }: { icon: React.ReactNode; label: string }) {
  return <div className="flex flex-col items-center gap-2 px-4 py-8 text-center text-[11px] text-muted-foreground"><span className="[&_svg]:size-4 [&_svg]:opacity-50">{icon}</span>{label}</div>
}

function MachineItem({ machine, onRename, onDelete }: { machine: Machine; onRename: () => void; onDelete: () => void }) {
  return <div className="group flex min-h-14 items-start gap-2 rounded-[5px] px-2.5 py-2 hover:bg-black/[.045]"><span className={cn("mt-1.5 size-1.5 shrink-0 rounded-full bg-zinc-400", machine.status === "online" && "bg-emerald-500")} /><div className="min-w-0 flex-1"><div className="truncate text-xs font-medium">{machineName(machine)}</div><div className="mt-1 truncate font-mono text-[9px] text-muted-foreground">{machine.root}</div></div><DropdownMenu><DropdownMenuTrigger asChild><Button size="icon" variant="ghost" className="size-7 shrink-0 opacity-0 group-hover:opacity-100 data-[state=open]:opacity-100" aria-label={`Actions for ${machineName(machine)}`}><MoreHorizontal /></Button></DropdownMenuTrigger><DropdownMenuContent align="end"><DropdownMenuItem onSelect={onRename}><Pencil />Rename</DropdownMenuItem><DropdownMenuSeparator /><DropdownMenuItem className="text-destructive focus:text-destructive" onSelect={onDelete}><Trash2 />Delete</DropdownMenuItem></DropdownMenuContent></DropdownMenu></div>
}

function AgentItem({ agent, machine, selected, onClick }: { agent: Agent; machine?: Machine; selected: boolean; onClick: () => void }) {
  return <button onClick={onClick} className={cn("grid min-h-12 w-full grid-cols-[minmax(0,1fr)_auto] items-center gap-2 rounded-[5px] px-2.5 py-2 text-left hover:bg-black/[.045]", selected && "bg-black/[.075] hover:bg-black/[.075]")}><span className="min-w-0"><span className="block truncate text-xs font-medium">{agent.name}</span><span className="mt-1 block truncate text-[9px] text-muted-foreground">{agent.kind} · {machineName(machine, agent.server_id)}</span></span><Status value={agent.status} /></button>
}
