import { observePreparedAgent } from '@/lib/session-orchestration'
import { preparedClaudeArgs } from "@/lib/prepared-claude"
import { AppNodeRoute } from "@/components/app-node-route"
import type { AppNodeSelection } from "@/lib/app-tree"
import { useAgentEmbedPrewarm } from "@/components/use-agent-embed-prewarm"
import { useAgentOriginCapability } from "@/components/use-agent-origin-capability"
import { agentOriginUrl, interfaceSignature } from "@/lib/agent-origin"
import { agentUiIsRefused, useAgentUiFallback } from "@/components/use-agent-ui-fallback"
import { useWorkspacePins } from "@/components/use-workspace-pins"
import { agentCreationFailure, type AgentCreationFailure } from "@/lib/agent-creation-failure"
import { WorkloadSurface } from "@/components/workload-surface"
import { appOwnUrl } from "@/lib/app-url"
import { readEmbedAppearance, withEmbedTheme } from "@/lib/embed-ui"
import { WorkloadFocusBar } from "@/components/workload-focus-bar"
import { WorkspaceTabBar, type TabLabel, type TabAction, type TabActions } from "@/components/workspace-tab-bar"
import { OVERVIEW_TAB, useWorkspaceTabs } from "@/components/use-workspace-tabs"
import { TopBar } from "@/components/top-bar"
import { CommandPalette, usePaletteShortcut, type PaletteItem } from "@/components/command-palette"
import { SettingsHub } from "@/components/settings-hub/hub"
import { useOrgDirectory } from "@/components/settings-hub/use-org-directory"
import { ORGANIZATION_SECTIONS, PERSONAL_SECTIONS, WORKSPACE_SECTIONS, organizationSettingsPath, parseSettingsRoute, personalSettingsPath, workspaceSettingsPath } from "@/lib/console-routes"
import { WorkspaceOverview } from "@/components/workspace-overview"
import { AppSettingsView } from "@/components/apps-view"
import { InvitationEntry, readPendingInvitation, type InvitationIntent } from "@/components/invitation-entry"
import { AuthScreen } from "@/components/auth-screen"
import { IconButton } from "@/components/control-plane-ui"
import { ErrorToast } from "@/components/error-toast"
import { MachineOverviewView } from "@/components/machine-view"
import { type TerminalPaneHandle } from "@/components/terminal-pane"
import { TreeResizer,useTreeWidth } from "@/components/tree-resizer"
import { Button } from "@/components/ui/button"
import { TooltipProvider } from "@/components/ui/tooltip"
import { useWorkspaceLiveFeed } from "@/components/use-workspace-live-feed"
import { useWorkspaceMetrics } from "@/components/use-workspace-metrics"
import { useWorkspaceRoute } from "@/components/use-workspace-route"
import { WorkloadTree,type NewAgentFolder } from "@/components/workload-tree"
import { workspaceWorkloads } from "@/lib/workspace-workloads"
import { MachinesSection, SidebarSearch } from "@/components/sidebar-sections"
import { useWorkspaceDialogState } from "@/components/workspace-dialog-state"
import { WorkspaceDialogs } from "@/components/workspace-dialogs"
import { ApiError,api,machineName,proxyUrl,type Agent,type AgentLaunchProfile,type AppDeployment,type Machine,type Organization,type Snapshot,type User,type WorkloadTreeState,type Workspace } from "@/lib/api"
import { formatCommandLine,parseCommandLine } from "@/lib/command-line"
import { MainView,PREVIEW_CONNECT,PREVIEW_INSTALL,PREVIEW_ORG,PREVIEW_PROFILES,PREVIEW_USER,PREVIEW_WORKSPACE,RenameTarget,activeStatuses,authorizationReturnUrl,controlCharacter,defaultAgentName,defaultProfileAgentName,machineOnline,replaceWorkspace,workloadPath,workspacePath,isWorkloadDeepLink } from "@/lib/control-plane"
import { clearFirstRunTour,firstRunTourMode,shouldAutoStartFirstRunTour,startFirstRunTour,stopFirstRunTour,type FirstRunTourHost } from "@/lib/first-run-tour"
import { readTheme,subscribeTheme,type Theme } from "@/lib/theme"
import { cn } from "@/lib/utils"
import { agentPath } from "@/lib/workload-path"
import {
AppWindow,
Bot,
ChevronLeft,
ChevronRight,
FolderKanban,
LayoutDashboard,
Monitor,
PanelLeftClose,
RotateCw,
Settings as SettingsIcon,
Upload,
UserRound,
ExternalLink
} from "lucide-react"
import { FormEvent,useCallback,useEffect,useMemo,useRef,useState } from "react"
import { Navigate,useLocation,useParams } from "react-router"
import { sidebarShortcutKeys, sidebarShortcutLabel } from "@/lib/shortcut"
import { statusLabel, t, type MessageKey } from "@/i18n"


export function WorkspaceApp() {
  const route = useParams<{ organizationId?: string; workspaceId?: string }>()
  const location = useLocation()
  // Core's trusted Policy link (/orgs/:o/workspaces/:w#policy-provider) is the
  // workspace's Policy settings page.
  if (location.hash === "#policy-provider" && route.organizationId && route.workspaceId && !parseSettingsRoute(location.pathname))
    return <Navigate replace to={{ pathname: workspaceSettingsPath(route.organizationId, route.workspaceId, "policy"), search: location.search }} />
  return <WorkspaceAppContent />
}

function WorkspaceAppContent() {
  const { route, location, routeSelectionRef, routingRef, replaceWorkspaceRoute, replaceWorkloadRoute, routeWorkloadId, focusMode } = useWorkspaceRoute()
  const [pendingInvite, setPendingInvite] = useState<InvitationIntent | null>(() => {
    const token = new URLSearchParams(window.location.search).get("invite")
    return token === null ? null : { token }
  })
  const [pendingInviteCheckedFor, setPendingInviteCheckedFor] = useState<string | null>(null)
  // Fixed for the page: settings navigation must not leave or enter the tour preview.
  const [preview] = useState(() => firstRunTourMode() === "preview")
  const settingsRoute = parseSettingsRoute(location.pathname)
  const linkSearch = preview ? "?tour=preview" : ""
  const [user, setUserState] = useState<User | null | undefined>(preview ? PREVIEW_USER : undefined)
  const [authEpoch, setAuthEpoch] = useState(0)
  const setUser = useCallback<React.Dispatch<React.SetStateAction<User | null | undefined>>>((next) => {
    setAuthEpoch(epoch => epoch + 1)
    setUserState(next)
  }, [])
  const agentCapabilities = useAgentOriginCapability(user?.user_id, preview)
  const agentOriginMode = agentCapabilities.mode
  const pendingInviteChecked = preview || pendingInvite !== null || Boolean(user && pendingInviteCheckedFor === user.user_id)
  const [organizations, setOrganizations] = useState<Organization[]>(preview ? [PREVIEW_ORG] : [])
  const [organizationId, setOrganizationId] = useState<string | null>(preview ? PREVIEW_ORG.organization_id : null)
  const organizationIdRef = useRef<string | null>(preview ? PREVIEW_ORG.organization_id : null)
  const [organizationsLoaded, setOrganizationsLoaded] = useState(preview)
  const [workspaces, setWorkspaces] = useState<Workspace[]>([])
  // The organization the workspace list belongs to (settings pages must not show another org's list).
  const [workspacesOrganizationId, setWorkspacesOrganizationId] = useState<string | null>(preview ? PREVIEW_ORG.organization_id : null)
  const [workspaceId, setWorkspaceId] = useState<string | null>(null)
  const workspaceIdRef = useRef<string | null>(null)
  workspaceIdRef.current = workspaceId
  const [loadedSnapshot, setSnapshot] = useState<Snapshot | null>(null)
  const snapshot = loadedSnapshot?.workspace.workspace_id === workspaceId ? loadedSnapshot : null
  const [uploadAgent, setUploadAgent] = useState<Agent | undefined>()
  const uploadMachine = snapshot?.servers.find((machine) => machine.server_id === uploadAgent?.server_id)
  const [selectedAgentId, setSelectedAgentId] = useState<string | null>(null)
  const [selectedMachineId, setSelectedMachineId] = useState<string | null>(null)
  const [sidebarQuery, setSidebarQuery] = useState("")
  useEffect(() => { setSidebarQuery("") }, [workspaceId])
  const [appNodeIntent, setAppNodeIntent] = useState<{ workspaceId: string | null; ownerId: string; endpoint: string } | null>(null)
  const [selectedAppId, setSelectedAppId] = useState<string | null>(null)
  const [preparedCreated, setPreparedCreated] = useState(false)
  const [preparedObservation, setPreparedObservation] = useState<{ workspace: string; agent: Agent; session: string }>()
  const [preparedReadiness, setPreparedReadiness] = useState<'observing' | 'ready' | 'notReady'>('observing')
  useEffect(() => {
    if (!preparedObservation || preparedObservation.workspace !== workspaceId) return
    const controller = new AbortController()
    setPreparedReadiness('observing')
    void observePreparedAgent(preparedObservation.workspace, preparedObservation.agent, preparedObservation.session, controller.signal).then(ready => {
      if (!controller.signal.aborted) setPreparedReadiness(ready ? 'ready' : 'notReady')
    })
    return () => controller.abort()
  }, [preparedObservation, workspaceId])
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
  const [reservationCleaned, setReservationCleaned] = useState(false)
  const [creationFailure, setCreationFailure] = useState<AgentCreationFailure | null>(null)
  const dialogState = useWorkspaceDialogState()
  const {
    preparedClaude, preparedAttempted, setPreparedAttempted,
    setCreateOrganizationOpen, setCreateWorkspaceOpen,
    createAgentOpen, setCreateAgentOpen, agentFolder, setAgentFolder, setRenameError,
    setCreateAgentError, creatingAgent, setCreatingAgent, deletingApp, setDeletingApp,
    setProfileEditorOpen, editingLaunchProfile, setEditingLaunchProfile, launchingProfile,
    setLaunchingProfile, deletingProfile, setDeletingProfile, setInstallOpen,
    setUploadOpen, renameTarget,
    setRenameTarget, deleteTarget, setDeleteTarget, createOrganizationName, setCreateOrganizationName,
    createWorkspaceName, setCreateWorkspaceName, agentName, setAgentName, agentNameCustomized,
    setAgentNameCustomized, agentProfileId, setAgentProfileId, agentServerId, setAgentServerId, agentCwd,
    agentCommandLine, setAgentCommandLine, launchProfileName, setLaunchProfileName,
    launchProfileDescription, setLaunchProfileDescription, launchProfileCwd, setLaunchProfileCwd,
    launchProfileCommandLine, setLaunchProfileCommandLine, launchMachineId, setLaunchMachineId,
    launchAgentName, setLaunchAgentName, renameName, setRenameName, setInstallCommand, setConnectCommand,
    setWindowsInstallCommand, setWindowsConnectCommand,
  } = dialogState
  const [placementRevision, setPlacementRevision] = useState(0)
  const [workloadTree, setWorkloadTree] = useState<WorkloadTreeState>({ folders: [], placements: [] })
  const [launchProfiles, setLaunchProfiles] = useState<AgentLaunchProfile[]>(preview ? PREVIEW_PROFILES : [])
  const createAgentOpenRef = useRef(false)
  const [launchProfilesLoading, setLaunchProfilesLoading] = useState(false)

  useEffect(() => {
    const parameters = new URLSearchParams(location.search)
    if (!parameters.has("invite")) return
    setPendingInvite({ token: parameters.get("invite") ?? "" })
    parameters.delete("invite")
    routingRef.current.navigate({ pathname: location.pathname, search: parameters.toString(), hash: location.hash }, { replace: true })
  }, [location.search, location.pathname, location.hash, routingRef])

  const showError = useCallback((reason: unknown) => setError(reason instanceof Error ? reason.message : t("app.something_went_wrong")), [])

  // Probe once per authenticated account, not on workspace navigation or polls.
  // Existing servers return 404; the normal no-invitation console still works.
  useEffect(() => {
    if (!user) { setPendingInviteCheckedFor(null); return }
    if (preview || pendingInvite !== null) { setPendingInviteCheckedFor(user.user_id); return }
    let disposed = false
    setPendingInviteCheckedFor(null)
    readPendingInvitation().then((intent) => { if (!disposed && intent) setPendingInvite(intent) })
      .catch((reason) => { if (!disposed) showError(reason) })
      .finally(() => { if (!disposed) setPendingInviteCheckedFor(user.user_id) })
    return () => { disposed = true }
    // Pending intent is deliberately not a dependency: dismissing a token must
    // not immediately retrieve it again. A new account gets its own probe.
  }, [user?.user_id, preview, showError])

  useEffect(() => {
    if (preview) return
    api<User>("/api/auth/me").then(setUser).catch((reason) => {
      if (reason instanceof ApiError && reason.status === 401) setUser(null)
      else showError(reason)
    })
  }, [preview, showError])

  useEffect(() => {
    if (!user || pendingInvite !== null || !pendingInviteChecked) return
    const returnTo = authorizationReturnUrl()
    if (returnTo) window.location.assign(returnTo)
  }, [user, pendingInvite, pendingInviteChecked])

  const loadOrganizations = useCallback(async (preferred?: string, requirePreferred = false) => {
    const data = await api<{ organizations: Organization[] }>("/api/organizations")
    if (requirePreferred && !data.organizations.some((item) => item.organization_id === preferred)) throw new Error(t("invite.switch_failed"))
    setOrganizations(data.organizations)
    setOrganizationsLoaded(true)
    const routeSelection = routeSelectionRef.current
    // An explicit target (trusted Policy link or workload deep link) that is unavailable is reported, never replaced
    // by the default organization, workspace or workload.
    if (routeSelection.organizationId && !data.organizations.some((item) => item.organization_id === routeSelection.organizationId)) {
      if (routingRef.current.hash === "#policy-provider") throw new Error(t("app.policy_settings_target_is_unavailable"))
      if (isWorkloadDeepLink(routingRef.current.pathname)) throw new Error(t("app.workload_not_found"))
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

  useEffect(() => { if (user && !preview && pendingInvite === null && pendingInviteChecked) loadOrganizations().catch(showError) }, [user, preview, pendingInvite, pendingInviteChecked, loadOrganizations, showError])

  const syncWorkspaces = useCallback(async () => {
    if (preview) return
    if (!organizationId) {
      setWorkspaces([])
      setWorkspacesOrganizationId(null)
      return
    }
    const data = await api<{ workspaces: Workspace[] }>(`/api/workspaces?organization_id=${encodeURIComponent(organizationId)}`)
    if (organizationIdRef.current !== organizationId) return
    setWorkspaces(data.workspaces)
    setWorkspacesOrganizationId(organizationId)
    const routeSelection = routeSelectionRef.current
    if (routeSelection.workspaceId && !data.workspaces.some((item) => item.workspace_id === routeSelection.workspaceId)) {
      if (routingRef.current.hash === "#policy-provider") throw new Error(t("app.policy_settings_target_is_unavailable"))
      if (isWorkloadDeepLink(routingRef.current.pathname)) throw new Error(t("app.workload_not_found"))
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

  const { apps, setApps, appsLoading, loadApps, refreshSnapshot, originReadAgents } = useWorkspaceLiveFeed({
    preview, userId: user?.user_id, isolatedAgentOrigins: agentOriginMode === "isolated", workspaceId, workspaceIdRef, snapshot, setSnapshot, setWorkspaces,
    setSelectedAgentId, setSelectedMachineId, syncWorkspaces, showError,
  })

  const pins = useWorkspacePins(user?.user_id ?? null, workspaceId, preview)
  const workloads = useMemo(() => workspaceWorkloads(snapshot?.agents ?? [], apps), [snapshot?.agents, apps])
  const tabs = useWorkspaceTabs(user?.user_id, workspaceId, isMobile, workloads.map((agent) => agent.agent_id), Boolean(snapshot) && !appsLoading, preview)
  // Deep link: the /workloads/:id present when a workspace first becomes ready selects that workload; an unknown
  // or inaccessible ID is reported and never replaced by another workload. After that the URL only follows the
  // selection, so our own URL updates are never mistaken for a new link.
  const deepLinkWorkspace = useRef<string | null>(null)
  // Capture readiness for this render. The initialization effect below marks
  // the workspace before its selection state commits; URL synchronization must
  // not treat that same render's empty selection as the user's intent.
  const deepLinkInitialized = deepLinkWorkspace.current === workspaceId
  const firstPathname = useRef(location.pathname)
  const inventoryReady = Boolean(snapshot) && !appsLoading
  const routeMatchesWorkspace = Boolean(workspaceId) && route.workspaceId === workspaceId
  useEffect(() => {
    if (!inventoryReady || !routeMatchesWorkspace || !workspaceId || deepLinkWorkspace.current === workspaceId) return
    deepLinkWorkspace.current = workspaceId
    if (!routeWorkloadId) {
      // A /workloads segment that is not a valid ID (malformed) is an explicit target too: report it. The URL may
      // already have been canonicalized, so the first URL of the page load decides.
      const requested = isWorkloadDeepLink(firstPathname.current)
      firstPathname.current = ""
      if (requested) setError(t("app.workload_not_found"))
      return
    }
    firstPathname.current = ""
    if (workloads.some((agent) => agent.agent_id === routeWorkloadId)) {
      // Inventory can finish after a phone user has already opened the Tree.
      // Restore the URL's selection without reversing that explicit gesture.
      const keepTreeOpen = isMobile && mobileTreeOpen
      showAgentTerminal(routeWorkloadId, undefined, true)
      if (keepTreeOpen) { setMobileTerminalOpen(false); setMobileTreeOpen(true) }
    } else { setError(t("app.workload_not_found")); replaceWorkloadRoute(null) }
    // showAgentTerminal is recreated every render; the workspace marker makes this run once per workspace.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [routeWorkloadId, inventoryReady, routeMatchesWorkspace, workspaceId, workloads])
  // The address bar follows the selection so the URL can be copied, shared or reloaded.
  const selectedWorkloadId = mainView === "terminal" ? selectedAgentId : null
  // A settings page owns the URL; following resumes when the workspace is shown again.
  const onSettingsPage = Boolean(settingsRoute)
  useEffect(() => {
    if (!inventoryReady || !routeMatchesWorkspace || preview || !deepLinkInitialized || onSettingsPage) return
    if (selectedWorkloadId !== routeWorkloadId) replaceWorkloadRoute(selectedWorkloadId)
  }, [inventoryReady, routeMatchesWorkspace, preview, routeWorkloadId, selectedWorkloadId, workspaceId, replaceWorkloadRoute, deepLinkInitialized, onSettingsPage])
  // Registered Agent UIs and routed Apps open directly; other workloads retain
  // the Console deep link. Never use a descriptor's ui_path as a navigation URL.
  const appOwnUrls = useMemo(() => {
    const blocked = [window.location.origin, proxyUrl("/")].map((value) => { try { return new URL(value).origin } catch { return value } })
    return new Map(apps.flatMap((app) => { const url = appOwnUrl(app, blocked); return url ? [[app.runtime_agent_id ?? app.app_id, url] as const] : [] }))
  }, [apps])
  const authorizedOriginAgents = new Map(originReadAgents.map((agent) => [agent.agent_id, agent]))
  const blockedAgentOrigins = [window.location.origin, new URL(proxyUrl("/")).origin]
  const interfaceWorkspace = workspaceId ? encodeURIComponent(workspaceId) : null
  const agentOwnUrls = new Map<string, string>(interfaceWorkspace ? workloads.filter((agent) => agent.kind !== "app" && agent.interface?.ui_path && !apps.some((app) => (app.runtime_agent_id ?? app.app_id) === agent.agent_id)).flatMap((agent) => {
    const authorized = authorizedOriginAgents.get(agent.agent_id)
    const isolatedUrl = authorized && interfaceSignature(authorized) === interfaceSignature(agent) ? agentOriginUrl(authorized, blockedAgentOrigins) : null
    const url = agentOriginMode === "isolated" ? isolatedUrl : agentOriginMode === "legacy" ? withEmbedTheme(proxyUrl(`/api/workspaces/${interfaceWorkspace}/agents/${encodeURIComponent(agent.agent_id)}/interface/ui/`), theme, readEmbedAppearance()) : null
    return url ? [[agent.agent_id, url] as const] : []
  }) : [])
  const embedPrewarm = useAgentEmbedPrewarm(user?.user_id, workspaceId,
    !preview && agentCapabilities.prewarm && agentCapabilities.embed === "isolated" && agentOriginMode === "isolated",
    workloads.flatMap(agent => {
      const origin = agentOwnUrls.get(agent.agent_id)
      return origin && snapshot?.servers.some(machine => machine.server_id === agent.server_id && machine.status === "online")
        ? [{ id: agent.agent_id, origin, revision: `${interfaceSignature(agent)}:${agent.run_generation ?? 0}` }] : []
    }), tabs.tabs.map(tab => tab.id))
  const probeAgent = agentOriginMode === "legacy" && mainView === "terminal" ? workloads.find((agent) => agent.agent_id === selectedAgentId && agentOwnUrls.has(agent.agent_id) &&
    snapshot?.servers.some((machine) => machine.server_id === agent.server_id && machine.status === "online")) : undefined
  const probePath = probeAgent && interfaceWorkspace
    ? proxyUrl(`/api/workspaces/${interfaceWorkspace}/agents/${encodeURIComponent(probeAgent.agent_id)}/interface/ui/`) : undefined
  const uiProbeScope = JSON.stringify([user?.user_id, workspaceId, proxyUrl("/"), preview]) + ":"
  useAgentUiFallback(`${uiProbeScope}${probeAgent?.agent_id}`,
    preview ? undefined : probePath, user?.user_id)
  for (const id of agentOwnUrls.keys()) if (agentOriginMode === "legacy" && agentUiIsRefused(`${uiProbeScope}${id}`)) agentOwnUrls.delete(id)
  const appEndpoint = appNodeIntent?.workspaceId === workspaceId && appNodeIntent?.ownerId === selectedAgentId ? appNodeIntent.endpoint : new URLSearchParams(location.search).get("app_node")
  const selectedNodeApp = appEndpoint !== null ? apps.find(app => (app.runtime_agent_id ?? app.app_id) === selectedAgentId) : undefined
  const appNodeHref = (selection: AppNodeSelection) => `${workloadPath(organizationId, workspaceId, selection.ownerId).split("?")[0]}?app_node=${encodeURIComponent(selection.path.join("/"))}`
  const openAppNode = (selection: AppNodeSelection) => {
    setAppNodeIntent({ workspaceId, ownerId: selection.ownerId, endpoint: selection.path.join("/") })
    showAgentTerminal(selection.ownerId, undefined, true)
    routingRef.current.navigate(appNodeHref(selection))
  }
  useEffect(() => {
    if (!appNodeIntent || appNodeIntent.workspaceId !== workspaceId || appNodeIntent.ownerId !== selectedAgentId || mainView !== "terminal") return
    const href = `${workloadPath(organizationId, workspaceId, appNodeIntent.ownerId).split("?")[0]}?app_node=${encodeURIComponent(appNodeIntent.endpoint)}`
    if (location.pathname + location.search !== href) routingRef.current.navigate(href, { replace: true })
    else setAppNodeIntent(null)
  }, [appNodeIntent, workspaceId, selectedAgentId, mainView, organizationId, location.pathname, location.search])
  const workloadHref = (id: string) => appOwnUrls.get(id) ?? agentOwnUrls.get(id) ?? workloadPath(organizationId, workspaceId, id)
  // Synchronous to preserve the user gesture; session authentication stays in
  // cookies. No credential or caller-supplied navigation URL is added.
  const openInNewTab = (id: string) => {
    const own = appOwnUrls.get(id) ?? agentOwnUrls.get(id)
    window.open(own ?? workloadPath(organizationId, workspaceId, id, true), "_blank", "noopener,noreferrer")
  }
  const focusedId = focusMode && selectedWorkloadId ? selectedWorkloadId : null
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
    setMainView((current) => current === "terminal" || current === "app" || current === "machine" ? "overview" : current)
  }, [workspaceId, preview])


  const onlineMachines = snapshot?.servers.filter((machine) => machine.status === "online") ?? []
  const selectedApp = apps.find((app) => app.app_id === selectedAppId)
  const selectedAppMachine = snapshot?.servers.find((machine) => machine.server_id === selectedApp?.server_id)
  const selectedCreateProfile = launchProfiles.find((profile) => profile.profile_id === agentProfileId)
  const organization = organizations.find((item) => item.organization_id === organizationId)
  const workspace = workspaces.find((item) => item.workspace_id === workspaceId)

  useEffect(() => {
    document.title = organization && workspace
      ? `${organization.name} / ${workspace.name}`
      : "Treer"
    return () => { document.title = "Treer" }
  }, [organization, workspace])


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
    setAppNodeIntent(null)
    // Clear the reload target in the same turn as the navigation gesture; the
    // selection-following effect can run after an immediate reload or URL copy.
    if (!preview && isWorkloadDeepLink(window.location.pathname)) replaceWorkloadRoute(null)
    setMainView("overview")
    setSelectedAgentId(null)
    setMobileTerminalOpen(false)
    setMobileTreeOpen(false)
    setCtrlModifier(false)
  }

  function browseMobileTree() {
    setMobileTerminalOpen(false)
    setMobileTreeOpen(true)
    setCtrlModifier(false)
    if (mainView !== "terminal") setMainView("overview")
    requestAnimationFrame(() => {
      const sidebar = document.getElementById("tree-sidebar")
      if (!sidebar?.getClientRects().length) return
      const target = sidebar.querySelector<HTMLElement>('[aria-selected="true"] [data-tree-primary]')
        ?? sidebar.querySelector<HTMLElement>('[data-tree-primary]') ?? sidebar.querySelector<HTMLElement>("button")
      target?.focus()
    })
  }

  function closeMobileSurface() {
    setMobileTerminalOpen(false)
    setCtrlModifier(false)
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
    setUploadAgent(undefined)
    setUploadOpen(false)
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

  function showAgentTerminal(agentId: string, logs?: boolean, keepAppNode = false) {
    if (!keepAppNode) setAppNodeIntent(null)
    if (!keepAppNode && new URLSearchParams(routingRef.current.search).has("app_node")) {
      const parameters = new URLSearchParams(routingRef.current.search)
      parameters.delete("app_node")
      routingRef.current.navigate({ pathname: workloadPath(organizationId, workspaceId, agentId).split("?")[0], search: parameters.toString() })
    }
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

  function closeTabs(ids: string[]) {
    tabs.closeMany(ids)
    if (selectedAgentId && ids.includes(selectedAgentId)) {
      const next = tabs.tabs.find((tab) => !ids.includes(tab.id))
      if (next) showAgentTerminal(next.id)
      else showOverview()
    }
  }

  const tabActions = new Map<string, TabActions>(tabs.tabs.flatMap((tab) => {
    const label = tabLabels.get(tab.id)
    if (!label) return []
    const app = apps.find((entry) => (entry.runtime_agent_id ?? entry.app_id) === tab.id)
    const logs = tab.logs && tab.appId === (app?.app_id ?? null)
    return [[tab.id, { app: Boolean(app), logs, online: machineOnline(snapshot?.servers.find((machine) => machine.server_id === label.agent.server_id)), active: activeStatuses.has(label.agent.status), ui: !logs && Boolean(app || label.agent.interface?.ui_path) }]]
  }))
  function tabAction(id: string, action: TabAction) {
    const agent = tabLabels.get(id)?.agent
    if (!agent || !workspaceId) return
    // Same origin, same session, no token in the URL. noopener keeps the new page from reaching this one.
    if (action === "newtab") { openInNewTab(id); return }
    if (action === "logs") setShowWorkloadLogs(!tabActions.get(id)?.logs, id)
    if (action === "rename") openRename({ kind: "agent", id, name: agent.name })
    if (action === "upload") openUpload(agent)
    if (action === "reload") tabs.reload(id)
    if (action === "fullscreen") { showAgentTerminal(id); openMobileTerminal() }
    if (action === "back") closeMobileSurface()
  }

  function applyOrganization(value: string) {
    organizationIdRef.current = value
    workspaceIdRef.current = null
    setOrganizationId(value)
    setWorkspaceId(null)
  }

  function applyWorkspace(value: string) {
    workspaceIdRef.current = value
    setWorkspaceId(value)
    setSnapshot((current) => current?.workspace.workspace_id === value ? current : null)
    setApps([])
    setSelectedAppId(null)
    setLaunchProfiles([])
  }

  function selectWorkspace(value: string) {
    applyWorkspace(value)
    replaceWorkspaceRoute(organizationId, value)
  }

  // The URL is the source of truth for the context: Back/Forward, settings
  // links and the context switcher all navigate, and the selection follows.
  useEffect(() => {
    if (preview || !organizationsLoaded) return
    const target = route.organizationId
    if (!target) return
    if (target !== organizationIdRef.current) {
      if (organizations.some((item) => item.organization_id === target)) applyOrganization(target)
      return
    }
    const targetWorkspace = route.workspaceId
    if (targetWorkspace && targetWorkspace !== workspaceIdRef.current && workspacesOrganizationId === target && workspaces.some((item) => item.workspace_id === targetWorkspace)) applyWorkspace(targetWorkspace)
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [route.organizationId, route.workspaceId, organizations, organizationsLoaded, workspaces, workspacesOrganizationId, preview])

  useEffect(() => {
    if (createAgentOpen && !onlineMachines.some((machine) => machine.server_id === agentServerId)) setAgentServerId(onlineMachines[0]?.server_id ?? "")
  }, [createAgentOpen, onlineMachines, agentServerId])


  async function createOrganization(event: FormEvent) {
    event.preventDefault()
    try {
      const data = await api<{ organization: Organization }>("/api/organizations", { method: "POST", body: JSON.stringify({ name: createOrganizationName }) })
      setCreateOrganizationOpen(false); setCreateOrganizationName(""); await loadOrganizations(data.organization.organization_id)
      if (settingsRoute) routingRef.current.navigate(workspacePath(data.organization.organization_id, null))
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
      setWorkspaces(list.workspaces); setWorkspacesOrganizationId(organizationId)
      if (settingsRoute) { applyWorkspace(data.workspace.workspace_id); routingRef.current.navigate(workspacePath(organizationId, data.workspace.workspace_id)) }
      else selectWorkspace(data.workspace.workspace_id)
    } catch (reason) { showError(reason) }
  }

  async function renameWorkspace(name: string) {
    if (!workspaceId) return
    if (preview) { setWorkspaces((items) => items.map((item) => item.workspace_id === workspaceId ? { ...item, name } : item)); return }
    const data = await api<{ workspace: Workspace }>(`/api/workspaces/${encodeURIComponent(workspaceId)}`, { method: "PATCH", body: JSON.stringify({ name }) })
    setWorkspaces((items) => replaceWorkspace(items, data.workspace))
    setSnapshot((current) => current?.workspace.workspace_id === data.workspace.workspace_id ? { ...current, workspace: data.workspace } : current)
  }

  // After a workspace is deleted its settings URL is gone; land on the organization.
  async function workspaceDeleted(targetId: string) {
    setWorkspaces((items) => items.filter((item) => item.workspace_id !== targetId))
    setSnapshot(null)
    setSelectedAgentId(null)
    setSelectedMachineId(null)
    workspaceIdRef.current = null
    setWorkspaceId(null)
    routingRef.current.navigate(workspacePath(organizationId, null), { replace: true })
    await syncWorkspaces().catch(showError)
  }

  // Leaving or deleting an organization: return to the Console's default context.
  async function organizationGone(targetId: string) {
    routingRef.current.navigate("/", { replace: true })
    if (organizationIdRef.current === targetId) organizationIdRef.current = null
    await loadOrganizations().catch(showError)
  }
  async function createAgent(event: FormEvent) {
    event.preventDefault()
    if (preview) {
      setCreateAgentOpen(false)
      return
    }
    if (!workspaceId || creatingAgent || (creationFailure && creationFailure.creation_state !== "identity_removed")) return
    const creationMachineId = agentServerId
    const creationWorkspaceId = workspaceId
    const folder = agentFolder
    const placement = folder ? { folder_id: folder.folderId } : {}
    setCreatingAgent(true); setCreateAgentError(""); setCreationFailure(null); setReservationCleaned(false)
    try {
      let agent
      if (agentProfileId === "prepared-claude") {
        const machine = snapshot?.servers.find(item => item.server_id === agentServerId)
        const args = machine && preparedClaudeArgs(agentServerId, machine.root, agentCwd, preparedClaude)
        if (preparedAttempted || !args) throw new Error(t("prepared.invalid"))
        setPreparedAttempted(true)
        agent = await api<Agent>(`/api/workspaces/${encodeURIComponent(workspaceId)}/agents`, { method: "POST", body: JSON.stringify({ ...placement, server_id: agentServerId, kind: "command", name: agentName, cwd: agentCwd, args, cols: 120, rows: 36 }) })
      } else if (agentProfileId === "terminal") {
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
      if (workspaceIdRef.current === creationWorkspaceId) {
        showAgentTerminal(agentId)
        if (agentProfileId === "prepared-claude") { setPreparedCreated(true); setPreparedObservation({ workspace: creationWorkspaceId, agent, session: preparedClaude.sessionId }) }
      }
    } catch (reason) { if (workspaceIdRef.current === creationWorkspaceId) {
      const failure = reason instanceof ApiError ? agentCreationFailure(reason.details, creationMachineId) : null
      setCreationFailure(failure)
      setCreateAgentError(reason instanceof ApiError && reason.code === "command_not_found"
        ? failure?.command
          ? t("provider.command_missing", { command: failure.command, machine: machineName(snapshot?.servers.find((item) => item.server_id === creationMachineId), creationMachineId) })
          : t("provider.command_missing_unknown")
        : reason instanceof Error ? reason.message : t("app.create_failed"))
    } }
    finally { setCreatingAgent(false) }
  }

  function openCreateAgent(reset = true, folder: NewAgentFolder | null = null) {
    setPreparedCreated(false)
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
    // Every "set up a machine" path lands in the same Add machine dialog.
    if (!preview && snapshot && snapshot.servers.length === 0) {
      void openInstall()
      return
    }
    openCreateAgent(true, folder ?? null)
  }

  useEffect(() => { setPreparedCreated(false); setCreateAgentOpen(false); setAgentFolder(null); setCreationFailure(null); setReservationCleaned(false) }, [workspaceId])

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
    if (settingsRoute?.scope === "workspace" && settingsRoute.section === "launch-profiles") loadLaunchProfiles().catch(showError)
  }, [settingsRoute?.scope, settingsRoute?.section, loadLaunchProfiles, showError])

  useEffect(() => {
    setApps([])
    loadApps().catch(showError)
  }, [loadApps, showError, setApps])

  useEffect(() => {
    if (mainView === "app" && selectedAppId && !appsLoading && !selectedApp) setMainView("overview")
  }, [mainView, selectedAppId, selectedApp, appsLoading])

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

  // Settings are pages: open them by URL (push) so Back returns here.
  function openSettingsPage(path: string) {
    setMobileTerminalOpen(false)
    const [pathname, query = ""] = path.split("?")
    const search = new URLSearchParams(query)
    if (preview) search.set("tour", "preview")
    routingRef.current.navigate({ pathname, search: search.toString() })
  }

  function openWorkspaceSettings(section: (typeof WORKSPACE_SECTIONS)[number] = "general") {
    if (!organizationId || !workspaceId) {
      setCreateWorkspaceName("")
      setCreateWorkspaceOpen(true)
      return
    }
    openSettingsPage(workspaceSettingsPath(organizationId, workspaceId, section))
  }

  // Leaves a settings page for the workspace it belongs to (or the current one).
  function exitSettings() {
    routingRef.current.navigate({ pathname: workloadPath(organizationId, workspaceId, mainView === "terminal" ? selectedAgentId : null).split("?")[0], search: linkSearch })
  }

  // A machine page opened from the settings list goes back there.
  const [machineFromSettings, setMachineFromSettings] = useState(false)
  function closeMachineOverview() {
    if (machineFromSettings) { routingRef.current.navigate(-1); return }
    showOverview()
  }
  // Returning to the Machines settings page (Close, Back or browser Back) retires that machine page.
  // Only an actual return to settings counts: the machine page's own navigation commits later.
  const wasOnSettingsPage = useRef(Boolean(settingsRoute))
  useEffect(() => {
    const entered = Boolean(settingsRoute) && !wasOnSettingsPage.current
    wasOnSettingsPage.current = Boolean(settingsRoute)
    if (!entered || !machineFromSettings) return
    setMachineFromSettings(false)
    setMainView((current) => current === "machine" ? "overview" : current)
  })

  function createWorkspaceIn(targetOrganizationId: string) {
    if (targetOrganizationId !== organizationId) routingRef.current.navigate({ pathname: workspacePath(targetOrganizationId, null), search: linkSearch })
    setCreateWorkspaceName("")
    setCreateWorkspaceOpen(true)
  }

  function selectContext(targetOrganizationId: string, targetWorkspaceId: string | null) {
    routingRef.current.navigate({ pathname: workspacePath(targetOrganizationId, targetWorkspaceId), search: linkSearch })
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
      const target = workspaceSettingsPath(organizationId ?? PREVIEW_ORG.organization_id, workspaceId ?? PREVIEW_WORKSPACE.workspace_id, "machines")
      if (routingRef.current.pathname !== target) openSettingsPage(target)
    },
    closeWorkspaceSettings: () => { if (parseSettingsRoute(routingRef.current.pathname)) exitSettings() },
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

  function openUpload(agent: Agent) {
    setUploadAgent(agent)
    closeMobileSurface()
    setUploadOpen(true)
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

  const [paletteOpen, setPaletteOpen] = useState(false)
  // Members and groups feed the settings pages, the palette's People and Overview pin authors.
  const directory = useOrgDirectory(organizationId, Boolean(user) && pendingInvite === null, preview)
  const members = directory.members

  const { traffic, virtualHosts, services } = useWorkspaceMetrics({ workspaceId, workspaceIdRef, mainView, showError })

  function showMachineOverview(serverId: string) {
    setMachineFromSettings(false)
    setSelectedMachineId(serverId)
    setMainView("machine")
    setMobileTerminalOpen(false)
  }

  // From the settings Machines list: push the workspace URL so Back returns to the list.
  function openMachineFromSettings(serverId: string) {
    routingRef.current.navigate({ pathname: workspacePath(organizationId, workspaceId), search: linkSearch })
    showMachineOverview(serverId)
    setMachineFromSettings(true)
  }

  async function removeMachines(machineIds: string[]) {
    if (!workspaceId) return
    const failures: string[] = []
    for (const id of machineIds) {
      try { await api(`/api/workspaces/${encodeURIComponent(workspaceId)}/servers/${encodeURIComponent(id)}`, { method: "DELETE" }) }
      catch (reason) { failures.push(`${machineName(snapshot?.servers.find((machine) => machine.server_id === id), id)}: ${reason instanceof Error ? reason.message : String(reason)}`) }
    }
    if (workspaceIdRef.current === workspaceId) await refreshSnapshot()
    if (failures.length) throw new Error(failures.join("; "))
  }

  function openPage(run: () => void) {
    // Workspace destinations leave a settings page first (push, so Back returns to it).
    if (settingsRoute) routingRef.current.navigate({ pathname: workspacePath(organizationId, workspaceId), search: linkSearch })
    run()
  }

  const paletteItems = useMemo<PaletteItem[]>(() => {
    if (!paletteOpen) return []
    const items: PaletteItem[] = []
    for (const agent of workloads) {
      const app = apps.find((entry) => (entry.runtime_agent_id ?? entry.app_id) === agent.agent_id)
      items.push({ id: agent.agent_id, group: "workloads", label: app?.name ?? agent.name, detail: agentPath(agent, workloadTree), keywords: agent.kind, icon: app ? <AppWindow /> : <Bot />, run: () => openPage(() => showAgentTerminal(agent.agent_id)) })
    }
    for (const machine of snapshot?.servers ?? []) items.push({ id: machine.server_id, group: "machines", label: machineName(machine), detail: machine.status === "online" ? t("status.online") : t("machine.offline"), keywords: machine.hostname, icon: <Monitor />, run: () => openPage(() => showMachineOverview(machine.server_id)) })
    if (organizationId) for (const member of members) items.push({ id: member.user_id, group: "people", label: member.preferred_name, detail: member.email, icon: <UserRound />, run: () => openSettingsPage(`${organizationSettingsPath(organizationId, "members")}?q=${encodeURIComponent(member.preferred_name)}`) })
    if (organizationId) for (const item of workspaces) items.push({ id: item.workspace_id, group: "workspaces", label: item.name, detail: organization?.name, icon: <FolderKanban />, run: () => selectContext(organizationId, item.workspace_id) })
    const page = (id: string, label: MessageKey, scope: string, path: string) => items.push({ id, group: "settings", label: `${scope} › ${t(label)}`, icon: <SettingsIcon />, run: () => openSettingsPage(path) })
    const personalLabels: Record<(typeof PERSONAL_SECTIONS)[number], MessageKey> = { profile: "hub.profile", sessions: "hub.sessions", preferences: "hub.preferences", organizations: "hub.organizations" }
    const orgLabels: Record<(typeof ORGANIZATION_SECTIONS)[number], MessageKey> = { general: "settings.nav.general", members: "org.members", invitations: "hub.invitations", groups: "org.groups", workspaces: "org.workspaces", audit: "hub.audit_log", danger: "apps.danger_zone" }
    const workspaceLabels: Record<(typeof WORKSPACE_SECTIONS)[number], MessageKey> = { general: "settings.nav.general", access: "hub.access", machines: "wsettings.machines", "launch-profiles": "wsettings.launch_profiles", policy: "hub.policy", audit: "hub.audit_log", danger: "apps.danger_zone" }
    if (organizationId && workspaceId && workspace) for (const section of WORKSPACE_SECTIONS) page(`ws:${section}`, workspaceLabels[section], workspace.name, workspaceSettingsPath(organizationId, workspaceId, section))
    if (organizationId && organization) for (const section of ORGANIZATION_SECTIONS) page(`org:${section}`, orgLabels[section], organization.name, organizationSettingsPath(organizationId, section))
    for (const section of PERSONAL_SECTIONS) page(`me:${section}`, personalLabels[section], t("hub.group_you"), personalSettingsPath(section))
    return items
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [paletteOpen, workloads, apps, snapshot?.servers, members, workspaces, organizationId, workspaceId, organization, workspace, workloadTree, settingsRoute?.scope])
  usePaletteShortcut(() => setPaletteOpen(true), Boolean(user) && pendingInvite === null && !focusMode)

  function openAppExternal(app: AppDeployment) {
    const url = app.public_url ?? (workspaceId ? proxyUrl(`/api/workspaces/${encodeURIComponent(workspaceId)}/virtual-hosts/${encodeURIComponent(app.hostname)}/proxy/`) : null)
    if (url) window.open(url, "_blank", "noopener,noreferrer")
  }

  async function copy(value: string) {
    try { await navigator.clipboard.writeText(value) }
    catch { showError(t("app.unable_to_copy_to_the")) }
  }

  async function logout() {
    if (preview) return
    embedPrewarm.cancel()
    try { await api("/api/auth/logout", { method: "POST", body: "{}" }) }
    finally { window.location.href = "/" }
  }

  if (user === undefined || (user && pendingInvite === null && !pendingInviteChecked)) return <div className="grid min-h-dvh place-items-center bg-sidebar text-sm text-muted-foreground">{t("app.loading_treer")}</div>
  if (pendingInvite !== null) return <InvitationEntry key={"token" in pendingInvite ? pendingInvite.token : pendingInvite.pending_id} intent={pendingInvite} user={user} onAuthenticated={setUser} onDismiss={() => setPendingInvite(null)} onJoined={async (id) => {
    // Set route selection before releasing the gate so initial discovery cannot
    // select an unrelated first organization after accepting the invitation.
    routeSelectionRef.current = { organizationId: id, workspaceId: null }
    replaceWorkspaceRoute(id, null)
    await loadOrganizations(id, true)
    setPendingInvite(null)
  }} />
  if (!user) return <AuthScreen onAuthenticated={setUser} />

  const hubMachines = snapshot?.workspace.workspace_id === workspaceId ? snapshot?.servers ?? null : null
  return <TooltipProvider delayDuration={350}>
    <div className="flex h-dvh min-h-0 flex-col bg-background">
      {!focusedId && !(isMobile && mobileTerminalOpen && !settingsRoute) && <TopBar user={user} preview={preview} isMobile={isMobile} organizations={organizations} organizationId={organizationId} workspaces={workspacesOrganizationId === organizationId ? workspaces : []} workspaceId={workspaceId} settingsActive={Boolean(settingsRoute)} linkSearch={linkSearch}
        onSelectWorkspace={selectContext} onCreateOrganization={() => { setCreateOrganizationName(""); setCreateOrganizationOpen(true) }} onCreateWorkspace={createWorkspaceIn} onOrganizationSettings={(id) => openSettingsPage(organizationSettingsPath(id, "general"))}
        onOpenSearch={() => setPaletteOpen(true)} onProfile={() => openSettingsPage(personalSettingsPath("profile"))} onMyOrganizations={() => openSettingsPage(personalSettingsPath("organizations"))} onTour={replayFirstRunTour} onLogout={logout} />}
      {settingsRoute && <SettingsHub route={settingsRoute} isMobile={isMobile} preview={preview} user={user} onUserChange={setUser} organizations={organizations} organizationsLoaded={organizationsLoaded} currentOrganizationId={organizationId}
        workspaces={workspaces} workspacesOrganizationId={workspacesOrganizationId} workspacesLoaded={workspacesOrganizationId !== null} currentWorkspaceId={workspaceId} machines={hubMachines} agents={snapshot?.agents ?? []} apps={apps} profiles={launchProfiles} profilesLoading={launchProfilesLoading}
        directory={directory} showError={showError} copy={copy} logout={() => void logout()} exit={exitSettings} createOrganization={() => { setCreateOrganizationName(""); setCreateOrganizationOpen(true) }} createWorkspace={() => { setCreateWorkspaceName(""); setCreateWorkspaceOpen(true) }}
        refreshOrganizations={() => loadOrganizations(organizationIdRef.current ?? undefined)} organizationGone={(id) => void organizationGone(id)}
        workspace={{ renameWorkspace, addMachine: () => void openInstall(), openMachine: openMachineFromSettings, renameMachine: (machine: Machine) => openRename({ kind: "machine", id: machine.server_id, name: machineName(machine) }), removeMachines,
          policyAppInstalled: (app) => setApps((current) => [...current.filter((item) => item.app_id !== app.app_id), app].sort((left, right) => left.name.localeCompare(right.name))),
          refreshProfiles: () => void loadLaunchProfiles().catch(showError), newProfile: openNewLaunchProfile, editProfile: openEditLaunchProfile, launchProfile: openLaunchProfile, deleteProfile: setDeletingProfile, workspaceDeleted: (id) => void workspaceDeleted(id) }} />}
      <main style={treeWidth.style} className={cn("grid min-h-0 flex-1 bg-background md:grid-cols-[var(--tree-width)_var(--tree-divider)_minmax(0,1fr)] md:grid-rows-1 md:overflow-hidden", treeWidth.dragging && "select-none cursor-col-resize", mobileTerminalIdle || mobileSidebarHidden ? "grid-rows-1 overflow-hidden" : "grid-rows-[374px_minmax(620px,1fr)] overflow-auto", focusedId && "grid-cols-1 grid-rows-1 overflow-hidden md:grid-cols-1", settingsRoute && "hidden md:hidden")}>
        <aside id="tree-sidebar" className={cn("flex min-h-0 min-w-0 flex-col border-b bg-sidebar md:border-b-0", mobileSidebarHidden && "hidden md:flex", treeWidth.collapsed && "md:invisible md:overflow-hidden", focusedId && "hidden md:hidden")}>
        {preview && <div className="treer-tour-banner"><span>{t("app.tour_preview_empty_first_run")}</span></div>}
        <nav aria-label={t("app.workspace")} className="px-2 pb-2">
          <button type="button" aria-label={t("app.workspace_overview")} aria-current={mainView === "overview" ? "page" : undefined} onClick={showOverview} className={cn("flex h-7 w-full items-center gap-2 rounded-md px-2 text-left text-[13px] outline-none transition-colors hover:bg-accent focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring motion-reduce:transition-none max-md:h-11 [@media(pointer:coarse)]:h-11", mainView === "overview" && "bg-primary/10 font-medium hover:bg-primary/10 dark:bg-primary/20 dark:hover:bg-primary/20")}><LayoutDashboard aria-hidden="true" className={cn("size-4", mainView === "overview" ? "text-primary" : "text-muted-foreground")} />{t("app.overview")}</button>
        </nav>
        {workspaceId && <SidebarSearch value={sidebarQuery} onChange={setSidebarQuery} />}
        {/* One scroll region: the Tree, then the machines that run it. */}
        <div className="thin-scroll min-h-0 flex-1 overflow-y-auto overflow-x-hidden">
          {workspaceId && <WorkloadTree appTree={{ scope: JSON.stringify([user.user_id, authEpoch, workspaceId]), selected: selectedNodeApp && appEndpoint !== null ? { ownerId: selectedAgentId!, path: appEndpoint.split("/") } : null, open: openAppNode, href: appNodeHref }} prewarm={embedPrewarm} pins={pins} key={workspaceId} workspaceId={workspaceId} agents={snapshot?.agents ?? []} apps={apps} machines={snapshot?.servers ?? []} selectedId={selectedAgentId} onOpen={showAgentTerminal} workloadHref={workloadHref} onOpenInNewTab={openInNewTab} onSettings={openAppSettings} onRename={(agent) => openRename({ kind: "agent", id: agent.agent_id, name: agent.name })} onDelete={(agent) => setDeleteTarget({ kind: "agent", id: agent.agent_id, name: agent.name })} onNew={openCreateAgentOrMachineSetup} onAddMachine={() => void openInstall()} placementRevision={placementRevision} onTreeChange={setWorkloadTree} preview={preview} query={sidebarQuery} />}
          {workspaceId && Boolean(snapshot?.servers.length) && <MachinesSection machines={snapshot?.servers ?? []} agents={snapshot?.agents ?? []} query={sidebarQuery} selectedId={mainView === "machine" ? selectedMachineId : null} onOpen={showMachineOverview} onAdd={() => void openInstall()} />}
        </div>

        {/* Settings live in the top bar and the settings pages; the sidebar only shows what runs here. */}
        <div className="flex shrink-0 items-center gap-1 border-t p-2 pb-[max(0.5rem,env(safe-area-inset-bottom))]">
          <button type="button" onClick={() => openWorkspaceSettings()} disabled={!organizationId} className="flex h-8 min-w-0 flex-1 items-center gap-2 rounded-md px-2 text-left text-[13px] text-muted-foreground outline-none hover:bg-accent hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring disabled:opacity-50 max-md:h-11"><SettingsIcon aria-hidden="true" className="size-4 shrink-0" /><span className="truncate">{workspace ? t("app.workspace_settings") : t("org.create_workspace")}</span></button>
          <button ref={treeWidth.collapseRef} type="button" aria-label={t("resizer.collapse_tree_sidebar")} title={`${t("resizer.collapse_tree_sidebar")} (${sidebarShortcutLabel()})`} aria-keyshortcuts={sidebarShortcutKeys()} aria-expanded="true" aria-controls="tree-sidebar" onClick={treeWidth.toggle} className="hidden size-8 shrink-0 place-items-center rounded-md text-muted-foreground outline-none hover:bg-accent hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring md:grid"><PanelLeftClose aria-hidden="true" className="size-4" /></button>
        </div>
      </aside>
      {!focusedId && <TreeResizer settings={treeWidth} />}

      <section className={cn("min-h-0 min-w-0 flex-col", treeWidth.dragging && "pointer-events-none", mobileTerminalIdle ? "hidden md:flex" : "flex")}>
        {focusedId ? <WorkloadFocusBar path={tabLabels.get(focusedId)?.path ?? focusedId} status={tabLabels.get(focusedId)?.agent.status ? statusLabel(tabLabels.get(focusedId)!.agent.status) : undefined} consoleHref={workloadPath(organizationId, workspaceId, focusedId)} onOpenConsole={() => routingRef.current.navigate({ pathname: location.pathname, search: "", hash: location.hash })} /> : <WorkspaceTabBar key={`${user.user_id}:${workspaceId}`} tabs={tabs.tabs} labels={tabLabels} activeId={mainView === "overview" ? OVERVIEW_TAB : mainView === "terminal" ? selectedAgentId : null} limit={tabs.limit} liveCount={tabs.liveCount} mobile={isMobile} onOpen={selectTab} onClose={closeTab} actions={tabActions} onAction={tabAction} onCloseMany={closeTabs} onBrowseTree={browseMobileTree} sidebarCollapsed={treeWidth.collapsed} expandRef={treeWidth.expandRef} onExpandSidebar={treeWidth.toggle} />}
        <p role="status" className="sr-only">{tabNotice}</p>
        {preparedCreated && <p role="status" className="px-4 py-2 text-sm">{t("prepared.created")} {t(`sessionCopy.${preparedReadiness}`)}</p>}
        {/* Pages carry their own title and Close; the bar stays for the phone back button and audit refresh. */}
        {(mainView === "machine" || mainView === "app") && isMobile && <header className="flex h-12 shrink-0 min-w-0 items-center justify-between gap-4 border-b px-3 sm:px-5">

          <div className="flex min-w-0 items-center gap-1.5 overflow-hidden text-xs text-muted-foreground">
            {isMobile && <IconButton label={t("org.back")} className="mr-1 md:hidden" onClick={mainView === "machine" ? closeMachineOverview : showOverview}><ChevronLeft /></IconButton>}
            <button type="button" onClick={showOverview} aria-label={t("app.back_to_workspace_overview")} className="hidden truncate rounded-sm outline-none hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring sm:block">{workspace?.name ?? t("app.workspace")}</button><ChevronRight className="hidden size-3 shrink-0 sm:block" /><strong className="truncate font-medium text-foreground">{mainView === "app" ? selectedApp?.name ?? t("apps.app") : machineName(selectedMachine, selectedMachineId ?? "")}</strong></div>
        </header>}
        <div className={cn("relative min-h-0 flex-1", mainView !== "overview" && mainView !== "terminal" && "hidden")}>
          <div id="workspace-panel-overview" role="tabpanel" aria-labelledby={isMobile ? undefined : "workspace-tab-overview"} aria-label={isMobile ? t("app.overview") : undefined} hidden={mainView !== "overview"} className="absolute inset-0 overflow-auto">
            <WorkspaceOverview pins={pins} members={members} key={workspaceId} workspace={workspace} agents={snapshot?.agents ?? []} apps={apps} machines={snapshot?.servers ?? []} tree={workloadTree} loading={Boolean(workspaceId && !snapshot)} workloadHref={workloadHref} onOpen={showAgentTerminal} onNewAgent={() => openCreateAgentOrMachineSetup()} onAddMachine={() => void openInstall()} onOpenMachine={showMachineOverview} start={{ hasOrganization: Boolean(organization), onCreateOrganization: () => { setCreateOrganizationName(""); setCreateOrganizationOpen(true) }, onCreateWorkspace: () => { setCreateWorkspaceName(""); setCreateWorkspaceOpen(true) } }} />
          </div>
          {workspaceId && tabs.tabs.map((tab) => {
            const label = tabLabels.get(tab.id)
            if (!label) return null
            const visible = mainView === "terminal" && selectedAgentId === tab.id && !mobileTerminalIdle
            const app = apps.find((entry) => (entry.runtime_agent_id ?? entry.app_id) === tab.id)
            const machine = snapshot?.servers.find((entry) => entry.server_id === label.agent.server_id)
            return <div key={`${workspaceId}:${tab.id}`} id={`workspace-panel-${tab.id}`} role="tabpanel" aria-labelledby={isMobile ? undefined : `workspace-tab-${tab.id}`} aria-label={isMobile ? label.path : undefined} hidden={!visible} className="absolute inset-0 min-h-0 overflow-hidden">
              {selectedNodeApp && appEndpoint !== null && selectedAgentId === tab.id ? <div className="h-full overflow-auto"><AppNodeRoute key={JSON.stringify([user.user_id, authEpoch, workspaceId, selectedNodeApp.app_id, appEndpoint])} workspaceId={workspaceId} appId={selectedNodeApp.app_id} ownerId={tab.id} endpoint={appEndpoint} theme={theme} visible={visible} /></div> : tab.live ? <WorkloadSurface ref={visible ? terminalPaneRef : undefined} workspaceId={workspaceId} agent={label.agent} app={app} machine={machine} path={label.path} tab={tab} embedMode={agentCapabilities.embed} embedOrigin={agentOriginMode === "isolated" ? agentOwnUrls.get(tab.id) : undefined} visible={visible} mobile={isMobile && mobileTerminalOpen} theme={theme} ctrlArmed={ctrlArmed} transformInput={transformTerminalInput} onToggleCtrl={() => setCtrlModifier(!ctrlArmedRef.current)} onCopy={copy} onLogs={(logs) => setShowWorkloadLogs(logs)} actions={<>
                <IconButton label={t("surface.upload_file")} className="size-7 text-muted-foreground max-md:size-11" disabled={!machineOnline(machine)} onClick={() => tabAction(tab.id, "upload")}><Upload /></IconButton>
                <IconButton label={t("app.open_in_new_tab")} className="size-7 text-muted-foreground max-md:size-11" onClick={() => tabAction(tab.id, "newtab")}><ExternalLink /></IconButton>
                <IconButton label={t("app.reconnect_terminal")} className="size-7 text-muted-foreground max-md:size-11" onClick={() => tabAction(tab.id, "reload")}><RotateCw /></IconButton>
              </>} /> : <div className="grid size-full place-content-center gap-3 p-6 text-center"><p className="text-sm font-medium">{t("app.page_unloaded")}</p><p className="max-w-sm text-xs text-muted-foreground">{t("app.this_browser_page_was_unloaded")}</p><Button onClick={() => showAgentTerminal(tab.id)}>{t("app.reload_tab")}</Button></div>}
            </div>
          })}
        </div>
        {mainView !== "overview" && mainView !== "terminal" && <div className="min-h-0 flex-1 overflow-auto">{mainView === "app" ? <AppSettingsView app={selectedApp} machine={selectedAppMachine} onOpen={openAppExternal} onAccess={updateAppAccess} onAction={appLifecycle} onDelete={setDeletingApp} onClose={showOverview} onCopy={copy} /> : mainView === "machine" ? <MachineOverviewView key={`${user.user_id}/${workspaceId}/${selectedMachineId}`} machine={selectedMachine} agents={snapshot?.agents.filter((agent) => agent.server_id === selectedMachineId) ?? []} services={services.filter((service) => service.server_id === selectedMachineId)} virtualHosts={virtualHosts.filter((host) => host.destination_server_id === selectedMachineId)} traffic={traffic} machines={snapshot?.servers ?? []} workspaceId={workspaceId} onOpenAgent={showAgentTerminal} onClose={closeMachineOverview} onCopy={copy} onRename={(machine) => openRename({ kind: "machine", id: machine.server_id, name: machineName(machine) })} onDelete={(machine) => setDeleteTarget({ kind: "machine", id: machine.server_id, name: machineName(machine) })} /> : null}</div>}

      </section>
    </main>
    </div>

    <CommandPalette open={paletteOpen} onOpenChange={setPaletteOpen} items={paletteItems} />
    <ErrorToast message={error} onDismiss={() => setError(null)} />

    <WorkspaceDialogs state={dialogState}
      creationFailure={creationFailure}
      reservationCleaned={reservationCleaned}
      onReservationCleaned={() => { setCreationFailure(null); setCreateAgentError(""); setReservationCleaned(true) }}
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
      uploadScopeActive={route.workspaceId === workspaceId && route.organizationId === organizationId}
      selectedAgentMachine={uploadMachine}
      confirmDelete={confirmDelete}
      workspace={workspace}
      user={user}
    />
  </TooltipProvider>
}
