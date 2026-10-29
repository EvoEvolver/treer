import { useCallback, useEffect, useState, type ReactNode } from "react"
import { Link, Navigate, useLocation, useNavigate } from "react-router"
import { ArrowLeft, ChevronLeft, ChevronRight } from "lucide-react"
import { api, type Agent, type AgentLaunchProfile, type AppDeployment, type Machine, type Organization, type User, type Workspace, type WorkspaceAccess } from "@/lib/api"
import { defaultSection, organizationSettingsPath, personalSettingsPath, settingsListPath, validSection, workspaceSettingsPath, type SettingsRoute } from "@/lib/console-routes"
import { cn } from "@/lib/utils"
import { t, type MessageKey } from "@/i18n"
import type { OrgDirectory } from "@/components/settings-hub/use-org-directory"
import { OrganizationsPage, PreferencesPage, ProfilePage, SessionsPage } from "@/components/settings-hub/personal-pages"
import { InvitationsPage, GroupsPage, MembersPage, OrgAuditPage, OrgDangerPage, OrgGeneralPage, OrgWorkspacesPage, type OrgPageContext } from "@/components/settings-hub/org-pages"
import { WorkspaceAccessPage, WorkspaceAuditPage, WorkspaceDangerPage, WorkspaceGeneralPage, WorkspaceMachinesPage, WorkspacePolicyPage, WorkspaceProfilesPage, type WorkspacePageContext } from "@/components/settings-hub/workspace-pages"

export type SettingsHubProps = {
  route: SettingsRoute
  isMobile: boolean
  preview: boolean
  user: User
  onUserChange: (user: User) => void
  organizations: Organization[]
  organizationsLoaded: boolean
  currentOrganizationId: string | null
  workspaces: Workspace[]
  workspacesOrganizationId: string | null
  workspacesLoaded: boolean
  currentWorkspaceId: string | null
  machines: Machine[] | null
  agents: Agent[]
  apps: AppDeployment[]
  profiles: AgentLaunchProfile[]
  profilesLoading: boolean
  directory: OrgDirectory
  showError: (reason: unknown) => void
  copy: (value: string) => Promise<void>
  logout: () => void
  exit: () => void
  createOrganization: () => void
  createWorkspace: () => void
  refreshOrganizations: () => Promise<void>
  organizationGone: (organizationId: string) => void
  workspace: Omit<WorkspacePageContext, "organization" | "workspace" | "user" | "preview" | "directory" | "access" | "accessError" | "reloadAccess" | "machines" | "agents" | "apps" | "profiles" | "profilesLoading" | "showError" | "copy">
}

type NavItem = { section: string; label: MessageKey; path: string }

const personalItems: { section: string; label: MessageKey }[] = [
  { section: "profile", label: "hub.profile" }, { section: "sessions", label: "hub.sessions" }, { section: "preferences", label: "hub.preferences" }, { section: "organizations", label: "hub.organizations" },
]
const organizationItems: { section: string; label: MessageKey }[] = [
  { section: "general", label: "settings.nav.general" }, { section: "members", label: "org.members" }, { section: "invitations", label: "hub.invitations" }, { section: "groups", label: "org.groups" }, { section: "workspaces", label: "org.workspaces" }, { section: "audit", label: "hub.audit_log" }, { section: "danger", label: "apps.danger_zone" },
]
const workspaceItems: { section: string; label: MessageKey }[] = [
  { section: "general", label: "settings.nav.general" }, { section: "access", label: "hub.access" }, { section: "machines", label: "wsettings.machines" }, { section: "launch-profiles", label: "wsettings.launch_profiles" }, { section: "policy", label: "hub.policy" }, { section: "audit", label: "hub.audit_log" }, { section: "danger", label: "apps.danger_zone" },
]

export function SettingsHub(props: SettingsHubProps) {
  const { route, isMobile, preview, organizations, workspaces } = props
  const location = useLocation()
  const navigate = useNavigate()
  const linkSearch = preview ? "?tour=preview" : ""
  const organizationId = route.scope === "personal" ? props.currentOrganizationId : route.organizationId
  const organization = organizations.find((item) => item.organization_id === organizationId)
  const workspaceList = props.workspacesOrganizationId === organizationId ? workspaces : []
  const workspaceId = route.scope === "workspace" ? route.workspaceId : props.workspacesOrganizationId === organizationId ? props.currentWorkspaceId : null
  const workspace = workspaceList.find((item) => item.workspace_id === workspaceId)
  const [access, setAccess] = useState<{ id: string; value: WorkspaceAccess | null; error: string | null } | null>(null)
  const accessId = route.scope === "workspace" && workspace ? workspace.workspace_id : null
  const reloadAccess = useCallback(async () => {
    if (!accessId || preview) return
    try { const data = await api<{ access: WorkspaceAccess }>(`/api/workspaces/${encodeURIComponent(accessId)}/access`); setAccess({ id: accessId, value: data.access, error: null }) }
    catch (reason) { setAccess({ id: accessId, value: null, error: reason instanceof Error ? reason.message : String(reason) }) }
  }, [accessId, preview])
  useEffect(() => { void reloadAccess() }, [reloadAccess])
  useEffect(() => { document.getElementById("settings-content")?.scrollTo({ top: 0 }) }, [location.pathname])

  const groups: { id: string; title: string; items: NavItem[] }[] = [
    { id: "personal", title: t("hub.group_you"), items: personalItems.map((item) => ({ ...item, path: personalSettingsPath(item.section as never) })) },
    ...(organization ? [{ id: "organization", title: organization.name, items: organizationItems.map((item) => ({ ...item, path: organizationSettingsPath(organization.organization_id, item.section as never) })) }] : []),
    ...(organization && workspace ? [{ id: "workspace", title: t("hub.group_workspace", { name: workspace.name }), items: workspaceItems.map((item) => ({ ...item, path: workspaceSettingsPath(organization.organization_id, workspace.workspace_id, item.section as never) })) }] : []),
  ]
  const activeGroup = route.scope === "personal" ? "personal" : route.scope
  const exitLabel = workspace && organization ? t("hub.back_to", { name: workspace.name }) : t("hub.back_to_console")

  const nav = <nav aria-label={t("settings.title")} className="space-y-5">
    <button type="button" onClick={props.exit} className="flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-left text-xs text-muted-foreground outline-none hover:bg-accent hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring max-md:min-h-11 max-md:text-sm"><ArrowLeft aria-hidden="true" className="size-3.5 shrink-0" /><span className="truncate">{exitLabel}</span></button>
    {groups.map((group) => <div key={group.id} role="group" aria-label={group.title}>
      <h2 className="mb-1 truncate px-2 text-[11px] font-semibold uppercase tracking-wider text-muted-foreground">{group.title}</h2>
      <ul>{group.items.map((item) => {
        const current = group.id === activeGroup && (route.section ?? (isMobile ? null : defaultSection(route.scope))) === item.section
        return <li key={item.section}><Link to={{ pathname: item.path, search: linkSearch }} aria-current={current ? "page" : undefined}
          className={cn("flex items-center justify-between rounded-md px-2 py-1.5 text-[13px] outline-none hover:bg-accent focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring max-md:min-h-11 max-md:text-sm", current && "bg-primary/10 font-medium text-foreground hover:bg-primary/10 dark:bg-primary/20")}>
          <span className="truncate">{t(item.label)}{item.section === "organizations" && organizations.length ? ` (${organizations.length})` : ""}</span>{isMobile && <ChevronRight aria-hidden="true" className="size-4 text-muted-foreground" />}
        </Link></li>
      })}</ul>
    </div>)}
  </nav>

  let page: ReactNode
  if (!validSection(route)) page = <NotFound text={t("hub.page_not_found")} />
  else if (route.scope === "personal") {
    const section = route.section ?? "profile"
    page = section === "profile" ? <ProfilePage user={props.user} preview={preview} onUserChange={props.onUserChange} />
      : section === "sessions" ? <SessionsPage preview={preview} onLogout={props.logout} />
      : section === "preferences" ? <PreferencesPage />
      : <OrganizationsPage organizations={organizations} currentOrganizationId={props.currentOrganizationId} onCreate={props.createOrganization} linkSearch={linkSearch} />
  } else if (!organization) page = props.organizationsLoaded ? <NotFound text={t("hub.organization_unavailable")} /> : <Loading />
  else if (route.scope === "organization") {
    const context: OrgPageContext = { organization, user: props.user, preview, directory: props.directory, workspaces: workspaceList, linkSearch, showError: props.showError, copy: props.copy, refreshOrganizations: props.refreshOrganizations, createWorkspace: props.createWorkspace, organizationGone: props.organizationGone }
    const section = route.section ?? "general"
    page = section === "general" ? <OrgGeneralPage context={context} /> : section === "members" ? <MembersPage context={context} /> : section === "invitations" ? <InvitationsPage context={context} />
      : section === "groups" ? <GroupsPage context={context} /> : section === "workspaces" ? <OrgWorkspacesPage context={context} /> : section === "audit" ? <OrgAuditPage context={context} /> : <OrgDangerPage context={context} />
  } else if (!workspace) page = props.workspacesLoaded && props.workspacesOrganizationId === organizationId ? <NotFound text={t("hub.workspace_unavailable")} /> : <Loading />
  else {
    const machines = props.currentWorkspaceId === workspace.workspace_id ? props.machines : null
    const context: WorkspacePageContext = { ...props.workspace, organization, workspace, user: props.user, preview, directory: props.directory, access: access?.id === workspace.workspace_id ? access.value : null, accessError: access?.id === workspace.workspace_id ? access.error : null, reloadAccess, machines, agents: props.agents, apps: props.apps, profiles: props.profiles, profilesLoading: props.profilesLoading, showError: props.showError, copy: props.copy }
    const section = route.section ?? "general"
    page = section === "general" ? <WorkspaceGeneralPage context={context} /> : section === "access" ? <WorkspaceAccessPage context={context} /> : section === "machines" ? <WorkspaceMachinesPage context={context} />
      : section === "launch-profiles" ? <WorkspaceProfilesPage context={context} /> : section === "policy" ? <WorkspacePolicyPage context={context} /> : section === "audit" ? <WorkspaceAuditPage context={context} /> : <WorkspaceDangerPage context={context} />
  }

  // Desktop has no list page: the scope's first page is shown at its root URL.
  if (!isMobile && route.section === null) {
    const path = route.scope === "personal" ? personalSettingsPath("profile") : route.scope === "organization" ? organizationSettingsPath(route.organizationId, "general") : workspaceSettingsPath(route.organizationId, route.workspaceId, "general")
    return <Navigate replace to={{ pathname: path, search: location.search }} />
  }

  function back() {
    // A real Back when the list is the previous entry; otherwise go up to the list.
    const index = (window.history.state as { idx?: number } | null)?.idx ?? 0
    if (index > 0 && (location.state as { fromList?: boolean } | null)?.fromList) navigate(-1)
    else navigate({ pathname: settingsListPath(route), search: linkSearch })
  }

  if (isMobile) return <main aria-label={t("settings.title")} id="settings-content" className="min-h-0 flex-1 overflow-y-auto bg-background">
    {route.section === null ? <div className="px-4 py-4 pb-[max(1rem,env(safe-area-inset-bottom))]"><h1 className="mb-4 px-2 text-xl font-semibold">{t("settings.title")}</h1><MobileNavState>{nav}</MobileNavState></div>
      : <div className="px-4 pb-[max(1.5rem,env(safe-area-inset-bottom))]">
        <div className="sticky top-0 z-10 -mx-4 mb-4 flex h-12 items-center border-b bg-background px-2"><button type="button" onClick={back} className="flex min-h-11 items-center gap-1 rounded-md px-2 text-sm text-primary outline-none focus-visible:ring-2 focus-visible:ring-ring"><ChevronLeft aria-hidden="true" className="size-4" />{t("org.back")}</button><span className="ml-1 truncate text-xs text-muted-foreground">{groups.find((group) => group.id === activeGroup)?.title}</span></div>
        {page}
      </div>}
  </main>

  return <div className="grid min-h-0 flex-1 grid-cols-[232px_minmax(0,1fr)] bg-background">
    <div className="thin-scroll min-h-0 overflow-y-auto border-r bg-sidebar px-3 py-4">{nav}</div>
    <main id="settings-content" aria-label={t("settings.title")} className="min-h-0 overflow-y-auto"><div className="mx-auto w-full max-w-[920px] px-6 py-8 lg:px-10">{page}</div></main>
  </div>
}

// On phones the list marks its links so the detail page's Back can return with history.
function MobileNavState({ children }: { children: ReactNode }) {
  const navigate = useNavigate()
  return <div onClickCapture={(event) => {
    const anchor = (event.target as HTMLElement).closest("a")
    if (!anchor || event.metaKey || event.ctrlKey || event.shiftKey || event.button !== 0) return
    const url = new URL(anchor.href)
    if (url.origin !== window.location.origin) return
    event.preventDefault()
    navigate({ pathname: url.pathname, search: url.search }, { state: { fromList: true } })
  }}>{children}</div>
}

function NotFound({ text }: { text: string }) {
  return <div role="alert" className="rounded-lg border border-dashed p-8 text-center text-sm text-muted-foreground">{text}</div>
}

function Loading() {
  return <p role="status" className="text-xs text-muted-foreground">{t("hub.loading")}</p>
}
