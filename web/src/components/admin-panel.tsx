import { agentPath } from "@/lib/workload-path"
import { EmptyState, Status } from "@/components/control-plane-ui"
import { Button } from "@/components/ui/button"
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog"
import { Input } from "@/components/ui/input"
import { Label } from "@/components/ui/label"
import { Textarea } from "@/components/ui/textarea"
import { api, ApiError, type AdminDashboard, type AdminInvitation, type AdminMachine, type AdminOrganization, type AdminUser, type AdminUserDetail, type Agent, type ControlPlaneUpdateStatus, type PlatformAuditEvent } from "@/lib/api"
import { clearAdminTour, firstRunTourMode, shouldAutoStartAdminTour, startAdminTour, stopFirstRunTour, type AdminTourHost } from "@/lib/first-run-tour"
import { cn } from "@/lib/utils"
import {
  Building2,
  ChevronDown,
  Copy,
  Download,
  KeyRound,
  ListChecks,
  LogOut,
  RotateCw,
  ScrollText,
  Server,
  ShieldCheck,
  TerminalSquare,
  Users
} from "lucide-react"
import type * as React from "react"
import { FormEvent, useCallback, useEffect, useRef, useState } from "react"
import { roleLabel, t, tnodes } from "@/i18n"

export const PREVIEW_ADMIN_INVITE = "https://treer.example/?invite=inv_preview"

export const EMPTY_ADMIN_DASHBOARD: AdminDashboard = { user_count: 0, organization_count: 0, machine_count: 0, agent_count: 0 }

export type AdminInventory = "users" | "machines" | "agents" | "organizations" | "invitations" | "activity" | null

export function AdminPanel() {
  const preview = firstRunTourMode() === "preview"
  const [authenticated, setAuthenticated] = useState<boolean | undefined>(preview ? true : undefined)
  const [password, setPassword] = useState("")
  const [dashboard, setDashboard] = useState<AdminDashboard | null>(preview ? EMPTY_ADMIN_DASHBOARD : null)
  const [inventory, setInventory] = useState<AdminInventory>(null)
  const [inviteUrl, setInviteUrl] = useState("")
  const [error, setError] = useState("")
  const [submitting, setSubmitting] = useState(false)

  const loadDashboard = useCallback(async () => {
    if (preview) return
    setDashboard(await api<AdminDashboard>("/api/admin/dashboard"))
  }, [preview])

  useEffect(() => {
    if (preview) return
    api<{ admin: boolean }>("/api/admin/me")
      .then(() => { setAuthenticated(true); return loadDashboard() })
      .catch((reason) => {
        if (reason instanceof ApiError && reason.status === 401) setAuthenticated(false)
        else setError(reason instanceof Error ? reason.message : t("admin.unable_to_load_admin_panel"))
      })
  }, [preview, loadDashboard])

  async function login(event: FormEvent) {
    event.preventDefault(); setSubmitting(true); setError("")
    try {
      await api("/api/admin/login", { method: "POST", body: JSON.stringify({ password }) })
      setPassword(""); setAuthenticated(true); await loadDashboard()
    } catch (reason) { setError(reason instanceof Error ? reason.message : t("auth.err.failed")) }
    finally { setSubmitting(false) }
  }

  async function createInvite() {
    if (preview) {
      setInviteUrl(PREVIEW_ADMIN_INVITE)
      return
    }
    setError("")
    try {
      const data = await api<{ url: string }>("/api/admin/invitations", { method: "POST", body: "{}" })
      setInviteUrl(data.url)
    } catch (reason) { setError(reason instanceof Error ? reason.message : t("admin.unable_to_create_invitation")) }
  }

  async function logout() {
    if (preview) return
    await api("/api/admin/logout", { method: "POST", body: "{}" })
    setAuthenticated(false); setDashboard(null); setInviteUrl(""); setInventory(null)
  }

  const tourHostRef = useRef<AdminTourHost | null>(null)
  tourHostRef.current = {
    openInvite: () => { void createInvite() },
    closeInvite: () => setInviteUrl(""),
  }

  function replayAdminTour() {
    clearAdminTour()
    if (tourHostRef.current) startAdminTour(tourHostRef.current, { persist: !preview })
  }

  useEffect(() => {
    if (!authenticated) return
    if (!shouldAutoStartAdminTour()) return
    const timer = window.setTimeout(() => {
      if (!tourHostRef.current) return
      startAdminTour(tourHostRef.current, { persist: !preview })
    }, 450)
    return () => {
      window.clearTimeout(timer)
      stopFirstRunTour()
    }
  }, [authenticated, preview])

  if (authenticated === undefined) return <div className="grid min-h-dvh place-items-center bg-sidebar text-sm text-muted-foreground">{t("admin.loading_admin")}</div>
  if (!authenticated) return <main className="grid min-h-dvh place-items-center bg-sidebar p-4"><form onSubmit={login} className="w-full max-w-[390px] rounded-lg border bg-background p-7 shadow-sm"><div className="mb-6 grid size-9 place-items-center rounded-md bg-[#37352f] text-white"><ShieldCheck className="size-4" /></div><h1 className="text-xl font-semibold">{t("admin.treer_administration")}</h1><p className="mt-1 text-sm text-muted-foreground">{t("admin.platform_access_is_separate_from")}</p><div className="mt-6 space-y-2"><Label htmlFor="admin-password">{t("admin.admin_password")}</Label><Input id="admin-password" type="password" autoComplete="current-password" value={password} onChange={(event) => setPassword(event.target.value)} required autoFocus /></div><div className="mt-3 min-h-5 text-xs text-destructive">{error}</div><div className="mt-4 flex justify-end"><Button type="submit" disabled={submitting}>{submitting ? t("auth.wait") : t("admin.open_admin_panel")}</Button></div></form></main>

  return <main className="min-h-dvh bg-sidebar">
    {preview && <div className="treer-tour-banner"><span>{t("admin.tour_preview_admin_panel_no")}</span></div>}
    <header className="border-b bg-background"><div className="mx-auto flex h-14 w-full max-w-4xl items-center justify-between px-5"><div className="flex min-w-0 items-center gap-2.5 text-sm font-semibold"><span className="grid size-7 shrink-0 place-items-center rounded bg-[#37352f] text-white"><ShieldCheck className="size-3.5" /></span><span className="truncate">{t("admin.treer_administration")}</span></div><div className="flex shrink-0 items-center gap-1"><Button variant="ghost" size="sm" asChild><a href="/" data-tour="admin-workspace-link">{t("admin.user_workspace")}</a></Button><Button size="icon" variant="ghost" aria-label={t("userMenu.logout")} onClick={logout} disabled={preview}><LogOut /></Button></div></div></header>
    <div className="mx-auto max-w-4xl px-5 py-10">
      <div className="mb-8 flex flex-col items-start gap-4 sm:flex-row sm:items-end sm:justify-between">
        <div><h1 className="text-2xl font-semibold">{t("admin.platform_overview")}</h1><p className="mt-1 text-sm text-muted-foreground">{t("admin.current_resources_across_all_organizations")}</p></div>
        <div className="flex gap-1"><Button size="sm" variant="outline" onClick={replayAdminTour}><ListChecks />{t("admin.replay_tour")}</Button><Button size="sm" onClick={loadDashboard} disabled={preview}><RotateCw />{t("admin.refresh")}</Button></div>
      </div>
      {error && <div className="mb-5 rounded border border-destructive/30 bg-destructive/5 px-3 py-2 text-sm text-destructive">{error}</div>}
      <div className="grid grid-cols-3 border-y" data-tour="admin-overview">
        <AdminCountCard label={t("admin.users")} icon={<Users className="size-3.5" />} value={dashboard?.user_count} active={inventory === "users"} onClick={() => setInventory(inventory === "users" ? null : "users")} />
        <AdminCountCard label={t("wsettings.machines")} icon={<Server className="size-3.5" />} value={dashboard?.machine_count} active={inventory === "machines"} bordered onClick={() => setInventory(inventory === "machines" ? null : "machines")} />
        <AdminCountCard label={t("admin.agents")} icon={<TerminalSquare className="size-3.5" />} value={dashboard?.agent_count} active={inventory === "agents"} onClick={() => setInventory(inventory === "agents" ? null : "agents")} />
      </div>
      {inventory && <AdminInventoryPanel kind={inventory} preview={preview} onError={setError} onChanged={loadDashboard} />}
      <ControlPlaneUpdate preview={preview} onError={setError} />
      <section className="mt-12" data-tour="admin-invite">
        <h2 className="text-sm font-semibold">{t("admin.user_invitations")}</h2>
        <div className="mt-3 grid gap-4 border-y py-4 sm:grid-cols-[minmax(0,1fr)_auto] sm:items-center">
          <div><div className="text-sm font-medium">{t("admin.invite_a_new_user")}</div><div className="mt-1 text-xs text-muted-foreground">{t("admin.registration_creates_a_personal_organization")}</div></div>
          <div className="flex flex-wrap gap-2">
            <Button size="sm" variant={inventory === "invitations" ? "secondary" : "outline"} onClick={() => setInventory(inventory === "invitations" ? null : "invitations")}>{t("admin.pending_invitations")}</Button>
            <Button size="sm" onClick={createInvite}><KeyRound />{t("admin.create_invitation")}</Button>
          </div>
        </div>
      </section>
      <section className="mt-12">
        <h2 className="text-sm font-semibold">{t("admin.organizations")}</h2>
        <div className="mt-3 grid gap-4 border-y py-4 sm:grid-cols-[minmax(0,1fr)_auto] sm:items-center">
          <div><div className="text-sm font-medium">{t("admin.all_organizations")}</div><div className="mt-1 text-xs text-muted-foreground">{t("admin.org_count", { count: dashboard?.organization_count ?? "—" })}</div></div>
          <Button size="sm" variant={inventory === "organizations" ? "secondary" : "outline"} onClick={() => setInventory(inventory === "organizations" ? null : "organizations")}><Building2 />{t("admin.view_organizations")}</Button>
        </div>
      </section>
      <section className="mt-12">
        <h2 className="text-sm font-semibold">{t("admin.admin_activity")}</h2>
        <div className="mt-3 grid gap-4 border-y py-4 sm:grid-cols-[minmax(0,1fr)_auto] sm:items-center">
          <div><div className="text-sm font-medium">{t("admin.recent_operator_actions")}</div><div className="mt-1 text-xs text-muted-foreground">{t("admin.password_resets_session_revokes_and")}</div></div>
          <Button size="sm" variant={inventory === "activity" ? "secondary" : "outline"} onClick={() => setInventory(inventory === "activity" ? null : "activity")}><ScrollText />{t("admin.view_activity")}</Button>
        </div>
      </section>
    </div>
    <Dialog open={Boolean(inviteUrl)} onOpenChange={(open) => !open && setInviteUrl("")}><DialogContent data-tour="admin-invite-dialog"><DialogHeader><DialogTitle>{t("admin.user_invitation")}</DialogTitle><DialogDescription>{t("admin.this_one_time_registration_link")}</DialogDescription></DialogHeader><Textarea readOnly value={inviteUrl} className="min-h-24 font-mono text-xs" /><DialogFooter><Button variant="outline" onClick={() => setInviteUrl("")}>{t("org.close")}</Button><Button onClick={() => navigator.clipboard.writeText(inviteUrl)}><Copy />{t("dialogs.copy_link")}</Button></DialogFooter></DialogContent></Dialog>
  </main>
}

export function AdminCountCard({ label, icon, value, active, bordered, onClick }: { label: string; icon: React.ReactNode; value?: number; active: boolean; bordered?: boolean; onClick: () => void }) {
  return <button type="button" onClick={onClick} className={cn("py-6 text-left transition-colors hover:bg-accent/40", bordered && "border-x px-6", !bordered && "px-0", active && "bg-accent/50")}>
    <div className="flex items-center gap-2 text-xs text-muted-foreground">{icon}{label}</div>
    <div className="mt-2 text-3xl font-semibold tabular-nums">{value ?? "-"}</div>
  </button>
}

export function AdminInventoryPanel({ kind, preview, onError, onChanged }: { kind: Exclude<AdminInventory, null>; preview: boolean; onError: (message: string) => void; onChanged: () => Promise<void> }) {
  const [query, setQuery] = useState("")
  const [users, setUsers] = useState<AdminUser[]>([])
  const [machines, setMachines] = useState<AdminMachine[]>([])
  const [agents, setAgents] = useState<Agent[]>([])
  const [organizations, setOrganizations] = useState<AdminOrganization[]>([])
  const [invitations, setInvitations] = useState<AdminInvitation[]>([])
  const [events, setEvents] = useState<PlatformAuditEvent[]>([])
  const [openMachine, setOpenMachine] = useState<string | null>(null)
  const [machineDetail, setMachineDetail] = useState<AdminMachine | null>(null)
  const [userDetail, setUserDetail] = useState<AdminUserDetail | null>(null)
  const [resetUrl, setResetUrl] = useState("")
  const [resetEmailed, setResetEmailed] = useState(false)
  const [loading, setLoading] = useState(false)

  const load = useCallback(async () => {
    if (preview) return
    setLoading(true)
    try {
      if (kind === "users") {
        const data = await api<{ users: AdminUser[] }>(`/api/admin/users${query.trim() ? `?q=${encodeURIComponent(query.trim())}` : ""}`)
        setUsers(data.users)
      } else if (kind === "machines") {
        setMachines((await api<{ machines: AdminMachine[] }>("/api/admin/machines")).machines)
      } else if (kind === "agents") {
        setAgents((await api<{ agents: Agent[] }>("/api/admin/agents")).agents)
      } else if (kind === "organizations") {
        setOrganizations((await api<{ organizations: AdminOrganization[] }>("/api/admin/organizations")).organizations)
      } else if (kind === "invitations") {
        setInvitations((await api<{ invitations: AdminInvitation[] }>("/api/admin/invitations")).invitations)
      } else {
        setEvents((await api<{ events: PlatformAuditEvent[] }>("/api/admin/activity")).events)
      }
    } catch (reason) {
      onError(reason instanceof Error ? reason.message : t("admin.unable_to_load_admin_inventory"))
    } finally {
      setLoading(false)
    }
  }, [kind, preview, query, onError])

  useEffect(() => { void load() }, [load])

  async function openUser(userId: string) {
    try { setUserDetail((await api<{ user: AdminUserDetail }>(`/api/admin/users/${userId}`)).user) }
    catch (reason) { onError(reason instanceof Error ? reason.message : t("admin.unable_to_load_user")) }
  }

  async function resetPassword(userId: string) {
    try {
      const data = await api<{ url: string; emailed: boolean }>(`/api/admin/users/${userId}/password-reset`, { method: "POST", body: "{}" })
      setResetUrl(data.url)
      setResetEmailed(data.emailed)
      await load()
    } catch (reason) { onError(reason instanceof Error ? reason.message : t("admin.unable_to_issue_password_reset")) }
  }

  async function revokeSessions(userId: string) {
    try {
      await api(`/api/admin/users/${userId}/revoke-sessions`, { method: "POST", body: "{}" })
      await load()
    } catch (reason) { onError(reason instanceof Error ? reason.message : t("admin.unable_to_sign_the_user")) }
  }

  async function toggleMachine(serverId: string) {
    if (openMachine === serverId) { setOpenMachine(null); setMachineDetail(null); return }
    setOpenMachine(serverId)
    try { setMachineDetail((await api<{ machine: AdminMachine }>(`/api/admin/machines/${serverId}`)).machine) }
    catch (reason) { onError(reason instanceof Error ? reason.message : t("admin.unable_to_load_machine")) }
  }

  async function revokeInvite(token: string) {
    try {
      await api(`/api/admin/invitations/${encodeURIComponent(token)}`, { method: "DELETE" })
      await load()
      await onChanged()
    } catch (reason) { onError(reason instanceof Error ? reason.message : t("admin.unable_to_revoke_invitation")) }
  }

  const title = kind === "users" ? t("admin.users") : kind === "machines" ? t("wsettings.machines") : kind === "agents" ? t("admin.agents") : kind === "organizations" ? t("admin.organizations") : kind === "invitations" ? t("admin.pending_invitations") : t("admin.admin_activity")

  return <div className="border-b">
    <div className="flex items-center justify-between gap-3 py-3">
      <div className="text-sm font-medium">{title}</div>
      {kind === "users" && <Input value={query} onChange={(event) => setQuery(event.target.value)} placeholder={t("admin.search_name_or_email")} className="h-8 max-w-xs text-xs" />}
    </div>
    {loading && <div className="py-6 text-xs text-muted-foreground">{t("admin.loading")}</div>}
    {!loading && kind === "users" && (users.length ? users.map((user) => (
      <div key={user.user_id} className="grid gap-3 border-t py-3 sm:grid-cols-[minmax(0,1fr)_auto] sm:items-center">
        <div className="min-w-0"><div className="truncate text-sm font-medium">{user.preferred_name}</div><div className="mt-0.5 truncate text-[11px] text-muted-foreground">{user.email}</div></div>
        <div className="flex flex-wrap gap-1">
          <Button size="sm" variant="outline" onClick={() => void openUser(user.user_id)}>{t("admin.details")}</Button>
          <Button size="sm" variant="outline" onClick={() => void resetPassword(user.user_id)}>{t("admin.reset_password")}</Button>
          <Button size="sm" variant="outline" onClick={() => void revokeSessions(user.user_id)}>{t("admin.sign_out_everywhere")}</Button>
        </div>
      </div>
    )) : <EmptyState icon={<Users />} label={t("admin.no_users_match_this_search")} />)}
    {!loading && kind === "machines" && (machines.length ? machines.map((machine) => (
      <div key={machine.server_id} className="border-t">
        <button type="button" className="flex w-full items-center gap-3 py-3 text-left" onClick={() => void toggleMachine(machine.server_id)}>
          <ChevronDown className={cn("size-3.5 shrink-0 text-muted-foreground transition-transform", openMachine === machine.server_id && "rotate-180")} />
          <span className="min-w-0 flex-1"><span className="block truncate text-sm font-medium">{machine.name}</span><span className="mt-0.5 block truncate text-[11px] text-muted-foreground">{machine.hostname || machine.server_id} · {machine.workspace_name}</span></span>
          <Status value={machine.status} />
        </button>
        {openMachine === machine.server_id && <div className="pb-3 pl-7">
          {(machineDetail?.agents ?? []).length ? machineDetail!.agents!.map((agent) => (
            <div key={agent.agent_id} className="flex items-center justify-between gap-3 py-1.5 text-sm">
              <span className="min-w-0 truncate">{agentPath(agent)}<span className="ml-2 text-[11px] text-muted-foreground">{agent.kind}{agent.interface ? " · AIS" : ""}</span></span>
              <Status value={agent.status} />
            </div>
          )) : <div className="py-2 text-[11px] text-muted-foreground">{t("admin.no_live_agents_on_this")}</div>}
        </div>}
      </div>
    )) : <EmptyState icon={<Server />} label={t("admin.no_enrolled_machines")} />)}
    {!loading && kind === "agents" && (agents.length ? Object.entries(agents.reduce<Record<string, Agent[]>>((groups, agent) => {
      const key = agent.server_id
      groups[key] = groups[key] ? [...groups[key], agent] : [agent]
      return groups
    }, {})).map(([serverId, group]) => (
      <div key={serverId} className="border-t py-3">
        <div className="text-[11px] text-muted-foreground">{serverId}</div>
        {group.map((agent) => (
          <div key={agent.agent_id} className="mt-1 flex items-center justify-between gap-3 text-sm">
            <span className="truncate">{agentPath(agent)}<span className="ml-2 text-[11px] text-muted-foreground">{agent.kind}{agent.interface ? " · AIS" : ""}</span></span>
            <Status value={agent.status} />
          </div>
        ))}
      </div>
    )) : <EmptyState icon={<TerminalSquare />} label={t("admin.no_live_agents")} />)}
    {!loading && kind === "organizations" && (organizations.length ? organizations.map((organization) => (
      <div key={organization.organization_id} className="grid gap-1 border-t py-3 sm:grid-cols-[minmax(0,1fr)_auto] sm:items-center">
        <div><div className="text-sm font-medium">{organization.name}</div><div className="mt-0.5 text-[11px] text-muted-foreground">{organization.owner_email || "No owner"} · {organization.workspace_count} workspaces · {organization.machine_count} machines</div></div>
      </div>
    )) : <EmptyState icon={<Building2 />} label={t("admin.no_organizations")} />)}
    {!loading && kind === "invitations" && (invitations.length ? invitations.map((invitation) => (
      <div key={invitation.token} className="grid gap-3 border-t py-3 sm:grid-cols-[minmax(0,1fr)_auto] sm:items-center">
        <div className="min-w-0 font-mono text-[11px] text-muted-foreground">{invitation.created_at}</div>
        <div className="flex gap-1">
          <Button size="sm" variant="outline" onClick={() => navigator.clipboard.writeText(invitation.url)}><Copy />{t("admin.copy")}</Button>
          <Button size="sm" variant="outline" onClick={() => void revokeInvite(invitation.token)}>{t("admin.revoke")}</Button>
        </div>
      </div>
    )) : <EmptyState icon={<KeyRound />} label={t("admin.no_pending_invitations")} />)}
    {!loading && kind === "activity" && (events.length ? events.map((event) => (
      <div key={event.event_id} className="border-t py-3 text-sm">
        <div className="font-medium">{event.action}</div>
        <div className="mt-0.5 text-[11px] text-muted-foreground">{event.occurred_at} · {event.resource_kind} {event.resource_name || event.resource_id}</div>
      </div>
    )) : <EmptyState icon={<ScrollText />} label={t("admin.no_admin_activity_yet")} />)}
    <Dialog open={Boolean(userDetail)} onOpenChange={(open) => !open && setUserDetail(null)}>
      <DialogContent className="max-w-lg">
        <DialogHeader><DialogTitle>{userDetail?.preferred_name}</DialogTitle><DialogDescription>{userDetail?.email}</DialogDescription></DialogHeader>
        {userDetail && <div className="space-y-3 text-sm">
          <div className="text-xs text-muted-foreground">Verified {userDetail.email_verified ? "yes" : "no"} · OAuth {userDetail.oauth_providers.join(", ") || "none"}</div>
          <div>
            <div className="text-xs font-medium text-muted-foreground">{t("admin.organizations")}</div>
            {userDetail.organizations.map((organization) => <div key={organization.organization_id}>{organization.name} · {roleLabel(organization.role)}</div>)}
          </div>
          <div>
            <div className="text-xs font-medium text-muted-foreground">{t("org.workspaces")}</div>
            {userDetail.workspaces.map((workspace) => <div key={workspace.workspace_id}>{workspace.name}</div>)}
            {userDetail.workspaces.length === 0 && <div className="text-muted-foreground">{t("admin.none")}</div>}
          </div>
        </div>}
      </DialogContent>
    </Dialog>
    <Dialog open={Boolean(resetUrl)} onOpenChange={(open) => !open && setResetUrl("")}>
      <DialogContent>
        <DialogHeader><DialogTitle>{t("admin.password_reset")}</DialogTitle><DialogDescription>{resetEmailed ? "A reset email was sent. The link also works if you copy it." : t("admin.email_sending_is_not_configured")}</DialogDescription></DialogHeader>
        <Textarea readOnly value={resetUrl} className="min-h-24 font-mono text-xs" />
        <DialogFooter><Button variant="outline" onClick={() => setResetUrl("")}>{t("org.close")}</Button><Button onClick={() => navigator.clipboard.writeText(resetUrl)}><Copy />{t("dialogs.copy_link")}</Button></DialogFooter>
      </DialogContent>
    </Dialog>
  </div>
}

export function ControlPlaneUpdate({ preview, onError }: { preview?: boolean; onError: (message: string) => void }) {
  const [status, setStatus] = useState<ControlPlaneUpdateStatus | null>(null)
  const [configured, setConfigured] = useState<boolean | null>(null)
  const [busy, setBusy] = useState(false)

  const loadStatus = useCallback(async () => {
    try {
      const next = await api<ControlPlaneUpdateStatus>("/api/admin/update")
      setConfigured(true)
      setStatus(next)
      return next
    } catch (reason) {
      if (reason instanceof ApiError && reason.status === 404) {
        setConfigured(false)
        setStatus(null)
        return null
      }
      throw reason
    }
  }, [])

  useEffect(() => {
    if (preview) {
      setConfigured(false)
      return
    }
    loadStatus().catch((reason) => onError(reason instanceof Error ? reason.message : t("admin.unable_to_load_control_plane")))
  }, [preview, loadStatus, onError])

  async function checkForUpdates() {
    setBusy(true)
    try {
      const next = await api<ControlPlaneUpdateStatus>("/api/admin/update/check")
      setConfigured(true)
      setStatus(next)
    } catch (reason) {
      onError(reason instanceof Error ? reason.message : t("admin.unable_to_check_for_updates"))
    } finally {
      setBusy(false)
    }
  }

  async function applyUpdate() {
    setBusy(true)
    onError("")
    try {
      const accepted = await api<ControlPlaneUpdateStatus>("/api/admin/update", { method: "POST", body: "{}" })
      setStatus(accepted)
      for (let attempt = 0; attempt < 60; attempt += 1) {
        await new Promise((resolve) => window.setTimeout(resolve, 2000))
        try {
          const next = await loadStatus()
          if (!next) return
          if (next.job?.state === "running") continue
          if (next.job?.state === "failed") {
            onError(next.job.error || t("admin.control_plane_update_failed"))
            return
          }
          return
        } catch {
          // Proxy and updater bounce while Compose recreates them.
        }
      }
      onError(t("admin.the_update_is_still_running"))
    } catch (reason) {
      onError(reason instanceof Error ? reason.message : t("admin.unable_to_apply_the_update"))
    } finally {
      setBusy(false)
    }
  }

  if (configured === null) return null
  if (!configured) {
    return <section className="mt-12">
      <h2 className="text-sm font-semibold">{t("admin.control_plane")}</h2>
      <p className="mt-3 max-w-2xl text-sm leading-6 text-muted-foreground">{t("admin.control_plane_updates_are_not")}</p>
    </section>
  }

  const jobRunning = status?.job?.state === "running" || busy
  const digestLabel = (value?: string | null) => value ? value.replace(/^sha256:/, "").slice(0, 12) : "unknown"

  return <section className="mt-12">
    <h2 className="text-sm font-semibold">{t("admin.control_plane")}</h2>
    <p className="mt-1 text-xs text-muted-foreground">{tnodes("admin.updates_note", { command: <span className="font-mono">{t("admin.treer_agent_server_update")}</span> })}</p>
    <div className="mt-3 grid gap-4 border-y py-4 sm:grid-cols-[minmax(0,1fr)_auto] sm:items-start">
      <div>
        <div className="text-sm font-medium">Channel {status?.channel ?? "—"}</div>
        <div className="mt-2 space-y-1">
          {(status?.services ?? []).map((service) => (
            <div key={service.name} className="flex flex-wrap items-baseline gap-x-3 text-xs">
              <span className="w-16 font-medium">{service.name}</span>
              <span className="text-muted-foreground">{service.present ? service.version || "running" : "missing"}</span>
              <span className="font-mono text-[10px] text-muted-foreground">{digestLabel(service.digest)}</span>
              {service.update_available && <span className="text-emerald-700">{t("admin.update_available")}</span>}
            </div>
          ))}
        </div>
        {status?.job?.state === "failed" && <div className="mt-2 text-xs text-destructive">{status.job.error}</div>}
        {jobRunning && <div className="mt-2 text-xs text-muted-foreground">{t("admin.applying_images_this_page_may")}</div>}
      </div>
      <div className="flex flex-wrap gap-2">
        <Button size="sm" variant="outline" onClick={() => { void checkForUpdates() }} disabled={jobRunning}><RotateCw />{t("admin.check_for_updates")}</Button>
        <Button size="sm" onClick={() => { void applyUpdate() }} disabled={jobRunning || !(status?.update_available || (status?.services ?? []).some((service) => service.update_available))}><Download />{t("admin.apply_update")}</Button>
      </div>
    </div>
  </section>
}
