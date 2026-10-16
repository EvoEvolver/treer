import { AdminTraffic } from "@/components/admin-traffic"
import { EmptyState } from "@/components/control-plane-ui"
import { Button } from "@/components/ui/button"
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog"
import { Input } from "@/components/ui/input"
import { Label } from "@/components/ui/label"
import { Textarea } from "@/components/ui/textarea"
import { api, ApiError, type AdminDashboard, type AdminInvitation, type ControlPlaneUpdateStatus } from "@/lib/api"
import { clearAdminTour, firstRunTourMode, shouldAutoStartAdminTour, startAdminTour, stopFirstRunTour, type AdminTourHost } from "@/lib/first-run-tour"
import {
  Building2,
  Copy,
  Download,
  KeyRound,
  ListChecks,
  LogOut,
  RotateCw,
  Server,
  ShieldCheck,
  TerminalSquare,
  Users
} from "lucide-react"
import type * as React from "react"
import { FormEvent, useCallback, useEffect, useRef, useState } from "react"
import { t, tnodes } from "@/i18n"

export const PREVIEW_ADMIN_INVITE = "https://treer.example/?invite=inv_preview"

export const EMPTY_ADMIN_DASHBOARD: AdminDashboard = { user_count: 0, organization_count: 0, machine_count: 0, agent_count: 0 }

export function AdminPanel() {
  const preview = firstRunTourMode() === "preview"
  const [authenticated, setAuthenticated] = useState<boolean | undefined>(preview ? true : undefined)
  const [password, setPassword] = useState("")
  const [dashboard, setDashboard] = useState<AdminDashboard | null>(preview ? EMPTY_ADMIN_DASHBOARD : null)
  const [invitationsOpen, setInvitationsOpen] = useState(false)
  const [inviteUrl, setInviteUrl] = useState("")
  const [refreshRevision, setRefreshRevision] = useState(0)
  const [error, setError] = useState("")
  const [submitting, setSubmitting] = useState(false)

  const loadDashboard = useCallback(async () => {
    if (preview) return
    setDashboard(await api<AdminDashboard>("/api/admin/dashboard", { cache: "no-store" }))
    setRefreshRevision((value) => value + 1)
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
    setAuthenticated(false); setDashboard(null); setInviteUrl(""); setInvitationsOpen(false)
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
        <div className="flex gap-1"><Button size="sm" variant="outline" onClick={replayAdminTour}><ListChecks />{t("admin.replay_tour")}</Button><Button size="sm" onClick={() => void loadDashboard().catch(() => setError(t("admin.unable_to_load_admin_panel")))} disabled={preview}><RotateCw />{t("admin.refresh")}</Button></div>
      </div>
      {error && <div className="mb-5 rounded border border-destructive/30 bg-destructive/5 px-3 py-2 text-sm text-destructive">{error}</div>}
      <p className="mb-4 text-xs text-muted-foreground">{t("admin.privacy_summary")}</p>
      <div role="group" aria-label={t("admin.resource_totals")} className="grid grid-cols-2 gap-x-6 border-y sm:grid-cols-4" data-tour="admin-overview">
        <AdminCountCard label={t("admin.users")} icon={<Users className="size-3.5" />} value={dashboard?.user_count} />
        <AdminCountCard label={t("admin.organizations")} icon={<Building2 className="size-3.5" />} value={dashboard?.organization_count} />
        <AdminCountCard label={t("wsettings.machines")} icon={<Server className="size-3.5" />} value={dashboard?.machine_count} />
        <AdminCountCard label={t("admin.agents")} icon={<TerminalSquare className="size-3.5" />} value={dashboard?.agent_count} />
      </div>
      <AdminTraffic preview={preview} refreshRevision={refreshRevision} />
      <ControlPlaneUpdate preview={preview} onError={setError} />
      <section className="mt-12" data-tour="admin-invite">
        <h2 className="text-sm font-semibold">{t("admin.user_invitations")}</h2>
        <div className="mt-3 grid gap-4 border-y py-4 sm:grid-cols-[minmax(0,1fr)_auto] sm:items-center">
          <div><div className="text-sm font-medium">{t("admin.invite_a_new_user")}</div><div className="mt-1 text-xs text-muted-foreground">{t("admin.registration_creates_a_personal_organization")}</div></div>
          <div className="flex flex-wrap gap-2">
            <Button size="sm" variant={invitationsOpen ? "secondary" : "outline"} aria-expanded={invitationsOpen} onClick={() => setInvitationsOpen(!invitationsOpen)}>{t("admin.pending_invitations")}</Button>
            <Button size="sm" onClick={createInvite}><KeyRound />{t("admin.create_invitation")}</Button>
          </div>
        </div>
      </section>
      {invitationsOpen && <AdminInvitations preview={preview} onError={setError} onChanged={loadDashboard} />}
    </div>
    <Dialog open={Boolean(inviteUrl)} onOpenChange={(open) => !open && setInviteUrl("")}><DialogContent data-tour="admin-invite-dialog"><DialogHeader><DialogTitle>{t("admin.user_invitation")}</DialogTitle><DialogDescription>{t("admin.this_one_time_registration_link")}</DialogDescription></DialogHeader><Textarea readOnly value={inviteUrl} className="min-h-24 font-mono text-xs" /><DialogFooter><Button variant="outline" onClick={() => setInviteUrl("")}>{t("org.close")}</Button><Button onClick={() => navigator.clipboard.writeText(inviteUrl)}><Copy />{t("dialogs.copy_link")}</Button></DialogFooter></DialogContent></Dialog>
  </main>
}

function AdminCountCard({ label, icon, value }: { label: string; icon: React.ReactNode; value?: number }) {
  return <div className="py-5">
    <div className="flex items-center gap-2 text-xs text-muted-foreground">{icon}{label}</div>
    <div className="mt-2 text-3xl font-semibold tabular-nums">{value ?? "—"}</div>
  </div>
}

function AdminInvitations({ preview, onError, onChanged }: { preview: boolean; onError: (message: string) => void; onChanged: () => Promise<void> }) {
  const [invitations, setInvitations] = useState<AdminInvitation[]>([])
  const [loading, setLoading] = useState(false)
  const [unsupported, setUnsupported] = useState(false)
  const load = useCallback(async () => {
    if (preview) return
    setLoading(true)
    setUnsupported(false)
    try {
      const data = await api<{ invitations: unknown }>("/api/admin/invitations", { cache: "no-store" })
      if (!Array.isArray(data.invitations) || data.invitations.length > 100 || !data.invitations.every((row: unknown) => {
        if (!row || typeof row !== "object") return false
        const item = row as Record<string, unknown>
        return typeof item.invitation_id === "string" && /^inv_[a-f0-9]{64}$/.test(item.invitation_id)
          && typeof item.token_prefix === "string" && item.token_prefix.length <= 8
          && typeof item.created_at === "string" && Number.isFinite(Date.parse(item.created_at))
          && (item.used_at === null || (typeof item.used_at === "string" && Number.isFinite(Date.parse(item.used_at))))
          && !("token" in item) && !("url" in item) && !("email" in item)
      })) { setInvitations([]); setUnsupported(true); return }
      setInvitations(data.invitations as AdminInvitation[])
    } catch { setInvitations([]); onError(t("admin.unable_to_load_admin_inventory")) }
    finally { setLoading(false) }
  }, [preview, onError])
  useEffect(() => { void load() }, [load])
  async function revokeInvite(invitationId: string) {
    try {
      await api(`/api/admin/invitations/${encodeURIComponent(invitationId)}`, { method: "DELETE" })
      await load()
      await onChanged()
    } catch { onError(t("admin.unable_to_revoke_invitation")) }
  }
  return <section className="border-b" aria-label={t("admin.pending_invitations")}>
    {loading ? <p role="status" className="py-6 text-xs text-muted-foreground">{t("admin.loading")}</p>
      : unsupported ? <p role="alert" className="py-4 text-xs text-muted-foreground">{t("admin.invitations_unsupported")}</p>
      : invitations.length ? invitations.map((invitation) => <div key={invitation.invitation_id} className="flex flex-wrap items-center justify-between gap-3 border-t py-3">
        <span className="min-w-0 text-[11px] text-muted-foreground"><span className="font-mono">{invitation.token_prefix}…</span> · {invitation.created_at}<span className="mt-1 block">{t(invitation.used_at ? "admin.invitation_used" : "admin.invitation_pending")}</span></span>
        <Button size="sm" variant="outline" disabled={invitation.used_at !== null} onClick={() => void revokeInvite(invitation.invitation_id)}>{t("admin.revoke")}</Button>
      </div>) : <EmptyState icon={<KeyRound />} label={t("admin.no_pending_invitations")} />}
  </section>
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
