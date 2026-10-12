import { AgentBreadcrumbs } from "@/components/agent-breadcrumbs"
import { AppSettingsView, AppsView } from "@/components/apps-view"
import { AuditView } from "@/components/audit-view"
import { AuthScreen } from "@/components/auth-screen"
import { Field, IconButton, MachineRecovery, MobileTerminalKey, SimpleNameDialog } from "@/components/control-plane-ui"
import { MachineOverviewView } from "@/components/machine-view"
import { OrganizationSettingsView } from "@/components/organization-settings-view"
import { SettingsDialog } from "@/components/settings"
import { TerminalPane, type TerminalPaneHandle } from "@/components/terminal-pane"
import { Button } from "@/components/ui/button"
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog"
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuLabel, DropdownMenuSeparator, DropdownMenuTrigger } from "@/components/ui/dropdown-menu"
import { Input } from "@/components/ui/input"
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select"
import { Textarea } from "@/components/ui/textarea"
import { TooltipProvider } from "@/components/ui/tooltip"
import { WorkloadTree, type NewAgentFolder } from "@/components/workload-tree"
import { WorkspaceSettingsView } from "@/components/workspace-settings-view"
import { ApiError, api, machineName, proxyUrl, websocketUrl, type Agent, type AgentLaunchProfile, type AgentTrafficRecord, type AppDeployment, type MachineFileUpload, type MachineService, type MachineTrafficRecord, type Member, type Organization, type OrganizationAuditEvent, type OrganizationGroup, type Snapshot, type User, type VirtualNetworkHost, type Workspace, type WorkloadTreeState } from "@/lib/api"
import { formatCommandLine, parseCommandLine } from "@/lib/command-line"
import { DeleteTarget, MainView, PREVIEW_CONNECT, PREVIEW_INSTALL, PREVIEW_ORG, PREVIEW_PROFILES, PREVIEW_USER, PREVIEW_WORKSPACE, RenameTarget, TerminalState, activeStatuses, arrayBufferToBase64, authorizationReturnUrl, controlCharacter, defaultAgentName, defaultProfileAgentName, formatBytes, initials, machineOnline, replaceWorkspace, visibleWorkspaceSnapshot, workspacePath } from "@/lib/control-plane"
import { interfaceFrameKey, postEmbedTheme, withEmbedTheme } from "@/lib/embed-ui"
import { clearFirstRunTour, firstRunTourMode, shouldAutoStartFirstRunTour, startFirstRunTour, stopFirstRunTour, type FirstRunTourHost } from "@/lib/first-run-tour"
import { readTheme, subscribeTheme, type Theme } from "@/lib/theme"
import { agentPath } from "@/lib/workload-path"
import { cn } from "@/lib/utils"
import {
  ArrowDown,
  ArrowLeft,
  ArrowRight,
  ArrowUp,
  ChevronLeft,
  ChevronRight,
  Copy,
  CornerDownLeft,
  Delete,
  FolderKanban,
  Keyboard,
  ListChecks,
  LogOut,
  Maximize2,
  MoreHorizontal,
  Pencil,
  Play,
  Plus,
  Rocket,
  RotateCw,
  Settings as SettingsIcon,
  Square,
  Trash2,
  Upload,
  X
} from "lucide-react"
import { FormEvent, useCallback, useEffect, useMemo, useRef, useState } from "react"
import { useLocation, useNavigate, useParams } from "react-router"

export function WorkspaceApp() {
  const route = useParams<{ organizationId?: string; workspaceId?: string }>()
  const navigate = useNavigate()
  const location = useLocation()
  const routeSelectionRef = useRef({ organizationId: route.organizationId ?? null, workspaceId: route.workspaceId ?? null })
  routeSelectionRef.current = { organizationId: route.organizationId ?? null, workspaceId: route.workspaceId ?? null }
  const routingRef = useRef({ navigate, search: location.search, hash: location.hash })
  routingRef.current = { navigate, search: location.search, hash: location.hash }
  // Canonicalizing the URL must not reset workspace loading or close its event
  // socket when React Router changes the navigate function's identity.
  const replaceWorkspaceRoute = useCallback((organizationId: string | null, workspaceId: string | null) => {
    const routing = routingRef.current
    routing.navigate({ pathname: workspacePath(organizationId, workspaceId), search: routing.search, hash: routing.hash }, { replace: true })
  }, [])
  const preview = firstRunTourMode() === "preview"
  const [user, setUser] = useState<User | null | undefined>(preview ? PREVIEW_USER : undefined)
  const [organizations, setOrganizations] = useState<Organization[]>(preview ? [PREVIEW_ORG] : [])
  const [organizationId, setOrganizationId] = useState<string | null>(preview ? PREVIEW_ORG.organization_id : null)
  const organizationIdRef = useRef<string | null>(preview ? PREVIEW_ORG.organization_id : null)
  const [workspaces, setWorkspaces] = useState<Workspace[]>([])
  const [workspaceId, setWorkspaceId] = useState<string | null>(null)
  const workspaceIdRef = useRef<string | null>(null)
  workspaceIdRef.current = workspaceId
  const [loadedSnapshot, setSnapshot] = useState<Snapshot | null>(null)
  const snapshot = loadedSnapshot?.workspace.workspace_id === workspaceId ? loadedSnapshot : null
  const [selectedAgentId, setSelectedAgentId] = useState<string | null>(null)
  const [showWorkloadLogs, setShowWorkloadLogs] = useState(false)
  const [selectedMachineId, setSelectedMachineId] = useState<string | null>(null)
  const [selectedAppId, setSelectedAppId] = useState<string | null>(null)
  const [terminalStatus, setTerminalStatus] = useState<TerminalState>("not attached")
  const [interfaceUiRevision, setInterfaceUiRevision] = useState(0)
  const [theme, setTheme] = useState<Theme>(() => readTheme())
  const [mainView, setMainView] = useState<MainView>("terminal")
  const [settingsOpen, setSettingsOpen] = useState(false)
  const [mobileTerminalOpen, setMobileTerminalOpen] = useState(false)
  const [isMobile, setIsMobile] = useState(() => window.matchMedia("(max-width: 767px)").matches)
  const [ctrlArmed, setCtrlArmed] = useState(false)
  const terminalPaneRef = useRef<TerminalPaneHandle>(null)
  const interfaceFrameRef = useRef<HTMLIFrameElement>(null)
  const themeRef = useRef(theme)
  themeRef.current = theme
  const ctrlArmedRef = useRef(false)
  const [error, setError] = useState<string | null>(null)
  const [createOrganizationOpen, setCreateOrganizationOpen] = useState(false)
  const [createWorkspaceOpen, setCreateWorkspaceOpen] = useState(false)
  const [deleteWorkspaceOpen, setDeleteWorkspaceOpen] = useState(false)
  const [createAgentOpen, setCreateAgentOpen] = useState(false)
  const [agentFolder, setAgentFolder] = useState<NewAgentFolder | null>(null)
  const [placementRevision, setPlacementRevision] = useState(0)
  const [workloadTree, setWorkloadTree] = useState<WorkloadTreeState>({ folders: [], placements: [] })
  const [renameError, setRenameError] = useState("")
  const [createAgentError, setCreateAgentError] = useState("")
  const [creatingAgent, setCreatingAgent] = useState(false)
  const [deletingApp, setDeletingApp] = useState<AppDeployment | null>(null)
  const [profileEditorOpen, setProfileEditorOpen] = useState(false)
  const [editingLaunchProfile, setEditingLaunchProfile] = useState<AgentLaunchProfile | null>(null)
  const [launchingProfile, setLaunchingProfile] = useState<AgentLaunchProfile | null>(null)
  const [deletingProfile, setDeletingProfile] = useState<AgentLaunchProfile | null>(null)
  const [installOpen, setInstallOpen] = useState(false)
  const [inviteOpen, setInviteOpen] = useState(false)
  const [uploadOpen, setUploadOpen] = useState(false)
  const [uploadDirectory, setUploadDirectory] = useState(".")
  const [uploadFile, setUploadFile] = useState<File | null>(null)
  const [overwriteUpload, setOverwriteUpload] = useState(false)
  const [uploading, setUploading] = useState(false)
  const [uploadInputKey, setUploadInputKey] = useState(0)
  const [uploadMessage, setUploadMessage] = useState("")
  const [uploadError, setUploadError] = useState("")
  const [renameTarget, setRenameTarget] = useState<RenameTarget>(null)
  const [deleteTarget, setDeleteTarget] = useState<DeleteTarget>(null)
  const [createOrganizationName, setCreateOrganizationName] = useState("")
  const [organizationName, setOrganizationName] = useState("")
  const [createWorkspaceName, setCreateWorkspaceName] = useState("")
  const [workspaceName, setWorkspaceName] = useState("")
  const [agentName, setAgentName] = useState(defaultAgentName("terminal"))
  const [agentNameCustomized, setAgentNameCustomized] = useState(false)
  const [agentProfileId, setAgentProfileId] = useState("terminal")
  const [agentServerId, setAgentServerId] = useState("")
  const [agentCwd, setAgentCwd] = useState(".")
  const [agentCommandLine, setAgentCommandLine] = useState("")
  const [launchProfiles, setLaunchProfiles] = useState<AgentLaunchProfile[]>(preview ? PREVIEW_PROFILES : [])
  const [apps, setApps] = useState<AppDeployment[]>([])
  const [appsLoading, setAppsLoading] = useState(false)
  const createAgentOpenRef = useRef(false)
  const [launchProfilesLoading, setLaunchProfilesLoading] = useState(false)
  const [launchProfileName, setLaunchProfileName] = useState("")
  const [launchProfileDescription, setLaunchProfileDescription] = useState("")
  const [launchProfileCwd, setLaunchProfileCwd] = useState(".")
  const [launchProfileCommandLine, setLaunchProfileCommandLine] = useState("")
  const [launchMachineId, setLaunchMachineId] = useState("")
  const [launchAgentName, setLaunchAgentName] = useState("")
  const [renameName, setRenameName] = useState("")
  const [unixInstallCommand, setInstallCommand] = useState("")
  const [unixConnectCommand, setConnectCommand] = useState("")
  const [windowsInstallCommand, setWindowsInstallCommand] = useState("")
  const [windowsConnectCommand, setWindowsConnectCommand] = useState("")
  const [installPlatform, setInstallPlatform] = useState("unix")
  const installCommand = installPlatform === "windows" ? windowsInstallCommand : unixInstallCommand
  const connectCommand = installPlatform === "windows" ? windowsConnectCommand : unixConnectCommand
  const [inviteUrl, setInviteUrl] = useState("")
  const [members, setMembers] = useState<Member[]>([])
  const [groups, setGroups] = useState<OrganizationGroup[]>([])
  const [groupName, setGroupName] = useState("")
  const [auditEvents, setAuditEvents] = useState<OrganizationAuditEvent[]>([])
  const [traffic, setTraffic] = useState<MachineTrafficRecord[]>([])
  const [agentTraffic, setAgentTraffic] = useState<AgentTrafficRecord[]>([])
  const [auditLoading, setAuditLoading] = useState(false)
  const [virtualHosts, setVirtualHosts] = useState<VirtualNetworkHost[]>([])
  const [services, setServices] = useState<MachineService[]>([])

  const showError = useCallback((reason: unknown) => setError(reason instanceof Error ? reason.message : "Something went wrong"), [])

  useEffect(() => {
    if (preview) return
    api<User>("/api/auth/me").then(setUser).catch((reason) => {
      if (reason instanceof ApiError && reason.status === 401) setUser(null)
      else showError(reason)
    })
  }, [preview, showError])

  useEffect(() => {
    if (!user) return
    const returnTo = authorizationReturnUrl()
    if (returnTo) window.location.assign(returnTo)
  }, [user])

  const loadOrganizations = useCallback(async (preferred?: string) => {
    const data = await api<{ organizations: Organization[] }>("/api/organizations")
    setOrganizations(data.organizations)
    const routeSelection = routeSelectionRef.current
    const selected = preferred && data.organizations.some((item) => item.organization_id === preferred)
      ? preferred
      : routeSelection.organizationId && data.organizations.some((item) => item.organization_id === routeSelection.organizationId)
        ? routeSelection.organizationId
        : organizationIdRef.current && data.organizations.some((item) => item.organization_id === organizationIdRef.current)
          ? organizationIdRef.current
          : data.organizations[0]?.organization_id ?? null
    organizationIdRef.current = selected
    setOrganizationId(selected)
    replaceWorkspaceRoute(selected, routeSelection.organizationId === selected ? routeSelection.workspaceId : null)
  }, [replaceWorkspaceRoute])

  useEffect(() => { if (user && !preview) loadOrganizations().catch(showError) }, [user, preview, loadOrganizations, showError])

  const syncWorkspaces = useCallback(async () => {
    if (preview) return
    if (!organizationId) {
      setWorkspaces([])
      return
    }
    const data = await api<{ workspaces: Workspace[] }>(`/api/workspaces?organization_id=${encodeURIComponent(organizationId)}`)
    if (organizationIdRef.current !== organizationId) return
    setWorkspaces(data.workspaces)
    const routeSelection = routeSelectionRef.current
    const selected = routeSelection.organizationId === organizationId
      && routeSelection.workspaceId
      && data.workspaces.some((item) => item.workspace_id === routeSelection.workspaceId)
      ? routeSelection.workspaceId
      : data.workspaces[0]?.workspace_id ?? null
    workspaceIdRef.current = selected
    setWorkspaceId(selected)
    replaceWorkspaceRoute(organizationId, selected)
  }, [organizationId, preview, replaceWorkspaceRoute])

  useEffect(() => {
    if (preview) {
      return
    }
    workspaceIdRef.current = null
    setWorkspaceId(null)
    setSnapshot(null)
    void syncWorkspaces().catch(showError)
  }, [organizationId, preview, syncWorkspaces, showError])

  const refreshSnapshot = useCallback(async () => {
    if (preview || !workspaceId) return
    const requestedWorkspaceId = workspaceId
    const data = visibleWorkspaceSnapshot(await api<Snapshot>(`/api/workspaces/${encodeURIComponent(requestedWorkspaceId)}/snapshot`))
    if (workspaceIdRef.current !== requestedWorkspaceId || data.workspace.workspace_id !== requestedWorkspaceId) return
    setSnapshot(data)
    setWorkspaces((items) => replaceWorkspace(items, data.workspace))
  }, [preview, workspaceId])

  useEffect(() => {
    if (preview) return
    if (!workspaceId) {
      setSnapshot(null)
      setSelectedAgentId(null)
      return
    }
    let disposed = false
    let socket: WebSocket | null = null
    let timer: number | undefined
    setSnapshot((current) => current?.workspace.workspace_id === workspaceId ? current : null)
    refreshSnapshot().catch(showError)
    const connect = () => {
      if (disposed || workspaceIdRef.current !== workspaceId) return
      socket = new WebSocket(websocketUrl(`/api/workspaces/${encodeURIComponent(workspaceId)}/events`))
      socket.onmessage = (event) => {
        if (disposed || workspaceIdRef.current !== workspaceId) return
        const message = JSON.parse(event.data) as { event: string; revision?: number; data?: Snapshot | Workspace | Agent }
        if (message.event === "workspace.snapshot" && message.data) {
          const next = visibleWorkspaceSnapshot(message.data as Snapshot)
          if (next.workspace.workspace_id !== workspaceId) return
          setSnapshot(next)
          setWorkspaces((items) => replaceWorkspace(items, next.workspace))
        } else if (message.event === "workspace.renamed" && message.data) {
          const updated = message.data as Workspace
          setWorkspaces((items) => replaceWorkspace(items, updated))
          setSnapshot((current) => current?.workspace.workspace_id === updated.workspace_id ? { ...current, workspace: updated } : current)
        } else if (message.event === "agent.updated" && message.data) {
          const updated = message.data as Agent
          setApps((current) => current.map((app) => app.runtime_agent_id === updated.agent_id || app.app_id === updated.agent_id ? { ...app, name: updated.name === `app:${app.name}` ? app.name : updated.name } : app))
          setSnapshot((current) => {
            if (!current || current.workspace.workspace_id !== workspaceId) return current
            const agents = current.agents.some((agent) => agent.agent_id === updated.agent_id)
              ? current.agents.map((agent) => agent.agent_id === updated.agent_id ? updated : agent)
              : [...current.agents, updated]
            return { ...current, revision: Math.max(current.revision, message.revision ?? current.revision), agents }
          })
        } else if (message.event === "agent.deleted" && message.data) {
          const removed = message.data as Agent
          setApps((current) => current.filter((app) => app.runtime_agent_id !== removed.agent_id && app.app_id !== removed.agent_id))
          setSnapshot((current) => {
            if (!current || current.workspace.workspace_id !== workspaceId) return current
            return { ...current, revision: Math.max(current.revision, message.revision ?? current.revision), agents: current.agents.filter((agent) => agent.agent_id !== removed.agent_id) }
          })
        } else if (message.event === "workspace.deleted" && message.data) {
          const removed = message.data as Workspace
          setWorkspaces((items) => items.filter((item) => item.workspace_id !== removed.workspace_id))
          if (removed.workspace_id === workspaceId) {
            workspaceIdRef.current = null
            setSnapshot(null)
            setSelectedAgentId(null)
            setSelectedMachineId(null)
            void syncWorkspaces().catch(showError)
          }
        }
        else refreshSnapshot().catch(showError)
      }
      socket.onclose = () => {
        if (disposed || workspaceIdRef.current !== workspaceId) return
        timer = window.setTimeout(() => connect(), 1200)
      }
    }
    connect()
    return () => { disposed = true; window.clearTimeout(timer); socket?.close() }
  }, [preview, workspaceId, refreshSnapshot, syncWorkspaces, showError])

  useEffect(() => {
    const agents = snapshot?.agents ?? []
    setSelectedAgentId((current) => current && (agents.some((agent) => agent.agent_id === current) || apps.some((app) => (app.runtime_agent_id ?? app.app_id) === current)) ? current : agents[0]?.agent_id ?? (apps[0] ? apps[0].runtime_agent_id ?? apps[0].app_id : null))
  }, [snapshot, apps])

  const selectedAgentApp = apps.find((app) => (app.runtime_agent_id ?? app.app_id) === selectedAgentId)
  const selectedRuntimeAgent = snapshot?.agents.find((agent) => agent.agent_id === selectedAgentId) ?? (selectedAgentApp ? { agent_id: selectedAgentApp.runtime_agent_id ?? selectedAgentApp.app_id, server_id: selectedAgentApp.server_id, name: selectedAgentApp.name, kind: "app", status: selectedAgentApp.status } : undefined)
  const selectedAgent = selectedRuntimeAgent && selectedAgentApp ? { ...selectedRuntimeAgent, name: selectedAgentApp.name } : selectedRuntimeAgent
  const selectedAgentPath = selectedAgent ? agentPath(selectedAgent, workloadTree) : ""
  const selectedAgentMachine = snapshot?.servers.find((machine) => machine.server_id === selectedAgent?.server_id)
  const selectedAgentMachineOnline = machineOnline(selectedAgentMachine)
  const selectedAgentInterface = !showWorkloadLogs && !selectedAgentApp && selectedAgentMachineOnline && selectedAgent?.interface?.ui_path ? selectedAgent.interface : undefined
  const showAppUi = Boolean(selectedAgentApp && !showWorkloadLogs)
  const onlineMachines = snapshot?.servers.filter((machine) => machine.status === "online") ?? []
  const selectedApp = apps.find((app) => app.app_id === selectedAppId)
  const selectedAppMachine = snapshot?.servers.find((machine) => machine.server_id === selectedApp?.server_id)
  const selectedCreateProfile = launchProfiles.find((profile) => profile.profile_id === agentProfileId)
  const organization = organizations.find((item) => item.organization_id === organizationId)
  const workspace = workspaces.find((item) => item.workspace_id === workspaceId)
  const workspaceMachineCount = snapshot?.workspace.workspace_id === workspaceId
    ? snapshot.servers.length
    : undefined

  useEffect(() => {
    document.title = organization && workspace
      ? `${organization.name} / ${workspace.name}`
      : "Treer"
    return () => { document.title = "Treer" }
  }, [organization, workspace])

  const terminalActive = Boolean(selectedAgent && activeStatuses.has(selectedAgent.status))
  const interfaceUiUrl = useMemo(() => {
    // App status carries desired state; the runtime record carries Agent
    // lifecycle states (idle, working, ...), never "running".
    if (workspaceId && selectedAgentApp && showAppUi && selectedAgentMachineOnline && selectedAgentApp.status === "running" && selectedAgent && activeStatuses.has(selectedAgent.status)) {
      return withEmbedTheme(
        proxyUrl(`/api/workspaces/${encodeURIComponent(workspaceId)}/apps/${encodeURIComponent(selectedAgentApp.app_id)}/ui/`),
        theme,
      )
    }
    if (!workspaceId || !selectedAgent || !selectedAgentInterface) return null
    return withEmbedTheme(
      proxyUrl(`/api/workspaces/${encodeURIComponent(workspaceId)}/agents/${encodeURIComponent(selectedAgent.agent_id)}/interface/ui/`),
      theme,
    )
    // Theme is snapshotted when the Agent interface loads or reloads so toggling
    // Treer appearance does not remount the iframe. Live updates use postMessage
    // and the iframe color-scheme style. A new registered_at is a new
    // registration and should rebuild the src.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [workspaceId, selectedAgent?.agent_id, selectedAgent?.status, selectedAgentApp?.app_id, selectedAgentApp?.status, selectedAgentApp?.hostname, showAppUi, selectedAgentMachineOnline, selectedAgentInterface?.instance_id, selectedAgentInterface?.registered_at, interfaceUiRevision])
  const setTerminalState = useCallback((value: TerminalState) => setTerminalStatus(value), [])
  const currentRole = organization?.role ?? "member"
  const canManageMembers = ["owner", "admin"].includes(currentRole)

  useEffect(() => {
    if (mainView === "organization") setOrganizationName(organization?.name ?? "")
    if (mainView === "workspace") setWorkspaceName(workspace?.name ?? "")
  }, [mainView, organization?.organization_id, organization?.name, workspace?.workspace_id, workspace?.name])
  const mobileTerminalIdle = isMobile && mainView === "terminal" && !mobileTerminalOpen
  const mobileSidebarHidden = isMobile && mainView !== "terminal"
  const selectedMachine = snapshot?.servers.find((machine) => machine.server_id === selectedMachineId)

  const transformTerminalInput = useCallback((data: string) => {
    if (!ctrlArmedRef.current) return data
    const control = controlCharacter(data)
    if (control === null) return data
    ctrlArmedRef.current = false
    setCtrlArmed(false)
    return control + data.slice(1)
  }, [])

  function setCtrlModifier(armed: boolean) {
    ctrlArmedRef.current = armed
    setCtrlArmed(armed)
    if (armed) requestAnimationFrame(() => terminalPaneRef.current?.focus())
  }

  function openMobileTerminal() {
    setMobileTerminalOpen(true)
  }

  function closeMobileSurface() {
    setMobileTerminalOpen(false)
    setCtrlModifier(false)
  }

  function refreshAgentView() {
    if (interfaceUiUrl) {
      setInterfaceUiRevision((value) => value + 1)
      return
    }
    setSelectedAgentId(null)
    requestAnimationFrame(() => setSelectedAgentId(selectedAgent?.agent_id ?? null))
  }

  useEffect(() => {
    if (!mobileTerminalOpen) return
    const overflow = document.body.style.overflow
    document.body.style.overflow = "hidden"
    requestAnimationFrame(() => terminalPaneRef.current?.focus())
    return () => { document.body.style.overflow = overflow }
  }, [mobileTerminalOpen])

  useEffect(() => {
    setMobileTerminalOpen(false)
    setCtrlModifier(false)
  }, [workspaceId])

  useEffect(() => {
    const media = window.matchMedia("(max-width: 767px)")
    const update = () => setIsMobile(media.matches)
    update()
    media.addEventListener("change", update)
    return () => media.removeEventListener("change", update)
  }, [])

  useEffect(() => subscribeTheme(setTheme), [])

  useEffect(() => {
    postEmbedTheme(interfaceFrameRef.current, theme)
  }, [theme])

  useEffect(() => {
    if (isMobile || !mobileTerminalOpen) return
    setMobileTerminalOpen(false)
    setCtrlModifier(false)
  }, [isMobile, mobileTerminalOpen])

  function showAgentTerminal(agentId: string, logs = false) {
    setSelectedAgentId(agentId)
    setShowWorkloadLogs(logs)
    setMainView("terminal")
    if (isMobile) setMobileTerminalOpen(true)
  }

  function selectOrganization(value: string) {
    organizationIdRef.current = value
    workspaceIdRef.current = null
    setOrganizationId(value)
    setWorkspaceId(null)
    replaceWorkspaceRoute(value, null)
  }

  function selectWorkspace(value: string) {
    workspaceIdRef.current = value
    setWorkspaceId(value)
    setSnapshot((current) => current?.workspace.workspace_id === value ? current : null)
    setApps([])
    setSelectedAppId(null)
    setLaunchProfiles([])
    replaceWorkspaceRoute(organizationId, value)
  }

  useEffect(() => {
    if (createAgentOpen && !onlineMachines.some((machine) => machine.server_id === agentServerId)) setAgentServerId(onlineMachines[0]?.server_id ?? "")
  }, [createAgentOpen, onlineMachines, agentServerId])


  async function createOrganization(event: FormEvent) {
    event.preventDefault()
    try {
      const data = await api<{ organization: Organization }>("/api/organizations", { method: "POST", body: JSON.stringify({ name: createOrganizationName }) })
      setCreateOrganizationOpen(false); setCreateOrganizationName(""); await loadOrganizations(data.organization.organization_id)
    } catch (reason) { showError(reason) }
  }

  async function createWorkspace(event: FormEvent) {
    event.preventDefault()
    if (preview) {
      setCreateWorkspaceOpen(false)
      setCreateWorkspaceName("")
      setWorkspaces([PREVIEW_WORKSPACE])
      setWorkspaceId(PREVIEW_WORKSPACE.workspace_id)
      setSnapshot({ revision: 0, workspace: PREVIEW_WORKSPACE, servers: [], agents: [] })
      return
    }
    if (!organizationId) return
    try {
      const data = await api<{ workspace: Workspace }>("/api/workspaces", { method: "POST", body: JSON.stringify({ organization_id: organizationId, name: createWorkspaceName }) })
      setCreateWorkspaceOpen(false); setCreateWorkspaceName("")
      const list = await api<{ workspaces: Workspace[] }>(`/api/workspaces?organization_id=${encodeURIComponent(organizationId)}`)
      setWorkspaces(list.workspaces); selectWorkspace(data.workspace.workspace_id)
    } catch (reason) { showError(reason) }
  }

  async function renameOrganization(event: FormEvent) {
    event.preventDefault()
    if (!organizationId) return
    try {
      await api(`/api/organizations/${encodeURIComponent(organizationId)}`, { method: "PATCH", body: JSON.stringify({ name: organizationName }) })
      await loadOrganizations(organizationId)
    } catch (reason) { showError(reason) }
  }

  async function renameWorkspace(event: FormEvent) {
    event.preventDefault()
    if (!workspaceId) return
    try {
      const data = await api<{ workspace: Workspace }>(`/api/workspaces/${encodeURIComponent(workspaceId)}`, { method: "PATCH", body: JSON.stringify({ name: workspaceName }) })
      setWorkspaces((items) => replaceWorkspace(items, data.workspace))
      setSnapshot((current) => current?.workspace.workspace_id === data.workspace.workspace_id ? { ...current, workspace: data.workspace } : current)
      setWorkspaceName(data.workspace.name)
    } catch (reason) { showError(reason) }
  }

  async function confirmDeleteWorkspace() {
    const targetId = workspaceId
    if (!targetId) return
    try {
      await api(`/api/workspaces/${encodeURIComponent(targetId)}`, { method: "DELETE" })
    } catch (reason) { showError(reason); return }
    setDeleteWorkspaceOpen(false)
    setWorkspaces((items) => items.filter((item) => item.workspace_id !== targetId))
    setSnapshot(null)
    setSelectedAgentId(null)
    setSelectedMachineId(null)
    await syncWorkspaces().catch(showError)
  }
  async function createAgent(event: FormEvent) {
    event.preventDefault()
    if (preview) {
      setCreateAgentOpen(false)
      return
    }
    if (!workspaceId || creatingAgent) return
    const creationWorkspaceId = workspaceId
    const folder = agentFolder
    const placement = folder ? { folder_id: folder.folderId } : {}
    setCreatingAgent(true); setCreateAgentError("")
    try {
      let agent
      if (agentProfileId === "terminal") {
        agent = await api<Agent>(`/api/workspaces/${encodeURIComponent(workspaceId)}/agents`, { method: "POST", body: JSON.stringify({ ...placement, server_id: agentServerId, kind: "shell", name: agentName, cwd: agentCwd, args: [], cols: 120, rows: 36 }) })
      } else if (agentProfileId === "manual") {
        const parsed = parseCommandLine(agentCommandLine)
        agent = await api<Agent>(`/api/workspaces/${encodeURIComponent(workspaceId)}/agents`, { method: "POST", body: JSON.stringify({ ...placement, server_id: agentServerId, kind: "command", name: agentName, cwd: agentCwd, args: [parsed.command, ...parsed.args], cols: 120, rows: 36 }) })
      } else {
        agent = await api<Agent>(`/api/workspaces/${encodeURIComponent(workspaceId)}/launch-profiles/${encodeURIComponent(agentProfileId)}/launch`, { method: "POST", body: JSON.stringify({ ...placement, server_id: agentServerId, agent_name: agentName, cols: 120, rows: 36 }) })
      }
      const agentId = agent.agent_id
      if (!agentId) throw new Error("Create agent did not return an id")
      // Name and final placement are reserved atomically by the Proxy.
      if (workspaceIdRef.current === creationWorkspaceId) {
        setCreateAgentOpen(false)
        setAgentFolder(null)
      }
      if (folder && workspaceIdRef.current === creationWorkspaceId) setPlacementRevision((revision) => revision + 1)
      if (workspaceIdRef.current !== creationWorkspaceId) return
      setCreateAgentOpen(false)
      showAgentTerminal(agentId)
      await refreshSnapshot()
    } catch (reason) { if (workspaceIdRef.current === creationWorkspaceId) setCreateAgentError(reason instanceof Error ? reason.message : "Create failed") }
    finally { setCreatingAgent(false) }
  }

  function openCreateAgent(reset = true, folder: NewAgentFolder | null = null) {
    setCreateAgentError("")
    if (creatingAgent) return
    if (reset || !createAgentOpenRef.current) {
      setAgentFolder(folder)
      setAgentProfileId("terminal")
      setAgentName(defaultAgentName("terminal"))
      setAgentNameCustomized(false)
      setAgentCommandLine("")
    }
    setCreateAgentOpen(true)
    if (!preview) void loadLaunchProfiles().catch(showError)
  }

  function openCreateAgentOrMachineSetup(folder?: NewAgentFolder) {
    if (!preview && snapshot && snapshot.servers.length === 0) {
      openWorkspaceSettings()
      return
    }
    openCreateAgent(true, folder ?? null)
  }

  useEffect(() => { setCreateAgentOpen(false); setAgentFolder(null) }, [workspaceId])

  function selectAgentProfile(profileId: string) {
    changeAgentProfile(profileId)
  }

  function changeAgentCommandLine(commandLine: string) {
    setAgentCommandLine(commandLine)
    if (agentNameCustomized) return
    try {
      const { command } = parseCommandLine(commandLine)
      setAgentName(defaultProfileAgentName(command.split(/[/\\]/).pop() || "command"))
    } catch {
      // Keep the last generated name until the command line is valid again.
    }
  }

  function changeAgentProfile(profileId: string) {
    setAgentProfileId(profileId)
    if (profileId === "terminal") {
      setAgentName(defaultAgentName("terminal"))
      setAgentNameCustomized(false)
      return
    }
    if (profileId === "manual") {
      setAgentName(defaultAgentName("command"))
      setAgentNameCustomized(false)
      setAgentCommandLine("")
      return
    }
    const profile = launchProfiles.find((item) => item.profile_id === profileId)
    if (!profile) return
    setAgentName(defaultProfileAgentName(profile.name))
    setAgentNameCustomized(false)
  }

  const loadLaunchProfiles = useCallback(async () => {
    if (preview) return
    if (!workspaceId) {
      setLaunchProfiles([])
      return
    }
    setLaunchProfilesLoading(true)
    const requestedWorkspaceId = workspaceId
    try {
      const data = await api<{ profiles: AgentLaunchProfile[] }>(`/api/workspaces/${encodeURIComponent(requestedWorkspaceId)}/launch-profiles`)
      if (workspaceIdRef.current !== requestedWorkspaceId) return
      setLaunchProfiles(data.profiles)
    } finally {
      if (workspaceIdRef.current === requestedWorkspaceId) setLaunchProfilesLoading(false)
    }
  }, [preview, workspaceId])

  useEffect(() => {
    if (mainView === "workspace") loadLaunchProfiles().catch(showError)
  }, [mainView, loadLaunchProfiles, showError])

  const loadApps = useCallback(async () => {
    if (preview || !workspaceId) {
      setApps([])
      return
    }
    setAppsLoading(true)
    const requestedWorkspaceId = workspaceId
    try {
      const data = await api<{ apps: AppDeployment[] }>(`/api/workspaces/${encodeURIComponent(requestedWorkspaceId)}/apps`)
      if (workspaceIdRef.current !== requestedWorkspaceId) return
      setApps(data.apps)
    } finally {
      if (workspaceIdRef.current === requestedWorkspaceId) setAppsLoading(false)
    }
  }, [preview, workspaceId])

  useEffect(() => {
    loadApps().catch(showError)
  }, [loadApps, showError])

  useEffect(() => {
    if (mainView === "app" && selectedAppId && !appsLoading && !selectedApp) setMainView("terminal")
  }, [mainView, selectedAppId, selectedApp, appsLoading])

  function openApps() {
    setMainView("terminal")
    setMobileTerminalOpen(false)
  }

  function openAppSettings(app: AppDeployment) {
    setSelectedAppId(app.app_id)
    setMainView("app")
    setMobileTerminalOpen(false)
  }

  async function updateAppAccess(app: AppDeployment, access: "public" | "workspace") {
    if (!workspaceId) return
    try {
      const data = await api<{ app: AppDeployment }>(`/api/workspaces/${encodeURIComponent(workspaceId)}/apps/${encodeURIComponent(app.app_id)}/access`, {
        method: "PATCH",
        body: JSON.stringify({ access }),
      })
      setApps((current) => current.map((item) => item.app_id === data.app.app_id ? data.app : item))
    } catch (reason) { showError(reason) }
  }

  async function appLifecycle(app: AppDeployment, action: "start" | "stop" | "restart") {
    if (!workspaceId) return
    try {
      await api(`/api/workspaces/${encodeURIComponent(workspaceId)}/agents/${encodeURIComponent(app.runtime_agent_id ?? app.app_id)}/${action}`, { method: "POST", body: "{}" })
      await loadApps()
    } catch (reason) { showError(reason) }
  }

  async function deleteAppDeployment() {
    if (!workspaceId || !deletingApp) return
    try {
      await api(`/api/workspaces/${encodeURIComponent(workspaceId)}/apps/${encodeURIComponent(deletingApp.app_id)}`, { method: "DELETE" })
      setDeletingApp(null)
      setSelectedAppId(null)
      setMainView("terminal")
      await loadApps()
    } catch (reason) { showError(reason) }
  }

  function openOrganizationSettings() {
    if (!organization) {
      setCreateOrganizationName("")
      setCreateOrganizationOpen(true)
      return
    }
    setOrganizationName(organization.name)
    setMainView("organization")
    setMobileTerminalOpen(false)
  }

  function openWorkspaceSettings() {
    if (!workspace) {
      setCreateWorkspaceName("")
      setCreateWorkspaceOpen(true)
      return
    }
    setWorkspaceName(workspace.name)
    setMainView("workspace")
    setMobileTerminalOpen(false)
  }

  function closeMainView() {
    setMainView("terminal")
  }

  function closeMachineOverview() {
    setMainView("workspace")
  }

  function openSettings() {
    setSettingsOpen(true)
  }

  function openNewLaunchProfile() {
    setEditingLaunchProfile(null)
    setLaunchProfileName("")
    setLaunchProfileDescription("")
    setLaunchProfileCwd(".")
    setLaunchProfileCommandLine("")
    setProfileEditorOpen(true)
  }

  function openEditLaunchProfile(profile: AgentLaunchProfile) {
    setEditingLaunchProfile(profile)
    setLaunchProfileName(profile.name)
    setLaunchProfileDescription(profile.description)
    setLaunchProfileCwd(profile.cwd)
    setLaunchProfileCommandLine(formatCommandLine(profile.command, profile.args))
    setProfileEditorOpen(true)
  }

  async function saveLaunchProfile(event: FormEvent) {
    event.preventDefault()
    if (!workspaceId) return
    try {
      const parsed = parseCommandLine(launchProfileCommandLine)
      const path = editingLaunchProfile
        ? `/api/workspaces/${encodeURIComponent(workspaceId)}/launch-profiles/${encodeURIComponent(editingLaunchProfile.profile_id)}`
        : `/api/workspaces/${encodeURIComponent(workspaceId)}/launch-profiles`
      await api(path, {
        method: editingLaunchProfile ? "PATCH" : "POST",
        body: JSON.stringify({
          name: launchProfileName,
          description: launchProfileDescription,
          cwd: launchProfileCwd,
          command: parsed.command,
          args: parsed.args,
        }),
      })
      setProfileEditorOpen(false)
      setEditingLaunchProfile(null)
      await loadLaunchProfiles()
    } catch (reason) { showError(reason) }
  }

  function openLaunchProfile(profile: AgentLaunchProfile) {
    setLaunchingProfile(profile)
    setLaunchMachineId((current) => onlineMachines.some((machine) => machine.server_id === current) ? current : onlineMachines[0]?.server_id ?? "")
    setLaunchAgentName(defaultProfileAgentName(profile.name))
  }

  async function launchFromProfile(event: FormEvent) {
    event.preventDefault()
    if (!workspaceId || !launchingProfile) return
    try {
      const agent = await api<Agent>(`/api/workspaces/${encodeURIComponent(workspaceId)}/launch-profiles/${encodeURIComponent(launchingProfile.profile_id)}/launch`, {
        method: "POST",
        body: JSON.stringify({ server_id: launchMachineId, agent_name: launchAgentName, cols: 120, rows: 36 }),
      })
      setLaunchingProfile(null)
      await refreshSnapshot()
      showAgentTerminal(agent.agent_id)
    } catch (reason) { showError(reason) }
  }

  async function deleteLaunchProfile() {
    if (!workspaceId || !deletingProfile) return
    try {
      await api(`/api/workspaces/${encodeURIComponent(workspaceId)}/launch-profiles/${encodeURIComponent(deletingProfile.profile_id)}`, { method: "DELETE" })
      setDeletingProfile(null)
      await loadLaunchProfiles()
    } catch (reason) { showError(reason) }
  }

  async function openInstall() {
    if (preview || !workspaceId) {
      const origin = window.location.origin
      setInstallCommand(preview ? PREVIEW_INSTALL : `curl -fsSL '${origin}/install.sh' | sh`)
      setConnectCommand(preview ? PREVIEW_CONNECT : `treer-agent-server connect --key 'enr_v1_…' --proxy '${origin}/'`)
      const windowsOrigin = preview ? "https://treer.example" : origin
      setWindowsInstallCommand(`Invoke-WebRequest '${windowsOrigin}/install.ps1' -OutFile "$env:TEMP\\treer-install.ps1"; powershell.exe -NoProfile -ExecutionPolicy Bypass -File "$env:TEMP\\treer-install.ps1"; if ($LASTEXITCODE -eq 0) { $env:Path += ";$env:LOCALAPPDATA\\Treer\\bin" }`)
      setWindowsConnectCommand(preview ? PREVIEW_CONNECT : `treer-agent-server connect --key 'enr_v1_…' --proxy '${origin}/'`)
      setInstallOpen(true)
      return
    }
    try {
      const data = await api<{ install_command: string; connect_command: string; windows_install_command?: string; windows_connect_command?: string }>(`/api/workspaces/${encodeURIComponent(workspaceId)}/bootstrap`, { method: "POST", body: "{}" })
      setWindowsInstallCommand(data.windows_install_command ?? "Update the Proxy to obtain the Windows installer.")
      setWindowsConnectCommand(data.windows_connect_command ?? data.connect_command)
      setInstallCommand(data.install_command); setConnectCommand(data.connect_command); setInstallOpen(true)
    } catch (reason) { showError(reason) }
  }

  useEffect(() => {
    createAgentOpenRef.current = createAgentOpen
  }, [createAgentOpen])

  const tourHostRef = useRef<FirstRunTourHost | null>(null)
  tourHostRef.current = {
    openCreateWorkspace: () => { setCreateWorkspaceName(""); setCreateWorkspaceOpen(true) },
    closeCreateWorkspace: () => setCreateWorkspaceOpen(false),
    prepareWorkspaceForMachineSteps: () => {
      if (!preview) return
      setWorkspaces([PREVIEW_WORKSPACE])
      setWorkspaceId(PREVIEW_WORKSPACE.workspace_id)
      setSnapshot({ revision: 0, workspace: PREVIEW_WORKSPACE, servers: [], agents: [] })
    },
    openWorkspaceSettings: () => {
      setWorkspaceName(workspaces.find((item) => item.workspace_id === workspaceId)?.name ?? PREVIEW_WORKSPACE.name)
      setMainView("workspace")
      setMobileTerminalOpen(false)
    },
    closeWorkspaceSettings: closeMainView,
    openInstall,
    closeInstall: () => setInstallOpen(false),
    openCreateAgent,
    closeCreateAgent: () => setCreateAgentOpen(false),
    setAgentLaunch: (kind) => {
      if (kind === "ui-profile") {
        const profile = launchProfiles[0]
        changeAgentProfile(profile?.profile_id ?? "terminal")
        return
      }
      changeAgentProfile(kind)
    },
  }

  function replayFirstRunTour() {
    if (!user) return
    clearFirstRunTour(user.user_id)
    if (tourHostRef.current) startFirstRunTour(tourHostRef.current, { userId: user.user_id, persist: !preview })
  }

  useEffect(() => {
    if (!user) return
    if (!shouldAutoStartFirstRunTour(user.user_id)) return
    const timer = window.setTimeout(() => {
      if (!tourHostRef.current) return
      startFirstRunTour(tourHostRef.current, { userId: user.user_id, persist: !preview })
    }, 450)
    return () => {
      window.clearTimeout(timer)
      stopFirstRunTour()
    }
  }, [user, preview])

  function openRename(target: NonNullable<RenameTarget>) { setRenameError(""); setRenameTarget(target); setRenameName(target.name) }

  function openUpload() {
    closeMobileSurface()
    setUploadDirectory(".")
    setUploadFile(null)
    setOverwriteUpload(false)
    setUploadMessage("")
    setUploadError("")
    setUploadInputKey((value) => value + 1)
    setUploadOpen(true)
  }

  async function uploadToAgentMachine(event: FormEvent) {
    event.preventDefault()
    if (!workspaceId || !selectedAgent || !uploadFile) return
    if (uploadFile.size > 16 * 1024 * 1024) {
      setUploadError("Files must be 16 MiB or smaller.")
      return
    }
    setUploading(true)
    setUploadMessage("")
    setUploadError("")
    try {
      const result = await api<MachineFileUpload>(`/api/workspaces/${encodeURIComponent(workspaceId)}/machines/${encodeURIComponent(selectedAgent.server_id)}/files`, {
        method: "POST",
        body: JSON.stringify({
          directory: uploadDirectory,
          file_name: uploadFile.name,
          content_base64: arrayBufferToBase64(await uploadFile.arrayBuffer()),
          overwrite: overwriteUpload,
        }),
      })
      setUploadMessage(`Uploaded ${result.path} (${formatBytes(result.bytes_written)}).`)
      setUploadFile(null)
      setUploadInputKey((value) => value + 1)
    } catch (reason) {
      setUploadError(reason instanceof Error ? reason.message : "Upload failed")
    } finally {
      setUploading(false)
    }
  }

  async function submitRename(event: FormEvent) {
    event.preventDefault()
    if (!workspaceId || !renameTarget) return
    const resource = renameTarget.kind === "machine" ? "servers" : "agents"
    setRenameError("")
    try {
      await api(`/api/workspaces/${encodeURIComponent(workspaceId)}/${resource}/${encodeURIComponent(renameTarget.id)}`, { method: "PATCH", body: JSON.stringify({ name: renameName }) })
      if (workspaceIdRef.current !== workspaceId) return
      if (renameTarget.kind === "agent") setApps((current) => current.map((app) => app.runtime_agent_id === renameTarget.id || app.app_id === renameTarget.id ? { ...app, name: renameName } : app))
      setRenameTarget(null); await refreshSnapshot()
    } catch (reason) { setRenameError(reason instanceof Error ? reason.message : "Rename failed") }
  }

  async function confirmDelete() {
    if (!workspaceId || !deleteTarget) return
    const resource = deleteTarget.kind === "machine" ? "servers" : "agents"
    try {
      await api(`/api/workspaces/${encodeURIComponent(workspaceId)}/${resource}/${encodeURIComponent(deleteTarget.id)}`, { method: "DELETE" })
      if (workspaceIdRef.current !== workspaceId) return
      if (deleteTarget.kind === "agent") setApps((current) => current.filter((app) => app.runtime_agent_id !== deleteTarget.id && app.app_id !== deleteTarget.id))
      setDeleteTarget(null); await refreshSnapshot()
    } catch (reason) { showError(reason) }
  }

  async function stopAgent(agentId = selectedAgentId) {
    if (!workspaceId || !agentId) return
    try { await api(`/api/workspaces/${encodeURIComponent(workspaceId)}/agents/${encodeURIComponent(agentId)}/stop`, { method: "POST", body: "{}" }) }
    catch (reason) { showError(reason) }
  }

  const loadOrganizationSettings = useCallback(async () => {
    if (preview) {
      setMembers([{ ...PREVIEW_USER, role: "owner" }])
      setGroups([])
      return
    }
    const requestedOrganizationId = organizationId
    if (!requestedOrganizationId) {
      setMembers([])
      setGroups([])
      return
    }
    const [memberData, groupData] = await Promise.all([
      api<{ members: Member[] }>(`/api/organizations/${encodeURIComponent(requestedOrganizationId)}/members`),
      api<{ groups: OrganizationGroup[] }>(`/api/organizations/${encodeURIComponent(requestedOrganizationId)}/groups`),
    ])
    if (organizationIdRef.current !== requestedOrganizationId) return
    setMembers(memberData.members)
    setGroups(groupData.groups)
  }, [preview, organizationId])

  useEffect(() => {
    if (mainView === "organization") loadOrganizationSettings().catch(showError)
  }, [mainView, loadOrganizationSettings, showError])

  async function createGroup(event: FormEvent) {
    event.preventDefault()
    if (!organizationId) return
    try {
      await api(`/api/organizations/${encodeURIComponent(organizationId)}/groups`, { method: "POST", body: JSON.stringify({ name: groupName }) })
      setGroupName("")
      await loadOrganizationSettings()
    } catch (reason) { showError(reason) }
  }

  async function deleteGroup(groupId: string) {
    if (!organizationId) return
    try {
      await api(`/api/organizations/${encodeURIComponent(organizationId)}/groups/${encodeURIComponent(groupId)}`, { method: "DELETE" })
      await loadOrganizationSettings()
    } catch (reason) { showError(reason) }
  }

  async function setGroupMember(groupId: string, userId: string, present: boolean) {
    if (!organizationId) return
    try {
      const data = await api<{ groups: OrganizationGroup[] }>(`/api/organizations/${encodeURIComponent(organizationId)}/groups/${encodeURIComponent(groupId)}/members/${encodeURIComponent(userId)}`, { method: present ? "PUT" : "DELETE" })
      setGroups(data.groups)
    } catch (reason) { showError(reason) }
  }

  async function updateMemberRole(userId: string, role: Member["role"]) {
    if (!organizationId) return
    try {
      await api(`/api/organizations/${encodeURIComponent(organizationId)}/members/${encodeURIComponent(userId)}`, { method: "PATCH", body: JSON.stringify({ role }) })
      await loadOrganizationSettings()
    } catch (reason) { showError(reason) }
  }

  async function removeMember(userId: string) {
    if (!organizationId) return
    try {
      await api(`/api/organizations/${encodeURIComponent(organizationId)}/members/${encodeURIComponent(userId)}`, { method: "DELETE" })
      await loadOrganizationSettings()
    } catch (reason) { showError(reason) }
  }

  async function createInvite() {
    if (!organizationId) return
    try {
      const invitation = await api<{ url: string }>(`/api/organizations/${encodeURIComponent(organizationId)}/invitations`, { method: "POST", body: "{}" })
      setInviteUrl(invitation.url); setInviteOpen(true)
    } catch (reason) { showError(reason) }
  }

  const loadNetwork = useCallback(async () => {
    if (!workspaceId) {
      setVirtualHosts([])
      setServices([])
      return
    }
    const [hostData, serviceData] = await Promise.all([
      api<{ hosts: VirtualNetworkHost[] }>(`/api/workspaces/${encodeURIComponent(workspaceId)}/virtual-hosts`),
      api<{ services: MachineService[] }>(`/api/workspaces/${encodeURIComponent(workspaceId)}/services`),
    ])
    setVirtualHosts(hostData.hosts)
    setServices(serviceData.services)
  }, [workspaceId])

  function showMachineOverview(serverId: string) {
    setSelectedMachineId(serverId)
    setMainView("machine")
  }

  const loadAudit = useCallback(async () => {
    if (!organizationId || !workspaceId || !canManageMembers) {
      setAuditEvents([])
      return
    }
    setAuditLoading(true)
    try {
      const auditData = await api<{ events: OrganizationAuditEvent[] }>(`/api/organizations/${encodeURIComponent(organizationId)}/audit-events?workspace_id=${encodeURIComponent(workspaceId)}&limit=100`)
      setAuditEvents(auditData.events)
    } finally {
      setAuditLoading(false)
    }
  }, [organizationId, workspaceId, canManageMembers])

  const loadTraffic = useCallback(async () => {
    const requestedWorkspaceId = workspaceId
    if (!requestedWorkspaceId) {
      setTraffic([])
      setAgentTraffic([])
      return
    }
    const data = await api<{ traffic: MachineTrafficRecord[] }>(`/api/workspaces/${encodeURIComponent(requestedWorkspaceId)}/traffic?hours=24`)
    const detail = await api<{ traffic: AgentTrafficRecord[] }>(`/api/workspaces/${encodeURIComponent(requestedWorkspaceId)}/traffic/agents?hours=24`).catch((error: unknown) => {
      if (error instanceof ApiError && error.status === 404) return { traffic: [] }
      throw error
    })
    if (workspaceIdRef.current === requestedWorkspaceId) {
      setTraffic(data.traffic)
      setAgentTraffic(detail.traffic)
    }
  }, [workspaceId])

  const refreshAudit = useCallback(() => {
    void Promise.all([loadAudit(), loadTraffic()]).catch(showError)
  }, [loadAudit, loadTraffic, showError])

  useEffect(() => {
    if (mainView === "audit") loadAudit().catch(showError)
    if (mainView === "machine") loadNetwork().catch(showError)
    if (mainView !== "audit" && mainView !== "machine") return
    loadTraffic().catch(showError)
    const timer = window.setInterval(() => loadTraffic().catch(showError), 10_000)
    return () => window.clearInterval(timer)
  }, [mainView, loadAudit, loadNetwork, loadTraffic, showError])

  function openAudit() {
    setMainView("audit")
  }

  function openVirtualHost(hostname: string) {
    if (!workspaceId) return
    const url = proxyUrl(`/api/workspaces/${encodeURIComponent(workspaceId)}/virtual-hosts/${encodeURIComponent(hostname)}/proxy/`)
    window.open(url, "_blank", "noopener,noreferrer")
  }

  function openApp(app: AppDeployment) {
    if (!app.public_url) {
      openVirtualHost(app.hostname)
      return
    }
    window.open(app.public_url, "_blank", "noopener,noreferrer")
  }

  async function copy(value: string) {
    try { await navigator.clipboard.writeText(value) }
    catch { showError("Unable to copy to the clipboard") }
  }

  async function logout() {
    if (preview) return
    try { await api("/api/auth/logout", { method: "POST", body: "{}" }) }
    finally { window.location.href = "/" }
  }

  if (user === undefined) return <div className="grid min-h-dvh place-items-center bg-sidebar text-sm text-muted-foreground">Loading Treer...</div>
  if (!user) return <AuthScreen onAuthenticated={setUser} />

  return <TooltipProvider delayDuration={350}>
      <main className={cn("grid h-dvh min-h-0 bg-background md:grid-cols-[272px_minmax(0,1fr)] md:grid-rows-1 md:overflow-hidden", mobileTerminalIdle || mobileSidebarHidden ? "grid-rows-1 overflow-hidden" : "grid-rows-[374px_minmax(620px,1fr)] overflow-auto")}>
        <aside className={cn("flex min-h-0 flex-col border-b bg-sidebar md:border-b-0 md:border-r", mobileSidebarHidden && "hidden md:flex")}>
        {preview && <div className="treer-tour-banner"><span>Tour preview · empty first-run account, no server writes</span></div>}
        <div className="grid min-h-[58px] grid-cols-[32px_minmax(0,1fr)_32px] items-center gap-2 px-3 py-2">
          <div className="grid size-8 place-items-center rounded-[5px] bg-[#e8deee] text-[10px] font-bold text-[#694a73]">{initials(organization?.name ?? "Treer")}</div>
          <div className="min-w-0"><div className="mb-0.5 px-1 text-[9px] font-semibold uppercase text-muted-foreground">Organization</div><Select value={organizationId ?? undefined} onValueChange={selectOrganization}><SelectTrigger aria-label="Organization" className="h-7 border-0 bg-transparent px-1 shadow-none hover:bg-accent"><SelectValue placeholder="No organization" /></SelectTrigger><SelectContent>{organizations.map((item) => <SelectItem key={item.organization_id} value={item.organization_id}>{item.name}</SelectItem>)}</SelectContent></Select></div>
          <IconButton label={organization ? "Organization settings" : "Create organization"} className={cn("size-8", mainView === "organization" && "bg-accent")} onClick={openOrganizationSettings}>{organization ? <MoreHorizontal /> : <Plus />}</IconButton>
        </div>
        <div className="grid grid-cols-[20px_minmax(0,1fr)_32px] items-center gap-2 px-3 pb-3 pl-5">
          <FolderKanban className="size-3.5 text-muted-foreground" />
          <div data-tour="workspace-select">
            <Select value={workspaceId ?? undefined} onValueChange={selectWorkspace} disabled={!organizationId}><SelectTrigger aria-label="Workspace" className="h-7 border-0 bg-transparent px-1 text-xs shadow-none hover:bg-accent"><SelectValue placeholder="No workspace" /></SelectTrigger><SelectContent>{workspaces.map((item) => <SelectItem key={item.workspace_id} value={item.workspace_id}>{item.name}</SelectItem>)}</SelectContent></Select>
          </div>
          <span data-tour="create-workspace"><IconButton label={workspace ? "Workspace settings" : "Create workspace"} disabled={!organizationId} className={cn(mainView === "workspace" && "bg-accent")} onClick={openWorkspaceSettings}>{workspace ? <MoreHorizontal /> : <Plus />}</IconButton></span>
        </div>
        <WorkloadTree key={workspaceId} workspaceId={workspaceId} agents={snapshot?.agents ?? []} apps={apps} machines={snapshot?.servers ?? []} selectedId={selectedAgentId} onOpen={showAgentTerminal} onSettings={openAppSettings} onRename={(agent) => openRename({ kind: "agent", id: agent.agent_id, name: agent.name })} onDelete={(agent) => setDeleteTarget({ kind: "agent", id: agent.agent_id, name: agent.name })} onNew={openCreateAgentOrMachineSetup} placementRevision={placementRevision} onTreeChange={setWorkloadTree} preview={preview} />

        <div className="shrink-0 border-t p-2">
          <DropdownMenu>
            <DropdownMenuTrigger asChild><button type="button" aria-label="User menu" className="grid h-11 w-full grid-cols-[28px_minmax(0,1fr)_20px] items-center gap-2 rounded-[5px] px-2 text-left hover:bg-accent"><span className="grid size-7 place-items-center rounded bg-[#e8deee] text-[10px] font-bold text-[#694a73]">{initials(user.preferred_name)}</span><span className="min-w-0"><span className="block truncate text-xs font-medium">{user.preferred_name}</span><span className="block truncate text-[9px] text-muted-foreground">{user.email}</span></span><MoreHorizontal className="size-4 text-muted-foreground" /></button></DropdownMenuTrigger>
            <DropdownMenuContent side="top" align="start" className="w-60"><DropdownMenuLabel><span className="block truncate">{user.preferred_name}</span><span className="mt-0.5 block truncate text-[10px] font-normal text-muted-foreground">{user.email} · {currentRole}</span></DropdownMenuLabel><DropdownMenuSeparator /><DropdownMenuItem onSelect={openSettings}><SettingsIcon />Settings</DropdownMenuItem><DropdownMenuItem onSelect={replayFirstRunTour}><ListChecks />Replay product tour</DropdownMenuItem><DropdownMenuSeparator /><DropdownMenuItem onSelect={logout} disabled={preview}><LogOut />Log out</DropdownMenuItem></DropdownMenuContent>
          </DropdownMenu>
        </div>
      </aside>

      <section className={cn("min-h-0 min-w-0 grid-rows-[48px_minmax(0,1fr)]", mobileTerminalIdle ? "hidden md:grid" : "grid")}>
        <header className="flex min-w-0 items-center justify-between gap-4 border-b px-3 sm:px-5">
          <div className="flex min-w-0 items-center gap-1.5 overflow-hidden text-xs text-muted-foreground">
            {isMobile && mainView !== "terminal" && <IconButton label="Back" className="mr-1 md:hidden" onClick={mainView === "machine" ? closeMachineOverview : mainView === "app" ? openApps : mainView === "audit" ? openOrganizationSettings : closeMainView}><ChevronLeft /></IconButton>}
            <span className="hidden truncate sm:block">{mainView === "organization" || mainView === "audit" ? organization?.name ?? "Organization" : workspace?.name ?? "Workspace"}</span><ChevronRight className="hidden size-3 shrink-0 sm:block" />{!(mainView === "terminal" && selectedAgent) && <strong title={mainView === "terminal" ? selectedAgentPath : undefined} className="truncate font-medium text-foreground">{mainView === "organization" || mainView === "workspace" ? "Settings" : mainView === "apps" ? "Apps" : mainView === "app" ? selectedApp?.name ?? "App" : mainView === "audit" ? "Audit" : selectedAgent?.name ?? "Terminal"}</strong>}{mainView === "terminal" && selectedAgent && <AgentBreadcrumbs path={selectedAgentPath} />}</div>
          {mainView === "terminal" ? <div className="flex shrink-0 items-center gap-0.5">
            <IconButton label={selectedAgentInterface || showAppUi ? "Open full-screen interface" : "Open full-screen terminal"} className="md:hidden" disabled={!selectedAgent} onClick={openMobileTerminal}><Maximize2 /></IconButton>
            {selectedAgentApp && <Button size="sm" variant="ghost" onClick={() => setShowWorkloadLogs((value) => !value)}>{showWorkloadLogs ? "Show App" : "Show logs"}</Button>}
            <IconButton label="Rename agent" disabled={!selectedAgent} onClick={() => selectedAgent && openRename({ kind: "agent", id: selectedAgent.agent_id, name: selectedAgent.name })}><Pencil /></IconButton>
            <IconButton label="Upload file" disabled={!selectedAgent || !selectedAgentMachineOnline} onClick={openUpload}><Upload /></IconButton>
            <IconButton label={selectedAgentInterface || showAppUi ? "Reload interface" : "Reconnect terminal"} disabled={!selectedAgent} onClick={refreshAgentView}><RotateCw /></IconButton>
            <IconButton label="Stop agent" disabled={!selectedAgent || !terminalActive} onClick={() => void stopAgent()}><Square /></IconButton>
            <IconButton label="Delete agent" disabled={!selectedAgent} className="text-destructive hover:text-destructive" onClick={() => selectedAgent && setDeleteTarget({ kind: "agent", id: selectedAgent.agent_id, name: selectedAgent.name })}><Trash2 /></IconButton>
          </div> : mainView === "apps" ? <IconButton label="Refresh Apps" onClick={loadApps} disabled={appsLoading}><RotateCw /></IconButton> : mainView === "audit" ? <IconButton label="Refresh audit" onClick={refreshAudit} disabled={auditLoading}><RotateCw /></IconButton> : null}
        </header>
        {mainView === "organization" ? <OrganizationSettingsView organization={organization} name={organizationName} workspaces={workspaces} selectedWorkspaceId={workspaceId} members={members} groups={groups} groupName={groupName} canManage={canManageMembers} currentRole={currentRole} onNameChange={setOrganizationName} onRename={renameOrganization} onCreateOrganization={() => { setCreateOrganizationName(""); setCreateOrganizationOpen(true) }} onCreateWorkspace={() => { setCreateWorkspaceName(""); setCreateWorkspaceOpen(true) }} onSelectWorkspace={(selectedWorkspaceId) => { selectWorkspace(selectedWorkspaceId); closeMainView() }} onInvite={createInvite} onMemberRoleChange={updateMemberRole} onRemoveMember={removeMember} onGroupNameChange={setGroupName} onCreateGroup={createGroup} onDeleteGroup={deleteGroup} onSetGroupMember={setGroupMember} onOpenAudit={openAudit} onClose={closeMainView} /> : mainView === "terminal" && selectedAgent && !selectedAgentMachineOnline ? <div className={cn("grid min-h-0 place-items-center bg-sidebar px-6 text-center", mobileTerminalOpen && "fixed inset-0 z-[100] bg-sidebar pt-[env(safe-area-inset-top)]")}>
          <div className="max-w-sm">
            {mobileTerminalOpen && <Button variant="ghost" onClick={closeMobileSurface}>Back to workloads</Button>}
            <p className="text-sm font-medium text-zinc-800">Machine is offline</p>
            <p className="mt-2 text-xs leading-5 text-muted-foreground">{machineName(selectedAgentMachine, selectedAgent.server_id)} is not connected to the control plane. It may be stopped, waking from sleep, or fenced as a duplicate.</p>
            {workspaceId && <MachineRecovery workspaceId={workspaceId} onCopy={copy} reason="Copy a recovery command and run it on that machine. restart-controller keeps Agents; start launches a stopped Host." />}
          </div>
        </div> : mainView === "terminal" && showAppUi && !interfaceUiUrl ? <div className={cn("grid min-h-0 place-items-center bg-background px-6 text-center", mobileTerminalOpen && "fixed inset-0 z-[100] bg-background pt-[env(safe-area-inset-top)]")}>
          <div className="max-w-sm space-y-3">
            <p className="text-sm font-medium">App is not running</p>
            <p className="text-xs text-muted-foreground">Start this App from its workload menu to open its interface.</p>
            <Button variant="outline" onClick={() => setShowWorkloadLogs(true)}>Show logs</Button>
            {mobileTerminalOpen && <Button variant="ghost" onClick={closeMobileSurface}>Back to workloads</Button>}
          </div>
        </div> : mainView === "terminal" && interfaceUiUrl ? <div className={cn("min-h-0 min-w-0 overflow-hidden bg-background", mobileTerminalOpen && "fixed inset-0 z-[100] grid h-[100dvh] grid-rows-[44px_minmax(0,1fr)] bg-background pt-[env(safe-area-inset-top)]")}>
          {mobileTerminalOpen && <div className="flex min-w-0 items-center justify-between gap-3 border-b bg-background px-3.5"><span title={selectedAgentPath} className="truncate text-xs font-semibold text-foreground">{selectedAgentPath || "Interface"}</span><div className="flex shrink-0 items-center gap-1"><IconButton label="Upload file" disabled={!selectedAgentMachineOnline} onClick={openUpload}><Upload /></IconButton><button type="button" className="grid size-8 place-items-center rounded-[5px] text-muted-foreground hover:bg-accent hover:text-foreground" aria-label="Close full-screen interface" onClick={closeMobileSurface}><X className="size-4" /></button></div></div>}
          <iframe ref={interfaceFrameRef} key={interfaceFrameKey(selectedAgent?.agent_id ?? "", selectedAgentInterface?.instance_id ?? selectedAgentApp?.hostname ?? "", selectedAgentInterface?.registered_at ?? String(selectedAgent?.run_generation ?? ""), interfaceUiRevision)} src={interfaceUiUrl} title={`${selectedAgentPath || "Agent"} interface`} className="block size-full min-h-0 border-0 bg-background" style={{ colorScheme: theme }} sandbox="allow-scripts allow-forms allow-same-origin allow-modals allow-downloads" onLoad={() => postEmbedTheme(interfaceFrameRef.current, themeRef.current)} />
        </div> : mainView === "terminal" ? mobileTerminalIdle ? null : <div className="flex min-h-0 justify-center overflow-hidden px-3 pb-4 pt-4 sm:px-8 sm:pb-7 sm:pt-6 lg:px-16">
          <div className={cn("grid h-full min-h-0 w-full max-w-[1120px] grid-rows-[42px_minmax(0,1fr)] overflow-hidden rounded-md border border-zinc-800 bg-[#0f1215] shadow-[0_8px_28px_rgba(15,18,21,.14)]", mobileTerminalOpen && "fixed inset-0 z-[100] h-[100dvh] max-w-none grid-rows-[44px_minmax(0,1fr)_auto] rounded-none border-0 shadow-none")}>
            <div className="flex min-w-0 items-center justify-between gap-3 border-b border-zinc-800 bg-[#191d20] px-3.5"><div className="flex min-w-0 items-baseline gap-2"><span title={selectedAgentPath} className="truncate text-xs font-semibold text-zinc-200">{selectedAgentPath || "Terminal"}</span>{selectedAgent && <span className="hidden truncate font-mono text-[9px] text-zinc-500 sm:block">{selectedAgent.agent_id} · {machineName(snapshot?.servers.find((item) => item.server_id === selectedAgent.server_id))}</span>}</div><div className="flex shrink-0 items-center gap-2"><span className="inline-flex items-center gap-1.5 text-[9px] uppercase text-zinc-500"><span className="size-1.5 rounded-full bg-current" />{terminalStatus}</span>{mobileTerminalOpen && <><IconButton label="Upload file" className="text-zinc-400 hover:bg-white/10 hover:text-zinc-100" disabled={!selectedAgentMachineOnline} onClick={openUpload}><Upload /></IconButton><button type="button" className="grid size-8 place-items-center rounded-[5px] text-zinc-400 hover:bg-white/10 hover:text-zinc-100" aria-label="Close full-screen terminal" onClick={closeMobileSurface}><X className="size-4" /></button></>}</div></div>
            <div className="min-h-0 min-w-0 overflow-hidden"><TerminalPane ref={terminalPaneRef} key={`${workspaceId}:${selectedAgentId}`} workspaceId={workspaceId} agentId={selectedAgentId} active={terminalActive} onStatusChange={setTerminalState} transformInput={transformTerminalInput} /></div>
            {mobileTerminalOpen && <div className="border-t border-zinc-800 bg-[#191d20] px-2 pt-2 pb-[max(0.5rem,env(safe-area-inset-bottom))]">
              <div className="grid grid-cols-6 gap-1.5">
                <MobileTerminalKey label="Escape" onClick={() => terminalPaneRef.current?.send("\x1b")}>Esc</MobileTerminalKey>
                <MobileTerminalKey label="Tab" onClick={() => terminalPaneRef.current?.send("\t")}>Tab</MobileTerminalKey>
                <MobileTerminalKey label="Control modifier for next key" active={ctrlArmed} onClick={() => setCtrlModifier(!ctrlArmedRef.current)}>Ctrl</MobileTerminalKey>
                <MobileTerminalKey label="Open keyboard" onClick={() => terminalPaneRef.current?.focus()}><Keyboard className="size-4" /></MobileTerminalKey>
                <MobileTerminalKey label="Backspace" onClick={() => terminalPaneRef.current?.send("\x7f")}><Delete className="size-4" /></MobileTerminalKey>
                <MobileTerminalKey label="Enter" onClick={() => terminalPaneRef.current?.send("\r")}><CornerDownLeft className="size-4" /></MobileTerminalKey>
              </div>
              <div className="mt-1.5 grid grid-cols-8 gap-1.5">
                <MobileTerminalKey label="Control C" onClick={() => terminalPaneRef.current?.send("\x03")}>^C</MobileTerminalKey>
                <MobileTerminalKey label="Control D" onClick={() => terminalPaneRef.current?.send("\x04")}>^D</MobileTerminalKey>
                <MobileTerminalKey label="Control Z" onClick={() => terminalPaneRef.current?.send("\x1a")}>^Z</MobileTerminalKey>
                <MobileTerminalKey label="Control L" onClick={() => terminalPaneRef.current?.send("\x0c")}>^L</MobileTerminalKey>
                <MobileTerminalKey label="Left arrow" onClick={() => terminalPaneRef.current?.send("\x1b[D")}><ArrowLeft className="size-4" /></MobileTerminalKey>
                <MobileTerminalKey label="Up arrow" onClick={() => terminalPaneRef.current?.send("\x1b[A")}><ArrowUp className="size-4" /></MobileTerminalKey>
                <MobileTerminalKey label="Down arrow" onClick={() => terminalPaneRef.current?.send("\x1b[B")}><ArrowDown className="size-4" /></MobileTerminalKey>
                <MobileTerminalKey label="Right arrow" onClick={() => terminalPaneRef.current?.send("\x1b[C")}><ArrowRight className="size-4" /></MobileTerminalKey>
              </div>
            </div>}
          </div>
        </div> : mainView === "workspace" ? <WorkspaceSettingsView workspace={workspace} organization={organization} name={workspaceName} machines={snapshot?.servers ?? []} machineCount={workspaceMachineCount} profiles={launchProfiles} profilesLoading={launchProfilesLoading} apps={apps} onPolicyAppInstalled={(app) => setApps((current) => [...current.filter((item) => item.app_id !== app.app_id), app].sort((left, right) => left.name.localeCompare(right.name)))} canDelete={canManageMembers} preview={preview} onNameChange={setWorkspaceName} onRename={renameWorkspace} onAddMachine={openInstall} onOpenMachine={(machine) => showMachineOverview(machine.server_id)} onRenameMachine={(machine) => openRename({ kind: "machine", id: machine.server_id, name: machineName(machine) })} onDeleteMachine={(machine) => setDeleteTarget({ kind: "machine", id: machine.server_id, name: machineName(machine) })} onCopy={copy} onRefreshProfiles={loadLaunchProfiles} onNewProfile={openNewLaunchProfile} onEditProfile={openEditLaunchProfile} onLaunchProfile={openLaunchProfile} onDeleteProfile={setDeletingProfile} onDelete={() => setDeleteWorkspaceOpen(true)} onClose={closeMainView} /> : mainView === "apps" ? <AppsView apps={apps} machines={snapshot?.servers ?? []} loading={appsLoading} onOpen={openApp} onSettings={openAppSettings} /> : mainView === "app" ? <AppSettingsView app={selectedApp} machine={selectedAppMachine} onOpen={openApp} onAccess={updateAppAccess} onAction={appLifecycle} onDelete={setDeletingApp} onClose={openApps} onCopy={copy} /> : mainView === "machine" ? <MachineOverviewView machine={selectedMachine} agents={snapshot?.agents.filter((agent) => agent.server_id === selectedMachineId) ?? []} services={services.filter((service) => service.server_id === selectedMachineId)} virtualHosts={virtualHosts.filter((host) => host.destination_server_id === selectedMachineId)} traffic={traffic} machines={snapshot?.servers ?? []} workspaceId={workspaceId} onOpenAgent={showAgentTerminal} onClose={closeMachineOverview} onCopy={copy} /> : mainView === "audit" ? <AuditView events={auditEvents} traffic={traffic} agentTraffic={agentTraffic} agents={snapshot?.agents ?? []} machines={snapshot?.servers ?? []} loading={auditLoading} /> : null}
      </section>
    </main>

    {error && <div className="fixed bottom-4 left-1/2 z-[90] flex max-w-[calc(100vw-2rem)] -translate-x-1/2 items-center gap-3 rounded-md border bg-background px-4 py-3 text-sm shadow-lg"><span className="truncate">{error}</span><Button size="sm" variant="ghost" onClick={() => setError(null)}>Dismiss</Button></div>}

    <SettingsDialog open={settingsOpen} onOpenChange={setSettingsOpen} user={user} onUserChange={setUser} onError={showError} />

    <SimpleNameDialog open={createOrganizationOpen} onOpenChange={setCreateOrganizationOpen} title="Create organization" description="Organizations contain members and workspaces." label="Organization name" value={createOrganizationName} onValueChange={setCreateOrganizationName} onSubmit={createOrganization} />
    <SimpleNameDialog open={createWorkspaceOpen} onOpenChange={setCreateWorkspaceOpen} title="Create workspace" description={`Add a workspace to ${organization?.name ?? "this organization"}.`} label="Workspace name" value={createWorkspaceName} onValueChange={setCreateWorkspaceName} onSubmit={createWorkspace} dataTour="create-workspace-dialog" />

    <Dialog open={profileEditorOpen} onOpenChange={(open) => { setProfileEditorOpen(open); if (!open) setEditingLaunchProfile(null) }}><DialogContent className="max-w-xl"><form onSubmit={saveLaunchProfile} className="grid gap-4 sm:grid-cols-2"><DialogHeader className="sm:col-span-2"><DialogTitle>{editingLaunchProfile ? "Edit launch profile" : "New launch profile"}</DialogTitle><DialogDescription>Reusable Agent process settings for this workspace.</DialogDescription></DialogHeader><Field label="Profile name"><Input value={launchProfileName} onChange={(event) => setLaunchProfileName(event.target.value)} required autoFocus maxLength={80} /></Field><Field label="Working directory"><Input className="font-mono" value={launchProfileCwd} onChange={(event) => setLaunchProfileCwd(event.target.value)} required /></Field><div className="sm:col-span-2"><Field label="Description"><Input value={launchProfileDescription} onChange={(event) => setLaunchProfileDescription(event.target.value)} maxLength={1000} /></Field></div><div className="sm:col-span-2"><Field label="Command"><Input className="font-mono" value={launchProfileCommandLine} onChange={(event) => setLaunchProfileCommandLine(event.target.value)} placeholder="codex review --base main" required /></Field></div><DialogFooter className="sm:col-span-2"><Button type="button" variant="outline" onClick={() => setProfileEditorOpen(false)}>Cancel</Button><Button type="submit"><Rocket />{editingLaunchProfile ? "Save profile" : "Create profile"}</Button></DialogFooter></form></DialogContent></Dialog>

    <Dialog open={Boolean(launchingProfile)} onOpenChange={(open) => !open && setLaunchingProfile(null)}><DialogContent><form onSubmit={launchFromProfile} className="space-y-4"><DialogHeader><DialogTitle>Run {launchingProfile?.name}</DialogTitle><DialogDescription>Choose where to start this Agent.</DialogDescription></DialogHeader><Field label="Machine"><Select value={launchMachineId} onValueChange={setLaunchMachineId} required><SelectTrigger><SelectValue placeholder="Select an online machine" /></SelectTrigger><SelectContent>{onlineMachines.map((machine) => <SelectItem key={machine.server_id} value={machine.server_id}>{machineName(machine)}</SelectItem>)}</SelectContent></Select></Field><Field label="Agent name"><Input value={launchAgentName} onChange={(event) => setLaunchAgentName(event.target.value)} required maxLength={80} /></Field><DialogFooter><Button type="button" variant="outline" onClick={() => setLaunchingProfile(null)}>Cancel</Button><Button type="submit" disabled={!launchMachineId}><Play />Run Agent</Button></DialogFooter></form></DialogContent></Dialog>

    <Dialog open={Boolean(deletingProfile)} onOpenChange={(open) => !open && setDeletingProfile(null)}><DialogContent><DialogHeader><DialogTitle>Delete launch profile</DialogTitle><DialogDescription>Delete {deletingProfile?.name}? Existing Agents are not affected.</DialogDescription></DialogHeader><DialogFooter><Button variant="outline" onClick={() => setDeletingProfile(null)}>Cancel</Button><Button variant="destructive" onClick={deleteLaunchProfile}>Delete profile</Button></DialogFooter></DialogContent></Dialog>

    <Dialog open={createAgentOpen} onOpenChange={setCreateAgentOpen}>
      <DialogContent data-tour="create-agent-dialog">
        <form onSubmit={createAgent} className="min-w-0 space-y-4">
          <DialogHeader>
            <DialogTitle>Create agent</DialogTitle>
            <DialogDescription>Start a terminal or agent on an online machine in this workspace.</DialogDescription>
          </DialogHeader>
          {agentFolder && <p className="break-all text-xs text-muted-foreground">Tree folder: {agentFolder.path}</p>}
          <Field label="Machine">
            <Select value={agentServerId} onValueChange={setAgentServerId} required>
              <SelectTrigger aria-label="Machine"><SelectValue placeholder="Select a machine" /></SelectTrigger>
              <SelectContent>{onlineMachines.map((machine) => <SelectItem key={machine.server_id} value={machine.server_id}>{machineName(machine)}</SelectItem>)}</SelectContent>
            </Select>
          </Field>
          <Field label="Launch">
            <Select value={agentProfileId} onValueChange={selectAgentProfile}>
              <SelectTrigger aria-label="Launch" data-tour="agent-launch"><SelectValue /></SelectTrigger>
              <SelectContent>
                <SelectItem value="terminal">Terminal</SelectItem>
                <SelectItem value="manual">Custom command</SelectItem>
                {launchProfiles.map((profile) => <SelectItem key={profile.profile_id} value={profile.profile_id}>{profile.name}</SelectItem>)}
              </SelectContent>
            </Select>
          </Field>
          {agentProfileId === "terminal" || agentProfileId === "manual"
            ? <Field label="Working directory"><Input value={agentCwd} onChange={(event) => setAgentCwd(event.target.value)} /></Field>
            : selectedCreateProfile
              ? <div className="min-w-0 max-w-full overflow-hidden rounded-md border bg-muted/30 px-3 py-2"><code className="block max-w-full truncate text-xs" title={formatCommandLine(selectedCreateProfile.command, selectedCreateProfile.args)}>{formatCommandLine(selectedCreateProfile.command, selectedCreateProfile.args)}</code><span className="mt-1 block max-w-full truncate text-[10px] text-muted-foreground">{selectedCreateProfile.cwd || "."}</span></div>
              : null}
          {agentProfileId === "manual" && <Field label="Command"><Input aria-label="Command" className="font-mono" value={agentCommandLine} onChange={(event) => changeAgentCommandLine(event.target.value)} placeholder="/path/to/your-launcher --option" required /></Field>}
          {selectedCreateProfile && <span className="block text-[10px] text-muted-foreground">Runs the saved command on this machine. Install its runtime and dependencies before launching.</span>}
          <Field label="Name"><Input aria-label="Name" value={agentName} onChange={(event) => { setAgentName(event.target.value); setAgentNameCustomized(true) }} required /></Field>
          {createAgentError && <p role="alert" className="break-words text-sm text-destructive">{createAgentError}</p>}
          <DialogFooter>
            <Button type="button" variant="outline" onClick={() => setCreateAgentOpen(false)}>Cancel</Button>
            <Button type="submit" disabled={creatingAgent || !agentServerId || (agentProfileId === "manual" && !agentCommandLine.trim())}>{creatingAgent ? "Starting…" : agentProfileId === "terminal" ? "Create terminal" : "Create agent"}</Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
    <Dialog open={Boolean(deletingApp)} onOpenChange={(open) => !open && setDeletingApp(null)}><DialogContent><DialogHeader><DialogTitle>Delete App</DialogTitle><DialogDescription>Delete {deletingApp?.name}, stop its process, and remove its service and virtual hostname?</DialogDescription></DialogHeader><DialogFooter><Button variant="outline" onClick={() => setDeletingApp(null)}>Cancel</Button><Button variant="destructive" onClick={deleteAppDeployment}>Delete App</Button></DialogFooter></DialogContent></Dialog>


    <Dialog open={installOpen} onOpenChange={setInstallOpen}>
      <DialogContent className="max-w-xl" data-tour="add-machine-dialog">
        <DialogHeader><DialogTitle>Add machine</DialogTitle><DialogDescription>Install Treer, then connect this workspace.</DialogDescription></DialogHeader>
        <div className="space-y-4">
          <Field label="Operating system"><Select value={installPlatform} onValueChange={setInstallPlatform}><SelectTrigger aria-label="Installation operating system"><SelectValue /></SelectTrigger><SelectContent><SelectItem value="unix">Linux / macOS</SelectItem><SelectItem value="windows">Windows · PowerShell</SelectItem></SelectContent></Select></Field>
          <Field label="1. Install Treer"><div className="space-y-2"><Textarea readOnly value={installCommand} className="min-h-20 font-mono text-xs" /><Button size="sm" variant="outline" onClick={() => copy(installCommand)}><Copy />Copy install command</Button></div></Field>
          <Field label="2. Connect workspace"><div className="space-y-2"><Textarea readOnly value={connectCommand} className="min-h-24 font-mono text-xs" /><Button size="sm" onClick={() => copy(connectCommand)}><Copy />Copy connection command</Button></div></Field>
        </div>
        <DialogFooter><Button variant="outline" onClick={() => setInstallOpen(false)}>Close</Button></DialogFooter>
      </DialogContent>
    </Dialog>

    <Dialog open={Boolean(renameTarget)} onOpenChange={(open) => !open && setRenameTarget(null)}><DialogContent><form onSubmit={submitRename}><DialogHeader><DialogTitle>Rename {renameTarget?.kind}</DialogTitle><DialogDescription>Rename {renameTarget?.kind === "agent" ? agentPath(snapshot?.agents.find((agent) => agent.agent_id === renameTarget.id) ?? { agent_id: renameTarget.id, name: renameTarget.name, server_id: "", kind: "", status: "" }, workloadTree) : renameTarget?.name}. Names must be unique within the same folder.</DialogDescription></DialogHeader><div className="my-5"><Field label="Name"><Input value={renameName} onChange={(event) => setRenameName(event.target.value)} required autoFocus /></Field></div>{renameError && <p role="alert" className="mb-4 break-words text-sm text-destructive">{renameError}</p>}<DialogFooter><Button type="button" variant="outline" onClick={() => setRenameTarget(null)}>Cancel</Button><Button type="submit">Rename</Button></DialogFooter></form></DialogContent></Dialog>

    <Dialog open={uploadOpen} onOpenChange={(open) => !uploading && setUploadOpen(open)}><DialogContent><form onSubmit={uploadToAgentMachine} className="space-y-4"><DialogHeader><DialogTitle>Upload file</DialogTitle><DialogDescription>Upload to {machineName(selectedAgentMachine, selectedAgent?.server_id)}. The destination directory is relative to the Machine root.</DialogDescription></DialogHeader><Field label="Destination directory"><Input aria-label="Destination directory" className="font-mono" value={uploadDirectory} onChange={(event) => setUploadDirectory(event.target.value)} placeholder="." required disabled={uploading} /></Field><Field label="File"><Input key={uploadInputKey} aria-label="File" type="file" onChange={(event) => setUploadFile(event.target.files?.[0] ?? null)} disabled={uploading} /></Field><label className="flex items-center gap-2 text-xs text-muted-foreground"><input type="checkbox" className="size-4 accent-foreground" checked={overwriteUpload} onChange={(event) => setOverwriteUpload(event.target.checked)} disabled={uploading} />Replace an existing file with the same name</label><p className="text-[10px] text-muted-foreground">Maximum 16 MiB</p>{uploadMessage && <p className="text-xs text-emerald-700">{uploadMessage}</p>}{uploadError && <p className="text-xs text-red-600">{uploadError}</p>}<DialogFooter><Button type="button" variant="outline" onClick={() => setUploadOpen(false)} disabled={uploading}>Close</Button><Button type="submit" disabled={!uploadFile || uploading || !selectedAgentMachineOnline}><Upload />{uploading ? "Uploading" : "Upload"}</Button></DialogFooter></form></DialogContent></Dialog>

    <Dialog open={Boolean(deleteTarget)} onOpenChange={(open) => !open && setDeleteTarget(null)}><DialogContent><DialogHeader><DialogTitle>Delete {deleteTarget?.kind}</DialogTitle><DialogDescription>{deleteTarget?.kind === "machine" ? `Remove ${deleteTarget.name} and all of its agents? Its credential will be revoked, but its local service will not be uninstalled.` : `Delete ${deleteTarget ? agentPath(snapshot?.agents.find((agent) => agent.agent_id === deleteTarget.id) ?? { agent_id: deleteTarget.id, name: deleteTarget.name, server_id: "", kind: "", status: "" }, workloadTree) : "Agent"} and stop its process? This agent will not return after reconnecting.`}</DialogDescription></DialogHeader><DialogFooter><Button variant="outline" onClick={() => setDeleteTarget(null)}>Cancel</Button><Button variant="destructive" onClick={confirmDelete}>Delete</Button></DialogFooter></DialogContent></Dialog>
    <Dialog open={deleteWorkspaceOpen} onOpenChange={(open) => !open && setDeleteWorkspaceOpen(false)}><DialogContent><DialogHeader><DialogTitle>Delete workspace</DialogTitle><DialogDescription>{workspaceMachineCount === undefined ? "Checking the workspace machine inventory..." : workspaceMachineCount > 0 ? `Delete all ${workspaceMachineCount} ${workspaceMachineCount === 1 ? "machine" : "machines"} from ${workspace?.name ?? "this workspace"} first.` : `Delete ${workspace?.name}? It will disappear from active views while historical traffic and messages are retained. This cannot be undone.`}</DialogDescription></DialogHeader><DialogFooter><Button variant="outline" onClick={() => setDeleteWorkspaceOpen(false)}>Cancel</Button><Button variant="destructive" disabled={workspaceMachineCount !== 0} onClick={() => void confirmDeleteWorkspace()}>Delete workspace</Button></DialogFooter></DialogContent></Dialog>

    <Dialog open={inviteOpen} onOpenChange={setInviteOpen}><DialogContent><DialogHeader><DialogTitle>Invite member</DialogTitle><DialogDescription>This registration link can be used once.</DialogDescription></DialogHeader><Textarea readOnly value={inviteUrl} className="min-h-24 font-mono text-xs" /><DialogFooter><Button variant="outline" onClick={() => setInviteOpen(false)}>Close</Button><Button onClick={() => copy(inviteUrl)}><Copy />Copy link</Button></DialogFooter></DialogContent></Dialog>
  </TooltipProvider>
}
