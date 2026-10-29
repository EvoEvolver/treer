import { FormEvent, useEffect, useMemo, useState } from "react"
import { Copy, Download, ExternalLink, MoreHorizontal, Pencil, Play, Plus, RotateCw, ShieldCheck, Square, Trash2 } from "lucide-react"
import { Button } from "@/components/ui/button"
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuSeparator, DropdownMenuTrigger } from "@/components/ui/dropdown-menu"
import { Input } from "@/components/ui/input"
import { Label } from "@/components/ui/label"
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select"
import { Status } from "@/components/control-plane-ui"
import { LaunchProfilesList } from "@/components/launch-profiles-list"
import { PolicyPublishAuthoritySettings } from "@/components/policy-publish-authority"
import { TrafficOverview } from "@/components/traffic-overview"
import { WorkspaceAudit } from "@/components/workspace-audit"
import { api, machineName, type Agent, type AgentLaunchProfile, type AppDeployment, type Machine, type Organization, type PolicyProviderStatus, type User, type Workspace, type WorkspaceAccess } from "@/lib/api"
import { computeEffectiveAccess, decodeEffectiveAccess, losesOwnership, type AccessVia, type EffectiveAccess } from "@/lib/access"
import { buildLabel, machineDisplayName, machineRecoveryCommands } from "@/lib/control-plane"
import { cn } from "@/lib/utils"
import { roleLabel, t } from "@/i18n"
import type { OrgDirectory } from "@/components/settings-hub/use-org-directory"
import { ApiEquivalent, Badge, ConfirmDialog, ErrorNote, PageHeader, PermissionNote, Picker, Section, Unavailable, UnsavedBar } from "@/components/settings-hub/ui"

export type WorkspacePageContext = {
  organization: Organization
  workspace: Workspace
  user: User
  preview: boolean
  directory: OrgDirectory
  access: WorkspaceAccess | null
  accessError: string | null
  reloadAccess: () => Promise<void>
  machines: Machine[] | null
  agents: Agent[]
  apps: AppDeployment[]
  profiles: AgentLaunchProfile[]
  profilesLoading: boolean
  showError: (reason: unknown) => void
  copy: (value: string) => Promise<void>
  renameWorkspace: (name: string) => Promise<void>
  addMachine: () => void
  openMachine: (machineId: string) => void
  renameMachine: (machine: Machine) => void
  removeMachines: (machineIds: string[]) => Promise<void>
  policyAppInstalled: (app: AppDeployment) => void
  refreshProfiles: () => void
  newProfile: () => void
  editProfile: (profile: AgentLaunchProfile) => void
  launchProfile: (profile: AgentLaunchProfile) => void
  deleteProfile: (profile: AgentLaunchProfile) => void
  workspaceDeleted: (workspaceId: string) => void
}

const wsPath = (workspace: Workspace) => `/api/workspaces/${encodeURIComponent(workspace.workspace_id)}`
const message = (reason: unknown) => reason instanceof Error ? reason.message : String(reason)
const isOwnerOf = (context: WorkspacePageContext) => context.access?.current_role === "owner" || context.preview

function OwnerNote({ context, children }: { context: WorkspacePageContext; children: string }) {
  if (isOwnerOf(context) || !context.access) return null
  return <PermissionNote>{children}</PermissionNote>
}

export function WorkspaceGeneralPage({ context }: { context: WorkspacePageContext }) {
  const { workspace, organization, preview, machines } = context
  const [name, setName] = useState(workspace.name)
  const [saving, setSaving] = useState(false)
  const [error, setError] = useState<string | null>(null)
  useEffect(() => setName(workspace.name), [workspace.name])
  async function save(event: FormEvent) {
    event.preventDefault()
    setSaving(true); setError(null)
    try { await context.renameWorkspace(name) } catch (reason) { setError(message(reason)) } finally { setSaving(false) }
  }
  return <>
    <PageHeader title={t("settings.nav.general")} description={t("wsettings.the_display_name_shown_to")} />
    {!preview && machines?.length === 0 && <div role="status" className="mb-6 flex flex-col items-start gap-3 rounded-md border bg-muted/50 p-4 text-sm sm:flex-row sm:items-center sm:justify-between">
      <div><p className="font-medium">{t("wsettings.add_a_machine_to_create")}</p><p className="mt-1 text-xs text-muted-foreground">{t("wsettings.agents_run_on_machines_connect")}</p></div>
      <Button variant="outline" className="shrink-0 max-md:h-11" onClick={context.addMachine}>{t("wsettings.set_up_a_machine")}</Button>
    </div>}
    <form onSubmit={save} className="max-w-md space-y-3">
      <div className="space-y-2"><Label htmlFor="workspace-name">{t("wsettings.workspace_name")}</Label><Input id="workspace-name" value={name} onChange={(event) => setName(event.target.value)} required maxLength={80} disabled={preview} className="max-md:h-11 max-md:text-base" /></div>
      <ErrorNote message={error} />
      <Button type="submit" className="max-md:h-11" disabled={saving || !name.trim() || name.trim() === workspace.name}>{saving ? t("common.saving") : t("common.save")}</Button>
    </form>
    <dl className="mt-6 grid max-w-md grid-cols-[auto_minmax(0,1fr)] gap-x-4 gap-y-2 text-xs">
      <dt className="text-muted-foreground">{t("hub.workspace_id")}</dt><dd className="truncate font-mono">{workspace.workspace_id}</dd>
      <dt className="text-muted-foreground">{t("app.organization")}</dt><dd className="truncate">{organization.name}</dd>
      {context.access && <><dt className="text-muted-foreground">{t("hub.your_role")}</dt><dd>{roleLabel(context.access.current_role)}</dd></>}
    </dl>
    <ApiEquivalent calls={[{ method: "PATCH", path: `/api/workspaces/${workspace.workspace_id}`, note: "{ name }" }]} />
  </>
}

type Draft = { mode?: WorkspaceAccess["access_mode"]; users: Record<string, WorkspaceAccess["current_role"] | null>; groups: Record<string, WorkspaceAccess["current_role"] | null> }
const emptyDraft: Draft = { users: {}, groups: {} }

function viaLabel(via: AccessVia) {
  if (via.kind === "direct") return t("hub.via_direct", { role: roleLabel(via.detail) })
  if (via.kind === "group") return t("hub.via_group", { group: via.detail })
  return via.detail === "organization" ? t("hub.via_everyone") : t("hub.via_org_role", { role: roleLabel(via.detail) })
}

export function WorkspaceAccessPage({ context }: { context: WorkspacePageContext }) {
  const { workspace, access, directory, user, organization, preview } = context
  const isOwner = isOwnerOf(context)
  const [effective, setEffective] = useState<EffectiveAccess | null | "unavailable">(null)
  const [draft, setDraft] = useState<Draft>(emptyDraft)
  const [saving, setSaving] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [revision, setRevision] = useState(0)
  useEffect(() => {
    if (preview) return
    let active = true
    api<unknown>(`${wsPath(workspace)}/access/effective`).then((value) => { if (active) setEffective(decodeEffectiveAccess(value) ?? "unavailable") }, () => { if (active) setEffective("unavailable") })
    return () => { active = false }
  }, [workspace.workspace_id, preview, revision])
  useEffect(() => setDraft(emptyDraft), [workspace.workspace_id])
  const next: WorkspaceAccess | null = useMemo(() => {
    if (!access) return null
    const members = [...access.members.filter((grant) => draft.users[grant.user_id] !== null).map((grant) => draft.users[grant.user_id] ? { ...grant, role: draft.users[grant.user_id]! } : grant),
      ...Object.entries(draft.users).filter(([id, role]) => role && !access.members.some((grant) => grant.user_id === id)).map(([id, role]) => { const member = directory.members.find((item) => item.user_id === id); return { user_id: id, preferred_name: member?.preferred_name ?? id, email: member?.email ?? "", role: role! } })]
    const groups = [...access.groups.filter((grant) => draft.groups[grant.group_id] !== null).map((grant) => draft.groups[grant.group_id] ? { ...grant, role: draft.groups[grant.group_id]! } : grant),
      ...Object.entries(draft.groups).filter(([id, role]) => role && !access.groups.some((grant) => grant.group_id === id)).map(([id, role]) => { const group = directory.groups.find((item) => item.group_id === id); return { group_id: id, name: group?.name ?? id, role: role!, member_count: group?.member_ids.length ?? 0 } })]
    return { ...access, access_mode: draft.mode ?? access.access_mode, members, groups }
  }, [access, draft, directory.members, directory.groups])
  const changes = access ? [
    ...(draft.mode && draft.mode !== access.access_mode ? [["mode", draft.mode] as const] : []),
    ...Object.entries(draft.users).filter(([id, role]) => (access.members.find((grant) => grant.user_id === id)?.role ?? null) !== role),
    ...Object.entries(draft.groups).filter(([id, role]) => (access.groups.find((grant) => grant.group_id === id)?.role ?? null) !== role),
  ] : []
  const orgManager = organization.role === "owner" || organization.role === "admin"
  const lockout = Boolean(changes.length && next && !orgManager && directory.loaded && losesOwnership(user.user_id, next, directory.members, directory.groups))
  const list = changes.length || effective === "unavailable" || effective === null
    ? next && directory.loaded ? computeEffectiveAccess(next, directory.members, directory.groups) : null
    : effective
  async function save() {
    if (!access || !next || lockout) return
    setSaving(true); setError(null)
    try {
      const base = wsPath(workspace)
      if (draft.mode && draft.mode !== access.access_mode) await api(`${base}/access`, { method: "PATCH", body: JSON.stringify({ access_mode: draft.mode }) })
      for (const [id, role] of Object.entries(draft.users)) if ((access.members.find((grant) => grant.user_id === id)?.role ?? null) !== role) await api(`${base}/access/users/${encodeURIComponent(id)}`, role ? { method: "PUT", body: JSON.stringify({ role }) } : { method: "DELETE" })
      for (const [id, role] of Object.entries(draft.groups)) if ((access.groups.find((grant) => grant.group_id === id)?.role ?? null) !== role) await api(`${base}/access/groups/${encodeURIComponent(id)}`, role ? { method: "PUT", body: JSON.stringify({ role }) } : { method: "DELETE" })
      setDraft(emptyDraft)
    } catch (reason) { setError(message(reason)) }
    finally { await context.reloadAccess(); setRevision((value) => value + 1); setSaving(false) }
  }
  const options = [
    ...directory.groups.filter((group) => !next?.groups.some((grant) => grant.group_id === group.group_id)).map((group) => ({ id: group.group_id, label: group.name, detail: t("org.member_count", { count: group.member_ids.length }), kind: t("hub.kind_group") })),
    ...directory.members.filter((member) => !next?.members.some((grant) => grant.user_id === member.user_id)).map((member) => ({ id: member.user_id, label: member.preferred_name, detail: member.email, kind: t("hub.kind_person") })),
  ]
  const roleSelect = (label: string, value: WorkspaceAccess["current_role"], onChange: (role: WorkspaceAccess["current_role"]) => void) => <Select value={value} onValueChange={onChange}><SelectTrigger aria-label={label} className="h-8 w-28 text-xs max-md:h-11"><SelectValue /></SelectTrigger><SelectContent><SelectItem value="member">{roleLabel("member")}</SelectItem><SelectItem value="owner">{roleLabel("owner")}</SelectItem></SelectContent></Select>
  return <>
    <PageHeader title={t("hub.access")} description={t("hub.access_description")} />
    <OwnerNote context={context}>{t("hub.only_ws_owners_access")}</OwnerNote>
    <ErrorNote message={context.accessError} />
    {access && <>
      <fieldset className="mb-5 space-y-2" disabled={!isOwner || preview}>
        <legend className="mb-2 text-sm font-medium">{t("apps.who_can_access")}</legend>
        {(["organization", "restricted"] as const).map((mode) => <label key={mode} className="flex cursor-pointer items-start gap-2 rounded-md border px-3 py-2 text-sm has-[:checked]:border-primary has-[:checked]:bg-primary/5 max-md:min-h-11">
          <input type="radio" name="access-mode" className="mt-1 accent-[hsl(var(--primary))]" checked={(next?.access_mode ?? access.access_mode) === mode} onChange={() => setDraft((current) => ({ ...current, mode }))} />
          <span><span className="block font-medium">{mode === "organization" ? t("wsettings.everyone_in_organization") : t("wsettings.selected_people_and_groups")}</span><span className="block text-xs text-muted-foreground">{mode === "organization" ? t("hub.mode_organization_body") : t("hub.mode_restricted_body")}</span></span>
        </label>)}
      </fieldset>
      <details className="mb-5 rounded-md border px-3 py-2 text-xs"><summary className="cursor-pointer font-medium max-md:min-h-11 max-md:py-3">{t("hub.role_mapping")}</summary><ul className="mt-2 list-disc space-y-1 pl-5 text-muted-foreground"><li>{t("hub.role_mapping_org")}</li><li>{t("hub.role_mapping_owner")}</li><li>{t("hub.role_mapping_member")}</li></ul></details>
      {isOwner && <div className="mb-4 max-w-md"><Picker label={t("hub.add_people_or_groups")} placeholder={t("hub.add_people_or_groups")} options={options} disabled={preview}
        onPick={(option) => setDraft((current) => option.kind === t("hub.kind_group") ? { ...current, groups: { ...current.groups, [option.id]: "member" } } : { ...current, users: { ...current.users, [option.id]: "member" } })} /></div>}
      <h2 className="mb-2 text-sm font-semibold">{t("hub.who_has_access")}{changes.length > 0 && <span className="ml-2 font-normal text-muted-foreground">{t("hub.preview_unsaved")}</span>}</h2>
      <ul aria-label={t("hub.who_has_access")} className="divide-y rounded-lg border">
        {next?.groups.map((grant) => <li key={`group:${grant.group_id}`} className="flex flex-wrap items-center justify-between gap-3 px-3 py-2">
          <div className="min-w-0"><p className="truncate text-sm font-medium">{grant.name} <Badge>{t("hub.kind_group")}</Badge></p><p className="text-xs text-muted-foreground">{t("org.member_count", { count: grant.member_count })}</p></div>
          {isOwner ? <div className="flex items-center gap-1">{roleSelect(t("hub.role_for", { name: grant.name }), grant.role, (role) => setDraft((current) => ({ ...current, groups: { ...current.groups, [grant.group_id]: role } })))}<Button size="icon" variant="ghost" className="size-8 max-md:size-11" aria-label={t("wsettings.remove_grant", { name: grant.name })} onClick={() => setDraft((current) => ({ ...current, groups: { ...current.groups, [grant.group_id]: null } }))}><Trash2 /></Button></div> : <span className="text-xs">{roleLabel(grant.role)}</span>}
        </li>)}
        {list?.members.map((entry) => {
          const direct = next?.members.find((grant) => grant.user_id === entry.user.id)
          return <li key={`user:${entry.user.id}`} className="flex flex-wrap items-center justify-between gap-3 px-3 py-2">
            <div className="min-w-0"><p className="truncate text-sm font-medium">{entry.user.name} {entry.user.id === user.user_id && <Badge>{t("hub.you")}</Badge>} <Badge tone={entry.role === "owner" ? "primary" : "muted"}>{roleLabel(entry.role)}</Badge></p><p className="truncate text-xs text-muted-foreground">{entry.via.map(viaLabel).join(" · ")}</p></div>
            {isOwner && direct ? <div className="flex items-center gap-1">{roleSelect(t("hub.role_for", { name: entry.user.name }), direct.role, (role) => setDraft((current) => ({ ...current, users: { ...current.users, [entry.user.id]: role } })))}<Button size="icon" variant="ghost" className="size-8 max-md:size-11" aria-label={t("wsettings.remove_grant", { name: entry.user.name })} onClick={() => setDraft((current) => ({ ...current, users: { ...current.users, [entry.user.id]: null } }))}><Trash2 /></Button></div> : null}
          </li>
        })}
        {!list && <li className="px-3 py-6 text-center text-xs text-muted-foreground">{t("hub.loading")}</li>}
        {list && !list.members.length && !next?.groups.length && <li className="px-3 py-6 text-center text-xs text-muted-foreground">{t("hub.nobody_has_access")}</li>}
      </ul>
      <ErrorNote message={error} />
      <UnsavedBar count={changes.length} saving={saving} warning={lockout ? t("hub.lockout_warning") : null} onSave={() => void save()} onDiscard={() => setDraft(emptyDraft)} />
    </>}
    <ApiEquivalent calls={[{ method: "GET", path: `/api/workspaces/${workspace.workspace_id}/access/effective` }, { method: "PATCH", path: `/api/workspaces/${workspace.workspace_id}/access`, note: "{ access_mode }" }, { method: "PUT", path: `/api/workspaces/${workspace.workspace_id}/access/users/{user_id}`, note: "{ role }" }, { method: "PUT", path: `/api/workspaces/${workspace.workspace_id}/access/groups/{group_id}`, note: "{ role }" }, { method: "DELETE", path: `/api/workspaces/${workspace.workspace_id}/access/users/{user_id}` }]} />
  </>
}

export function WorkspaceMachinesPage({ context }: { context: WorkspacePageContext }) {
  const { workspace, machines, agents, preview } = context
  const [selected, setSelected] = useState<string[]>([])
  const [removing, setRemoving] = useState<Machine[] | null>(null)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const list = machines ?? []
  useEffect(() => { setSelected((current) => current.filter((id) => list.some((machine) => machine.server_id === id))) }, [machines]) // eslint-disable-line react-hooks/exhaustive-deps
  const commands = machineRecoveryCommands(workspace.workspace_id)
  const allSelected = list.length > 0 && selected.length === list.length
  async function remove() {
    if (!removing) return
    setBusy(true); setError(null)
    try { await context.removeMachines(removing.map((machine) => machine.server_id)); setRemoving(null); setSelected([]) }
    catch (reason) { setError(message(reason)) }
    finally { setBusy(false) }
  }
  const affected = removing ? agents.filter((agent) => removing.some((machine) => machine.server_id === agent.server_id)) : []
  return <section data-tour="workspace-machines">
    <PageHeader title={t("wsettings.machines")} description={t("wsettings.hosts_enrolled_in_this_workspace")} actions={<span data-tour="add-machine"><Button variant="outline" className="max-md:h-11" onClick={context.addMachine}><Plus />{t("dialogs.add_machine")}</Button></span>} />
    {list.length > 0 && <div className="mb-2 flex min-h-9 flex-wrap items-center gap-3 text-xs">
      <label className="flex items-center gap-2 max-md:min-h-11"><input type="checkbox" className="size-4 accent-[hsl(var(--primary))]" checked={allSelected} onChange={() => setSelected(allSelected ? [] : list.map((machine) => machine.server_id))} aria-label={t("hub.select_all_machines")} />{t("hub.selected_count", { count: selected.length })}</label>
      {selected.length > 0 && <Button size="sm" variant="outline" className="text-destructive max-md:h-11" disabled={preview} onClick={() => setRemoving(list.filter((machine) => selected.includes(machine.server_id)))}><Trash2 />{t("hub.remove_selected", { count: selected.length })}</Button>}
    </div>}
    <ul aria-label={t("wsettings.machines")} className="divide-y rounded-lg border">
      {list.map((machine) => {
        const count = agents.filter((agent) => agent.server_id === machine.server_id).length
        const online = machine.status === "online"
        return <li key={machine.server_id} className="flex items-start gap-3 px-3 py-2.5">
          <input type="checkbox" className="mt-1.5 size-4 shrink-0 accent-[hsl(var(--primary))] max-md:mt-3.5" checked={selected.includes(machine.server_id)} onChange={(event) => setSelected((current) => event.target.checked ? [...current, machine.server_id] : current.filter((id) => id !== machine.server_id))} aria-label={t("hub.select_machine", { name: machineName(machine) })} />
          <button type="button" onClick={() => context.openMachine(machine.server_id)} className="min-w-0 flex-1 rounded-sm text-left outline-none focus-visible:ring-2 focus-visible:ring-ring max-md:min-h-11">
            <span className="flex items-center gap-2"><span className={cn("size-2 shrink-0 rounded-full", online ? "bg-status-online" : "bg-zinc-400")} /><span className="truncate text-sm font-medium">{machineDisplayName(machine, list)}</span><span className={cn("text-[11px]", online ? "text-muted-foreground" : "text-red-600")}>{online ? t("status.online") : t("machine.offline")}</span></span>
            <span className="mt-0.5 block truncate font-mono text-[11px] text-muted-foreground">{machine.root}</span>
            <span className="mt-0.5 block truncate text-[11px] text-muted-foreground">{t("sidebar.machine_agents", { count })} · {t("machine.builds", { controller: buildLabel(machine.controller_build), host: buildLabel(machine.host_build) })}</span>
          </button>
          <DropdownMenu><DropdownMenuTrigger asChild><Button size="icon" variant="ghost" className="size-8 shrink-0 max-md:size-11" aria-label={t("machine.actions_for", { name: machineName(machine) })}><MoreHorizontal /></Button></DropdownMenuTrigger><DropdownMenuContent align="end">
            <DropdownMenuItem onSelect={() => context.renameMachine(machine)}><Pencil />{t("machine.rename")}</DropdownMenuItem>
            {!online && <><DropdownMenuItem onSelect={() => void context.copy(commands.restartController)}><Copy />{t("machine.copy_restart_controller")}</DropdownMenuItem><DropdownMenuItem onSelect={() => void context.copy(commands.start)}><Copy />{t("machine.copy_restore")}</DropdownMenuItem></>}
            <DropdownMenuSeparator />
            <DropdownMenuItem className="text-destructive focus:text-destructive" onSelect={() => setRemoving([machine])}><Trash2 />{t("hub.remove_machine")}</DropdownMenuItem>
          </DropdownMenuContent></DropdownMenu>
        </li>
      })}
      {!list.length && <li className="px-3 py-8 text-center text-xs text-muted-foreground">{machines === null ? t("wsettings.loading_machines") : t("wsettings.no_machines_connected")}</li>}
    </ul>
    <ConfirmDialog open={Boolean(removing)} onOpenChange={(open) => !open && setRemoving(null)} title={removing?.length === 1 ? t("hub.remove_machine_title", { name: machineName(removing[0]) }) : t("hub.remove_machines_title", { count: removing?.length ?? 0 })} description={t("hub.remove_machines_body")} confirmLabel={t("hub.remove_machine")} busy={busy} error={error} onConfirm={() => void remove()}>
      <div className="rounded-md bg-muted/50 p-3 text-xs"><ul className="list-disc space-y-1 pl-5">{removing?.map((machine) => <li key={machine.server_id}>{t("hub.machine_with_agents", { name: machineName(machine), count: agents.filter((agent) => agent.server_id === machine.server_id).length })}</li>)}</ul>{affected.length > 0 && <p className="mt-2 text-muted-foreground">{t("hub.agents_removed_with_machine", { count: affected.length })}</p>}</div>
    </ConfirmDialog>
    <ApiEquivalent calls={[{ method: "GET", path: `/api/workspaces/${workspace.workspace_id}/snapshot` }, { method: "POST", path: `/api/workspaces/${workspace.workspace_id}/bootstrap`, note: t("hub.api_add_machine") }, { method: "PATCH", path: `/api/workspaces/${workspace.workspace_id}/servers/{server_id}`, note: "{ name }" }, { method: "DELETE", path: `/api/workspaces/${workspace.workspace_id}/servers/{server_id}` }]} />
  </section>
}

export function WorkspaceProfilesPage({ context }: { context: WorkspacePageContext }) {
  const { workspace, profiles, profilesLoading } = context
  return <>
    <PageHeader title={t("wsettings.launch_profiles")} description={t("wsettings.reusable_agent_commands_and_their")} actions={<><Button variant="ghost" size="icon" aria-label={t("wsettings.refresh_profiles")} onClick={context.refreshProfiles} disabled={profilesLoading}><RotateCw /></Button><Button variant="outline" className="max-md:h-11" onClick={context.newProfile}><Plus />{t("wsettings.new_profile")}</Button></>} />
    <LaunchProfilesList profiles={profiles} loading={profilesLoading} onEdit={context.editProfile} onLaunch={context.launchProfile} onDelete={context.deleteProfile} />
    <ApiEquivalent calls={[{ method: "GET", path: `/api/workspaces/${workspace.workspace_id}/launch-profiles` }, { method: "POST", path: `/api/workspaces/${workspace.workspace_id}/launch-profiles` }, { method: "PATCH", path: `/api/workspaces/${workspace.workspace_id}/launch-profiles/{profile_id}` }, { method: "POST", path: `/api/workspaces/${workspace.workspace_id}/launch-profiles/{profile_id}/launch` }, { method: "DELETE", path: `/api/workspaces/${workspace.workspace_id}/launch-profiles/{profile_id}` }]} />
  </>
}

export function WorkspacePolicyPage({ context }: { context: WorkspacePageContext }) {
  const { workspace, apps, machines, preview } = context
  const isOwner = isOwnerOf(context)
  const [status, setStatus] = useState<PolicyProviderStatus>()
  const [appId, setAppId] = useState("")
  const [failureMode, setFailureMode] = useState<"fail_closed" | "fail_open">("fail_closed")
  const [staleSeconds, setStaleSeconds] = useState("300")
  const [installMachineId, setInstallMachineId] = useState("")
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const online = (machines ?? []).filter((machine) => machine.status === "online")
  const base = `${wsPath(workspace)}/policy-provider`
  function adopt(next: PolicyProviderStatus) {
    setStatus(next); setAppId(next.provider?.app_id ?? ""); setFailureMode(next.provider?.failure_mode ?? "fail_closed"); setStaleSeconds(String(next.provider?.max_stale_seconds ?? 300))
  }
  useEffect(() => {
    if (preview) return
    api<PolicyProviderStatus>(base).then((next) => { adopt(next); setError(null) }).catch((reason) => setError(reason instanceof Error ? reason.message : t("wsettings.unable_to_load_policy_provider")))
  }, [workspace.workspace_id, preview]) // eslint-disable-line react-hooks/exhaustive-deps
  useEffect(() => {
    if (online.some((machine) => machine.server_id === installMachineId)) return
    setInstallMachineId(online[0]?.server_id ?? "")
  }, [machines]) // eslint-disable-line react-hooks/exhaustive-deps
  async function act(action: () => Promise<void>, fallback: string) {
    setBusy(true); setError(null)
    try { await action() } catch (reason) { setError(reason instanceof Error ? reason.message : fallback) } finally { setBusy(false) }
  }
  const save = () => act(async () => {
    const max = Number(staleSeconds)
    if (!Number.isInteger(max) || max < 0 || max > 86400) throw new Error(t("wsettings.stale_cache_window_must_be"))
    setStatus(await api<PolicyProviderStatus>(base, { method: "PUT", body: JSON.stringify({ app_id: appId, failure_mode: failureMode, max_stale_seconds: max }) }))
  }, t("wsettings.unable_to_configure_policy_provider"))
  const remove = () => act(async () => { await api(base, { method: "DELETE" }); adopt(await api<PolicyProviderStatus>(base)) }, t("wsettings.unable_to_remove_policy_provider"))
  const install = () => act(async () => { const next = await api<PolicyProviderStatus>(`${base}/default-app`, { method: "POST", body: JSON.stringify({ server_id: installMachineId }) }); adopt(next); if (next.app) context.policyAppInstalled(next.app) }, t("wsettings.unable_to_install_the_default"))
  const lifecycle = (action: "start" | "stop" | "restart") => act(async () => {
    if (!status?.app) return
    const data = await api<{ app: AppDeployment }>(`${wsPath(workspace)}/apps/${encodeURIComponent(status.app.app_id)}/${action}`, { method: "POST", body: "{}" })
    setStatus((current) => current ? { ...current, app: data.app } : current)
  }, t("wsettings.policy_action_failed", { action: t(`wsettings.action_${action}`) }))
  const provider = status?.provider
  const app = status?.app
  const providerApp = apps.find((item) => item.app_id === provider?.app_id) ?? app
  const summary = !status ? null : provider
    ? t(provider.failure_mode === "fail_closed" ? "hub.policy_summary_closed" : "hub.policy_summary_open", { app: providerApp?.name ?? provider.app_id, status: app?.status ?? "unavailable", seconds: provider.max_stale_seconds })
    : status.fallback ? t(status.fallback.effect === "allow" ? "hub.policy_summary_default_allow" : "hub.policy_summary_default", { name: status.fallback.name, mode: status.fallback.mode }) : t("hub.policy_summary_none")
  return <section id="policy-provider">
    <PageHeader title={t("hub.policy")} description={t("wsettings.workspace_authorization_supplied_by_a")} />
    <OwnerNote context={context}>{t("hub.only_ws_owners_policy")}</OwnerNote>
    <div role="status" aria-label={t("hub.policy_status")} className="mb-6 flex items-start gap-3 rounded-lg border p-4">
      <span className="grid size-9 shrink-0 place-items-center rounded-md bg-status-online/10 text-status-success"><ShieldCheck className="size-4" /></span>
      <div className="min-w-0 flex-1"><p className="text-sm font-medium">{summary ?? t("hub.loading")}</p>
        {!provider && status?.fallback && <div className="mt-2 flex flex-wrap items-center gap-2"><p className="text-xs font-semibold">{status.fallback.name}</p><p className="text-[11px] text-muted-foreground">{t("wsettings.active_mode", { mode: status.fallback.mode })}{status.fallback.effect ? ` · ${status.fallback.effect}` : ` · ${t("wsettings.revision", { revision: status.fallback.revision })}`}</p></div>}
        {provider && <div className="mt-2"><Status value={app?.status ?? "unavailable"} /></div>}
      </div>
    </div>
    <Section title={t("hub.policy_app_section")} description={t("hub.policy_app_section_body")}>
      <div className="space-y-4">
        <div className="max-w-md space-y-2"><Label>{t("wsettings.policy_app")}</Label><Select value={appId || undefined} onValueChange={setAppId} disabled={!isOwner || busy}><SelectTrigger aria-label={t("wsettings.policy_app")} className="max-md:h-11"><SelectValue placeholder={t("wsettings.not_configured")} /></SelectTrigger><SelectContent>{apps.filter((item) => item.status === "running" || item.app_id === provider?.app_id).map((item) => <SelectItem key={item.app_id} value={item.app_id}>{item.name}</SelectItem>)}</SelectContent></Select></div>
        {!provider && isOwner && <div className="grid max-w-xl gap-2 sm:grid-cols-[minmax(0,1fr)_auto] sm:items-end"><div className="space-y-2"><Label>{t("wsettings.install_default_app_on")}</Label><Select value={installMachineId || undefined} onValueChange={setInstallMachineId} disabled={busy}><SelectTrigger aria-label={t("wsettings.default_policy_app_machine")} className="max-md:h-11"><SelectValue placeholder={t("wsettings.no_online_machine")} /></SelectTrigger><SelectContent>{online.map((machine) => <SelectItem key={machine.server_id} value={machine.server_id}>{machineName(machine)}</SelectItem>)}</SelectContent></Select></div><Button variant="outline" className="max-md:h-11" onClick={() => void install()} disabled={!installMachineId || busy}>{busy ? <RotateCw className="animate-spin" /> : <Download />}{t("wsettings.install_default_policy_app")}</Button></div>}
        <div className="flex flex-wrap items-center gap-2">
          <Button size="sm" className="max-md:h-11" onClick={() => void save()} disabled={!isOwner || !appId || busy}>{busy && <RotateCw className="animate-spin" />}{t("wsettings.apply")}</Button>
          {app && app.status !== "running" && <Button size="sm" variant="outline" className="max-md:h-11" onClick={() => void lifecycle("start")} disabled={!isOwner || busy}><Play />{t("apps.start")}</Button>}
          {app?.status === "running" && <><Button size="sm" variant="outline" className="max-md:h-11" onClick={() => void lifecycle("restart")} disabled={!isOwner || busy}><RotateCw />{t("apps.restart")}</Button><Button size="sm" variant="outline" className="max-md:h-11" onClick={() => void lifecycle("stop")} disabled={!isOwner || busy}><Square />{t("apps.stop")}</Button></>}
          {provider && <Button size="sm" variant="outline" className="max-md:h-11" onClick={() => void remove()} disabled={!isOwner || busy}>{t("wsettings.remove")}</Button>}
          {app?.public_url && <Button size="sm" variant="outline" className="max-md:h-11" asChild><a href={app.public_url} target="_blank" rel="noreferrer"><ExternalLink />{t("wsettings.open_policy_app")}</a></Button>}
        </div>
        {!apps.length && !online.length && <p className="text-xs text-muted-foreground">{t("wsettings.connect_a_machine_to_install")}</p>}
        <ErrorNote message={error ?? status?.cache.last_error} />
      </div>
    </Section>
    <details className="rounded-lg border p-4" data-policy-advanced="">
      <summary className="cursor-pointer text-sm font-semibold max-md:min-h-11">{t("hub.advanced")}</summary>
      <div className="mt-4 space-y-4">
        <div className="grid max-w-xl gap-3 sm:grid-cols-2">
          <div className="space-y-2"><Label>{t("wsettings.provider_failure")}</Label><Select value={failureMode} onValueChange={(value: "fail_closed" | "fail_open") => setFailureMode(value)} disabled={!isOwner || busy}><SelectTrigger aria-label={t("wsettings.policy_provider_failure_mode")} className="max-md:h-11"><SelectValue /></SelectTrigger><SelectContent><SelectItem value="fail_closed">{t("wsettings.deny_after_stale_window")}</SelectItem><SelectItem value="fail_open">{t("wsettings.allow_after_stale_window")}</SelectItem></SelectContent></Select></div>
          <div className="space-y-2"><Label htmlFor="policy-stale-seconds">{t("wsettings.stale_seconds")}</Label><Input id="policy-stale-seconds" aria-label={t("wsettings.policy_provider_stale_seconds")} type="number" min={0} max={86400} value={staleSeconds} onChange={(event) => setStaleSeconds(event.target.value)} disabled={!isOwner || busy} className="max-md:h-11 max-md:text-base" /></div>
        </div>
        <p className="text-[11px] text-muted-foreground">{t("hub.advanced_apply_hint")}</p>
        {provider && <div className="grid gap-2 border-y py-3 text-[11px] text-muted-foreground sm:grid-cols-3"><span>{t("hub.revision_announced")} <strong className="text-foreground">rev {provider.revision_hint}</strong></span><span>{t("hub.revision_cached")} <strong className="text-foreground">{status?.cache.revision === undefined ? "—" : `rev ${status.cache.revision}`}</strong></span><span>{t("hub.cache_age")} <strong className="text-foreground">{status?.cache.age_seconds === undefined ? "—" : `${status.cache.age_seconds}s`}</strong></span></div>}
        <PolicyPublishAuthoritySettings key={`${workspace.workspace_id}/${provider?.app_id ?? "none"}`} workspaceId={workspace.workspace_id} status={status} isOwner={isOwner} disabled={preview || busy} onSaved={(next) => setStatus((current) => current ? { ...current, provider: next } : current)} />
      </div>
    </details>
    <ApiEquivalent calls={[{ method: "GET", path: `/api/workspaces/${workspace.workspace_id}/policy-provider` }, { method: "PUT", path: `/api/workspaces/${workspace.workspace_id}/policy-provider`, note: "{ app_id, failure_mode, max_stale_seconds }" }, { method: "POST", path: `/api/workspaces/${workspace.workspace_id}/policy-provider/default-app`, note: "{ server_id }" }, { method: "DELETE", path: `/api/workspaces/${workspace.workspace_id}/policy-provider` }, { method: "PUT", path: `/api/workspaces/${workspace.workspace_id}/policy-provider/publish-authority` }]} />
  </section>
}

export function WorkspaceAuditPage({ context }: { context: WorkspacePageContext }) {
  const { workspace, agents, machines, directory, preview } = context
  const [revision, setRevision] = useState(0)
  return <>
    <PageHeader title={t("audit.audit")} description={t("hub.workspace_audit_description")} actions={<Button variant="outline" className="max-md:h-11" onClick={() => setRevision((value) => value + 1)}><RotateCw />{t("hub.refresh")}</Button>} />
    <WorkspaceAudit key={`${workspace.workspace_id}/${revision}`} workspaceId={workspace.workspace_id} agents={agents} members={directory.members} preview={preview} />
    {!preview && <div className="mt-8"><TrafficOverview key={workspace.workspace_id} workspaceId={workspace.workspace_id} agents={agents} machines={machines ?? []} refreshRevision={revision} /></div>}
    <ApiEquivalent calls={[{ method: "GET", path: `/api/workspaces/${workspace.workspace_id}/audit-events` }, { method: "GET", path: `/api/workspaces/${workspace.workspace_id}/traffic?hours=24` }]} />
  </>
}

export function WorkspaceDangerPage({ context }: { context: WorkspacePageContext }) {
  const { workspace, machines, preview } = context
  const isOwner = isOwnerOf(context)
  const [open, setOpen] = useState(false)
  const [revoke, setRevoke] = useState(false)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const count = machines?.length
  useEffect(() => { if (open) { setRevoke(false); setError(null) } }, [open])
  async function remove() {
    setBusy(true); setError(null)
    try {
      await api(wsPath(workspace), { method: "DELETE", body: JSON.stringify({ confirm_name: workspace.name, revoke_machines: revoke }) })
      setOpen(false); context.workspaceDeleted(workspace.workspace_id)
    } catch (reason) { setError(message(reason)) }
    finally { setBusy(false) }
  }
  return <>
    <PageHeader title={t("apps.danger_zone")} description={t("wsettings.deleted_workspaces_disappear_from_active")} />
    <Section title={t("wsettings.delete_workspace")} tone="danger" description={t("hub.delete_workspace_body")}>
      <OwnerNote context={context}>{t("hub.only_ws_owners_delete")}</OwnerNote>
      <p className="mb-3 text-xs text-muted-foreground">{count === undefined ? t("wsettings.checking_machine_inventory") : count > 0 ? t("hub.workspace_has_machines", { count }) : t("wsettings.this_action_cannot_be_undone")}</p>
      <Button variant="destructive" className="max-md:h-11" disabled={!isOwner || preview || count === undefined} onClick={() => setOpen(true)}><Trash2 />{t("wsettings.delete_workspace")}</Button>
    </Section>
    <ConfirmDialog open={open} onOpenChange={setOpen} title={t("hub.delete_workspace_title", { name: workspace.name })} description={t("hub.delete_workspace_confirm")} confirmLabel={t("wsettings.delete_workspace")} confirmName={workspace.name} disabled={Boolean(count) && !revoke} busy={busy} error={error} onConfirm={() => void remove()}>
      {Boolean(count) && <label className="flex items-start gap-2 rounded-md border p-3 text-xs max-sm:min-h-11"><input type="checkbox" className="mt-0.5 size-4 accent-[hsl(var(--destructive))]" checked={revoke} onChange={(event) => setRevoke(event.target.checked)} /><span><span className="block font-medium">{t("hub.also_revoke_machines", { count: count ?? 0 })}</span><span className="text-muted-foreground">{t("hub.also_revoke_machines_body")}</span></span></label>}
    </ConfirmDialog>
    <ApiEquivalent calls={[{ method: "DELETE", path: `/api/workspaces/${workspace.workspace_id}`, note: "{ confirm_name, revoke_machines }" }]} />
  </>
}

export function WorkspaceAccessUnavailable() {
  return <Unavailable>{t("hub.access_unavailable")}</Unavailable>
}
