import { useEffect, useRef, useState, type KeyboardEvent as ReactKeyboardEvent } from "react"
import { Link } from "react-router"
import { Building2, Check, ChevronDown, ListChecks, LogOut, Moon, Plus, Search, Settings as SettingsIcon, Sun, UserRound } from "lucide-react"
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuLabel, DropdownMenuSeparator, DropdownMenuTrigger } from "@/components/ui/dropdown-menu"
import { api, type Organization, type User, type Workspace, type WorkspaceAccess } from "@/lib/api"
import { initials } from "@/lib/control-plane"
import { persistTheme, readTheme, subscribeTheme, type Theme } from "@/lib/theme"
import { cn } from "@/lib/utils"
import { roleLabel, t } from "@/i18n"

type AccessSummary = Pick<WorkspaceAccess, "access_mode" | "current_role">
const accessCache = new Map<string, AccessSummary | null>()
const workspaceCache = new Map<string, Workspace[]>()

export type TopBarProps = {
  user: User
  preview: boolean
  isMobile: boolean
  organizations: Organization[]
  organizationId: string | null
  workspaces: Workspace[]
  workspaceId: string | null
  settingsActive: boolean
  linkSearch: string
  onSelectWorkspace: (organizationId: string, workspaceId: string | null) => void
  onCreateOrganization: () => void
  onCreateWorkspace: (organizationId: string) => void
  onOrganizationSettings: (organizationId: string) => void
  onOpenSearch: () => void
  onProfile: () => void
  onMyOrganizations: () => void
  onTour: () => void
  onLogout: () => void
}

// Where you are and who you are: the context switcher on the left, search in
// the middle, Settings and your own menu on the right.
export function TopBar(props: TopBarProps) {
  const { isMobile, organizations, organizationId, workspaces, workspaceId } = props
  const organization = organizations.find((item) => item.organization_id === organizationId)
  const workspace = workspaces.find((item) => item.workspace_id === workspaceId)
  const [open, setOpen] = useState(false)
  const mac = typeof navigator !== "undefined" && /Mac|iPhone|iPad/.test(navigator.platform)
  const crumb = "flex h-8 min-w-0 items-center gap-1.5 rounded-md px-2 text-[13px] outline-none hover:bg-accent focus-visible:ring-2 focus-visible:ring-ring max-md:h-11"
  return <header className="relative z-30 flex h-11 shrink-0 items-center gap-2 border-b bg-sidebar px-2 max-md:h-12 md:px-3">
    <div data-tour="workspace-select" className="flex min-w-0 flex-1 items-center gap-0.5 md:flex-none">
      <button type="button" aria-haspopup="dialog" aria-expanded={open} aria-label={t("topbar.organization", { name: organization?.name ?? t("app.no_organization") })} onClick={() => setOpen((value) => !value)} className={cn(crumb, "shrink-0 font-semibold")}>
        <span aria-hidden="true" className="grid size-6 shrink-0 place-items-center rounded-md bg-primary text-[10px] font-semibold text-primary-foreground">{initials(organization?.name ?? "Treer")}</span>
        <span className="truncate max-md:sr-only">{organization?.name ?? t("app.no_organization")}</span><ChevronDown aria-hidden="true" className="size-3.5 shrink-0 text-muted-foreground" />
      </button>
      <span aria-hidden="true" className="text-muted-foreground/60">/</span>
      <button type="button" data-tour="create-workspace" aria-haspopup="dialog" aria-expanded={open} aria-label={t("topbar.workspace", { name: workspace?.name ?? t("app.no_workspace") })} onClick={() => setOpen((value) => !value)} className={cn(crumb, "max-md:min-w-0 max-md:flex-1")}>
        <span className="truncate">{workspace?.name ?? t("app.no_workspace")}</span><ChevronDown aria-hidden="true" className="size-3.5 shrink-0 text-muted-foreground" />
      </button>
    </div>
    {open && <ContextPopover {...props} onClose={() => setOpen(false)} />}
    <div className="hidden flex-1 justify-center md:flex">
      <button type="button" onClick={props.onOpenSearch} aria-label={t("topbar.search")} className="flex h-8 w-full max-w-[420px] items-center gap-2 rounded-md border bg-background px-2.5 text-left text-xs text-muted-foreground outline-none hover:border-foreground/30 focus-visible:ring-2 focus-visible:ring-ring">
        <Search aria-hidden="true" className="size-3.5" /><span className="flex-1 truncate">{t("topbar.search_placeholder")}</span><kbd className="rounded border bg-sidebar px-1 font-sans text-[10px]">{mac ? "⌘K" : "Ctrl K"}</kbd>
      </button>
    </div>
    <div className="flex shrink-0 items-center gap-1">
      {isMobile && <button type="button" onClick={props.onOpenSearch} aria-label={t("topbar.search")} className="grid size-11 place-items-center rounded-md text-muted-foreground hover:bg-accent hover:text-foreground"><Search aria-hidden="true" className="size-4" /></button>}
      <Link to={{ pathname: "/settings", search: props.linkSearch }} aria-label={t("settings.title")} aria-current={props.settingsActive ? "page" : undefined} className={cn("flex h-8 items-center gap-1.5 rounded-md px-2 text-xs text-muted-foreground outline-none hover:bg-accent hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring max-md:size-11 max-md:justify-center max-md:px-0", props.settingsActive && "font-semibold text-foreground")}>
        <SettingsIcon aria-hidden="true" className="size-4" /><span className="max-md:hidden">{t("settings.title")}</span>
      </Link>
      <AvatarMenu {...props} />
    </div>
  </header>
}

function ContextPopover({ organizations, organizationId, workspaces, workspaceId, preview, isMobile, onSelectWorkspace, onCreateOrganization, onCreateWorkspace, onOrganizationSettings, onClose }: TopBarProps & { onClose: () => void }) {
  const panel = useRef<HTMLDivElement>(null)
  const [focusOrg, setFocusOrg] = useState(organizationId)
  const [list, setList] = useState<Workspace[] | null>(focusOrg === organizationId ? workspaces : workspaceCache.get(focusOrg ?? "") ?? null)
  const [, setRevision] = useState(0)
  const focused = organizations.find((item) => item.organization_id === focusOrg)
  useEffect(() => {
    panel.current?.querySelector<HTMLElement>("[aria-current='true']")?.focus()
    const outside = (event: PointerEvent) => {
      const target = event.target as Element
      if (!panel.current?.contains(target) && !target.closest?.("[aria-haspopup='dialog']")) onClose()
    }
    const escape = (event: KeyboardEvent) => { if (event.key === "Escape") onClose() }
    document.addEventListener("pointerdown", outside)
    document.addEventListener("keydown", escape)
    return () => { document.removeEventListener("pointerdown", outside); document.removeEventListener("keydown", escape) }
  }, []) // eslint-disable-line react-hooks/exhaustive-deps
  useEffect(() => {
    if (!focusOrg) { setList([]); return }
    if (focusOrg === organizationId) { setList(workspaces); return }
    if (preview) { setList([]); return }
    let active = true
    setList(workspaceCache.get(focusOrg) ?? null)
    api<{ workspaces: Workspace[] }>(`/api/workspaces?organization_id=${encodeURIComponent(focusOrg)}`).then((data) => { workspaceCache.set(focusOrg, data.workspaces); if (active) setList(data.workspaces) }, () => { if (active) setList([]) })
    return () => { active = false }
  }, [focusOrg, organizationId, workspaces, preview])
  useEffect(() => {
    if (preview || !list) return
    let active = true
    for (const item of list.slice(0, 30)) {
      api<{ access: WorkspaceAccess }>(`/api/workspaces/${encodeURIComponent(item.workspace_id)}/access`).then((data) => { accessCache.set(item.workspace_id, { access_mode: data.access.access_mode, current_role: data.access.current_role }); if (active) setRevision((value) => value + 1) }, () => undefined)
    }
    return () => { active = false }
  }, [list, preview])
  function keys(event: ReactKeyboardEvent) {
    if (event.key !== "ArrowDown" && event.key !== "ArrowUp") return
    const items = [...(panel.current?.querySelectorAll<HTMLElement>("[data-context-item]") ?? [])]
    const index = items.indexOf(document.activeElement as HTMLElement)
    event.preventDefault()
    items[(index + (event.key === "ArrowDown" ? 1 : items.length - 1)) % items.length]?.focus()
  }
  const item = "flex w-full items-center justify-between gap-3 rounded-md px-2 py-1.5 text-left text-[13px] outline-none hover:bg-accent focus-visible:bg-accent max-md:min-h-11"
  return <div ref={panel} role="dialog" aria-label={t("topbar.switch_context")} onKeyDown={keys}
    className={cn("absolute top-full z-40 mt-1 grid overflow-hidden rounded-lg border bg-popover text-popover-foreground shadow-xl", isMobile ? "inset-x-2 max-h-[80dvh] grid-cols-1 overflow-y-auto" : "left-2 w-[560px] grid-cols-[210px_minmax(0,1fr)]")}>
    <div className={cn("p-2", isMobile ? "border-b" : "border-r")}>
      <p className="px-2 pb-1 pt-1 text-[11px] font-semibold uppercase tracking-wider text-muted-foreground">{t("topbar.organizations")}</p>
      <ul aria-label={t("topbar.organizations")}>{organizations.map((organization) => <li key={organization.organization_id}>
        <button type="button" data-context-item="" aria-current={organization.organization_id === focusOrg ? "true" : undefined} onClick={() => setFocusOrg(organization.organization_id)} onMouseEnter={() => !isMobile && setFocusOrg(organization.organization_id)} className={cn(item, organization.organization_id === focusOrg && "bg-accent/70")}>
          <span className="truncate">{organization.name}</span><span className="shrink-0 text-[11px] text-muted-foreground">{roleLabel(organization.role)}</span>
        </button>
      </li>)}</ul>
      <div className="my-1.5 border-t" />
      <button type="button" data-context-item="" onClick={() => { onClose(); onCreateOrganization() }} className={cn(item, "justify-start text-muted-foreground")}><Plus aria-hidden="true" className="size-3.5" />{t("org.new_organization")}</button>
      {focusOrg && <button type="button" data-context-item="" onClick={() => { onClose(); onOrganizationSettings(focusOrg) }} className={cn(item, "justify-start text-muted-foreground")}><Building2 aria-hidden="true" className="size-3.5" />{t("app.organization_settings")}</button>}
    </div>
    <div className="min-w-0 p-2">
      <p className="truncate px-2 pb-1 pt-1 text-[11px] font-semibold uppercase tracking-wider text-muted-foreground">{t("topbar.workspaces_in", { name: focused?.name ?? "" })}</p>
      <ul aria-label={t("topbar.workspaces_in", { name: focused?.name ?? "" })}>{(list ?? []).map((workspace) => {
        const access = accessCache.get(workspace.workspace_id)
        const current = workspace.workspace_id === workspaceId && focusOrg === organizationId
        return <li key={workspace.workspace_id}>
          <button type="button" data-context-item="" aria-label={t("topbar.open_workspace", { name: workspace.name })} onClick={() => { onClose(); if (focusOrg) onSelectWorkspace(focusOrg, workspace.workspace_id) }} className={cn(item, current && "bg-primary/10")}>
            <span className="flex min-w-0 items-center gap-2"><Check aria-hidden="true" className={cn("size-3.5 shrink-0", !current && "invisible")} /><span className="truncate">{workspace.name}</span></span>
            <span className="flex shrink-0 items-center gap-2 text-[11px]">{access?.access_mode === "restricted" && <span className="text-amber-700 dark:text-amber-400">{t("hub.restricted")}</span>}{access && <span className="text-muted-foreground">{roleLabel(access.current_role)}</span>}</span>
          </button>
        </li>
      })}{list === null && <li className="px-2 py-2 text-xs text-muted-foreground">{t("hub.loading")}</li>}{list?.length === 0 && <li className="px-2 py-2 text-xs text-muted-foreground">{t("org.no_workspaces_yet")}</li>}</ul>
      <div className="my-1.5 border-t" />
      {focusOrg && <button type="button" data-context-item="" onClick={() => { onClose(); onCreateWorkspace(focusOrg) }} className={cn(item, "justify-start text-muted-foreground")}><Plus aria-hidden="true" className="size-3.5" />{t("topbar.new_workspace")}</button>}
    </div>
  </div>
}

function AvatarMenu({ user, preview, onProfile, onMyOrganizations, onTour, onLogout }: TopBarProps) {
  const [theme, setTheme] = useState<Theme>(() => readTheme())
  useEffect(() => subscribeTheme(setTheme), [])
  const item = "min-h-11 gap-3 px-3 text-sm sm:min-h-0 sm:gap-2 sm:px-2 sm:text-xs"
  return <DropdownMenu>
    <DropdownMenuTrigger asChild>
      <button type="button" aria-label={t("userMenu.trigger")} className="ml-1 grid size-8 place-items-center rounded-full bg-[#694a73] text-xs font-semibold text-white outline-none ring-offset-2 ring-offset-sidebar focus-visible:ring-2 focus-visible:ring-ring max-md:size-11">{initials(user.preferred_name).slice(0, 1)}</button>
    </DropdownMenuTrigger>
    <DropdownMenuContent align="end" collisionPadding={8} className="max-h-[calc(100dvh-1rem)] w-[min(20rem,calc(100vw-1rem))] overflow-y-auto sm:w-60">
      <DropdownMenuLabel className="px-3 py-2.5 sm:px-2 sm:py-1.5">
        <span className="block truncate text-sm sm:text-xs">{user.preferred_name}</span>
        <span className="mt-0.5 block truncate text-xs font-normal text-muted-foreground sm:text-[10px]">{user.email}</span>
      </DropdownMenuLabel>
      <DropdownMenuSeparator />
      <DropdownMenuItem className={item} onSelect={onProfile}><UserRound />{t("hub.profile")}</DropdownMenuItem>
      <DropdownMenuItem className={item} onSelect={onMyOrganizations}><Building2 />{t("hub.my_organizations")}</DropdownMenuItem>
      <DropdownMenuItem className={item} onSelect={() => { const next = theme === "dark" ? "light" : "dark"; persistTheme(next); setTheme(next) }}>{theme === "dark" ? <Sun /> : <Moon />}{theme === "dark" ? t("topbar.light_theme") : t("topbar.dark_theme")}</DropdownMenuItem>
      <DropdownMenuItem className={item} onSelect={onTour}><ListChecks />{t("userMenu.tour")}</DropdownMenuItem>
      <DropdownMenuSeparator />
      <DropdownMenuItem className={item} onSelect={onLogout} disabled={preview}><LogOut />{t("userMenu.logout")}</DropdownMenuItem>
    </DropdownMenuContent>
  </DropdownMenu>
}
