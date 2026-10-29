import { FormEvent, useCallback, useEffect, useMemo, useState } from "react"
import { Link, useSearchParams } from "react-router"
import { Copy, MoreHorizontal, Plus, RotateCw, Trash2, UserPlus, X } from "lucide-react"
import { Button } from "@/components/ui/button"
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog"
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger } from "@/components/ui/dropdown-menu"
import { Input } from "@/components/ui/input"
import { Label } from "@/components/ui/label"
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select"
import { Textarea } from "@/components/ui/textarea"
import { AuditEventList } from "@/components/audit-view"
import { api, ApiError, isUnavailable, type CreatedInvitation, type Member, type Organization, type OrganizationAuditEvent, type OrganizationGroup, type OrganizationInvitation, type User, type Workspace } from "@/lib/api"
import { workspacePath } from "@/lib/control-plane"
import { workspaceSettingsPath } from "@/lib/console-routes"
import { cn } from "@/lib/utils"
import { roleLabel, t, type MessageKey } from "@/i18n"
import type { OrgDirectory } from "@/components/settings-hub/use-org-directory"
import { useWorkspaceAccessIndex, workspacesOf } from "@/components/settings-hub/use-workspace-access"
import { ApiEquivalent, Badge, ConfirmDialog, ErrorNote, PageHeader, PermissionNote, Picker, Section, Unavailable, UnsavedBar, formatDate } from "@/components/settings-hub/ui"

export type OrgPageContext = {
  organization: Organization
  user: User
  preview: boolean
  directory: OrgDirectory
  workspaces: Workspace[]
  linkSearch: string
  showError: (reason: unknown) => void
  copy: (value: string) => Promise<void>
  refreshOrganizations: () => Promise<void>
  createWorkspace: () => void
  organizationGone: (organizationId: string) => void
}

const orgPath = (organization: Organization) => `/api/organizations/${encodeURIComponent(organization.organization_id)}`
const message = (reason: unknown) => reason instanceof Error ? reason.message : String(reason)
const isManager = (role: Organization["role"]) => role === "owner" || role === "admin"

export function OrgGeneralPage({ context }: { context: OrgPageContext }) {
  const { organization, preview } = context
  const [name, setName] = useState(organization.name)
  const [saving, setSaving] = useState(false)
  const [error, setError] = useState<string | null>(null)
  useEffect(() => setName(organization.name), [organization.name])
  const canManage = isManager(organization.role)
  async function save(event: FormEvent) {
    event.preventDefault()
    setSaving(true); setError(null)
    try { await api(orgPath(organization), { method: "PATCH", body: JSON.stringify({ name }) }); await context.refreshOrganizations() }
    catch (reason) { setError(message(reason)) }
    finally { setSaving(false) }
  }
  return <>
    <PageHeader title={t("settings.nav.general")} description={t("org.the_organization_name_shown_to")} />
    {!canManage && <PermissionNote>{t("hub.only_managers_rename_org")}</PermissionNote>}
    <form onSubmit={save} className="max-w-md space-y-3">
      <div className="space-y-2"><Label htmlFor="organization-name">{t("org.organization_name")}</Label><Input id="organization-name" value={name} onChange={(event) => setName(event.target.value)} required maxLength={80} disabled={!canManage || preview} className="max-md:h-11 max-md:text-base" /></div>
      <ErrorNote message={error} />
      {canManage && <Button type="submit" className="max-md:h-11" disabled={saving || !name.trim() || name.trim() === organization.name}>{saving ? t("common.saving") : t("common.save")}</Button>}
    </form>
    <dl className="mt-6 grid max-w-md grid-cols-[auto_minmax(0,1fr)] gap-x-4 gap-y-2 text-xs">
      <dt className="text-muted-foreground">{t("hub.organization_id")}</dt><dd className="truncate font-mono">{organization.organization_id}</dd>
      <dt className="text-muted-foreground">{t("hub.your_role")}</dt><dd>{roleLabel(organization.role)}</dd>
    </dl>
    <ApiEquivalent calls={[{ method: "GET", path: "/api/organizations" }, { method: "PATCH", path: `/api/organizations/${organization.organization_id}`, note: "{ name }" }]} />
  </>
}

// Invitation links are shown once by Core; links created in this page session can be copied again.
const createdLinks = new Map<string, string>()

function useInvitations(organization: Organization, enabled: boolean, preview: boolean) {
  const [list, setList] = useState<OrganizationInvitation[] | "unavailable" | null>(null)
  const [error, setError] = useState<string | null>(null)
  const reload = useCallback(async () => {
    if (preview) { setList([]); return }
    try {
      const data = await api<{ invitations: OrganizationInvitation[] }>(`${orgPath(organization)}/invitations`)
      setList(Array.isArray(data.invitations) ? data.invitations : []); setError(null)
    } catch (reason) { if (isUnavailable(reason)) setList("unavailable"); else setError(message(reason)) }
  }, [organization.organization_id, preview]) // eslint-disable-line react-hooks/exhaustive-deps
  useEffect(() => { if (enabled) void reload() }, [enabled, reload])
  async function revoke(id: string) {
    await api(`${orgPath(organization)}/invitations/${encodeURIComponent(id)}`, { method: "DELETE" })
    createdLinks.delete(id)
    await reload()
  }
  return { list, error, reload, revoke }
}

const INVITATION_STATUS: Record<OrganizationInvitation["status"], MessageKey> = { pending: "hub.invitation_pending", accepted: "hub.invitation_accepted", revoked: "hub.invitation_revoked", expired: "hub.invitation_expired" }

function InvitationList({ invitations, canManage, statusFilter, onRevoke, onCopy }: { invitations: OrganizationInvitation[]; canManage: boolean; statusFilter: "pending" | "all"; onRevoke: (invitation: OrganizationInvitation) => void; onCopy: (url: string) => void }) {
  const shown = invitations.filter((item) => statusFilter === "all" || item.status === "pending")
  return <div className="overflow-x-auto rounded-lg border"><table className="w-full min-w-[560px] text-left text-xs">
    <thead className="border-b bg-muted/40 text-[11px] text-muted-foreground"><tr><th className="px-3 py-2 font-medium">{t("hub.invitee")}</th><th className="px-3 py-2 font-medium">{t("hub.role")}</th><th className="px-3 py-2 font-medium">{t("hub.status")}</th><th className="px-3 py-2 font-medium">{t("hub.expires")}</th><th className="px-3 py-2"><span className="sr-only">{t("hub.actions")}</span></th></tr></thead>
    <tbody className="divide-y">{shown.map((invitation) => {
      const url = createdLinks.get(invitation.id)
      const label = invitation.email || t("hub.open_link")
      return <tr key={invitation.id}>
        <td className="px-3 py-2"><span className="block truncate font-medium">{label}</span>{invitation.created_by?.name && <span className="block truncate text-muted-foreground">{t("hub.invited_by", { name: invitation.created_by.name })}</span>}</td>
        <td className="px-3 py-2">{roleLabel(invitation.role)}</td>
        <td className="px-3 py-2"><Badge tone={invitation.status === "pending" ? "primary" : "muted"}>{t(INVITATION_STATUS[invitation.status] ?? "hub.invitation_pending")}</Badge></td>
        <td className="px-3 py-2 text-muted-foreground">{formatDate(invitation.expires_at)}</td>
        <td className="px-3 py-2"><div className="flex justify-end gap-1">
          {invitation.status === "pending" && <Button size="sm" variant="ghost" className="max-md:h-11" disabled={!url} title={url ? undefined : t("hub.link_shown_once")} aria-label={t("hub.copy_invitation_link", { name: label })} onClick={() => url && onCopy(url)}><Copy />{t("dialogs.copy_link")}</Button>}
          {invitation.status === "pending" && canManage && <Button size="sm" variant="ghost" className="text-destructive max-md:h-11" aria-label={t("hub.revoke_invitation", { name: label })} onClick={() => onRevoke(invitation)}>{t("hub.revoke")}</Button>}
        </div></td>
      </tr>
    })}{!shown.length && <tr><td colSpan={5} className="px-3 py-6 text-center text-muted-foreground">{t("hub.no_invitations")}</td></tr>}</tbody>
  </table></div>
}

const EXPIRY_HOURS = [24, 72, 168, 720] as const

function InviteDialog({ open, onOpenChange, context, legacy, onCreated }: { open: boolean; onOpenChange: (open: boolean) => void; context: OrgPageContext; legacy: boolean; onCreated: () => void }) {
  const { organization, directory, workspaces } = context
  const [emails, setEmails] = useState("")
  const [role, setRole] = useState<Member["role"]>("member")
  const [expiry, setExpiry] = useState<string>("168")
  const [groupIds, setGroupIds] = useState<string[]>([])
  const [workspaceIds, setWorkspaceIds] = useState<string[]>([])
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [results, setResults] = useState<{ email: string; url: string; emailed: boolean }[] | null>(null)
  useEffect(() => { if (open) { setEmails(""); setRole("member"); setExpiry("168"); setGroupIds([]); setWorkspaceIds([]); setError(null); setResults(null) } }, [open])
  const addresses = emails.split(/[\s,;]+/).map((value) => value.trim()).filter(Boolean)
  const invalid = addresses.filter((value) => !/^[^@\s]+@[^@\s]+\.[^@\s]+$/.test(value))
  async function submit(event: FormEvent) {
    event.preventDefault()
    if (invalid.length) return
    setBusy(true); setError(null)
    const created: { email: string; url: string; emailed: boolean }[] = []
    try {
      for (const email of addresses.length ? addresses : [""]) {
        const body = legacy ? {} : { ...(email ? { email } : {}), role, expires_in_hours: Number(expiry), ...(groupIds.length ? { group_ids: groupIds } : {}), ...(workspaceIds.length ? { workspace_ids: workspaceIds } : {}) }
        const invitation = await api<CreatedInvitation>(`${orgPath(organization)}/invitations`, { method: "POST", body: JSON.stringify(body) })
        if (invitation.id) createdLinks.set(invitation.id, invitation.url)
        created.push({ email: email || t("hub.open_link"), url: invitation.url, emailed: invitation.email_queued === true })
      }
      setResults(created); onCreated()
    } catch (reason) { setError(message(reason)); if (created.length) { setResults(created); onCreated() } }
    finally { setBusy(false) }
  }
  const toggle = (list: string[], id: string) => list.includes(id) ? list.filter((value) => value !== id) : [...list, id]
  return <Dialog open={open} onOpenChange={onOpenChange}><DialogContent className="max-w-lg max-sm:max-h-[100dvh] max-sm:overflow-y-auto">
    {results ? <div className="space-y-4">
      <DialogHeader><DialogTitle>{t("hub.invitations_created")}</DialogTitle><DialogDescription>{t("hub.invitations_created_body")}</DialogDescription></DialogHeader>
      <ul className="space-y-2">{results.map((result) => <li key={result.url} className="rounded-md border p-2"><p className="mb-1 flex items-center gap-2 truncate text-xs font-medium">{result.email}{result.emailed && <Badge tone="primary">{t("hub.emailed")}</Badge>}</p><div className="flex gap-2"><Textarea readOnly aria-label={t("hub.invitation_link_for", { name: result.email })} value={result.url} className="min-h-0 flex-1 resize-none font-mono text-[11px]" rows={2} /><Button variant="outline" size="sm" onClick={() => void context.copy(result.url)}><Copy />{t("dialogs.copy_link")}</Button></div></li>)}</ul>
      <ErrorNote message={error} />
      <DialogFooter><Button onClick={() => onOpenChange(false)}>{t("hub.done")}</Button></DialogFooter>
    </div> : <form onSubmit={submit} className="space-y-4">
      <DialogHeader><DialogTitle>{t("hub.invite_people")}</DialogTitle><DialogDescription>{legacy ? t("hub.invite_legacy") : t("hub.invite_description")}</DialogDescription></DialogHeader>
      {!legacy && <>
        <div className="space-y-2"><Label htmlFor="invite-emails">{t("hub.invite_emails")}</Label><Textarea id="invite-emails" value={emails} onChange={(event) => setEmails(event.target.value)} placeholder="ada@example.com, grace@example.com" className="min-h-16 max-sm:text-base" /><p className="text-[11px] text-muted-foreground">{t("hub.invite_emails_hint")}</p>{invalid.length > 0 && <p role="alert" className="text-xs text-destructive">{t("hub.invalid_emails", { emails: invalid.join(", ") })}</p>}</div>
        <div className="grid gap-3 sm:grid-cols-2">
          <div className="space-y-2"><Label>{t("hub.role")}</Label><Select value={role} onValueChange={(value: Member["role"]) => setRole(value)}><SelectTrigger aria-label={t("hub.invite_role")} className="max-sm:h-11"><SelectValue /></SelectTrigger><SelectContent><SelectItem value="member">{t("role.member")}</SelectItem><SelectItem value="admin">{t("role.admin")}</SelectItem>{organization.role === "owner" && <SelectItem value="owner">{t("role.owner")}</SelectItem>}</SelectContent></Select></div>
          <div className="space-y-2"><Label>{t("hub.expires_after")}</Label><Select value={expiry} onValueChange={setExpiry}><SelectTrigger aria-label={t("hub.expires_after")} className="max-sm:h-11"><SelectValue /></SelectTrigger><SelectContent>{EXPIRY_HOURS.map((hours) => <SelectItem key={hours} value={String(hours)}>{hours < 48 ? t("hub.hours", { count: hours }) : t("hub.days", { count: hours / 24 })}</SelectItem>)}</SelectContent></Select></div>
        </div>
        {directory.groups.length > 0 && <fieldset><legend className="mb-2 text-sm font-medium">{t("hub.add_to_groups")} <span className="font-normal text-muted-foreground">{t("hub.optional")}</span></legend><div className="flex flex-wrap gap-2">{directory.groups.map((group) => <button key={group.group_id} type="button" aria-pressed={groupIds.includes(group.group_id)} onClick={() => setGroupIds((list) => toggle(list, group.group_id))} className={cn("rounded-full border px-3 py-1 text-xs max-sm:min-h-11", groupIds.includes(group.group_id) ? "border-primary bg-primary/10 text-primary" : "hover:bg-accent")}>{group.name}</button>)}</div></fieldset>}
        {workspaces.length > 0 && <fieldset><legend className="mb-2 text-sm font-medium">{t("hub.grant_workspaces")} <span className="font-normal text-muted-foreground">{t("hub.optional")}</span></legend><div className="flex flex-wrap gap-2">{workspaces.map((workspace) => <button key={workspace.workspace_id} type="button" aria-pressed={workspaceIds.includes(workspace.workspace_id)} onClick={() => setWorkspaceIds((list) => toggle(list, workspace.workspace_id))} className={cn("rounded-full border px-3 py-1 text-xs max-sm:min-h-11", workspaceIds.includes(workspace.workspace_id) ? "border-primary bg-primary/10 text-primary" : "hover:bg-accent")}>{workspace.name}</button>)}</div></fieldset>}
      </>}
      <ErrorNote message={error} />
      <DialogFooter><Button type="button" variant="outline" className="max-sm:h-11" onClick={() => onOpenChange(false)}>{t("common.cancel")}</Button><Button type="submit" className="max-sm:h-11" disabled={busy || invalid.length > 0}>{busy ? t("hub.working") : addresses.length > 1 ? t("hub.create_invitations", { count: addresses.length }) : t("hub.create_invitation")}</Button></DialogFooter>
    </form>}
  </DialogContent></Dialog>
}

export function MembersPage({ context }: { context: OrgPageContext }) {
  const { organization, user, directory, workspaces, preview } = context
  const canManage = isManager(organization.role)
  const isOwner = organization.role === "owner"
  const [params, setParams] = useSearchParams()
  const tab = params.get("tab") === "pending" ? "pending" : "active"
  const query = params.get("q") ?? ""
  const [roleFilter, setRoleFilter] = useState("all")
  const [groupFilter, setGroupFilter] = useState("all")
  const [pending, setPending] = useState<Record<string, Member["role"]>>({})
  const [saving, setSaving] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [removing, setRemoving] = useState<Member | null>(null)
  const [removeBusy, setRemoveBusy] = useState(false)
  const [removeError, setRemoveError] = useState<string | null>(null)
  const [revoking, setRevoking] = useState<OrganizationInvitation | null>(null)
  const [inviteOpen, setInviteOpen] = useState(false)
  const invitations = useInvitations(organization, canManage, preview)
  const index = useWorkspaceAccessIndex(workspaces, directory.loaded, preview)
  useEffect(() => { setPending({}) }, [organization.organization_id])
  const setParam = (key: string, value: string | null) => setParams((current) => { const next = new URLSearchParams(current); if (value) next.set(key, value); else next.delete(key); return next }, { replace: true })
  const pendingInvitations = Array.isArray(invitations.list) ? invitations.list.filter((item) => item.status === "pending") : []
  const shown = useMemo(() => {
    const q = query.trim().toLocaleLowerCase()
    return directory.members.filter((member) => (!q || member.preferred_name.toLocaleLowerCase().includes(q) || member.email.toLocaleLowerCase().includes(q))
      && (roleFilter === "all" || member.role === roleFilter)
      && (groupFilter === "all" || directory.groups.some((group) => group.group_id === groupFilter && group.member_ids.includes(member.user_id))))
  }, [directory.members, directory.groups, query, roleFilter, groupFilter])
  const changes = Object.entries(pending).filter(([id, role]) => directory.members.find((member) => member.user_id === id)?.role !== role)
  async function saveRoles() {
    setSaving(true); setError(null)
    try {
      for (const [userId, role] of changes) await api(`${orgPath(organization)}/members/${encodeURIComponent(userId)}`, { method: "PATCH", body: JSON.stringify({ role }) })
      setPending({}); await directory.reload()
    } catch (reason) { setError(message(reason)); await directory.reload() }
    finally { setSaving(false) }
  }
  async function remove() {
    if (!removing) return
    setRemoveBusy(true); setRemoveError(null)
    try { await api(`${orgPath(organization)}/members/${encodeURIComponent(removing.user_id)}`, { method: "DELETE" }); setRemoving(null); await directory.reload() }
    catch (reason) { setRemoveError(message(reason)) }
    finally { setRemoveBusy(false) }
  }
  async function revoke() {
    if (!revoking) return
    try { await invitations.revoke(revoking.id); setRevoking(null) } catch (reason) { context.showError(reason) }
  }
  // Core: owners may change any role and remove other owners while one owner remains (409 last_owner otherwise).
  const ownerCount = directory.members.filter((member) => member.role === "owner").length
  const removeReason = (member: Member) => !canManage ? t("hub.only_managers_remove") : member.user_id === user.user_id ? t("hub.leave_instead") : member.role === "owner" && (!isOwner || ownerCount <= 1) ? t("hub.owner_cannot_be_removed") : null
  const losing = removing ? workspacesOf(removing.user_id, workspaces, index, directory.members, directory.groups) : []
  const losingGroups = removing ? directory.groups.filter((group) => group.member_ids.includes(removing.user_id)) : []
  return <>
    <PageHeader title={t("org.members")} description={t("hub.members_summary", { members: directory.members.length, pending: pendingInvitations.length })}
      actions={canManage ? <Button className="max-md:h-11" onClick={() => setInviteOpen(true)} disabled={preview}><UserPlus />{t("hub.invite_people")}</Button> : undefined} />
    {!canManage && <PermissionNote>{t("hub.members_read_only")}</PermissionNote>}
    {canManage && !isOwner && <PermissionNote>{t("hub.only_owners_change_roles")}</PermissionNote>}
    <div role="tablist" aria-label={t("org.members")} className="mb-4 flex gap-5 border-b">
      {(["active", "pending"] as const).map((value) => <button key={value} type="button" role="tab" aria-selected={tab === value} onClick={() => setParam("tab", value === "pending" ? "pending" : null)} className={cn("-mb-px border-b-2 px-0.5 py-2 text-sm max-md:min-h-11", tab === value ? "border-primary font-medium text-foreground" : "border-transparent text-muted-foreground hover:text-foreground")}>{value === "active" ? t("hub.tab_active", { count: directory.members.length }) : t("hub.tab_pending", { count: pendingInvitations.length })}</button>)}
    </div>
    {tab === "active" ? <>
      <div className="mb-3 flex flex-wrap gap-2">
        <Input type="search" aria-label={t("hub.filter_members")} placeholder={t("hub.filter_members")} value={query} onChange={(event) => setParam("q", event.target.value || null)} className="w-full max-w-xs max-md:h-11 max-md:max-w-none max-md:text-base" />
        <Select value={roleFilter} onValueChange={setRoleFilter}><SelectTrigger aria-label={t("hub.filter_role")} className="w-auto min-w-36 max-md:h-11"><SelectValue /></SelectTrigger><SelectContent><SelectItem value="all">{t("hub.all_roles")}</SelectItem>{(["owner", "admin", "member"] as const).map((role) => <SelectItem key={role} value={role}>{roleLabel(role)}</SelectItem>)}</SelectContent></Select>
        {directory.groups.length > 0 && <Select value={groupFilter} onValueChange={setGroupFilter}><SelectTrigger aria-label={t("hub.filter_group")} className="w-auto min-w-36 max-md:h-11"><SelectValue /></SelectTrigger><SelectContent><SelectItem value="all">{t("hub.all_groups")}</SelectItem>{directory.groups.map((group) => <SelectItem key={group.group_id} value={group.group_id}>{group.name}</SelectItem>)}</SelectContent></Select>}
      </div>
      <div className="overflow-x-auto rounded-lg border"><table aria-label={t("org.members")} className="w-full min-w-[600px] text-left text-xs">
        <thead className="border-b bg-muted/40 text-[11px] text-muted-foreground"><tr><th className="px-3 py-2 font-medium">{t("dialogs.name")}</th><th className="px-3 py-2 font-medium">{t("hub.role")}</th><th className="px-3 py-2 font-medium">{t("org.groups")}</th><th className="px-3 py-2 font-medium">{t("org.workspaces")}</th><th className="px-3 py-2"><span className="sr-only">{t("hub.actions")}</span></th></tr></thead>
        <tbody className="divide-y">{shown.map((member) => {
          const role = pending[member.user_id] ?? member.role
          const reason = removeReason(member)
          const groups = directory.groups.filter((group) => group.member_ids.includes(member.user_id))
          const count = workspacesOf(member.user_id, workspaces, index, directory.members, directory.groups).length
          const editable = isOwner && member.user_id !== user.user_id
          return <tr key={member.user_id} className={cn(pending[member.user_id] && pending[member.user_id] !== member.role && "bg-amber-500/5")}>
            <td className="px-3 py-2"><span className="block truncate font-medium">{member.preferred_name} {member.user_id === user.user_id && <Badge>{t("hub.you")}</Badge>}</span><span className="block truncate text-muted-foreground">{member.email}</span></td>
            <td className="px-3 py-2">{editable ? <Select value={role} onValueChange={(value: Member["role"]) => setPending((current) => ({ ...current, [member.user_id]: value }))}><SelectTrigger aria-label={t("hub.role_for", { name: member.preferred_name })} className="h-8 w-28 text-xs max-md:h-11"><SelectValue /></SelectTrigger><SelectContent><SelectItem value="member">{roleLabel("member")}</SelectItem><SelectItem value="admin">{roleLabel("admin")}</SelectItem><SelectItem value="owner">{roleLabel("owner")}</SelectItem></SelectContent></Select> : <span>{roleLabel(member.role)}</span>}</td>
            <td className="px-3 py-2"><div className="flex flex-wrap gap-1">{groups.map((group) => <Badge key={group.group_id}>{group.name}</Badge>)}{!groups.length && <span className="text-muted-foreground">—</span>}</div></td>
            <td className="px-3 py-2 tabular-nums">{index[workspaces[0]?.workspace_id ?? ""] || !workspaces.length ? count : "…"}</td>
            <td className="px-3 py-2 text-right"><DropdownMenu><DropdownMenuTrigger asChild><Button size="icon" variant="ghost" className="size-8 max-md:size-11" aria-label={t("hub.member_actions", { name: member.preferred_name })}><MoreHorizontal /></Button></DropdownMenuTrigger><DropdownMenuContent align="end">
              <DropdownMenuItem className="text-destructive focus:text-destructive" disabled={Boolean(reason)} onSelect={() => { setRemoveError(null); setRemoving(member) }}><Trash2 />{t("hub.remove_from_org")}</DropdownMenuItem>
              {reason && <p className="max-w-56 px-2 pb-1.5 text-[11px] text-muted-foreground">{reason}</p>}
            </DropdownMenuContent></DropdownMenu></td>
          </tr>
        })}{!shown.length && <tr><td colSpan={5} className="px-3 py-6 text-center text-muted-foreground">{directory.loaded ? t("hub.no_matches") : t("hub.loading")}</td></tr>}</tbody>
      </table></div>
      <ErrorNote message={error ?? directory.error} />
      <UnsavedBar count={changes.length} saving={saving} onSave={() => void saveRoles()} onDiscard={() => setPending({})} />
    </> : <>
      {!canManage ? <PermissionNote>{t("hub.only_managers_see_invitations")}</PermissionNote> : invitations.list === "unavailable" ? <Unavailable>{t("hub.invitations_unavailable")}</Unavailable> : invitations.list === null ? <p className="text-xs text-muted-foreground">{t("hub.loading")}</p> : <InvitationList invitations={invitations.list} canManage={canManage} statusFilter="pending" onRevoke={setRevoking} onCopy={(url) => void context.copy(url)} />}
      <ErrorNote message={invitations.error} />
    </>}
    <InviteDialog open={inviteOpen} onOpenChange={setInviteOpen} context={context} legacy={invitations.list === "unavailable"} onCreated={() => void invitations.reload()} />
    <ConfirmDialog open={Boolean(removing)} onOpenChange={(open) => !open && setRemoving(null)} title={t("hub.remove_member_title", { name: removing?.preferred_name ?? "" })} description={t("hub.remove_member_body", { name: removing?.preferred_name ?? "", org: organization.name })} confirmLabel={t("hub.remove_from_org")} busy={removeBusy} error={removeError} onConfirm={() => void remove()}>
      <div className="space-y-2 rounded-md bg-muted/50 p-3 text-xs">
        <p className="font-medium">{t("hub.they_will_lose")}</p>
        <ul className="list-disc space-y-1 pl-5">{losing.map((workspace) => <li key={workspace.workspace_id}>{t("hub.lose_workspace", { name: workspace.name })}</li>)}{losingGroups.map((group) => <li key={group.group_id}>{t("hub.lose_group", { name: group.name })}</li>)}{!losing.length && !losingGroups.length && <li>{t("hub.lose_org_only")}</li>}</ul>
        <p className="text-muted-foreground">{t("hub.agents_keep_running")}</p>
      </div>
    </ConfirmDialog>
    <ConfirmDialog open={Boolean(revoking)} onOpenChange={(open) => !open && setRevoking(null)} title={t("hub.revoke_invitation_title")} description={t("hub.revoke_invitation_body", { name: revoking?.email || t("hub.open_link") })} confirmLabel={t("hub.revoke")} onConfirm={() => void revoke()} />
    <ApiEquivalent calls={[{ method: "GET", path: `/api/organizations/${organization.organization_id}/members` }, { method: "PATCH", path: `/api/organizations/${organization.organization_id}/members/{user_id}`, note: "{ role }" }, { method: "DELETE", path: `/api/organizations/${organization.organization_id}/members/{user_id}` }, { method: "POST", path: `/api/organizations/${organization.organization_id}/invitations`, note: "{ email?, role, expires_in_hours?, group_ids?, workspace_ids? }" }]} />
  </>
}

export function InvitationsPage({ context }: { context: OrgPageContext }) {
  const { organization, preview } = context
  const canManage = isManager(organization.role)
  const invitations = useInvitations(organization, canManage, preview)
  const [filter, setFilter] = useState<"pending" | "all">("pending")
  const [inviteOpen, setInviteOpen] = useState(false)
  const [revoking, setRevoking] = useState<OrganizationInvitation | null>(null)
  async function revoke() {
    if (!revoking) return
    try { await invitations.revoke(revoking.id); setRevoking(null) } catch (reason) { context.showError(reason) }
  }
  return <>
    <PageHeader title={t("hub.invitations")} description={t("hub.invitations_description")} actions={canManage ? <Button className="max-md:h-11" onClick={() => setInviteOpen(true)} disabled={preview}><UserPlus />{t("hub.invite_people")}</Button> : undefined} />
    {!canManage ? <PermissionNote>{t("hub.only_managers_see_invitations")}</PermissionNote> : invitations.list === "unavailable" ? <><Unavailable>{t("hub.invitations_unavailable")}</Unavailable></> : invitations.list === null ? <p className="text-xs text-muted-foreground">{t("hub.loading")}</p> : <>
      <div className="mb-3 flex items-center gap-2"><Select value={filter} onValueChange={(value: "pending" | "all") => setFilter(value)}><SelectTrigger aria-label={t("hub.status")} className="w-auto min-w-36 max-md:h-11"><SelectValue /></SelectTrigger><SelectContent><SelectItem value="pending">{t("hub.invitation_pending")}</SelectItem><SelectItem value="all">{t("hub.all_statuses")}</SelectItem></SelectContent></Select><Button variant="ghost" size="icon" aria-label={t("hub.refresh")} onClick={() => void invitations.reload()}><RotateCw /></Button></div>
      <InvitationList invitations={invitations.list} canManage={canManage} statusFilter={filter} onRevoke={setRevoking} onCopy={(url) => void context.copy(url)} />
    </>}
    <ErrorNote message={invitations.error} />
    <InviteDialog open={inviteOpen} onOpenChange={setInviteOpen} context={context} legacy={invitations.list === "unavailable"} onCreated={() => void invitations.reload()} />
    <ConfirmDialog open={Boolean(revoking)} onOpenChange={(open) => !open && setRevoking(null)} title={t("hub.revoke_invitation_title")} description={t("hub.revoke_invitation_body", { name: revoking?.email || t("hub.open_link") })} confirmLabel={t("hub.revoke")} onConfirm={() => void revoke()} />
    <ApiEquivalent calls={[{ method: "GET", path: `/api/organizations/${organization.organization_id}/invitations` }, { method: "POST", path: `/api/organizations/${organization.organization_id}/invitations` }, { method: "DELETE", path: `/api/organizations/${organization.organization_id}/invitations/{id}` }]} />
  </>
}

export function GroupsPage({ context }: { context: OrgPageContext }) {
  const { organization, directory, preview } = context
  const canManage = isManager(organization.role)
  const [selected, setSelected] = useState<string | null>(null)
  const [name, setName] = useState("")
  const [deleting, setDeleting] = useState<OrganizationGroup | null>(null)
  const [undo, setUndo] = useState<{ groupId: string; userId: string; present: boolean; text: string } | null>(null)
  const [error, setError] = useState<string | null>(null)
  const group = directory.groups.find((item) => item.group_id === selected) ?? directory.groups[0]
  async function create(event: FormEvent) {
    event.preventDefault()
    try {
      const data = await api<{ group?: OrganizationGroup }>(`${orgPath(organization)}/groups`, { method: "POST", body: JSON.stringify({ name }) })
      setName(""); await directory.reload(); if (data?.group?.group_id) setSelected(data.group.group_id)
    } catch (reason) { setError(message(reason)) }
  }
  async function setMember(groupId: string, userId: string, present: boolean, offerUndo = true) {
    setError(null)
    try {
      const data = await api<{ groups: OrganizationGroup[] }>(`${orgPath(organization)}/groups/${encodeURIComponent(groupId)}/members/${encodeURIComponent(userId)}`, { method: present ? "PUT" : "DELETE" })
      if (Array.isArray(data?.groups)) directory.setGroups(data.groups); else await directory.reload()
      const person = directory.members.find((member) => member.user_id === userId)?.preferred_name ?? userId
      const groupName = directory.groups.find((item) => item.group_id === groupId)?.name ?? ""
      setUndo(offerUndo ? { groupId, userId, present: !present, text: t(present ? "hub.added_to_group" : "hub.removed_from_group", { name: person, group: groupName }) } : null)
    } catch (reason) { setError(message(reason)) }
  }
  async function remove() {
    if (!deleting) return
    try { await api(`${orgPath(organization)}/groups/${encodeURIComponent(deleting.group_id)}`, { method: "DELETE" }); setDeleting(null); setSelected(null); await directory.reload() }
    catch (reason) { setError(message(reason)) }
  }
  const memberOptions = group ? directory.members.filter((member) => !group.member_ids.includes(member.user_id)).map((member) => ({ id: member.user_id, label: member.preferred_name, detail: member.email })) : []
  return <>
    <PageHeader title={t("org.groups")} description={t("org.member_groups_used_for_workspace")} />
    {!canManage && <PermissionNote>{t("hub.only_managers_groups")}</PermissionNote>}
    {canManage && <form onSubmit={create} className="mb-4 flex max-w-md gap-2"><Input aria-label={t("org.group_name")} placeholder={t("hub.new_group_name")} value={name} onChange={(event) => setName(event.target.value)} required maxLength={80} disabled={preview} className="max-md:h-11 max-md:text-base" /><Button type="submit" variant="outline" className="max-md:h-11" disabled={!name.trim() || preview}><Plus />{t("org.create")}</Button></form>}
    <div className="grid gap-4 md:grid-cols-[220px_minmax(0,1fr)]">
      <ul aria-label={t("org.groups")} className="space-y-0.5">{directory.groups.map((item) => <li key={item.group_id}><button type="button" aria-current={item.group_id === group?.group_id ? "true" : undefined} onClick={() => setSelected(item.group_id)} className={cn("flex w-full items-center justify-between gap-2 rounded-md px-3 py-2 text-left text-sm max-md:min-h-11", item.group_id === group?.group_id ? "bg-accent font-medium" : "hover:bg-accent/60")}><span className="truncate">{item.name}</span><span className="text-xs tabular-nums text-muted-foreground">{item.member_ids.length}</span></button></li>)}{!directory.groups.length && <li className="px-3 py-6 text-center text-xs text-muted-foreground">{t("org.no_groups_yet")}</li>}</ul>
      {group && <section aria-label={group.name} className="rounded-lg border p-4">
        <div className="mb-3 flex items-center justify-between gap-2"><div className="min-w-0"><h2 className="truncate text-sm font-semibold">{group.name}</h2><p className="text-xs text-muted-foreground">{t("org.member_count", { count: group.member_ids.length })}</p></div>{canManage && <Button variant="ghost" size="sm" className="text-destructive max-md:h-11" onClick={() => setDeleting(group)}><Trash2 />{t("hub.delete_group")}</Button>}</div>
        <ul aria-label={t("hub.group_members", { name: group.name })} className="mb-3 flex flex-wrap gap-2">{group.member_ids.map((id) => {
          const member = directory.members.find((item) => item.user_id === id)
          const label = member?.preferred_name ?? id
          return <li key={id} className="inline-flex items-center gap-1 rounded-full border bg-background py-0.5 pl-3 pr-1 text-xs"><span className="max-w-48 truncate">{label}</span>{canManage && <button type="button" aria-label={t("hub.remove_from_group", { name: label, group: group.name })} onClick={() => void setMember(group.group_id, id, false)} className="grid size-6 place-items-center rounded-full text-muted-foreground hover:bg-accent hover:text-foreground max-md:size-9"><X aria-hidden="true" className="size-3" /></button>}</li>
        })}{!group.member_ids.length && <li className="text-xs text-muted-foreground">{t("hub.group_empty")}</li>}</ul>
        {canManage && <Picker label={t("hub.add_member_to_group", { group: group.name })} placeholder={t("hub.search_people")} options={memberOptions} onPick={(option) => void setMember(group.group_id, option.id, true)} disabled={preview} />}
      </section>}
    </div>
    {undo && <div role="status" className="mt-4 flex items-center justify-between gap-3 rounded-md border bg-muted/40 px-3 py-2 text-xs"><span>{undo.text}</span><Button size="sm" variant="outline" onClick={() => { const action = undo; setUndo(null); void setMember(action.groupId, action.userId, action.present, false) }}>{t("hub.undo")}</Button></div>}
    <ErrorNote message={error ?? directory.error} />
    <ConfirmDialog open={Boolean(deleting)} onOpenChange={(open) => !open && setDeleting(null)} title={t("hub.delete_group_title", { name: deleting?.name ?? "" })} description={t("hub.delete_group_body", { count: deleting?.member_ids.length ?? 0 })} confirmLabel={t("hub.delete_group")} onConfirm={() => void remove()} />
    <ApiEquivalent calls={[{ method: "GET", path: `/api/organizations/${organization.organization_id}/groups` }, { method: "POST", path: `/api/organizations/${organization.organization_id}/groups`, note: "{ name }" }, { method: "PUT", path: `/api/organizations/${organization.organization_id}/groups/{group_id}/members/{user_id}` }, { method: "DELETE", path: `/api/organizations/${organization.organization_id}/groups/{group_id}/members/{user_id}` }, { method: "DELETE", path: `/api/organizations/${organization.organization_id}/groups/{group_id}` }]} />
  </>
}

export function OrgWorkspacesPage({ context }: { context: OrgPageContext }) {
  const { organization, workspaces, preview, linkSearch } = context
  const index = useWorkspaceAccessIndex(workspaces, true, preview)
  return <>
    <PageHeader title={t("org.workspaces")} description={t("org.workspaces_owned_by_this_organization")} actions={<Button variant="outline" className="max-md:h-11" onClick={context.createWorkspace}><Plus />{t("org.create_workspace")}</Button>} />
    <ul aria-label={t("org.workspaces")} className="divide-y rounded-lg border">{workspaces.map((workspace) => {
      const access = index[workspace.workspace_id]?.access
      return <li key={workspace.workspace_id} className="flex flex-wrap items-center justify-between gap-3 px-4 py-3">
        <div className="min-w-0"><p className="truncate text-sm font-medium">{workspace.name} {access?.access_mode === "restricted" && <Badge tone="warning">{t("hub.restricted")}</Badge>}</p><p className="truncate text-xs text-muted-foreground">{access ? t("hub.workspace_role", { role: roleLabel(access.current_role) }) : <span className="font-mono">{workspace.workspace_id}</span>}</p></div>
        <div className="flex shrink-0 gap-2"><Button asChild variant="outline" size="sm" className="max-md:h-11"><Link to={{ pathname: workspacePath(organization.organization_id, workspace.workspace_id), search: linkSearch }} aria-label={t("org.select_workspace", { name: workspace.name })}>{t("hub.open")}</Link></Button><Button asChild variant="ghost" size="sm" className="max-md:h-11"><Link to={{ pathname: workspaceSettingsPath(organization.organization_id, workspace.workspace_id, "general"), search: linkSearch }} aria-label={t("hub.settings_for", { name: workspace.name })}>{t("settings.title")}</Link></Button></div>
      </li>
    })}{!workspaces.length && <li className="px-4 py-6 text-center text-xs text-muted-foreground">{t("org.no_workspaces_yet")}</li>}</ul>
    <ApiEquivalent calls={[{ method: "GET", path: `/api/workspaces?organization_id=${organization.organization_id}` }, { method: "POST", path: "/api/workspaces", note: "{ organization_id, name }" }]} />
  </>
}

export function OrgAuditPage({ context }: { context: OrgPageContext }) {
  const { organization, directory, preview } = context
  const canManage = isManager(organization.role)
  const [events, setEvents] = useState<OrganizationAuditEvent[]>([])
  const [loading, setLoading] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const load = useCallback(async () => {
    if (!canManage || preview) return
    setLoading(true); setError(null)
    try { setEvents((await api<{ events: OrganizationAuditEvent[] }>(`${orgPath(organization)}/audit-events?limit=100`)).events) }
    catch (reason) { setError(message(reason)) }
    finally { setLoading(false) }
  }, [organization.organization_id, canManage, preview]) // eslint-disable-line react-hooks/exhaustive-deps
  useEffect(() => { void load() }, [load])
  return <>
    <PageHeader title={t("audit.audit")} description={t("org.management_events_and_workspace_traffic")} actions={canManage ? <Button variant="outline" className="max-md:h-11" onClick={() => void load()} disabled={loading}><RotateCw />{t("hub.refresh")}</Button> : undefined} />
    {canManage ? <AuditEventList events={events} members={directory.members} loading={loading} /> : <PermissionNote>{t("hub.only_managers_audit")}</PermissionNote>}
    <ErrorNote message={error} />
    <ApiEquivalent calls={[{ method: "GET", path: `/api/organizations/${organization.organization_id}/audit-events?limit=100`, note: "workspace_id?, before?" }]} />
  </>
}

export function OrgDangerPage({ context }: { context: OrgPageContext }) {
  const { organization, user, directory, preview, workspaces } = context
  const isOwner = organization.role === "owner"
  const owners = directory.members.filter((member) => member.role === "owner")
  const soleOwner = isOwner && owners.length <= 1
  const [dialog, setDialog] = useState<"leave" | "transfer" | "delete" | null>(null)
  const [target, setTarget] = useState("")
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const open = (value: typeof dialog) => { setError(null); setTarget(""); setDialog(value) }
  async function run(action: () => Promise<unknown>, gone: boolean) {
    setBusy(true); setError(null)
    try { await action(); setDialog(null); if (gone) context.organizationGone(organization.organization_id); else { await context.refreshOrganizations(); await directory.reload() } }
    catch (reason) {
      if (isUnavailable(reason)) setError(t("hub.action_unavailable"))
      else if (reason instanceof ApiError && reason.status === 409 && dialog === "delete") {
        const count = (reason.details as { workspace_count?: number; count?: number } | undefined)
        setError(t("hub.delete_org_has_workspaces", { count: count?.workspace_count ?? count?.count ?? workspaces.length }))
      } else setError(message(reason))
    } finally { setBusy(false) }
  }
  const transferOptions = directory.members.filter((member) => member.user_id !== user.user_id)
  return <>
    <PageHeader title={t("apps.danger_zone")} description={t("hub.org_danger_description")} />
    <Section title={t("hub.leave_org")} tone="danger" description={t("hub.leave_org_body")}>
      {soleOwner && <PermissionNote>{t("hub.sole_owner_cannot_leave")}</PermissionNote>}
      <Button variant="outline" className="text-destructive max-md:h-11" disabled={soleOwner || preview} onClick={() => open("leave")}>{t("hub.leave_org")}</Button>
    </Section>
    <Section title={t("hub.transfer_ownership")} tone="danger" description={t("hub.transfer_ownership_body")}>
      {!isOwner && <PermissionNote>{t("hub.only_owners_transfer")}</PermissionNote>}
      <Button variant="outline" className="text-destructive max-md:h-11" disabled={!isOwner || preview || !transferOptions.length} onClick={() => open("transfer")}>{t("hub.transfer_ownership")}</Button>
    </Section>
    <Section title={t("hub.delete_org")} tone="danger" description={t("hub.delete_org_body")}>
      {!isOwner && <PermissionNote>{t("hub.only_owners_delete_org")}</PermissionNote>}
      {isOwner && workspaces.length > 0 && <PermissionNote>{t("hub.delete_org_has_workspaces", { count: workspaces.length })}</PermissionNote>}
      <Button variant="destructive" className="max-md:h-11" disabled={!isOwner || preview} onClick={() => open("delete")}><Trash2 />{t("hub.delete_org")}</Button>
    </Section>
    <ConfirmDialog open={dialog === "leave"} onOpenChange={(value) => !value && setDialog(null)} title={t("hub.leave_org_title", { name: organization.name })} description={t("hub.leave_org_confirm")} confirmLabel={t("hub.leave_org")} confirmName={organization.name} busy={busy} error={error} onConfirm={() => void run(() => api(`${orgPath(organization)}/leave`, { method: "POST", body: "{}" }), true)} />
    <ConfirmDialog open={dialog === "transfer"} onOpenChange={(value) => !value && setDialog(null)} title={t("hub.transfer_ownership")} description={t("hub.transfer_ownership_confirm")} confirmLabel={t("hub.transfer_ownership")} confirmName={organization.name} disabled={!target} busy={busy} error={error} onConfirm={() => void run(() => api(`${orgPath(organization)}/transfer`, { method: "POST", body: JSON.stringify({ user_id: target }) }), false)}>
      <div className="space-y-2"><Label>{t("hub.new_owner")}</Label><Select value={target} onValueChange={setTarget}><SelectTrigger aria-label={t("hub.new_owner")} className="max-sm:h-11"><SelectValue placeholder={t("hub.choose_member")} /></SelectTrigger><SelectContent>{transferOptions.map((member) => <SelectItem key={member.user_id} value={member.user_id}>{member.preferred_name} · {member.email}</SelectItem>)}</SelectContent></Select></div>
    </ConfirmDialog>
    <ConfirmDialog open={dialog === "delete"} onOpenChange={(value) => !value && setDialog(null)} title={t("hub.delete_org_title", { name: organization.name })} description={t("hub.delete_org_confirm")} confirmLabel={t("hub.delete_org")} confirmName={organization.name} busy={busy} error={error} onConfirm={() => void run(() => api(orgPath(organization), { method: "DELETE", body: JSON.stringify({ confirm_name: organization.name }) }), true)} />
    <ApiEquivalent calls={[{ method: "POST", path: `/api/organizations/${organization.organization_id}/leave` }, { method: "POST", path: `/api/organizations/${organization.organization_id}/transfer`, note: "{ user_id }" }, { method: "DELETE", path: `/api/organizations/${organization.organization_id}`, note: "{ confirm_name }" }]} />
  </>
}
