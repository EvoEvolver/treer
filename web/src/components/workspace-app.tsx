import { WorkloadSurface } from "@/components/workload-surface"
import { WorkspaceTabBar, type TabLabel } from "@/components/workspace-tab-bar"
import { OVERVIEW_TAB, useWorkspaceTabs } from "@/components/use-workspace-tabs"
import { workspaceWorkloads } from "@/lib/workspace-workloads"
import { UserMenu } from "@/components/user-menu"
import { WorkspaceOverview } from "@/components/workspace-overview"
import { LayoutDashboard, PanelLeft } from "lucide-react"
import { AgentBreadcrumbs } from "@/components/agent-breadcrumbs"
import { AppSettingsView,AppsView } from "@/components/apps-view"
import { AuditView } from "@/components/audit-view"
import { AuthScreen } from "@/components/auth-screen"
import { IconButton } from "@/components/control-plane-ui"
import { ErrorToast } from "@/components/error-toast"
import { MachineOverviewView } from "@/components/machine-view"
import { OrganizationSettingsView } from "@/components/organization-settings-view"
import { type TerminalPaneHandle } from "@/components/terminal-pane"
import { TreeResizer,useTreeWidth } from "@/components/tree-resizer"
import { Button } from "@/components/ui/button"
import { Select,SelectContent,SelectItem,SelectTrigger,SelectValue } from "@/components/ui/select"
import { TooltipProvider } from "@/components/ui/tooltip"
import { useOrganizationMembers } from "@/components/use-organization-members"
import { useWorkspaceLiveFeed } from "@/components/use-workspace-live-feed"
import { useWorkspaceMetrics } from "@/components/use-workspace-metrics"
import { useWorkspaceRoute } from "@/components/use-workspace-route"
import { WorkloadTree,type NewAgentFolder } from "@/components/workload-tree"
import { useWorkspaceDialogState } from "@/components/workspace-dialog-state"
import { WorkspaceDialogs } from "@/components/workspace-dialogs"
import { WorkspaceSettingsView } from "@/components/workspace-settings-view"
import { ApiError,api,machineName,proxyUrl,type Agent,type AgentLaunchProfile,type AppDeployment,type MachineFileUpload,type Organization,type Snapshot,type User,type WorkloadTreeState,type Workspace } from "@/lib/api"
import { formatCommandLine,parseCommandLine } from "@/lib/command-line"
import { MainView,PREVIEW_CONNECT,PREVIEW_INSTALL,PREVIEW_ORG,PREVIEW_PROFILES,PREVIEW_USER,PREVIEW_WORKSPACE,RenameTarget,activeStatuses,arrayBufferToBase64,authorizationReturnUrl,controlCharacter,defaultAgentName,defaultProfileAgentName,formatBytes,initials,machineOnline,replaceWorkspace } from "@/lib/control-plane"
import { clearFirstRunTour,firstRunTourMode,shouldAutoStartFirstRunTour,startFirstRunTour,stopFirstRunTour,type FirstRunTourHost } from "@/lib/first-run-tour"
import { readTheme,subscribeTheme,type Theme } from "@/lib/theme"
import { cn } from "@/lib/utils"
import { agentPath } from "@/lib/workload-path"
import {
ChevronLeft,
ChevronRight,
FolderKanban,
Maximize2,
MoreHorizontal,
Pencil,
Plus,
RotateCw,
Square,
Trash2,
Upload,
X
} from "lucide-react"
import { FormEvent,useCallback,useEffect,useMemo,useRef,useState } from "react"
import { useLocation,useParams } from "react-router"
import { roleLabel, t } from "@/i18n"

export function WorkspaceApp() {
  const route = useParams<{ organizationId?: string; workspaceId?: string }>()
  const location = useLocation()
  // A trusted settings deep link is a new scope, including SPA back/forward.
  // Unmount the old workspace and any confirmation before loading its target.
  const scope = location.hash === "#policy-provider" ? `policy:${route.organizationId ?? ""}/${route.workspaceId ?? ""}` : "workspace"
  return <WorkspaceAppContent key={scope} />
}

function WorkspaceAppContent() {
  const { route, location, routeSelectionRef, routingRef, replaceWorkspaceRoute } = useWorkspaceRoute()
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
  const [selectedMachineId, setSelectedMachineId] = useState<string | null>(null)
  const [selectedAppId, setSelectedAppId] = useState<string | null>(null)
  const [tabNotice, setTabNotice] = useState("")
  const [theme, setTheme] = useState<Theme>(() => readTheme())
  const [mainView, setMainView] = useState<MainView>("overview")
  const [mobileTerminalOpen, setMobileTerminalOpen] = useState(false)
  const [mobileTreeOpen, setMobileTreeOpen] = useState(preview)
  const [isMobile, setIsMobile] = useState(() => window.matchMedia("(max-width: 767px)").matches)
  const [ctrlArmed, setCtrlArmed] = useState(false)
  const terminalPaneRef = useRef<TerminalPaneHandle>(null)
  const treeWidth = useTreeWidth(user?.user_id, () => requestAnimationFrame(() => terminalPaneRef.current?.fit()))
  const ctrlArmedRef = useRef(false)
  const [error, setError] = useState<string | null>(null)
  const dialogState = useWorkspaceDialogState()
  const {
    setSettingsOpen, setCreateOrganizationOpen, setCreateWorkspaceOpen, setDeleteWorkspaceOpen,
    createAgentOpen, setCreateAgentOpen, agentFolder, setAgentFolder, setRenameError,
    setCreateAgentError, creatingAgent, setCreatingAgent, deletingApp, setDeletingApp,
    setProfileEditorOpen, editingLaunchProfile, setEditingLaunchProfile, launchingProfile,
    setLaunchingProfile, deletingProfile, setDeletingProfile, setInstallOpen, setInviteOpen,
    setUploadOpen, uploadDirectory, setUploadDirectory, uploadFile, setUploadFile, overwriteUpload,
    setOverwriteUpload, setUploading, setUploadInputKey, setUploadMessage, setUploadError, renameTarget,
    setRenameTarget, deleteTarget, setDeleteTarget, createOrganizationName, setCreateOrganizationName,
    createWorkspaceName, setCreateWorkspaceName, agentName, setAgentName, agentNameCustomized,
    setAgentNameCustomized, agentProfileId, setAgentProfileId, agentServerId, setAgentServerId, agentCwd,
    agentCommandLine, setAgentCommandLine, launchProfileName, setLaunchProfileName,
    launchProfileDescription, setLaunchProfileDescription, launchProfileCwd, setLaunchProfileCwd,
    launchProfileCommandLine, setLaunchProfileCommandLine, launchMachineId, setLaunchMachineId,
    launchAgentName, setLaunchAgentName, renameName, setRenameName, setInstallCommand, setConnectCommand,
    setWindowsInstallCommand, setWindowsConnectCommand, setInviteUrl,
  } = dialogState
  const [placementRevision, setPlacementRevision] = useState(0)
  const [workloadTree, setWorkloadTree] = useState<WorkloadTreeState>({ folders: [], placements: [] })
  const [organizationName, setOrganizationName] = useState("")
  const [workspaceName, setWorkspaceName] = useState("")
  const [launchProfiles, setLaunchProfiles] = useState<AgentLaunchProfile[]>(preview ? PREVIEW_PROFILES : [])
  const createAgentOpenRef = useRef(false)
  const [launchProfilesLoading, setLaunchProfilesLoading] = useState(false)

  const showError = useCallback((reason: unknown) => setError(reason instanceof Error ? reason.message : t("app.something_went_wrong")), [])

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
    if (routingRef.current.hash === "#policy-provider" && routeSelection.organizationId && !data.organizations.some((item) => item.organization_id === routeSelection.organizationId)) {
      throw new Error(t("app.policy_settings_target_is_unavailable"))
    }
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
    if (routingRef.current.hash === "#policy-provider" && routeSelection.workspaceId && !data.workspaces.some((item) => item.workspace_id === routeSelection.workspaceId)) {
      throw new Error(t("app.policy_settings_target_is_unavailable"))
    }
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

  const { apps, setApps, appsLoading, loadApps, refreshSnapshot } = useWorkspaceLiveFeed({
    preview, workspaceId, workspaceIdRef, snapshot, setSnapshot, setWorkspaces,
    setSelectedAgentId, setSelectedMachineId, syncWorkspaces, showError,
  })

  const workloads = useMemo(() => workspaceWorkloads(snapshot?.agents ?? [], apps), [snapshot?.agents, apps])
  const tabs = useWorkspaceTabs(user?.user_id, workspaceId, isMobile, workloads.map((agent) => agent.agent_id), Boolean(snapshot) && !appsLoading, preview)
  const tabLabels = new Map<string, TabLabel>(workloads.map((agent) => {
    const app = apps.find((entry) => (entry.runtime_agent_id ?? entry.app_id) === agent.agent_id)
    const machine = snapshot?.servers.find((entry) => entry.server_id === agent.server_id)
    const tab = tabs.tabs.find((entry) => entry.id === agent.agent_id)
    return [agent.agent_id, { agent: app ? { ...agent, name: app.name } : agent, path: agentPath(agent, workloadTree), machine: machineName(machine, agent.server_id), ui: Boolean((app || agent.interface?.ui_path) && !(tab?.logs && tab.appId === (app?.app_id ?? null))) }]
  }))

  useEffect(() => {
    if (!selectedAgentId) return
    const exists = snapshot?.agents.some((agent) => agent.agent_id === selectedAgentId)
      || apps.some((app) => (app.runtime_agent_id ?? app.app_id) === selectedAgentId)
    if (exists) return
    setSelectedAgentId(null)
    setMainView((current) => current === "terminal" ? "overview" : current)
  }, [snapshot, apps, selectedAgentId])

  useEffect(() => {
    setSelectedAgentId(null)
    setMobileTerminalOpen(false)
    setMobileTreeOpen((current) => preview || current)
    setMainView((current) => current === "terminal" || current === "app" || current === "machine" || current === "audit" ? "overview" : current)
  }, [workspaceId, preview])

  const selectedAgentApp = apps.find((app) => (app.runtime_agent_id ?? app.app_id) === selectedAgentId)
  const selectedTab = tabs.tabs.find((tab) => tab.id === selectedAgentId)
  const showWorkloadLogs = Boolean(selectedTab?.logs && selectedTab.appId === (selectedAgentApp?.app_id ?? null))
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
  const currentRole = organization?.role ?? "member"
  const canManageMembers = ["owner", "admin"].includes(currentRole)

  useEffect(() => {
    if (mainView === "organization") setOrganizationName(organization?.name ?? "")
    if (mainView === "workspace") setWorkspaceName(workspace?.name ?? "")
  }, [mainView, organization?.organization_id, organization?.name, workspace?.workspace_id, workspace?.name])
  const mobileTerminalIdle = isMobile && ((mainView === "terminal" && !mobileTerminalOpen) || (mainView === "overview" && mobileTreeOpen))
  const mobileSidebarHidden = isMobile && ((mainView === "terminal" && mobileTerminalOpen) || (mainView !== "terminal" && !(mainView === "overview" && mobileTreeOpen)))
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

  function showOverview() {
    setMainView("overview")
    setSelectedAgentId(null)
    setMobileTerminalOpen(false)
    setMobileTreeOpen(false)
    setCtrlModifier(false)
  }

  function closeMobileSurface() {
    setMobileTerminalOpen(false)
    setCtrlModifier(false)
  }

  function refreshAgentView() {
    if (selectedAgentId) tabs.reload(selectedAgentId)
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
    // A viewport change keeps the selected page visible. Closing it on a phone
    // still returns to the Tree; only a breakpoint change triggers this effect.
    if (isMobile && mainView === "terminal" && selectedAgentId) setMobileTerminalOpen(true)
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [isMobile])


  useEffect(() => {
    if (isMobile || !mobileTerminalOpen) return
    setMobileTerminalOpen(false)
    setCtrlModifier(false)
  }, [isMobile, mobileTerminalOpen])

  function showAgentTerminal(agentId: string, logs?: boolean) {
    const previous = tabs.tabs.find((tab) => tab.id === agentId)
    if (previous && !previous.live) setTabNotice(t("app.reloaded", { path: tabLabels.get(agentId)?.path ?? agentId }))
    else setTabNotice("")
    tabs.open(agentId, logs, apps.find((app) => (app.runtime_agent_id ?? app.app_id) === agentId)?.app_id ?? null)
    setCtrlModifier(false)
    setMobileTreeOpen(false)
    setSelectedAgentId(agentId)
    setMainView("terminal")
    if (isMobile) setMobileTerminalOpen(true)
  }

  function setShowWorkloadLogs(logs: boolean, agentId = selectedAgentId) {
    if (agentId) tabs.setLogs(agentId, logs, apps.find((app) => (app.runtime_agent_id ?? app.app_id) === agentId)?.app_id ?? null)
  }

  function selectTab(id: string) {
    if (id === OVERVIEW_TAB) showOverview()
    else showAgentTerminal(id)
  }

  function closeTab(id: string) {
    const index = tabs.tabs.findIndex((tab) => tab.id === id)
    const remaining = tabs.tabs.filter((tab) => tab.id !== id)
    tabs.close(id)
    if (id === selectedAgentId) {
      const next = remaining[Math.min(index, remaining.length - 1)]
      if (next) showAgentTerminal(next.id)
      else showOverview()
    }
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
      if (!agentId) throw new Error(t("app.create_agent_did_not_return"))
      // Name and final placement are reserved atomically by the Proxy.
      if (workspaceIdRef.current === creationWorkspaceId) {
        setCreateAgentOpen(false)
        setAgentFolder(null)
      }
      if (folder && workspaceIdRef.current === creationWorkspaceId) setPlacementRevision((revision) => revision + 1)
      if (workspaceIdRef.current !== creationWorkspaceId) return
      setCreateAgentOpen(false)
      await refreshSnapshot()
      if (workspaceIdRef.current === creationWorkspaceId) showAgentTerminal(agentId)
    } catch (reason) { if (workspaceIdRef.current === creationWorkspaceId) setCreateAgentError(reason instanceof Error ? reason.message : t("app.create_failed")) }
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

  useEffect(() => {
    setApps([])
    loadApps().catch(showError)
  }, [loadApps, showError, setApps])

  useEffect(() => {
    if (mainView === "app" && selectedAppId && !appsLoading && !selectedApp) setMainView("overview")
  }, [mainView, selectedAppId, selectedApp, appsLoading])

  function openApps() {
    showOverview()
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
      showOverview()
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

  useEffect(() => {
    if (location.hash !== "#policy-provider" || !workspace || route.workspaceId !== workspace.workspace_id || route.organizationId !== organizationId) return
    setWorkspaceName(workspace.name)
    setMainView("workspace")
    setMobileTerminalOpen(false)
  }, [location.hash, workspace?.workspace_id, route.workspaceId, route.organizationId, organizationId])

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
    showOverview()
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
      setWindowsInstallCommand(data.windows_install_command ?? t("app.update_the_proxy_to_obtain"))
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
    setMobileTreeOpen(true)
    clearFirstRunTour(user.user_id)
    if (tourHostRef.current) startFirstRunTour(tourHostRef.current, { userId: user.user_id, persist: !preview })
  }

  useEffect(() => {
    if (!user) return
    if (!shouldAutoStartFirstRunTour(user.user_id)) return
    setMobileTreeOpen(true)
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
      setUploadError(t("app.files_must_be_16_mib"))
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
      setUploadMessage(t("app.uploaded", { path: result.path, size: formatBytes(result.bytes_written) }))
      setUploadFile(null)
      setUploadInputKey((value) => value + 1)
    } catch (reason) {
      setUploadError(reason instanceof Error ? reason.message : t("app.upload_failed"))
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
    } catch (reason) { setRenameError(reason instanceof Error ? reason.message : t("app.rename_failed")) }
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

  const { members, groups, groupName, setGroupName, createGroup, deleteGroup, setGroupMember, updateMemberRole, removeMember } = useOrganizationMembers({
    preview, organizationId, organizationIdRef, mainView, showError,
  })

  async function createInvite() {
    if (!organizationId) return
    try {
      const invitation = await api<{ url: string }>(`/api/organizations/${encodeURIComponent(organizationId)}/invitations`, { method: "POST", body: "{}" })
      setInviteUrl(invitation.url); setInviteOpen(true)
    } catch (reason) { showError(reason) }
  }

  const { auditEvents, traffic, auditRefreshRevision, auditLoading, virtualHosts, services, refreshAudit } = useWorkspaceMetrics({
    workspaceId, workspaceIdRef, organizationId, canManageMembers, mainView, showError,
  })

  function showMachineOverview(serverId: string) {
    setSelectedMachineId(serverId)
    setMainView("machine")
  }

  function openAudit() {
    setMainView("audit")
  }

  function openAppExternal(app: AppDeployment) {
    const url = app.public_url ?? (workspaceId ? proxyUrl(`/api/workspaces/${encodeURIComponent(workspaceId)}/virtual-hosts/${encodeURIComponent(app.hostname)}/proxy/`) : null)
    if (url) window.open(url, "_blank", "noopener,noreferrer")
  }

  function openApp(app: AppDeployment) {
    showAgentTerminal(app.runtime_agent_id ?? app.app_id)
  }

  async function copy(value: string) {
    try { await navigator.clipboard.writeText(value) }
    catch { showError(t("app.unable_to_copy_to_the")) }
  }

  async function logout() {
    if (preview) return
    try { await api("/api/auth/logout", { method: "POST", body: "{}" }) }
    finally { window.location.href = "/" }
  }

  if (user === undefined) return <div className="grid min-h-dvh place-items-center bg-sidebar text-sm text-muted-foreground">{t("app.loading_treer")}</div>
  if (!user) return <AuthScreen onAuthenticated={setUser} />

  return <TooltipProvider delayDuration={350}>
      <main style={treeWidth.style} className={cn("grid h-dvh min-h-0 bg-background md:grid-cols-[var(--tree-width)_var(--tree-divider)_minmax(0,1fr)] md:grid-rows-1 md:overflow-hidden", treeWidth.dragging && "select-none cursor-col-resize", mobileTerminalIdle || mobileSidebarHidden ? "grid-rows-1 overflow-hidden" : "grid-rows-[374px_minmax(620px,1fr)] overflow-auto")}>
        <aside id="tree-sidebar" className={cn("flex min-h-0 min-w-0 flex-col border-b bg-sidebar md:border-b-0", mobileSidebarHidden && "hidden md:flex", treeWidth.collapsed && "md:invisible md:overflow-hidden")}>
        {preview && <div className="treer-tour-banner"><span>{t("app.tour_preview_empty_first_run")}</span></div>}
        <div className="grid min-h-[58px] grid-cols-[32px_minmax(0,1fr)_32px] items-center gap-2 px-3 py-2">
          <div className="grid size-8 place-items-center rounded-[5px] bg-[#e8deee] text-[10px] font-bold text-[#694a73]">{initials(organization?.name ?? "Treer")}</div>
          <div className="min-w-0"><div className="mb-0.5 px-1 text-[9px] font-semibold uppercase text-muted-foreground">{t("app.organization")}</div><Select value={organizationId ?? undefined} onValueChange={selectOrganization}><SelectTrigger aria-label={t("app.organization")} className="h-7 border-0 bg-transparent px-1 shadow-none hover:bg-accent"><SelectValue placeholder={t("app.no_organization")} /></SelectTrigger><SelectContent>{organizations.map((item) => <SelectItem key={item.organization_id} value={item.organization_id}>{item.name}</SelectItem>)}</SelectContent></Select></div>
          <IconButton label={organization ? t("app.organization_settings") : t("dialogs.create_organization")} className={cn("size-8", mainView === "organization" && "bg-accent")} onClick={openOrganizationSettings}>{organization ? <MoreHorizontal /> : <Plus />}</IconButton>
        </div>
        <div className="grid grid-cols-[20px_minmax(0,1fr)_32px] items-center gap-2 px-3 pb-3 pl-5">
          <FolderKanban className="size-3.5 text-muted-foreground" />
          <div data-tour="workspace-select">
            <Select value={workspaceId ?? undefined} onValueChange={selectWorkspace} disabled={!organizationId}><SelectTrigger aria-label={t("app.workspace")} className="h-7 border-0 bg-transparent px-1 text-xs shadow-none hover:bg-accent"><SelectValue placeholder={t("app.no_workspace")} /></SelectTrigger><SelectContent>{workspaces.map((item) => <SelectItem key={item.workspace_id} value={item.workspace_id}>{item.name}</SelectItem>)}</SelectContent></Select>
          </div>
          <span data-tour="create-workspace"><IconButton label={workspace ? t("app.workspace_settings") : t("org.create_workspace")} disabled={!organizationId} className={cn(mainView === "workspace" && "bg-accent")} onClick={openWorkspaceSettings}>{workspace ? <MoreHorizontal /> : <Plus />}</IconButton></span>
        </div>
        <button type="button" aria-label={t("app.workspace_overview")} onClick={showOverview} className={cn("mx-3 mb-2 flex min-h-10 items-center gap-2 rounded-md px-2 text-left text-xs outline-none hover:bg-accent focus-visible:ring-2 focus-visible:ring-ring", mainView === "overview" && "bg-accent")}><LayoutDashboard aria-hidden="true" className="size-3.5" />{t("app.overview")}</button>
        <WorkloadTree key={workspaceId} workspaceId={workspaceId} agents={snapshot?.agents ?? []} apps={apps} machines={snapshot?.servers ?? []} selectedId={selectedAgentId} onOpen={showAgentTerminal} onSettings={openAppSettings} onRename={(agent) => openRename({ kind: "agent", id: agent.agent_id, name: agent.name })} onDelete={(agent) => setDeleteTarget({ kind: "agent", id: agent.agent_id, name: agent.name })} onNew={openCreateAgentOrMachineSetup} placementRevision={placementRevision} onTreeChange={setWorkloadTree} preview={preview} />

        <UserMenu user={user} badge={initials(user.preferred_name)} role={roleLabel(currentRole)} preview={preview} onSettings={openSettings} onTour={replayFirstRunTour} onLogout={logout} />
      </aside>
      <TreeResizer settings={treeWidth} />

      <section className={cn("min-h-0 min-w-0 flex-col", treeWidth.dragging && "pointer-events-none", mobileTerminalIdle ? "hidden md:flex" : "flex")}>
        <WorkspaceTabBar key={`${user.user_id}:${workspaceId}`} tabs={tabs.tabs} labels={tabLabels} activeId={mainView === "overview" ? OVERVIEW_TAB : mainView === "terminal" ? selectedAgentId : null} limit={tabs.limit} liveCount={tabs.liveCount} mobile={isMobile} onOpen={selectTab} onClose={closeTab} />
        <p role="status" className="sr-only">{tabNotice}</p>
        <header className={cn("flex h-12 shrink-0 min-w-0 items-center justify-between gap-4 border-b px-3 sm:px-5", isMobile && mainView === "terminal" && "hidden")}>

          <div className="flex min-w-0 items-center gap-1.5 overflow-hidden text-xs text-muted-foreground">
            {isMobile && mainView !== "terminal" && mainView !== "overview" && <IconButton label={t("org.back")} className="mr-1 md:hidden" onClick={mainView === "machine" ? closeMachineOverview : mainView === "app" ? openApps : mainView === "audit" ? openOrganizationSettings : closeMainView}><ChevronLeft /></IconButton>}
            <button type="button" onClick={showOverview} aria-label={t("app.back_to_workspace_overview")} className="hidden truncate rounded-sm outline-none hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring sm:block">{mainView === "organization" || mainView === "audit" ? organization?.name ?? t("app.organization") : workspace?.name ?? t("app.workspace")}</button><ChevronRight className="hidden size-3 shrink-0 sm:block" />{!(mainView === "terminal" && selectedAgent) && <strong title={mainView === "terminal" ? selectedAgentPath : undefined} className="truncate font-medium text-foreground">{mainView === "overview" ? t("app.overview") : mainView === "organization" || mainView === "workspace" ? t("settings.title") : mainView === "apps" ? t("apps.apps") : mainView === "app" ? selectedApp?.name ?? t("apps.app") : mainView === "audit" ? t("audit.audit") : selectedAgent?.name ?? t("machine.terminal")}</strong>}{mainView === "terminal" && selectedAgent && <AgentBreadcrumbs path={selectedAgentPath} />}</div>
          {mainView === "overview" ? <Button variant="ghost" size="sm" aria-label={t("app.browse_tree")} className="md:hidden" onClick={() => setMobileTreeOpen(true)}><PanelLeft />{t("app.browse_tree")}</Button> : mainView === "terminal" ? <div className="flex shrink-0 items-center gap-0.5">
            <IconButton label={selectedAgentInterface || showAppUi ? t("app.open_full_screen_interface") : t("app.open_full_screen_terminal")} className="md:hidden" disabled={!selectedAgent} onClick={openMobileTerminal}><Maximize2 /></IconButton>
            {selectedAgentApp && <Button size="sm" variant="ghost" onClick={() => setShowWorkloadLogs(!showWorkloadLogs)}>{showWorkloadLogs ? t("surface.show_app") : t("surface.show_logs")}</Button>}
            <IconButton label={t("app.rename_agent")} disabled={!selectedAgent} onClick={() => selectedAgent && openRename({ kind: "agent", id: selectedAgent.agent_id, name: selectedAgent.name })}><Pencil /></IconButton>
            <IconButton label={t("surface.upload_file")} disabled={!selectedAgent || !selectedAgentMachineOnline} onClick={openUpload}><Upload /></IconButton>
            <IconButton label={selectedAgentInterface || showAppUi ? t("app.reload_interface") : t("app.reconnect_terminal")} disabled={!selectedAgent} onClick={refreshAgentView}><RotateCw /></IconButton>
            <IconButton label={t("app.stop_agent")} disabled={!selectedAgent || !terminalActive} onClick={() => void stopAgent()}><Square /></IconButton>
            <IconButton label={t("app.delete_agent")} disabled={!selectedAgent} className="text-destructive hover:text-destructive" onClick={() => selectedAgent && setDeleteTarget({ kind: "agent", id: selectedAgent.agent_id, name: selectedAgent.name })}><Trash2 /></IconButton>
          </div> : mainView === "apps" ? <IconButton label={t("app.refresh_apps")} onClick={loadApps} disabled={appsLoading}><RotateCw /></IconButton> : mainView === "audit" ? <IconButton label={t("app.refresh_audit")} onClick={refreshAudit} disabled={auditLoading}><RotateCw /></IconButton> : null}
        </header>
        <div className={cn("relative min-h-0 flex-1", mainView !== "overview" && mainView !== "terminal" && "hidden")}>
          <div id="workspace-panel-overview" role="tabpanel" aria-labelledby={isMobile ? undefined : "workspace-tab-overview"} aria-label={isMobile ? t("app.overview") : undefined} hidden={mainView !== "overview"} className="absolute inset-0 overflow-auto">
            <WorkspaceOverview key={workspaceId} workspace={workspace} agents={snapshot?.agents ?? []} apps={apps} machines={snapshot?.servers ?? []} tree={workloadTree} loading={Boolean(workspaceId && !snapshot)} onOpen={showAgentTerminal} onNewAgent={openCreateAgentOrMachineSetup} onWorkspaceSettings={openWorkspaceSettings} />
          </div>
          {workspaceId && tabs.tabs.map((tab) => {
            const label = tabLabels.get(tab.id)
            if (!label) return null
            const visible = mainView === "terminal" && selectedAgentId === tab.id && !mobileTerminalIdle
            const app = apps.find((entry) => (entry.runtime_agent_id ?? entry.app_id) === tab.id)
            const machine = snapshot?.servers.find((entry) => entry.server_id === label.agent.server_id)
            return <div key={`${workspaceId}:${tab.id}`} id={`workspace-panel-${tab.id}`} role="tabpanel" aria-labelledby={isMobile ? undefined : `workspace-tab-${tab.id}`} aria-label={isMobile ? label.path : undefined} hidden={!visible} className="absolute inset-0 min-h-0 overflow-hidden">
              {tab.live ? <WorkloadSurface ref={visible ? terminalPaneRef : undefined} workspaceId={workspaceId} agent={label.agent} app={app} machine={machine} path={label.path} tab={tab} visible={visible} mobile={isMobile && mobileTerminalOpen} theme={theme} ctrlArmed={ctrlArmed} transformInput={transformTerminalInput} onToggleCtrl={() => setCtrlModifier(!ctrlArmedRef.current)} onClose={closeMobileSurface} onUpload={openUpload} onCopy={copy} onLogs={(logs) => setShowWorkloadLogs(logs)} /> : <div className="grid size-full place-content-center gap-3 p-6 text-center"><p className="text-sm font-medium">{t("app.page_unloaded")}</p><p className="max-w-sm text-xs text-muted-foreground">{t("app.this_browser_page_was_unloaded")}</p><Button onClick={() => showAgentTerminal(tab.id)}>{t("app.reload_tab")}</Button></div>}
            </div>
          })}
        </div>
        {mainView !== "overview" && mainView !== "terminal" && <div className="min-h-0 flex-1 overflow-auto">{mainView === "organization" ? <OrganizationSettingsView organization={organization} name={organizationName} workspaces={workspaces} selectedWorkspaceId={workspaceId} members={members} groups={groups} groupName={groupName} canManage={canManageMembers} currentRole={currentRole} onNameChange={setOrganizationName} onRename={renameOrganization} onCreateOrganization={() => { setCreateOrganizationName(""); setCreateOrganizationOpen(true) }} onCreateWorkspace={() => { setCreateWorkspaceName(""); setCreateWorkspaceOpen(true) }} onSelectWorkspace={(selectedWorkspaceId) => { selectWorkspace(selectedWorkspaceId); closeMainView() }} onInvite={createInvite} onMemberRoleChange={updateMemberRole} onRemoveMember={removeMember} onGroupNameChange={setGroupName} onCreateGroup={createGroup} onDeleteGroup={deleteGroup} onSetGroupMember={setGroupMember} onOpenAudit={openAudit} onClose={closeMainView} /> : mainView === "workspace" ? <WorkspaceSettingsView key={workspaceId} workspace={workspace} organization={organization} name={workspaceName} machines={snapshot?.servers ?? []} machineCount={workspaceMachineCount} profiles={launchProfiles} profilesLoading={launchProfilesLoading} apps={apps} onPolicyAppInstalled={(app) => setApps((current) => [...current.filter((item) => item.app_id !== app.app_id), app].sort((left, right) => left.name.localeCompare(right.name)))} canDelete={canManageMembers} preview={preview} onNameChange={setWorkspaceName} onRename={renameWorkspace} onAddMachine={openInstall} onOpenMachine={(machine) => showMachineOverview(machine.server_id)} onRenameMachine={(machine) => openRename({ kind: "machine", id: machine.server_id, name: machineName(machine) })} onDeleteMachine={(machine) => setDeleteTarget({ kind: "machine", id: machine.server_id, name: machineName(machine) })} onCopy={copy} onRefreshProfiles={loadLaunchProfiles} onNewProfile={openNewLaunchProfile} onEditProfile={openEditLaunchProfile} onLaunchProfile={openLaunchProfile} onDeleteProfile={setDeletingProfile} onDelete={() => setDeleteWorkspaceOpen(true)} onClose={closeMainView} /> : mainView === "apps" ? <AppsView apps={apps} machines={snapshot?.servers ?? []} loading={appsLoading} onOpen={openApp} onSettings={openAppSettings} /> : mainView === "app" ? <AppSettingsView app={selectedApp} machine={selectedAppMachine} onOpen={openAppExternal} onAccess={updateAppAccess} onAction={appLifecycle} onDelete={setDeletingApp} onClose={openApps} onCopy={copy} /> : mainView === "machine" ? <MachineOverviewView machine={selectedMachine} agents={snapshot?.agents.filter((agent) => agent.server_id === selectedMachineId) ?? []} services={services.filter((service) => service.server_id === selectedMachineId)} virtualHosts={virtualHosts.filter((host) => host.destination_server_id === selectedMachineId)} traffic={traffic} machines={snapshot?.servers ?? []} workspaceId={workspaceId} onOpenAgent={showAgentTerminal} onClose={closeMachineOverview} onCopy={copy} /> : mainView === "audit" ? <AuditView events={auditEvents} workspaceId={workspaceId!} refreshRevision={auditRefreshRevision} agents={snapshot?.agents ?? []} machines={snapshot?.servers ?? []} loading={auditLoading} /> : null}</div>}

      </section>
    </main>

    <ErrorToast message={error} onDismiss={() => setError(null)} />

    <WorkspaceDialogs state={dialogState}
      user={user}
      setUser={setUser}
      showError={showError}
      createOrganization={createOrganization}
      organization={organization}
      createWorkspace={createWorkspace}
      saveLaunchProfile={saveLaunchProfile}
      launchFromProfile={launchFromProfile}
      onlineMachines={onlineMachines}
      deleteLaunchProfile={deleteLaunchProfile}
      createAgent={createAgent}
      selectAgentProfile={selectAgentProfile}
      launchProfiles={launchProfiles}
      selectedCreateProfile={selectedCreateProfile}
      changeAgentCommandLine={changeAgentCommandLine}
      deleteAppDeployment={deleteAppDeployment}
      copy={copy}
      submitRename={submitRename}
      snapshot={snapshot}
      workloadTree={workloadTree}
      uploadToAgentMachine={uploadToAgentMachine}
      selectedAgentMachine={selectedAgentMachine}
      selectedAgent={selectedAgent}
      selectedAgentMachineOnline={selectedAgentMachineOnline}
      confirmDelete={confirmDelete}
      workspaceMachineCount={workspaceMachineCount}
      workspace={workspace}
      confirmDeleteWorkspace={confirmDeleteWorkspace}
    />
  </TooltipProvider>
}
