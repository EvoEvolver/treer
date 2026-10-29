import { FormEvent, useEffect, useState } from "react"
import { Link } from "react-router"
import { Monitor, Plus } from "lucide-react"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Label } from "@/components/ui/label"
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select"
import { api, isUnavailable, type AuthSession, type Organization, type User } from "@/lib/api"
import { organizationSettingsPath } from "@/lib/console-routes"
import { persistTheme, readTheme, subscribeTheme, type Theme } from "@/lib/theme"
import { cn } from "@/lib/utils"
import { LOCALES, LOCALE_LABELS, roleLabel, setLocale, useLocale, t, type Locale } from "@/i18n"
import { ApiEquivalent, Badge, ConfirmDialog, ErrorNote, PageHeader, Section, Unavailable, formatDate } from "@/components/settings-hub/ui"

// Autofocus would open the on-screen keyboard over the page on a phone.
const finePointer = () => typeof matchMedia === "function" && matchMedia("(hover: hover) and (pointer: fine)").matches

export function ProfilePage({ user, preview, onUserChange }: { user: User; preview: boolean; onUserChange: (user: User) => void }) {
  const [name, setName] = useState(user.preferred_name)
  const [email, setEmail] = useState(user.email)
  const [saving, setSaving] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [saved, setSaved] = useState(false)
  useEffect(() => { setName(user.preferred_name); setEmail(user.email) }, [user])
  async function save(event: FormEvent) {
    event.preventDefault()
    setSaving(true); setError(null); setSaved(false)
    try {
      onUserChange(await api<User>("/api/auth/profile", { method: "PATCH", body: JSON.stringify({ email, preferred_name: name }) }))
      setSaved(true)
    } catch (reason) { setError(reason instanceof Error ? reason.message : String(reason)) }
    finally { setSaving(false) }
  }
  const dirty = name.trim() !== user.preferred_name || email.trim() !== user.email
  return <>
    <PageHeader title={t("hub.profile")} description={t("settings.account.body")} />
    <form onSubmit={save} className="max-w-md space-y-4">
      <div className="space-y-2"><Label htmlFor="settings-preferred-name">{t("settings.account.name")}</Label><Input id="settings-preferred-name" className="max-md:h-11 max-md:text-base" value={name} onChange={(event) => setName(event.target.value)} required autoFocus={finePointer()} maxLength={80} autoComplete="name" /></div>
      <div className="space-y-2"><Label htmlFor="settings-email">{t("settings.account.email")}</Label><Input id="settings-email" className="max-md:h-11 max-md:text-base" type="email" value={email} onChange={(event) => setEmail(event.target.value)} required maxLength={254} autoComplete="email" /></div>
      <ErrorNote message={error} />
      {saved && !dirty && <p role="status" className="text-xs text-status-success">{t("hub.saved")}</p>}
      <Button type="submit" className="max-md:h-11 max-md:w-full" disabled={saving || preview || !dirty}>{saving ? t("common.saving") : t("common.save")}</Button>
    </form>
    <ApiEquivalent calls={[{ method: "GET", path: "/api/auth/me" }, { method: "PATCH", path: "/api/auth/profile", note: "{ preferred_name, email }" }]} />
  </>
}

export function SessionsPage({ preview, onLogout }: { preview: boolean; onLogout: () => void }) {
  const [sessions, setSessions] = useState<AuthSession[] | "unavailable" | null>(preview ? [] : null)
  const [error, setError] = useState<string | null>(null)
  const [revoking, setRevoking] = useState<AuthSession | null>(null)
  const [busy, setBusy] = useState(false)
  async function load() {
    try {
      const data = await api<{ sessions: AuthSession[] }>("/api/auth/sessions")
      setSessions(Array.isArray(data.sessions) ? data.sessions : [])
    } catch (reason) { if (isUnavailable(reason)) setSessions("unavailable"); else setError(reason instanceof Error ? reason.message : String(reason)) }
  }
  useEffect(() => { if (!preview) void load() }, [preview])
  async function revoke() {
    if (!revoking) return
    setBusy(true); setError(null)
    try {
      await api(`/api/auth/sessions/${encodeURIComponent(revoking.id)}`, { method: "DELETE" })
      if (revoking.current) { onLogout(); return }
      setRevoking(null); await load()
    } catch (reason) { setError(reason instanceof Error ? reason.message : String(reason)) }
    finally { setBusy(false) }
  }
  return <>
    <PageHeader title={t("hub.sessions")} description={t("hub.sessions_description")} />
    <Section title={t("hub.active_sessions")}>
      {sessions === null && <p className="text-xs text-muted-foreground">{t("hub.loading")}</p>}
      {sessions === "unavailable" && <Unavailable>{t("hub.sessions_unavailable")}</Unavailable>}
      {Array.isArray(sessions) && <ul aria-label={t("hub.active_sessions")} className="divide-y border-y">{sessions.map((session) => <li key={session.id} className="flex flex-wrap items-center justify-between gap-3 py-3">
        <div className="flex min-w-0 items-center gap-3"><Monitor aria-hidden="true" className="size-4 shrink-0 text-muted-foreground" /><div className="min-w-0"><p className="truncate text-sm font-medium">{session.device || t("hub.unknown_device")} {session.current && <Badge tone="primary">{t("hub.this_device")}</Badge>}</p><p className="truncate text-xs text-muted-foreground">{[session.ip, t("hub.last_seen", { time: formatDate(session.last_seen_at) })].filter(Boolean).join(" · ")}</p></div></div>
        <Button variant="outline" size="sm" className="max-md:h-11" onClick={() => setRevoking(session)}>{session.current ? t("userMenu.logout") : t("hub.revoke")}</Button>
      </li>)}{!sessions.length && <li className="py-3 text-xs text-muted-foreground">{t("hub.no_sessions")}</li>}</ul>}
      <ErrorNote message={error} />
    </Section>
    <PasswordSection preview={preview} />
    <ConfirmDialog open={Boolean(revoking)} onOpenChange={(open) => !open && setRevoking(null)} title={t("hub.revoke_session")} description={revoking?.current ? t("hub.revoke_current_session") : t("hub.revoke_session_body", { device: revoking?.device ?? "" })} confirmLabel={t("hub.revoke")} busy={busy} onConfirm={() => void revoke()} />
    <ApiEquivalent calls={[{ method: "GET", path: "/api/auth/sessions" }, { method: "DELETE", path: "/api/auth/sessions/{id}" }, { method: "POST", path: "/api/auth/password", note: "{ current_password, new_password }" }]} />
  </>
}

function PasswordSection({ preview }: { preview: boolean }) {
  const [current, setCurrent] = useState("")
  const [next, setNext] = useState("")
  const [confirm, setConfirm] = useState("")
  const [state, setState] = useState<"idle" | "saving" | "saved" | "unavailable">("idle")
  const [error, setError] = useState<string | null>(null)
  async function submit(event: FormEvent) {
    event.preventDefault()
    if (next !== confirm) { setError(t("hub.passwords_do_not_match")); return }
    setState("saving"); setError(null)
    try {
      await api("/api/auth/password", { method: "POST", body: JSON.stringify({ current_password: current, new_password: next }) })
      setCurrent(""); setNext(""); setConfirm(""); setState("saved")
    } catch (reason) {
      if (isUnavailable(reason)) { setState("unavailable"); return }
      setError(reason instanceof Error ? reason.message : String(reason)); setState("idle")
    }
  }
  return <Section title={t("hub.change_password")} description={t("hub.change_password_body")}>
    {state === "unavailable" ? <Unavailable>{t("hub.password_unavailable")}</Unavailable> : <form onSubmit={submit} className="max-w-md space-y-3">
      <div className="space-y-2"><Label htmlFor="current-password">{t("hub.current_password")}</Label><Input id="current-password" type="password" autoComplete="current-password" value={current} onChange={(event) => setCurrent(event.target.value)} required className="max-md:h-11 max-md:text-base" /></div>
      <div className="space-y-2"><Label htmlFor="new-password">{t("hub.new_password")}</Label><Input id="new-password" type="password" autoComplete="new-password" minLength={8} value={next} onChange={(event) => setNext(event.target.value)} required className="max-md:h-11 max-md:text-base" /></div>
      <div className="space-y-2"><Label htmlFor="confirm-password">{t("hub.confirm_password")}</Label><Input id="confirm-password" type="password" autoComplete="new-password" minLength={8} value={confirm} onChange={(event) => setConfirm(event.target.value)} required className="max-md:h-11 max-md:text-base" /></div>
      <ErrorNote message={error} />
      {state === "saved" && <p role="status" className="text-xs text-status-success">{t("hub.password_changed")}</p>}
      <Button type="submit" className="max-md:h-11 max-md:w-full" disabled={preview || state === "saving"}>{state === "saving" ? t("common.saving") : t("hub.change_password")}</Button>
    </form>}
  </Section>
}

export function PreferencesPage() {
  const locale = useLocale()
  const [theme, setTheme] = useState<Theme>(() => readTheme())
  useEffect(() => subscribeTheme(setTheme), [])
  function changeTheme(next: Theme) { persistTheme(next); setTheme(next) }
  return <>
    <PageHeader title={t("hub.preferences")} description={t("hub.preferences_description")} />
    <div className="max-w-md space-y-6">
      <div className="space-y-2">
        <Label>{t("settings.general.theme")}</Label>
        <div role="group" aria-label={t("settings.general.theme")} className="inline-flex rounded-[5px] bg-muted p-0.5">
          {(["light", "dark"] as const).map((value) => <button key={value} type="button" aria-pressed={theme === value} onClick={() => changeTheme(value)} className={cn("h-8 rounded-[5px] px-3 text-xs font-medium max-md:h-11 max-md:px-5 max-md:text-sm", theme === value ? "bg-background text-foreground shadow-sm" : "text-muted-foreground hover:text-foreground")}>{t(value === "light" ? "settings.general.light" : "settings.general.dark")}</button>)}
        </div>
      </div>
      <div className="space-y-2">
        <Label htmlFor="settings-language">{t("common.language")}</Label>
        <Select value={locale} onValueChange={(value) => setLocale(value as Locale)}>
          <SelectTrigger id="settings-language" aria-label={t("common.language")} className="max-md:h-11 max-md:text-base"><SelectValue /></SelectTrigger>
          <SelectContent>{LOCALES.map((code) => <SelectItem key={code} value={code}>{LOCALE_LABELS[code]}</SelectItem>)}</SelectContent>
        </Select>
        <span className="block text-[11px] text-muted-foreground">{t("settings.general.languageHint")}</span>
      </div>
    </div>
    <ApiEquivalent calls={[{ method: "GET", path: "/api/auth/me", note: t("hub.preferences_local") }]} />
  </>
}

export function OrganizationsPage({ organizations, currentOrganizationId, onCreate, linkSearch }: { organizations: Organization[]; currentOrganizationId: string | null; onCreate: () => void; linkSearch: string }) {
  return <>
    <PageHeader title={t("hub.my_organizations")} description={t("hub.my_organizations_description")} actions={<Button variant="outline" className="max-md:h-11" onClick={onCreate}><Plus />{t("org.new_organization")}</Button>} />
    <ul aria-label={t("hub.my_organizations")} className="divide-y rounded-lg border">{organizations.map((organization) => <li key={organization.organization_id} className="flex items-center justify-between gap-3 px-4 py-3">
      <div className="min-w-0"><p className="truncate text-sm font-medium">{organization.name} {organization.organization_id === currentOrganizationId && <Badge tone="primary">{t("org.current")}</Badge>}</p><p className="truncate font-mono text-[11px] text-muted-foreground">{organization.organization_id}</p></div>
      <div className="flex shrink-0 items-center gap-3"><span className="text-xs text-muted-foreground">{roleLabel(organization.role)}</span><Button asChild variant="outline" size="sm" className="max-md:h-11"><Link to={{ pathname: organizationSettingsPath(organization.organization_id, "general"), search: linkSearch }}>{t("hub.open_settings")}</Link></Button></div>
    </li>)}{!organizations.length && <li className="px-4 py-6 text-center text-xs text-muted-foreground">{t("hub.no_organizations")}</li>}</ul>
    <ApiEquivalent calls={[{ method: "GET", path: "/api/organizations" }, { method: "POST", path: "/api/organizations", note: "{ name }" }]} />
  </>
}
