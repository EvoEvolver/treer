import { FormEvent, useCallback, useEffect, useRef, useState } from "react"
import type * as React from "react"
import {
  ArrowDown,
  ArrowLeft,
  ArrowRight,
  ArrowUp,
  Activity,
  ChevronRight,
  CirclePlus,
  Copy,
  CornerDownLeft,
  Delete,
  ExternalLink,
  FolderKanban,
  Github,
  GitBranch,
  KeyRound,
  Keyboard,
  LogOut,
  Mail,
  Maximize2,
  MoreHorizontal,
  Network,
  Pencil,
  Plus,
  RotateCw,
  Rocket,
  Play,
  ScrollText,
  Search,
  Server,
  Square,
  ShieldCheck,
  TerminalSquare,
  Trash2,
  UserRound,
  Users,
  X,
} from "lucide-react"
import { api, ApiError, machineName, proxyUrl, websocketUrl, type AdminDashboard, type Agent, type AgentLaunchProfile, type Machine, type MachineService, type MachineTrafficRecord, type Member, type Organization, type OrganizationAuditEvent, type ServiceIngress, type Snapshot, type User, type VirtualNetworkHost, type Workspace } from "@/lib/api"
import { formatCommandLine, parseCommandLine } from "@/lib/command-line"
import { cn } from "@/lib/utils"
import { TerminalPane, type TerminalPaneHandle } from "@/components/terminal-pane"
import { Button } from "@/components/ui/button"
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog"
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuLabel, DropdownMenuSeparator, DropdownMenuTrigger } from "@/components/ui/dropdown-menu"
import { Input } from "@/components/ui/input"
import { Label } from "@/components/ui/label"
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select"
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs"
import { Textarea } from "@/components/ui/textarea"
import { Tooltip, TooltipContent, TooltipProvider, TooltipTrigger } from "@/components/ui/tooltip"

type ConnectionState = "connecting" | "live" | "reconnecting" | "no workspace"
type TerminalState = "not attached" | "connecting" | "live" | "reconnecting" | "closed" | "error"
type MainView = "terminal" | "profiles" | "network" | "audit"
type AuthMode = "login" | "register" | "forgot" | "reset"
type AuthConfig = { github: boolean; google: boolean; invitation_required: boolean }
type RenameTarget = { kind: "machine" | "agent"; id: string; name: string } | null
type DeleteTarget = { kind: "machine" | "agent"; id: string; name: string } | null

const activeStatuses = new Set(["starting", "working", "idle", "blocked"])

function initials(value: string) {
  return value.trim().slice(0, 2).toUpperCase() || "T"
}

function defaultAgentName(kind: string) {
  const now = new Date()
  const month = String(now.getMonth() + 1).padStart(2, "0")
  const day = String(now.getDate()).padStart(2, "0")
  const prefix = kind === "terminal" ? "terminal" : kind === "command" ? "cmd" : kind === "codex" || kind === "claude" ? kind : "agent"
  return `${prefix}-${now.getFullYear()}-${month}-${day}`
}

function buildLabel(build: Machine["controller_build"]) {
  const commit = build.git_commit === "unknown" ? build.git_commit : build.git_commit.slice(0, 8)
  return `${build.version}@${commit}`
}

function ingressReturnUrl() {
  const value = new URLSearchParams(window.location.search).get("return_to")
  if (!value) return null
  try {
    const candidate = new URL(value)
    const authorize = new URL(proxyUrl("/.treer/ingress/authorize"))
    return candidate.origin === authorize.origin && candidate.pathname === authorize.pathname ? candidate.toString() : null
  } catch {
    return null
  }
}

function Status({ value }: { value: string }) {
  return <span className={cn("inline-flex shrink-0 items-center gap-1.5 text-[10px] font-medium capitalize text-zinc-500", value === "idle" && "text-emerald-700", ["working", "starting"].includes(value) && "text-sky-700", value === "blocked" && "text-amber-700", ["failed", "exited"].includes(value) && "text-red-600")}><span className="size-1.5 rounded-full bg-current opacity-75" />{value}</span>
}

function IconButton({ label, children, ...props }: React.ComponentProps<typeof Button> & { label: string }) {
  return <Tooltip><TooltipTrigger asChild><Button size="icon" variant="ghost" aria-label={label} {...props}>{children}</Button></TooltipTrigger><TooltipContent>{label}</TooltipContent></Tooltip>
}

function MobileTerminalKey({ label, active = false, children, onClick }: { label: string; active?: boolean; children: React.ReactNode; onClick: () => void }) {
  return <button type="button" aria-label={label} aria-pressed={active || undefined} onClick={onClick} className={cn("grid h-10 min-w-0 touch-manipulation select-none place-items-center rounded-[5px] border border-zinc-700 bg-[#24292d] px-1 text-[11px] font-medium text-zinc-200 active:bg-[#3a4248]", active && "border-sky-500 bg-sky-500/20 text-sky-200")}>{children}</button>
}

function controlCharacter(value: string) {
  const character = value[0]
  if (!character) return null
  const upper = character.toUpperCase()
  if (upper >= "A" && upper <= "Z") return String.fromCharCode(upper.charCodeAt(0) - 64)
  const special: Record<string, string> = { "@": "\x00", " ": "\x00", "[": "\x1b", "\\": "\x1c", "]": "\x1d", "^": "\x1e", "_": "\x1f", "?": "\x7f" }
  return special[character] ?? null
}

function AuthScreen({ onAuthenticated }: { onAuthenticated: (user: User) => void }) {
  const parameters = new URLSearchParams(window.location.search)
  const invite = parameters.get("invite")
  const resetToken = parameters.get("reset")
  const oauthError = parameters.get("oauth_error")
  const [mode, setMode] = useState<AuthMode>(resetToken ? "reset" : invite ? "register" : "login")
  const [authConfig, setAuthConfig] = useState<AuthConfig>({ github: false, google: false, invitation_required: true })
  const [email, setEmail] = useState("")
  const [preferredName, setPreferredName] = useState("")
  const [password, setPassword] = useState("")
  const [passwordConfirmation, setPasswordConfirmation] = useState("")
  const [error, setError] = useState(oauthError ? "OAuth sign-in failed. Try again." : "")
  const [notice, setNotice] = useState("")
  const [submitting, setSubmitting] = useState(false)

  useEffect(() => {
    api<AuthConfig>("/api/auth/config").then(setAuthConfig).catch(() => undefined)
    if (oauthError) window.history.replaceState(null, "", window.location.pathname)
  }, [oauthError])

  function showSignIn(message = "") {
    window.history.replaceState(null, "", window.location.pathname)
    setMode("login")
    setPassword("")
    setPasswordConfirmation("")
    setError("")
    setNotice(message)
  }

  async function submit(event: FormEvent) {
    event.preventDefault()
    setError("")
    setNotice("")
    if (mode === "reset" && password !== passwordConfirmation) {
      setError("Passwords do not match")
      return
    }
    setSubmitting(true)
    try {
      if (mode === "forgot") {
        await api<{ ok: boolean }>("/api/auth/request-password-reset", { method: "POST", body: JSON.stringify({ email }) })
        setNotice("If an account exists for this email, a reset link has been sent.")
        return
      }
      if (mode === "reset") {
        await api<{ ok: boolean }>("/api/auth/reset-password", { method: "POST", body: JSON.stringify({ token: resetToken, password }) })
        showSignIn("Password updated. Sign in with your new password.")
        return
      }
      const registering = mode === "register"
      const path = registering ? "/api/auth/register" : "/api/auth/login"
      const body = registering ? { invite, email, preferred_name: preferredName, password } : { email, password }
      const user = await api<User>(path, { method: "POST", body: JSON.stringify(body) })
      const returnTo = ingressReturnUrl()
      if (returnTo) {
        window.location.assign(returnTo)
        return
      }
      window.history.replaceState(null, "", window.location.pathname)
      onAuthenticated(user)
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : "Authentication failed")
    } finally {
      setSubmitting(false)
    }
  }

  function oauthUrl(provider: "github" | "google") {
    const url = new URL(proxyUrl(`/api/auth/oauth/${provider}/start`))
    if (invite) url.searchParams.set("invite", invite)
    return url.toString()
  }

  const title = mode === "register" ? "Join Treer" : mode === "forgot" ? "Reset your password" : mode === "reset" ? "Choose a new password" : "Sign in to Treer"
  const description = mode === "register" ? invite ? "Create your account from this invitation." : "Create your account and personal organization." : mode === "forgot" ? "Enter the email associated with your account." : mode === "reset" ? "Use at least 8 characters for your new password." : "Open your agent workspace."

  return <main className="grid min-h-dvh place-items-center bg-[#f7f7f5] p-4">
    <form onSubmit={submit} className="w-full max-w-[390px] rounded-lg border bg-background p-7 shadow-sm">
      <div className="mb-6 grid size-9 place-items-center rounded-md bg-[#37352f] font-serif text-lg font-bold text-white">T</div>
      <h1 className="text-xl font-semibold">{title}</h1>
      <p className="mt-1 text-sm text-muted-foreground">{description}</p>
      {(mode === "login" || mode === "register") && (authConfig.github || authConfig.google) && <div className="mt-6 space-y-2">
        {authConfig.github && <Button type="button" variant="outline" className="w-full" onClick={() => { window.location.href = oauthUrl("github") }}><Github />Continue with GitHub</Button>}
        {authConfig.google && <Button type="button" variant="outline" className="w-full" onClick={() => { window.location.href = oauthUrl("google") }}><Mail />Continue with Google</Button>}
        <div className="flex items-center gap-3 py-2 text-[11px] text-muted-foreground"><span className="h-px flex-1 bg-border" /><span>or use email</span><span className="h-px flex-1 bg-border" /></div>
      </div>}
      {mode !== "reset" && <div className={cn("space-y-2", (mode === "login" || mode === "register") && (authConfig.github || authConfig.google) ? "mt-2" : "mt-6")}><Label htmlFor="email">Email</Label><Input id="email" type="email" autoComplete="email" value={email} maxLength={254} onChange={(event) => setEmail(event.target.value)} required autoFocus /></div>}
      {mode === "register" && <div className="mt-4 space-y-2"><Label htmlFor="preferred-name">Preferred name</Label><Input id="preferred-name" autoComplete="name" value={preferredName} maxLength={80} onChange={(event) => setPreferredName(event.target.value)} required /></div>}
      {(mode === "login" || mode === "register" || mode === "reset") && <div className={cn("space-y-2", mode === "reset" ? "mt-6" : "mt-4")}><Label htmlFor="password">{mode === "reset" ? "New password" : "Password"}</Label><Input id="password" type="password" autoComplete={mode === "login" ? "current-password" : "new-password"} value={password} minLength={mode === "login" ? undefined : 8} maxLength={1024} onChange={(event) => setPassword(event.target.value)} required autoFocus={mode === "reset"} /></div>}
      {mode === "reset" && <div className="mt-4 space-y-2"><Label htmlFor="password-confirmation">Confirm new password</Label><Input id="password-confirmation" type="password" autoComplete="new-password" value={passwordConfirmation} minLength={8} maxLength={1024} onChange={(event) => setPasswordConfirmation(event.target.value)} required /></div>}
      {mode === "login" && <div className="mt-2 text-right"><Button type="button" variant="ghost" size="sm" className="h-auto px-0 py-1 text-xs text-muted-foreground" onClick={() => { setMode("forgot"); setError(""); setNotice("") }}>Forgot password?</Button></div>}
      <div className="mt-3 min-h-5 text-xs text-destructive">{error}</div>
      {notice && <div className="mt-2 text-xs leading-5 text-emerald-700">{notice}</div>}
      <div className="mt-4 flex items-center justify-between gap-3">
        {(mode === "register" || mode === "forgot" || mode === "reset") && <Button type="button" variant="ghost" className="px-0 text-primary" onClick={() => showSignIn()}>Back to sign in</Button>}
        {mode === "login" && !authConfig.invitation_required && <Button type="button" variant="ghost" className="px-0 text-primary" onClick={() => { setMode("register"); setError(""); setNotice("") }}>Create account</Button>}
        <Button type="submit" className="ml-auto" disabled={submitting}>{submitting ? "Please wait" : mode === "register" ? "Create account" : mode === "forgot" ? "Send reset link" : mode === "reset" ? "Update password" : "Sign in"}</Button>
      </div>
    </form>
  </main>
}

function AdminPanel() {
  const [authenticated, setAuthenticated] = useState<boolean | undefined>(undefined)
  const [password, setPassword] = useState("")
  const [dashboard, setDashboard] = useState<AdminDashboard | null>(null)
  const [inviteUrl, setInviteUrl] = useState("")
  const [error, setError] = useState("")
  const [submitting, setSubmitting] = useState(false)

  const loadDashboard = useCallback(async () => {
    setDashboard(await api<AdminDashboard>("/api/admin/dashboard"))
  }, [])

  useEffect(() => {
    api<{ admin: boolean }>("/api/admin/me")
      .then(() => { setAuthenticated(true); return loadDashboard() })
      .catch((reason) => {
        if (reason instanceof ApiError && reason.status === 401) setAuthenticated(false)
        else setError(reason instanceof Error ? reason.message : "Unable to load admin panel")
      })
  }, [loadDashboard])

  async function login(event: FormEvent) {
    event.preventDefault(); setSubmitting(true); setError("")
    try {
      await api("/api/admin/login", { method: "POST", body: JSON.stringify({ password }) })
      setPassword(""); setAuthenticated(true); await loadDashboard()
    } catch (reason) { setError(reason instanceof Error ? reason.message : "Authentication failed") }
    finally { setSubmitting(false) }
  }

  async function createInvite() {
    setError("")
    try {
      const data = await api<{ url: string }>("/api/admin/invitations", { method: "POST", body: "{}" })
      setInviteUrl(data.url)
    } catch (reason) { setError(reason instanceof Error ? reason.message : "Unable to create invitation") }
  }

  async function logout() {
    await api("/api/admin/logout", { method: "POST", body: "{}" })
    setAuthenticated(false); setDashboard(null); setInviteUrl("")
  }

  if (authenticated === undefined) return <div className="grid min-h-dvh place-items-center bg-[#f7f7f5] text-sm text-muted-foreground">Loading admin...</div>
  if (!authenticated) return <main className="grid min-h-dvh place-items-center bg-[#f7f7f5] p-4"><form onSubmit={login} className="w-full max-w-[390px] rounded-lg border bg-background p-7 shadow-sm"><div className="mb-6 grid size-9 place-items-center rounded-md bg-[#37352f] text-white"><ShieldCheck className="size-4" /></div><h1 className="text-xl font-semibold">Treer administration</h1><p className="mt-1 text-sm text-muted-foreground">Platform access is separate from user accounts.</p><div className="mt-6 space-y-2"><Label htmlFor="admin-password">Admin password</Label><Input id="admin-password" type="password" autoComplete="current-password" value={password} onChange={(event) => setPassword(event.target.value)} required autoFocus /></div><div className="mt-3 min-h-5 text-xs text-destructive">{error}</div><div className="mt-4 flex justify-end"><Button type="submit" disabled={submitting}>{submitting ? "Please wait" : "Open admin panel"}</Button></div></form></main>

  return <main className="min-h-dvh bg-[#f7f7f5]"><header className="border-b bg-background"><div className="mx-auto flex h-14 w-full max-w-4xl items-center justify-between px-5"><div className="flex min-w-0 items-center gap-2.5 text-sm font-semibold"><span className="grid size-7 shrink-0 place-items-center rounded bg-[#37352f] text-white"><ShieldCheck className="size-3.5" /></span><span className="truncate">Treer administration</span></div><div className="flex shrink-0 items-center gap-1"><Button variant="ghost" size="sm" className="hidden sm:inline-flex" asChild><a href="/">User workspace</a></Button><Button size="icon" variant="ghost" aria-label="Log out" onClick={logout}><LogOut /></Button></div></div></header><div className="mx-auto max-w-4xl px-5 py-10"><div className="mb-8 flex flex-col items-start gap-4 sm:flex-row sm:items-end sm:justify-between"><div><h1 className="text-2xl font-semibold">Platform overview</h1><p className="mt-1 text-sm text-muted-foreground">Current resources across all organizations.</p></div><Button size="sm" onClick={loadDashboard}><RotateCw />Refresh</Button></div>{error && <div className="mb-5 rounded border border-destructive/30 bg-destructive/5 px-3 py-2 text-sm text-destructive">{error}</div>}<div className="grid grid-cols-2 border-y"><div className="border-r py-6 pr-6"><div className="flex items-center gap-2 text-xs text-muted-foreground"><Server className="size-3.5" />Machines</div><div className="mt-2 text-3xl font-semibold tabular-nums">{dashboard?.machine_count ?? "-"}</div></div><div className="py-6 pl-6"><div className="flex items-center gap-2 text-xs text-muted-foreground"><TerminalSquare className="size-3.5" />Agents</div><div className="mt-2 text-3xl font-semibold tabular-nums">{dashboard?.agent_count ?? "-"}</div></div></div><section className="mt-12"><h2 className="text-sm font-semibold">User invitations</h2><div className="mt-3 grid gap-4 border-y py-4 sm:grid-cols-[minmax(0,1fr)_auto] sm:items-center"><div><div className="text-sm font-medium">Invite a new user</div><div className="mt-1 text-xs text-muted-foreground">Registration creates a personal organization owned by that user.</div></div><Button size="sm" onClick={createInvite}><KeyRound />Create invitation</Button></div></section></div><Dialog open={Boolean(inviteUrl)} onOpenChange={(open) => !open && setInviteUrl("")}><DialogContent><DialogHeader><DialogTitle>User invitation</DialogTitle><DialogDescription>This one-time registration link creates the user's personal organization.</DialogDescription></DialogHeader><Textarea readOnly value={inviteUrl} className="min-h-24 font-mono text-xs" /><DialogFooter><Button variant="outline" onClick={() => setInviteUrl("")}>Close</Button><Button onClick={() => navigator.clipboard.writeText(inviteUrl)}><Copy />Copy link</Button></DialogFooter></DialogContent></Dialog></main>
}


function formatBytes(value: number) {
  if (value < 1024) return `${value} B`
  const units = ["KB", "MB", "GB", "TB"]
  let amount = value / 1024
  let unit = units[0]
  for (let index = 1; index < units.length && amount >= 1024; index += 1) {
    amount /= 1024
    unit = units[index]
  }
  return `${amount >= 10 ? amount.toFixed(1) : amount.toFixed(2)} ${unit}`
}

const auditActionLabels: Record<string, string> = {
  "organization.created": "created the organization",
  "organization.renamed": "renamed the organization",
  "workspace.created": "created a workspace",
  "invitation.created": "created a member invitation",
  "member.role_updated": "changed a member role",
  "member.removed": "removed a member",
  "machine.renamed": "renamed a machine",
  "machine.deleted": "deleted a machine",
  "agent.created": "created an agent",
  "agent.renamed": "renamed an agent",
  "agent.stopped": "stopped an agent",
  "agent.deleted": "deleted an agent",
  "launch_profile.created": "created a launch profile",
  "launch_profile.updated": "updated a launch profile",
  "launch_profile.deleted": "deleted a launch profile",
}

function AuditView({ events, traffic, machines, loading }: { events: OrganizationAuditEvent[]; traffic: MachineTrafficRecord[]; machines: Machine[]; loading: boolean }) {
  const totalBytes = traffic.reduce((sum, item) => sum + item.payload_bytes, 0)
  const totalFrames = traffic.reduce((sum, item) => sum + item.payload_frames, 0)
  const routes = Array.from(traffic.reduce((items, item) => {
    const key = `${item.source_server_id}\u0000${item.destination_server_id}`
    const current = items.get(key) ?? { source: item.source_server_id, destination: item.destination_server_id, bytes: 0, frames: 0 }
    current.bytes += item.payload_bytes
    current.frames += item.payload_frames
    items.set(key, current)
    return items
  }, new Map<string, { source: string; destination: string; bytes: number; frames: number }>()).values()).sort((left, right) => right.bytes - left.bytes)
  const resolveMachine = (serverId: string) => machineName(machines.find((machine) => machine.server_id === serverId), serverId)

  return <div className="min-h-0 overflow-auto"><div className="mx-auto w-full max-w-[1120px] px-5 py-8 sm:px-8 sm:py-12 lg:px-14">
    <div className="mb-8 flex items-end justify-between gap-4"><div><div className="mb-2 grid size-9 place-items-center rounded-md bg-[#f8d9df] text-[#8b4452]"><ScrollText className="size-4" /></div><h1 className="text-2xl font-semibold">Audit</h1></div><span className="text-xs text-muted-foreground">Organization activity</span></div>
    <section className="mb-11"><div className="mb-3 flex items-center justify-between"><h2 className="text-sm font-semibold">Traffic</h2><span className="text-[10px] uppercase text-muted-foreground">Last 24 hours</span></div><div className="grid grid-cols-3 border-y">
      <div className="py-5 pr-3"><div className="text-[10px] text-muted-foreground">Relayed data</div><div className="mt-1 text-xl font-semibold tabular-nums sm:text-2xl">{formatBytes(totalBytes)}</div></div>
      <div className="border-x px-3 py-5 sm:px-6"><div className="text-[10px] text-muted-foreground">Data frames</div><div className="mt-1 text-xl font-semibold tabular-nums sm:text-2xl">{totalFrames.toLocaleString()}</div></div>
      <div className="py-5 pl-3 sm:pl-6"><div className="text-[10px] text-muted-foreground">Machine routes</div><div className="mt-1 text-xl font-semibold tabular-nums sm:text-2xl">{routes.length}</div></div>
    </div>{routes.length > 0 && <div className="mt-3 divide-y border-b">{routes.slice(0, 5).map((route) => <div key={`${route.source}:${route.destination}`} className="grid min-h-10 grid-cols-[minmax(0,1fr)_auto] items-center gap-4 text-xs"><span className="flex min-w-0 items-center gap-2"><span className="truncate">{resolveMachine(route.source)}</span><ArrowRight className="size-3 shrink-0 text-muted-foreground" /><span className="truncate">{resolveMachine(route.destination)}</span></span><span className="font-mono text-[10px] text-muted-foreground">{formatBytes(route.bytes)}</span></div>)}</div>}</section>
    <section><div className="mb-3 flex items-center justify-between"><h2 className="text-sm font-semibold">Activity</h2><span className="text-[10px] text-muted-foreground">{events.length} events</span></div><div className="border-y divide-y">{events.map((event) => { const actor = event.actor_name ?? event.actor_id ?? event.actor_kind; const resource = event.resource_name ?? event.resource_id; return <div key={event.event_id} className="grid min-h-16 grid-cols-[32px_minmax(0,1fr)] gap-3 py-3 sm:grid-cols-[32px_minmax(0,1fr)_auto] sm:items-center"><span className="grid size-8 place-items-center rounded bg-[#dcebea] text-[10px] font-bold text-[#35645f]">{initials(actor)}</span><div className="min-w-0"><div className="truncate text-xs"><span className="font-medium">{actor}</span> <span className="text-muted-foreground">{auditActionLabels[event.action] ?? event.action}</span></div><div className="mt-1 truncate font-mono text-[9px] text-muted-foreground">{resource}</div></div><time className="col-start-2 text-[10px] text-muted-foreground sm:col-start-auto" dateTime={event.occurred_at}>{new Date(event.occurred_at).toLocaleString()}</time></div>})}{!events.length && <EmptyState icon={loading ? <RotateCw className="animate-spin" /> : <Activity />} label={loading ? "Loading activity" : "No audit activity yet"} />}</div></section>
  </div></div>
}

function LaunchProfilesView({ profiles, loading, onEdit, onLaunch, onDelete }: { profiles: AgentLaunchProfile[]; loading: boolean; onEdit: (profile: AgentLaunchProfile) => void; onLaunch: (profile: AgentLaunchProfile) => void; onDelete: (profile: AgentLaunchProfile) => void }) {
  return <div className="min-h-0 overflow-auto"><div className="mx-auto w-full max-w-[1120px] px-5 py-8 sm:px-8 sm:py-12 lg:px-14">
    <div className="mb-8 flex items-end justify-between gap-4"><div><div className="mb-2 grid size-9 place-items-center rounded-md bg-[#dcead8] text-[#476345]"><Rocket className="size-4" /></div><h1 className="text-2xl font-semibold">Launch profiles</h1></div><span className="text-xs text-muted-foreground">{profiles.length} saved</span></div>
    <div className="border-y">
      <div className="hidden h-9 grid-cols-[minmax(140px,.8fr)_minmax(240px,1.6fr)_minmax(120px,.7fr)_auto] items-center gap-4 border-b text-[10px] font-medium uppercase text-muted-foreground sm:grid"><span>Profile</span><span>Command</span><span>Working directory</span><span className="w-28" /></div>
      {profiles.map((profile) => <div key={profile.profile_id} className="grid min-h-20 grid-cols-[minmax(0,1fr)_auto] items-center gap-x-3 gap-y-1 border-b py-3 last:border-b-0 sm:grid-cols-[minmax(140px,.8fr)_minmax(240px,1.6fr)_minmax(120px,.7fr)_auto] sm:gap-4">
        <span className="col-start-1 row-start-1 min-w-0 sm:col-start-auto sm:row-start-auto"><span className="block truncate text-xs font-medium">{profile.name}</span>{profile.description && <span className="mt-1 block truncate text-[10px] text-muted-foreground">{profile.description}</span>}</span>
        <code className="col-start-1 row-start-2 min-w-0 truncate text-[10px] text-muted-foreground sm:col-start-auto sm:row-start-auto" title={formatCommandLine(profile.command, profile.args)}>{formatCommandLine(profile.command, profile.args)}</code>
        <code className="col-start-1 row-start-3 min-w-0 truncate text-[10px] text-muted-foreground sm:col-start-auto sm:row-start-auto">{profile.cwd || "."}</code>
        <span className="col-start-2 row-span-3 row-start-1 flex items-center justify-end gap-1 sm:col-start-auto sm:row-span-1 sm:row-start-auto"><IconButton label={`Run ${profile.name}`} onClick={() => onLaunch(profile)}><Play /></IconButton><IconButton label={`Edit ${profile.name}`} onClick={() => onEdit(profile)}><Pencil /></IconButton><IconButton label={`Delete ${profile.name}`} className="text-destructive hover:text-destructive" onClick={() => onDelete(profile)}><Trash2 /></IconButton></span>
      </div>)}
      {!profiles.length && <EmptyState icon={<Rocket />} label={loading ? "Loading launch profiles" : "No launch profiles yet"} />}
    </div>
  </div></div>
}

function WorkspaceApp() {
  const [user, setUser] = useState<User | null | undefined>(undefined)
  const [organizations, setOrganizations] = useState<Organization[]>([])
  const [organizationId, setOrganizationId] = useState<string | null>(null)
  const [workspaces, setWorkspaces] = useState<Workspace[]>([])
  const [workspaceId, setWorkspaceId] = useState<string | null>(null)
  const [snapshot, setSnapshot] = useState<Snapshot | null>(null)
  const [selectedAgentId, setSelectedAgentId] = useState<string | null>(null)
  const [connection, setConnection] = useState<ConnectionState>("connecting")
  const [terminalStatus, setTerminalStatus] = useState<TerminalState>("not attached")
  const [mainView, setMainView] = useState<MainView>("terminal")
  const [mobileTerminalOpen, setMobileTerminalOpen] = useState(false)
  const [ctrlArmed, setCtrlArmed] = useState(false)
  const terminalPaneRef = useRef<TerminalPaneHandle>(null)
  const ctrlArmedRef = useRef(false)
  const [error, setError] = useState<string | null>(null)
  const [createOrganizationOpen, setCreateOrganizationOpen] = useState(false)
  const [createWorkspaceOpen, setCreateWorkspaceOpen] = useState(false)
  const [createAgentOpen, setCreateAgentOpen] = useState(false)
  const [profileEditorOpen, setProfileEditorOpen] = useState(false)
  const [editingLaunchProfile, setEditingLaunchProfile] = useState<AgentLaunchProfile | null>(null)
  const [launchingProfile, setLaunchingProfile] = useState<AgentLaunchProfile | null>(null)
  const [deletingProfile, setDeletingProfile] = useState<AgentLaunchProfile | null>(null)
  const [installOpen, setInstallOpen] = useState(false)
  const [membersOpen, setMembersOpen] = useState(false)
  const [createVirtualHostOpen, setCreateVirtualHostOpen] = useState(false)
  const [publishOpen, setPublishOpen] = useState(false)
  const [createServiceOpen, setCreateServiceOpen] = useState(false)
  const [editingService, setEditingService] = useState<MachineService | null>(null)
  const [inviteOpen, setInviteOpen] = useState(false)
  const [profileOpen, setProfileOpen] = useState(false)
  const [renameOrganizationOpen, setRenameOrganizationOpen] = useState(false)
  const [renameTarget, setRenameTarget] = useState<RenameTarget>(null)
  const [deleteTarget, setDeleteTarget] = useState<DeleteTarget>(null)
  const [organizationName, setOrganizationName] = useState("")
  const [preferredName, setPreferredName] = useState("")
  const [profileEmail, setProfileEmail] = useState("")
  const [workspaceName, setWorkspaceName] = useState("")
  const [agentName, setAgentName] = useState(defaultAgentName("terminal"))
  const [agentNameCustomized, setAgentNameCustomized] = useState(false)
  const [agentProfileId, setAgentProfileId] = useState("terminal")
  const [agentServerId, setAgentServerId] = useState("")
  const [agentCwd, setAgentCwd] = useState(".")
  const [agentCommandLine, setAgentCommandLine] = useState("codex")
  const [launchProfiles, setLaunchProfiles] = useState<AgentLaunchProfile[]>([])
  const [launchProfilesLoading, setLaunchProfilesLoading] = useState(false)
  const [launchProfileName, setLaunchProfileName] = useState("")
  const [launchProfileDescription, setLaunchProfileDescription] = useState("")
  const [launchProfileCwd, setLaunchProfileCwd] = useState(".")
  const [launchProfileCommandLine, setLaunchProfileCommandLine] = useState("")
  const [launchMachineId, setLaunchMachineId] = useState("")
  const [launchAgentName, setLaunchAgentName] = useState("")
  const [renameName, setRenameName] = useState("")
  const [installCommand, setInstallCommand] = useState("")
  const [connectCommand, setConnectCommand] = useState("")
  const [inviteUrl, setInviteUrl] = useState("")
  const [members, setMembers] = useState<Member[]>([])
  const [auditEvents, setAuditEvents] = useState<OrganizationAuditEvent[]>([])
  const [traffic, setTraffic] = useState<MachineTrafficRecord[]>([])
  const [auditLoading, setAuditLoading] = useState(false)
  const [virtualHosts, setVirtualHosts] = useState<VirtualNetworkHost[]>([])
  const [services, setServices] = useState<MachineService[]>([])
  const [ingresses, setIngresses] = useState<ServiceIngress[]>([])
  const [serviceHealth, setServiceHealth] = useState<Record<string, "healthy" | "unreachable">>({})
  const [virtualHostname, setVirtualHostname] = useState("")
  const [virtualServiceId, setVirtualServiceId] = useState("")
  const [publishServiceId, setPublishServiceId] = useState("")
  const [publishSlug, setPublishSlug] = useState("")
  const [publishAccess, setPublishAccess] = useState<"public" | "workspace">("public")
  const [serviceName, setServiceName] = useState("")
  const [serviceServerId, setServiceServerId] = useState("")
  const [serviceTargetHost, setServiceTargetHost] = useState("127.0.0.1")
  const [serviceTargetPort, setServiceTargetPort] = useState("")
  const [serviceProtocol, setServiceProtocol] = useState<"tcp" | "http">("http")

  const showError = useCallback((reason: unknown) => setError(reason instanceof Error ? reason.message : "Something went wrong"), [])

  useEffect(() => {
    api<User>("/api/auth/me").then(setUser).catch((reason) => {
      if (reason instanceof ApiError && reason.status === 401) setUser(null)
      else showError(reason)
    })
  }, [showError])

  useEffect(() => {
    if (!user) return
    const returnTo = ingressReturnUrl()
    if (returnTo) window.location.assign(returnTo)
  }, [user])

  const loadOrganizations = useCallback(async (preferred?: string) => {
    const data = await api<{ organizations: Organization[] }>("/api/organizations")
    setOrganizations(data.organizations)
    setOrganizationId((current) => {
      if (preferred && data.organizations.some((item) => item.organization_id === preferred)) return preferred
      if (current && data.organizations.some((item) => item.organization_id === current)) return current
      return data.organizations[0]?.organization_id ?? null
    })
  }, [])

  useEffect(() => { if (user) loadOrganizations().catch(showError) }, [user, loadOrganizations, showError])

  useEffect(() => {
    let cancelled = false
    setWorkspaceId(null)
    setSnapshot(null)
    if (!organizationId) {
      setWorkspaces([])
      setConnection("no workspace")
      return
    }
    api<{ workspaces: Workspace[] }>(`/api/workspaces?organization_id=${encodeURIComponent(organizationId)}`).then((data) => {
      if (cancelled) return
      setWorkspaces(data.workspaces)
      setWorkspaceId(data.workspaces[0]?.workspace_id ?? null)
      if (!data.workspaces.length) setConnection("no workspace")
    }).catch(showError)
    return () => { cancelled = true }
  }, [organizationId, showError])

  const refreshSnapshot = useCallback(async () => {
    if (!workspaceId) return
    const data = await api<Snapshot>(`/api/workspaces/${encodeURIComponent(workspaceId)}/snapshot`)
    setSnapshot(data)
  }, [workspaceId])

  useEffect(() => {
    if (!workspaceId) {
      setSnapshot(null)
      setSelectedAgentId(null)
      setConnection("no workspace")
      return
    }
    let disposed = false
    let socket: WebSocket | null = null
    let timer: number | undefined
    refreshSnapshot().catch(showError)
    const connect = (initial = false) => {
      if (disposed) return
      if (initial) setConnection("connecting")
      socket = new WebSocket(websocketUrl(`/api/workspaces/${encodeURIComponent(workspaceId)}/events`))
      socket.onopen = () => { if (!disposed) setConnection("live") }
      socket.onmessage = (event) => {
        if (disposed) return
        const message = JSON.parse(event.data) as { event: string; data?: Snapshot }
        if (message.event === "workspace.snapshot" && message.data) setSnapshot(message.data)
        else refreshSnapshot().catch(showError)
      }
      socket.onclose = () => {
        if (disposed) return
        setConnection("reconnecting")
        timer = window.setTimeout(() => connect(false), 1200)
      }
    }
    connect(true)
    return () => { disposed = true; window.clearTimeout(timer); socket?.close() }
  }, [workspaceId, refreshSnapshot, showError])

  useEffect(() => {
    const agents = snapshot?.agents ?? []
    setSelectedAgentId((current) => current && agents.some((agent) => agent.agent_id === current) ? current : agents[0]?.agent_id ?? null)
  }, [snapshot])

  const selectedAgent = snapshot?.agents.find((agent) => agent.agent_id === selectedAgentId)
  const onlineMachines = snapshot?.servers.filter((machine) => machine.status === "online") ?? []
  const selectedCreateProfile = launchProfiles.find((profile) => profile.profile_id === agentProfileId)
  const organization = organizations.find((item) => item.organization_id === organizationId)
  const workspace = workspaces.find((item) => item.workspace_id === workspaceId)
  const terminalActive = Boolean(selectedAgent && activeStatuses.has(selectedAgent.status))
  const setTerminalState = useCallback((value: TerminalState) => setTerminalStatus(value), [])
  const currentRole = organization?.role ?? "member"
  const canManageMembers = ["owner", "admin"].includes(currentRole)

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
    requestAnimationFrame(() => terminalPaneRef.current?.focus())
  }

  useEffect(() => {
    if (!mobileTerminalOpen) return
    const overflow = document.body.style.overflow
    document.body.style.overflow = "hidden"
    return () => { document.body.style.overflow = overflow }
  }, [mobileTerminalOpen])

  useEffect(() => {
    setMobileTerminalOpen(false)
    setCtrlModifier(false)
  }, [workspaceId, selectedAgentId])

  useEffect(() => {
    if (createAgentOpen && !onlineMachines.some((machine) => machine.server_id === agentServerId)) setAgentServerId(onlineMachines[0]?.server_id ?? "")
  }, [createAgentOpen, onlineMachines, agentServerId])

  async function createOrganization(event: FormEvent) {
    event.preventDefault()
    try {
      const data = await api<{ organization: Organization }>("/api/organizations", { method: "POST", body: JSON.stringify({ name: organizationName }) })
      setCreateOrganizationOpen(false); setOrganizationName(""); await loadOrganizations(data.organization.organization_id)
    } catch (reason) { showError(reason) }
  }

  async function createWorkspace(event: FormEvent) {
    event.preventDefault()
    if (!organizationId) return
    try {
      const data = await api<{ workspace: Workspace }>("/api/workspaces", { method: "POST", body: JSON.stringify({ organization_id: organizationId, name: workspaceName }) })
      setCreateWorkspaceOpen(false); setWorkspaceName("")
      const list = await api<{ workspaces: Workspace[] }>(`/api/workspaces?organization_id=${encodeURIComponent(organizationId)}`)
      setWorkspaces(list.workspaces); setWorkspaceId(data.workspace.workspace_id)
    } catch (reason) { showError(reason) }
  }

  async function renameOrganization(event: FormEvent) {
    event.preventDefault()
    if (!organizationId) return
    try {
      await api(`/api/organizations/${encodeURIComponent(organizationId)}`, { method: "PATCH", body: JSON.stringify({ name: organizationName }) })
      setRenameOrganizationOpen(false); await loadOrganizations(organizationId)
    } catch (reason) { showError(reason) }
  }

  async function updateProfile(event: FormEvent) {
    event.preventDefault()
    try {
      const updated = await api<User>("/api/auth/profile", { method: "PATCH", body: JSON.stringify({ email: profileEmail, preferred_name: preferredName }) })
      setUser(updated); setProfileOpen(false)
    } catch (reason) { showError(reason) }
  }

  async function createAgent(event: FormEvent) {
    event.preventDefault()
    if (!workspaceId) return
    try {
      let agent
      if (agentProfileId === "terminal") {
        agent = await api<Agent>(`/api/workspaces/${encodeURIComponent(workspaceId)}/agents`, { method: "POST", body: JSON.stringify({ server_id: agentServerId, kind: "command", name: agentName, cwd: agentCwd, args: [], cols: 120, rows: 36 }) })
      } else if (agentProfileId === "manual") {
        const parsed = parseCommandLine(agentCommandLine)
        const kind = parsed.command === "codex" || parsed.command === "claude" ? parsed.command : "command"
        const args = kind === "command" ? [parsed.command, ...parsed.args] : parsed.args
        agent = await api<Agent>(`/api/workspaces/${encodeURIComponent(workspaceId)}/agents`, { method: "POST", body: JSON.stringify({ server_id: agentServerId, kind, name: agentName, cwd: agentCwd, args, cols: 120, rows: 36 }) })
      } else {
        agent = await api<Agent>(`/api/workspaces/${encodeURIComponent(workspaceId)}/launch-profiles/${encodeURIComponent(agentProfileId)}/launch`, { method: "POST", body: JSON.stringify({ server_id: agentServerId, agent_name: agentName, cols: 120, rows: 36 }) })
      }
      setCreateAgentOpen(false); setSelectedAgentId(agent.agent_id); await refreshSnapshot()
    } catch (reason) { showError(reason) }
  }

  function openCreateAgent() {
    setAgentProfileId("terminal")
    setAgentName(defaultAgentName("terminal"))
    setAgentNameCustomized(false)
    setAgentCommandLine("codex")
    setCreateAgentOpen(true)
    void loadLaunchProfiles().catch(showError)
  }

  function changeAgentCommandLine(commandLine: string) {
    setAgentCommandLine(commandLine)
    if (agentNameCustomized) return
    try {
      const { command } = parseCommandLine(commandLine)
      const kind = command === "codex" || command === "claude" ? command : "command"
      setAgentName(defaultAgentName(kind))
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
      setAgentName(defaultAgentName("codex"))
      setAgentNameCustomized(false)
      setAgentCommandLine("codex")
      return
    }
    const profile = launchProfiles.find((item) => item.profile_id === profileId)
    if (!profile) return
    setAgentName(profile.name)
    setAgentNameCustomized(false)
  }

  const loadLaunchProfiles = useCallback(async () => {
    if (!workspaceId) {
      setLaunchProfiles([])
      return
    }
    setLaunchProfilesLoading(true)
    try {
      const data = await api<{ profiles: AgentLaunchProfile[] }>(`/api/workspaces/${encodeURIComponent(workspaceId)}/launch-profiles`)
      setLaunchProfiles(data.profiles)
    } finally {
      setLaunchProfilesLoading(false)
    }
  }, [workspaceId])

  useEffect(() => {
    if (mainView === "profiles") loadLaunchProfiles().catch(showError)
  }, [mainView, loadLaunchProfiles, showError])

  function openLaunchProfiles() {
    setMainView("profiles")
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
    setLaunchAgentName(profile.name)
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
      setSelectedAgentId(agent.agent_id)
      setMainView("terminal")
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
    if (!workspaceId) return
    try {
      const data = await api<{ install_command: string; connect_command: string }>(`/api/workspaces/${encodeURIComponent(workspaceId)}/bootstrap`, { method: "POST", body: "{}" })
      setInstallCommand(data.install_command); setConnectCommand(data.connect_command); setInstallOpen(true)
    } catch (reason) { showError(reason) }
  }

  function openRename(target: NonNullable<RenameTarget>) { setRenameTarget(target); setRenameName(target.name) }

  async function submitRename(event: FormEvent) {
    event.preventDefault()
    if (!workspaceId || !renameTarget) return
    const resource = renameTarget.kind === "machine" ? "servers" : "agents"
    try {
      await api(`/api/workspaces/${encodeURIComponent(workspaceId)}/${resource}/${encodeURIComponent(renameTarget.id)}`, { method: "PATCH", body: JSON.stringify({ name: renameName }) })
      setRenameTarget(null); await refreshSnapshot()
    } catch (reason) { showError(reason) }
  }

  async function confirmDelete() {
    if (!workspaceId || !deleteTarget) return
    const resource = deleteTarget.kind === "machine" ? "servers" : "agents"
    try {
      await api(`/api/workspaces/${encodeURIComponent(workspaceId)}/${resource}/${encodeURIComponent(deleteTarget.id)}`, { method: "DELETE" })
      setDeleteTarget(null); await refreshSnapshot()
    } catch (reason) { showError(reason) }
  }

  async function stopAgent() {
    if (!workspaceId || !selectedAgentId) return
    try { await api(`/api/workspaces/${encodeURIComponent(workspaceId)}/agents/${encodeURIComponent(selectedAgentId)}/stop`, { method: "POST", body: "{}" }) }
    catch (reason) { showError(reason) }
  }

  async function openMembers() {
    if (!organizationId) return
    try {
      const data = await api<{ members: Member[] }>(`/api/organizations/${encodeURIComponent(organizationId)}/members`)
      setMembers(data.members); setMembersOpen(true)
    } catch (reason) { showError(reason) }
  }

  async function updateMemberRole(userId: string, role: Member["role"]) {
    if (!organizationId) return
    try {
      await api(`/api/organizations/${encodeURIComponent(organizationId)}/members/${encodeURIComponent(userId)}`, { method: "PATCH", body: JSON.stringify({ role }) })
      await openMembers()
    } catch (reason) { showError(reason) }
  }

  async function removeMember(userId: string) {
    if (!organizationId) return
    try {
      await api(`/api/organizations/${encodeURIComponent(organizationId)}/members/${encodeURIComponent(userId)}`, { method: "DELETE" })
      await openMembers()
    } catch (reason) { showError(reason) }
  }

  async function createInvite() {
    if (!organizationId) return
    try {
      const invitation = await api<{ url: string }>(`/api/organizations/${encodeURIComponent(organizationId)}/invitations`, { method: "POST", body: "{}" })
      setInviteUrl(invitation.url); setMembersOpen(false); setInviteOpen(true)
    } catch (reason) { showError(reason) }
  }

  const loadNetwork = useCallback(async () => {
    if (!workspaceId) {
      setVirtualHosts([])
      setServices([])
      setIngresses([])
      return
    }
    const [hostData, serviceData, ingressData] = await Promise.all([
      api<{ hosts: VirtualNetworkHost[] }>(`/api/workspaces/${encodeURIComponent(workspaceId)}/virtual-hosts`),
      api<{ services: MachineService[] }>(`/api/workspaces/${encodeURIComponent(workspaceId)}/services`),
      api<{ ingresses: ServiceIngress[] }>(`/api/workspaces/${encodeURIComponent(workspaceId)}/ingresses`),
    ])
    setVirtualHosts(hostData.hosts)
    setServices(serviceData.services)
    setIngresses(ingressData.ingresses)
  }, [workspaceId])

  useEffect(() => {
    if (mainView === "network") loadNetwork().catch(showError)
  }, [mainView, loadNetwork, showError])

  function openNetwork() {
    setMainView("network")
  }

  const loadAudit = useCallback(async () => {
    if (!organizationId || !workspaceId || !canManageMembers) {
      setAuditEvents([])
      setTraffic([])
      return
    }
    setAuditLoading(true)
    try {
      const [auditData, trafficData] = await Promise.all([
        api<{ events: OrganizationAuditEvent[] }>(`/api/organizations/${encodeURIComponent(organizationId)}/audit-events?workspace_id=${encodeURIComponent(workspaceId)}&limit=100`),
        api<{ traffic: MachineTrafficRecord[] }>(`/api/workspaces/${encodeURIComponent(workspaceId)}/traffic?hours=24`),
      ])
      setAuditEvents(auditData.events)
      setTraffic(trafficData.traffic)
    } finally {
      setAuditLoading(false)
    }
  }, [organizationId, workspaceId, canManageMembers])

  useEffect(() => {
    if (mainView === "audit") loadAudit().catch(showError)
  }, [mainView, loadAudit, showError])

  function openAudit() {
    setMainView("audit")
  }

  function openCreateVirtualHost() {
    setVirtualServiceId((current) => current || services[0]?.service_id || "")
    setCreateVirtualHostOpen(true)
  }

  function openPublish() {
    setPublishServiceId((current) => current || services.find((service) => service.protocol === "http")?.service_id || "")
    setPublishOpen(true)
  }

  function openCreateService() {
    setEditingService(null)
    setServiceName("")
    setServiceTargetHost("127.0.0.1")
    setServiceTargetPort("")
    setServiceProtocol("http")
    setServiceServerId((current) => current || snapshot?.servers[0]?.server_id || "")
    setCreateServiceOpen(true)
  }

  function openEditService(service: MachineService) {
    setEditingService(service)
    setServiceName(service.name)
    setServiceServerId(service.server_id)
    setServiceTargetHost(service.target_host)
    setServiceTargetPort(String(service.target_port))
    setServiceProtocol(service.protocol)
    setCreateServiceOpen(true)
  }

  function openVirtualHost(hostname: string) {
    if (!workspaceId) return
    const url = proxyUrl(`/api/workspaces/${encodeURIComponent(workspaceId)}/virtual-hosts/${encodeURIComponent(hostname)}/proxy/`)
    window.open(url, "_blank", "noopener,noreferrer")
  }

  async function refreshNetwork() {
    try {
      await loadNetwork()
    } catch (reason) { showError(reason) }
  }

  async function createService(event: FormEvent) {
    event.preventDefault()
    if (!workspaceId) return
    try {
      const servicePath = editingService
        ? `/api/workspaces/${encodeURIComponent(workspaceId)}/services/${encodeURIComponent(editingService.service_id)}`
        : `/api/workspaces/${encodeURIComponent(workspaceId)}/services`
      await api(servicePath, {
        method: editingService ? "PATCH" : "POST",
        body: JSON.stringify({
          name: serviceName,
          server_id: serviceServerId,
          target_host: serviceTargetHost,
          target_port: Number(serviceTargetPort),
          protocol: serviceProtocol,
        }),
      })
      setServiceName(""); setServiceTargetPort("")
      await loadNetwork()
      setCreateServiceOpen(false)
      setEditingService(null)
    } catch (reason) { showError(reason) }
  }

  async function probeService(serviceId: string) {
    if (!workspaceId) return
    try {
      const result = await api<{ health: { healthy: boolean; error?: string } }>(`/api/workspaces/${encodeURIComponent(workspaceId)}/services/${encodeURIComponent(serviceId)}/probe`, { method: "POST", body: "{}" })
      setServiceHealth((current) => ({ ...current, [serviceId]: result.health.healthy ? "healthy" : "unreachable" }))
      if (!result.health.healthy) showError(result.health.error || "Service is unreachable")
    } catch (reason) { showError(reason) }
  }

  async function deleteService(serviceId: string) {
    if (!workspaceId) return
    try {
      await api(`/api/workspaces/${encodeURIComponent(workspaceId)}/services/${encodeURIComponent(serviceId)}`, { method: "DELETE" })
      await loadNetwork()
    } catch (reason) { showError(reason) }
  }

  async function createVirtualHost(event: FormEvent) {
    event.preventDefault()
    if (!workspaceId) return
    try {
      await api(`/api/workspaces/${encodeURIComponent(workspaceId)}/virtual-hosts`, {
        method: "POST",
        body: JSON.stringify({
          hostname: virtualHostname,
          service_id: virtualServiceId,
        }),
      })
      setVirtualHostname("")
      await loadNetwork()
      setCreateVirtualHostOpen(false)
    } catch (reason) { showError(reason) }
  }

  async function deleteVirtualHost(hostname: string) {
    if (!workspaceId) return
    try {
      await api(`/api/workspaces/${encodeURIComponent(workspaceId)}/virtual-hosts/${encodeURIComponent(hostname)}`, { method: "DELETE" })
      await loadNetwork()
    } catch (reason) { showError(reason) }
  }

  async function createIngress(event: FormEvent) {
    event.preventDefault()
    if (!workspaceId) return
    try {
      await api(`/api/workspaces/${encodeURIComponent(workspaceId)}/ingresses`, {
        method: "POST",
        body: JSON.stringify({ service_id: publishServiceId, slug: publishSlug || undefined, access: publishAccess }),
      })
      setPublishSlug("")
      await loadNetwork()
    } catch (reason) { showError(reason) }
  }

  async function updateIngress(ingressId: string, update: { access?: ServiceIngress["access"]; enabled?: boolean }) {
    if (!workspaceId) return
    try {
      await api(`/api/workspaces/${encodeURIComponent(workspaceId)}/ingresses/${encodeURIComponent(ingressId)}`, { method: "PATCH", body: JSON.stringify(update) })
      await loadNetwork()
    } catch (reason) { showError(reason) }
  }

  async function deleteIngress(ingressId: string) {
    if (!workspaceId) return
    try {
      await api(`/api/workspaces/${encodeURIComponent(workspaceId)}/ingresses/${encodeURIComponent(ingressId)}`, { method: "DELETE" })
      await loadNetwork()
    } catch (reason) { showError(reason) }
  }

  async function copy(value: string) {
    try { await navigator.clipboard.writeText(value) }
    catch { showError("Unable to copy to the clipboard") }
  }

  async function logout() {
    try { await api("/api/auth/logout", { method: "POST", body: "{}" }) }
    finally { window.location.href = "/" }
  }

  if (user === undefined) return <div className="grid min-h-dvh place-items-center bg-[#f7f7f5] text-sm text-muted-foreground">Loading Treer...</div>
  if (!user) return <AuthScreen onAuthenticated={setUser} />

  return <TooltipProvider delayDuration={350}>
    <main className="grid h-dvh min-h-0 grid-rows-[374px_minmax(620px,1fr)] overflow-auto bg-background md:grid-cols-[272px_minmax(0,1fr)] md:grid-rows-1 md:overflow-hidden">
      <aside className="flex min-h-0 flex-col border-b bg-[#f7f7f5] md:border-b-0 md:border-r">
        <div className="grid min-h-[58px] grid-cols-[32px_minmax(0,1fr)_32px] items-center gap-2 px-3 py-2">
          <div className="grid size-8 place-items-center rounded-[5px] bg-[#e8deee] text-[10px] font-bold text-[#694a73]">{initials(organization?.name ?? "Treer")}</div>
          <div className="min-w-0"><div className="mb-0.5 px-1 text-[9px] font-semibold uppercase text-muted-foreground">Organization</div><Select value={organizationId ?? undefined} onValueChange={setOrganizationId}><SelectTrigger className="h-7 border-0 bg-transparent px-1 shadow-none hover:bg-black/[.04]"><SelectValue placeholder="No organization" /></SelectTrigger><SelectContent>{organizations.map((item) => <SelectItem key={item.organization_id} value={item.organization_id}>{item.name}</SelectItem>)}</SelectContent></Select></div>
          <DropdownMenu><DropdownMenuTrigger asChild><Button size="icon" variant="ghost" className="size-8" aria-label="Organization actions"><MoreHorizontal /></Button></DropdownMenuTrigger><DropdownMenuContent align="end"><DropdownMenuItem onSelect={() => setCreateOrganizationOpen(true)}><Plus />Create organization</DropdownMenuItem>{canManageMembers && organization && <DropdownMenuItem onSelect={() => { setOrganizationName(organization.name); setRenameOrganizationOpen(true) }}><Pencil />Rename organization</DropdownMenuItem>}</DropdownMenuContent></DropdownMenu>
        </div>
        <div className="grid grid-cols-[20px_minmax(0,1fr)_32px] items-center gap-2 px-3 pb-3 pl-5">
          <FolderKanban className="size-3.5 text-muted-foreground" />
          <Select value={workspaceId ?? undefined} onValueChange={setWorkspaceId} disabled={!organizationId}><SelectTrigger className="h-7 border-0 bg-transparent px-1 text-xs shadow-none hover:bg-black/[.04]"><SelectValue placeholder="No workspace" /></SelectTrigger><SelectContent>{workspaces.map((item) => <SelectItem key={item.workspace_id} value={item.workspace_id}>{item.name}</SelectItem>)}</SelectContent></Select>
          <IconButton label="Create workspace" disabled={!organizationId} onClick={() => setCreateWorkspaceOpen(true)}><Plus /></IconButton>
        </div>
        <div className="px-2 pb-2">
          <Button variant={mainView === "profiles" ? "secondary" : "ghost"} className="h-8 w-full justify-start px-2 text-xs font-normal" onClick={openLaunchProfiles} disabled={!workspaceId}><Rocket className="size-3.5" />Profiles</Button>
          <Button variant={mainView === "network" ? "secondary" : "ghost"} className="h-8 w-full justify-start px-2 text-xs font-normal" onClick={openNetwork} disabled={!workspaceId}><Network className="size-3.5" />Network</Button>
          {canManageMembers && <Button variant={mainView === "audit" ? "secondary" : "ghost"} className="h-8 w-full justify-start px-2 text-xs font-normal" onClick={openAudit} disabled={!workspaceId}><ScrollText className="size-3.5" />Audit</Button>}
        </div>

        <Tabs defaultValue="agents" className="flex min-h-0 flex-1 flex-col overflow-hidden">
          <TabsList className="mx-2 grid h-auto grid-cols-2 bg-black/[.04] p-0.5">
            <TabsTrigger value="machines" className="h-8 gap-2 text-xs"><Server className="size-3.5" />Machines <span className="rounded-full bg-black/[.06] px-1.5 text-[9px]">{snapshot?.servers.length ?? 0}</span></TabsTrigger>
            <TabsTrigger value="agents" className="h-8 gap-2 text-xs"><TerminalSquare className="size-3.5" />Agents <span className="rounded-full bg-black/[.06] px-1.5 text-[9px]">{snapshot?.agents.length ?? 0}</span></TabsTrigger>
          </TabsList>
          <TabsContent value="machines" className="mt-0 min-h-0 flex-1 overflow-hidden data-[state=inactive]:hidden">
            <div className="flex h-full min-h-0 flex-col">
              <div className="flex h-10 shrink-0 items-center justify-between px-4 text-[11px] font-medium text-muted-foreground"><span>Machines</span><Button variant="ghost" size="sm" className="h-7 px-2" onClick={openInstall} disabled={!workspaceId}><CirclePlus className="size-3.5" />Add</Button></div>
              <div className="min-h-0 flex-1 overflow-auto px-2 pb-2">
                {snapshot?.servers.map((machine) => <MachineItem key={machine.server_id} machine={machine} onRename={() => openRename({ kind: "machine", id: machine.server_id, name: machineName(machine) })} onDelete={() => setDeleteTarget({ kind: "machine", id: machine.server_id, name: machineName(machine) })} />)}
                {snapshot && !snapshot.servers.length && <EmptyState icon={<Server />} label="No machines connected" />}
              </div>
            </div>
          </TabsContent>
          <TabsContent value="agents" className="mt-0 min-h-0 flex-1 overflow-hidden data-[state=inactive]:hidden">
            <div className="flex h-full min-h-0 flex-col">
              <div className="flex h-10 shrink-0 items-center justify-between px-4 text-[11px] font-medium text-muted-foreground"><span>Agents {snapshot && <span className="ml-1 font-mono text-[9px] text-zinc-400">rev {snapshot.revision}</span>}</span><Button variant="ghost" size="sm" className="h-7 px-2" onClick={openCreateAgent} disabled={!workspaceId || !onlineMachines.length}><Plus className="size-3.5" />New</Button></div>
              <div className="min-h-0 flex-1 overflow-auto px-2 pb-2">
                {snapshot?.agents.map((agent) => <AgentItem key={agent.agent_id} agent={agent} machine={snapshot.servers.find((item) => item.server_id === agent.server_id)} selected={mainView === "terminal" && agent.agent_id === selectedAgentId} onClick={() => { setSelectedAgentId(agent.agent_id); setMainView("terminal") }} />)}
                {snapshot && !snapshot.agents.length && <EmptyState icon={<TerminalSquare />} label="No agents in this workspace" />}
              </div>
            </div>
          </TabsContent>
        </Tabs>

        <div className="shrink-0 border-t p-2">
          <Button variant="ghost" className="h-8 w-full justify-start px-2 text-xs font-normal text-muted-foreground" onClick={openMembers} disabled={!organizationId}><Users className="size-3.5" />Members</Button>
          <DropdownMenu>
            <DropdownMenuTrigger asChild><button className="mt-1 grid h-11 w-full grid-cols-[28px_minmax(0,1fr)_20px] items-center gap-2 rounded-[5px] px-2 text-left hover:bg-black/[.05]"><span className="grid size-7 place-items-center rounded bg-[#e8deee] text-[10px] font-bold text-[#694a73]">{initials(user.preferred_name)}</span><span className="min-w-0"><span className="block truncate text-xs font-medium">{user.preferred_name}</span><span className="block truncate text-[9px] text-muted-foreground">{user.email}</span></span><MoreHorizontal className="size-4 text-muted-foreground" /></button></DropdownMenuTrigger>
            <DropdownMenuContent side="top" align="start" className="w-60"><DropdownMenuLabel><span className="block truncate">{user.preferred_name}</span><span className="mt-0.5 block truncate text-[10px] font-normal text-muted-foreground">{user.email} · {currentRole}</span></DropdownMenuLabel><DropdownMenuSeparator /><DropdownMenuItem onSelect={() => { setPreferredName(user.preferred_name); setProfileEmail(user.email); setProfileOpen(true) }}><Pencil />Edit profile</DropdownMenuItem><DropdownMenuItem onSelect={openMembers}><Users />Members</DropdownMenuItem><DropdownMenuSeparator /><DropdownMenuItem onSelect={logout}><LogOut />Log out</DropdownMenuItem></DropdownMenuContent>
          </DropdownMenu>
        </div>
      </aside>

      <section className="grid min-h-0 min-w-0 grid-rows-[48px_minmax(0,1fr)]">
        <header className="flex min-w-0 items-center justify-between gap-4 border-b px-3 sm:px-5">
          <div className="flex min-w-0 items-center gap-1.5 overflow-hidden text-xs text-muted-foreground"><span className="hidden truncate sm:block">{workspace?.name ?? "Workspace"}</span><ChevronRight className="hidden size-3 shrink-0 sm:block" /><strong className="truncate font-medium text-foreground">{mainView === "profiles" ? "Profiles" : mainView === "network" ? "Network" : mainView === "audit" ? "Audit" : selectedAgent?.name ?? "Terminal"}</strong></div>
          {mainView === "terminal" ? <div className="flex shrink-0 items-center gap-0.5">
            <IconButton label="Open full-screen terminal" className="md:hidden" disabled={!selectedAgent} onClick={openMobileTerminal}><Maximize2 /></IconButton>
            <IconButton label="Rename agent" disabled={!selectedAgent} onClick={() => selectedAgent && openRename({ kind: "agent", id: selectedAgent.agent_id, name: selectedAgent.name })}><Pencil /></IconButton>
            <IconButton label="Reconnect terminal" disabled={!selectedAgent} onClick={() => { setSelectedAgentId(null); requestAnimationFrame(() => setSelectedAgentId(selectedAgent?.agent_id ?? null)) }}><RotateCw /></IconButton>
            <IconButton label="Stop agent" disabled={!selectedAgent || !terminalActive} onClick={stopAgent}><Square /></IconButton>
            <IconButton label="Delete agent" disabled={!selectedAgent} className="text-destructive hover:text-destructive" onClick={() => selectedAgent && setDeleteTarget({ kind: "agent", id: selectedAgent.agent_id, name: selectedAgent.name })}><Trash2 /></IconButton>
          </div> : mainView === "profiles" ? <div className="flex shrink-0 items-center gap-1"><IconButton label="Refresh profiles" onClick={loadLaunchProfiles} disabled={launchProfilesLoading}><RotateCw /></IconButton><Button size="sm" className="h-8" onClick={openNewLaunchProfile}><Plus />New profile</Button></div> : mainView === "audit" ? <IconButton label="Refresh audit" onClick={loadAudit} disabled={auditLoading}><RotateCw /></IconButton> : <div className="flex shrink-0 items-center gap-1"><IconButton label="Refresh network" onClick={refreshNetwork}><RotateCw /></IconButton><Button size="sm" variant="outline" className="h-8" onClick={openCreateService} disabled={!snapshot?.servers.length}><Server />Add service</Button><Button size="sm" variant="outline" className="h-8" onClick={openCreateVirtualHost} disabled={!services.length}><Plus />Add host</Button><Button size="sm" className="h-8" onClick={openPublish} disabled={!services.some((service) => service.protocol === "http")}><ExternalLink />Publish</Button></div>}
        </header>
        {mainView === "terminal" ? <div className="flex min-h-0 justify-center overflow-hidden px-3 pb-4 pt-4 sm:px-8 sm:pb-7 sm:pt-6 lg:px-16">
          <div className={cn("grid h-full min-h-0 w-full max-w-[1120px] grid-rows-[42px_minmax(0,1fr)] overflow-hidden rounded-md border border-zinc-800 bg-[#0f1215] shadow-[0_8px_28px_rgba(15,18,21,.14)]", mobileTerminalOpen && "fixed inset-0 z-[100] h-[100dvh] max-w-none grid-rows-[44px_minmax(0,1fr)_auto] rounded-none border-0 shadow-none")}>
            <div className="flex min-w-0 items-center justify-between gap-3 border-b border-zinc-800 bg-[#191d20] px-3.5"><div className="flex min-w-0 items-baseline gap-2"><span className="truncate text-xs font-semibold text-zinc-200">{selectedAgent?.name ?? "Terminal"}</span>{selectedAgent && <span className="hidden truncate font-mono text-[9px] text-zinc-500 sm:block">{selectedAgent.agent_id} · {machineName(snapshot?.servers.find((item) => item.server_id === selectedAgent.server_id))}</span>}</div><div className="flex shrink-0 items-center gap-2"><span className="inline-flex items-center gap-1.5 text-[9px] uppercase text-zinc-500"><span className="size-1.5 rounded-full bg-current" />{terminalStatus}</span>{mobileTerminalOpen && <button type="button" className="grid size-8 place-items-center rounded-[5px] text-zinc-400 hover:bg-white/10 hover:text-zinc-100" aria-label="Close full-screen terminal" onClick={() => { setMobileTerminalOpen(false); setCtrlModifier(false) }}><X className="size-4" /></button>}</div></div>
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
        </div> : mainView === "profiles" ? <LaunchProfilesView profiles={launchProfiles} loading={launchProfilesLoading} onEdit={openEditLaunchProfile} onLaunch={openLaunchProfile} onDelete={setDeletingProfile} /> : mainView === "audit" ? <AuditView events={auditEvents} traffic={traffic} machines={snapshot?.servers ?? []} loading={auditLoading} /> : <div className="min-h-0 overflow-auto"><div className="mx-auto w-full max-w-[1120px] px-5 py-8 sm:px-8 sm:py-12 lg:px-14"><div className="mb-8 flex items-end justify-between gap-4"><div><div className="mb-2 grid size-9 place-items-center rounded-md bg-[#e8deee] text-[#694a73]"><Network className="size-4" /></div><h1 className="text-2xl font-semibold">Network</h1></div><span className="text-xs text-muted-foreground">{services.length} services · {virtualHosts.length} hosts</span></div><section className="mb-10"><h2 className="mb-3 text-sm font-semibold">Machine services</h2><div className="border-y"><div className="hidden h-9 grid-cols-[minmax(150px,1fr)_minmax(180px,1fr)_minmax(140px,1fr)_auto] items-center gap-4 border-b text-[10px] font-medium uppercase text-muted-foreground sm:grid"><span>Service</span><span>Target</span><span>Machine</span><span className="w-24" /></div>{services.map((service) => { const machine = snapshot?.servers.find((item) => item.server_id === service.server_id); const health = serviceHealth[service.service_id]; return <div key={service.service_id} className="grid min-h-16 grid-cols-[minmax(0,1fr)_auto] items-center gap-x-3 gap-y-1 border-b py-3 last:border-b-0 sm:grid-cols-[minmax(150px,1fr)_minmax(180px,1fr)_minmax(140px,1fr)_auto] sm:gap-4"><span className="col-start-1 row-start-1 min-w-0 truncate text-xs font-medium sm:col-start-auto sm:row-start-auto">{service.name}<span className="ml-2 font-mono text-[9px] uppercase text-muted-foreground">{service.protocol}</span>{health && <span className={cn("ml-2 text-[9px]", health === "healthy" ? "text-emerald-700" : "text-red-600")}>{health}</span>}</span><span className="col-start-1 row-start-2 min-w-0 truncate font-mono text-[10px] text-muted-foreground sm:col-start-auto sm:row-start-auto">{service.target_host}:{service.target_port}</span><span className="col-start-1 row-start-3 min-w-0 truncate text-[10px] text-muted-foreground sm:col-start-auto sm:row-start-auto">{machineName(machine, service.server_id)}</span><span className="col-start-2 row-span-3 row-start-1 flex items-center justify-end gap-1 sm:col-start-auto sm:row-span-1 sm:row-start-auto"><IconButton label={`Probe ${service.name}`} onClick={() => probeService(service.service_id)} disabled={machine?.status !== "online"}><RotateCw /></IconButton><IconButton label={`Edit ${service.name}`} onClick={() => openEditService(service)}><Pencil /></IconButton><IconButton label={`Delete ${service.name}`} className="text-destructive hover:text-destructive" onClick={() => deleteService(service.service_id)}><Trash2 /></IconButton></span></div>})}{!services.length && <EmptyState icon={<Server />} label="No machine services" />}</div></section><section><h2 className="mb-3 text-sm font-semibold">Virtual hosts</h2><div className="border-y"><div className="hidden h-9 grid-cols-[minmax(150px,1.2fr)_minmax(180px,1fr)_minmax(140px,1fr)_auto] items-center gap-4 border-b text-[10px] font-medium uppercase text-muted-foreground sm:grid"><span>Hostname</span><span>Service</span><span>Machine</span><span className="w-24" /></div>{virtualHosts.map((host) => { const machine = snapshot?.servers.find((item) => item.server_id === host.destination_server_id); const service = services.find((item) => item.service_id === host.service_id); return <div key={host.hostname} className="grid min-h-16 grid-cols-[minmax(0,1fr)_auto] items-center gap-x-3 gap-y-1 border-b py-3 last:border-b-0 sm:grid-cols-[minmax(150px,1.2fr)_minmax(180px,1fr)_minmax(140px,1fr)_auto] sm:gap-4"><button className="col-start-1 row-start-1 min-w-0 truncate text-left font-mono text-xs font-medium hover:underline sm:col-start-auto sm:row-start-auto" onClick={() => openVirtualHost(host.hostname)}>{host.hostname}</button><span className="col-start-1 row-start-2 min-w-0 truncate text-[10px] text-muted-foreground sm:col-start-auto sm:row-start-auto">{service?.name ?? host.service_id}</span><span className="col-start-1 row-start-3 min-w-0 truncate text-[10px] text-muted-foreground sm:col-start-auto sm:row-start-auto">{machineName(machine, host.destination_server_id)}</span><span className="col-start-2 row-span-3 row-start-1 flex items-center justify-end gap-1 sm:col-start-auto sm:row-span-1 sm:row-start-auto"><IconButton label={`Open ${host.hostname}`} onClick={() => openVirtualHost(host.hostname)} disabled={machine?.status !== "online" || service?.protocol !== "http"}><ExternalLink /></IconButton><IconButton label={`Delete ${host.hostname}`} className="text-destructive hover:text-destructive" onClick={() => deleteVirtualHost(host.hostname)}><Trash2 /></IconButton></span></div>})}{!virtualHosts.length && <EmptyState icon={<Network />} label="No virtual hosts" />}</div></section></div></div>}
      </section>
    </main>

    {error && <div className="fixed bottom-4 left-1/2 z-[90] flex max-w-[calc(100vw-2rem)] -translate-x-1/2 items-center gap-3 rounded-md border bg-background px-4 py-3 text-sm shadow-lg"><span className="truncate">{error}</span><Button size="sm" variant="ghost" onClick={() => setError(null)}>Dismiss</Button></div>}

    <SimpleNameDialog open={createOrganizationOpen} onOpenChange={setCreateOrganizationOpen} title="Create organization" description="Organizations contain members and workspaces." label="Organization name" value={organizationName} onValueChange={setOrganizationName} onSubmit={createOrganization} />
    <SimpleNameDialog open={createWorkspaceOpen} onOpenChange={setCreateWorkspaceOpen} title="Create workspace" description={`Add a workspace to ${organization?.name ?? "this organization"}.`} label="Workspace name" value={workspaceName} onValueChange={setWorkspaceName} onSubmit={createWorkspace} />

    <Dialog open={renameOrganizationOpen} onOpenChange={setRenameOrganizationOpen}><DialogContent><form onSubmit={renameOrganization}><DialogHeader><DialogTitle>Rename organization</DialogTitle><DialogDescription>Update the organization name shown to its members.</DialogDescription></DialogHeader><div className="my-5"><Field label="Organization name"><Input value={organizationName} onChange={(event) => setOrganizationName(event.target.value)} required autoFocus maxLength={80} /></Field></div><DialogFooter><Button type="button" variant="outline" onClick={() => setRenameOrganizationOpen(false)}>Cancel</Button><Button type="submit">Save</Button></DialogFooter></form></DialogContent></Dialog>

    <Dialog open={profileOpen} onOpenChange={setProfileOpen}><DialogContent><form onSubmit={updateProfile}><DialogHeader><DialogTitle>Edit profile</DialogTitle><DialogDescription>Your preferred name is visible to other organization members.</DialogDescription></DialogHeader><div className="my-5 space-y-4"><Field label="Preferred name"><Input value={preferredName} onChange={(event) => setPreferredName(event.target.value)} required autoFocus maxLength={80} /></Field><Field label="Email"><Input type="email" value={profileEmail} onChange={(event) => setProfileEmail(event.target.value)} required maxLength={254} /></Field></div><DialogFooter><Button type="button" variant="outline" onClick={() => setProfileOpen(false)}>Cancel</Button><Button type="submit">Save</Button></DialogFooter></form></DialogContent></Dialog>

    <Dialog open={profileEditorOpen} onOpenChange={(open) => { setProfileEditorOpen(open); if (!open) setEditingLaunchProfile(null) }}><DialogContent className="max-w-xl"><form onSubmit={saveLaunchProfile} className="grid gap-4 sm:grid-cols-2"><DialogHeader className="sm:col-span-2"><DialogTitle>{editingLaunchProfile ? "Edit launch profile" : "New launch profile"}</DialogTitle><DialogDescription>Reusable Agent process settings for this workspace.</DialogDescription></DialogHeader><Field label="Profile name"><Input value={launchProfileName} onChange={(event) => setLaunchProfileName(event.target.value)} required autoFocus maxLength={80} /></Field><Field label="Working directory"><Input className="font-mono" value={launchProfileCwd} onChange={(event) => setLaunchProfileCwd(event.target.value)} required /></Field><div className="sm:col-span-2"><Field label="Description"><Input value={launchProfileDescription} onChange={(event) => setLaunchProfileDescription(event.target.value)} maxLength={1000} /></Field></div><div className="sm:col-span-2"><Field label="Command"><Input className="font-mono" value={launchProfileCommandLine} onChange={(event) => setLaunchProfileCommandLine(event.target.value)} placeholder="codex review --base main" required /></Field></div><DialogFooter className="sm:col-span-2"><Button type="button" variant="outline" onClick={() => setProfileEditorOpen(false)}>Cancel</Button><Button type="submit"><Rocket />{editingLaunchProfile ? "Save profile" : "Create profile"}</Button></DialogFooter></form></DialogContent></Dialog>

    <Dialog open={Boolean(launchingProfile)} onOpenChange={(open) => !open && setLaunchingProfile(null)}><DialogContent><form onSubmit={launchFromProfile} className="space-y-4"><DialogHeader><DialogTitle>Run {launchingProfile?.name}</DialogTitle><DialogDescription>Choose where to start this Agent.</DialogDescription></DialogHeader><Field label="Machine"><Select value={launchMachineId} onValueChange={setLaunchMachineId} required><SelectTrigger><SelectValue placeholder="Select an online machine" /></SelectTrigger><SelectContent>{onlineMachines.map((machine) => <SelectItem key={machine.server_id} value={machine.server_id}>{machineName(machine)}</SelectItem>)}</SelectContent></Select></Field><Field label="Agent name"><Input value={launchAgentName} onChange={(event) => setLaunchAgentName(event.target.value)} required maxLength={80} /></Field><DialogFooter><Button type="button" variant="outline" onClick={() => setLaunchingProfile(null)}>Cancel</Button><Button type="submit" disabled={!launchMachineId}><Play />Run Agent</Button></DialogFooter></form></DialogContent></Dialog>

    <Dialog open={Boolean(deletingProfile)} onOpenChange={(open) => !open && setDeletingProfile(null)}><DialogContent><DialogHeader><DialogTitle>Delete launch profile</DialogTitle><DialogDescription>Delete {deletingProfile?.name}? Existing Agents are not affected.</DialogDescription></DialogHeader><DialogFooter><Button variant="outline" onClick={() => setDeletingProfile(null)}>Cancel</Button><Button variant="destructive" onClick={deleteLaunchProfile}>Delete profile</Button></DialogFooter></DialogContent></Dialog>

    <Dialog open={createAgentOpen} onOpenChange={setCreateAgentOpen}><DialogContent><form onSubmit={createAgent} className="space-y-4"><DialogHeader><DialogTitle>Create agent</DialogTitle><DialogDescription>Start a terminal or agent on an online machine in this workspace.</DialogDescription></DialogHeader><Field label="Launch"><Select value={agentProfileId} onValueChange={changeAgentProfile}><SelectTrigger><SelectValue /></SelectTrigger><SelectContent><SelectItem value="terminal">Terminal</SelectItem><SelectItem value="manual">Custom command</SelectItem>{launchProfiles.map((profile) => <SelectItem key={profile.profile_id} value={profile.profile_id}>{profile.name}</SelectItem>)}</SelectContent></Select></Field><Field label="Machine"><Select value={agentServerId} onValueChange={setAgentServerId} required><SelectTrigger><SelectValue placeholder="Select a machine" /></SelectTrigger><SelectContent>{onlineMachines.map((machine) => <SelectItem key={machine.server_id} value={machine.server_id}>{machineName(machine)}</SelectItem>)}</SelectContent></Select></Field>{agentProfileId === "terminal" || agentProfileId === "manual" ? <Field label="Working directory"><Input value={agentCwd} onChange={(event) => setAgentCwd(event.target.value)} /></Field> : selectedCreateProfile ? <div className="rounded-md border bg-muted/30 px-3 py-2"><code className="block truncate text-xs" title={formatCommandLine(selectedCreateProfile.command, selectedCreateProfile.args)}>{formatCommandLine(selectedCreateProfile.command, selectedCreateProfile.args)}</code><span className="mt-1 block truncate text-[10px] text-muted-foreground">{selectedCreateProfile.cwd || "."}</span></div> : null}{agentProfileId === "manual" && <Field label="Command"><Input className="font-mono" value={agentCommandLine} onChange={(event) => changeAgentCommandLine(event.target.value)} placeholder="codex" required /></Field>}<Field label="Name"><Input value={agentName} onChange={(event) => { setAgentName(event.target.value); setAgentNameCustomized(true) }} required /></Field><DialogFooter><Button type="button" variant="outline" onClick={() => setCreateAgentOpen(false)}>Cancel</Button><Button type="submit" disabled={!agentServerId}>{agentProfileId === "terminal" ? "Create terminal" : "Create agent"}</Button></DialogFooter></form></DialogContent></Dialog>

    <Dialog open={installOpen} onOpenChange={setInstallOpen}><DialogContent className="max-w-xl"><DialogHeader><DialogTitle>Add machine</DialogTitle><DialogDescription>Install Treer, then connect this workspace.</DialogDescription></DialogHeader><div className="space-y-4"><Field label="1. Install Treer"><div className="space-y-2"><Textarea readOnly value={installCommand} className="min-h-20 font-mono text-xs" /><Button size="sm" variant="outline" onClick={() => copy(installCommand)}><Copy />Copy install command</Button></div></Field><Field label="2. Connect workspace"><div className="space-y-2"><Textarea readOnly value={connectCommand} className="min-h-24 font-mono text-xs" /><Button size="sm" onClick={() => copy(connectCommand)}><Copy />Copy connection command</Button></div></Field></div><DialogFooter><Button variant="outline" onClick={() => setInstallOpen(false)}>Close</Button></DialogFooter></DialogContent></Dialog>

    <Dialog open={createServiceOpen} onOpenChange={(open) => { setCreateServiceOpen(open); if (!open) setEditingService(null) }}><DialogContent className="max-w-xl"><form onSubmit={createService} className="grid gap-4 sm:grid-cols-2"><DialogHeader className="sm:col-span-2"><DialogTitle>{editingService ? "Edit machine service" : "Register machine service"}</DialogTitle><DialogDescription>{editingService ? "Update the durable service target without changing its virtual hosts." : "Register a long-running service already available from its machine."}</DialogDescription></DialogHeader><Field label="Service name"><Input value={serviceName} onChange={(event) => setServiceName(event.target.value)} placeholder="API server" required autoFocus /></Field><Field label="Machine"><Select value={serviceServerId} onValueChange={setServiceServerId} required><SelectTrigger><SelectValue placeholder="Select machine" /></SelectTrigger><SelectContent>{snapshot?.servers.map((machine) => <SelectItem key={machine.server_id} value={machine.server_id}>{machineName(machine)}</SelectItem>)}</SelectContent></Select></Field><Field label="Target host"><Input className="font-mono" value={serviceTargetHost} onChange={(event) => setServiceTargetHost(event.target.value)} required /></Field><Field label="Target port"><Input type="number" min="1" max="65535" value={serviceTargetPort} onChange={(event) => setServiceTargetPort(event.target.value)} required /></Field><Field label="Protocol"><Select value={serviceProtocol} onValueChange={(value: "tcp" | "http") => setServiceProtocol(value)}><SelectTrigger><SelectValue /></SelectTrigger><SelectContent><SelectItem value="http">HTTP</SelectItem><SelectItem value="tcp">TCP</SelectItem></SelectContent></Select></Field><DialogFooter className="sm:col-span-2"><Button type="button" variant="outline" onClick={() => setCreateServiceOpen(false)}>Cancel</Button><Button type="submit" disabled={!serviceName || !serviceServerId || !serviceTargetPort}><Server />{editingService ? "Save service" : "Register service"}</Button></DialogFooter></form></DialogContent></Dialog>

    <Dialog open={createVirtualHostOpen} onOpenChange={setCreateVirtualHostOpen}><DialogContent><form onSubmit={createVirtualHost} className="space-y-4"><DialogHeader><DialogTitle>Add virtual host</DialogTitle><DialogDescription>Map a workspace hostname to a registered machine service.</DialogDescription></DialogHeader><Field label="Virtual hostname"><Input className="font-mono" value={virtualHostname} onChange={(event) => setVirtualHostname(event.target.value)} placeholder="app.internal" required autoFocus /></Field><Field label="Service"><Select value={virtualServiceId} onValueChange={setVirtualServiceId} required><SelectTrigger><SelectValue placeholder="Select service" /></SelectTrigger><SelectContent>{services.map((service) => <SelectItem key={service.service_id} value={service.service_id}>{service.name} · {service.target_host}:{service.target_port}</SelectItem>)}</SelectContent></Select></Field><DialogFooter><Button type="button" variant="outline" onClick={() => setCreateVirtualHostOpen(false)}>Cancel</Button><Button type="submit" disabled={!virtualHostname || !virtualServiceId}><Plus />Add host</Button></DialogFooter></form></DialogContent></Dialog>

    <Dialog open={Boolean(renameTarget)} onOpenChange={(open) => !open && setRenameTarget(null)}><DialogContent><form onSubmit={submitRename}><DialogHeader><DialogTitle>Rename {renameTarget?.kind}</DialogTitle><DialogDescription>Choose a clear name for this {renameTarget?.kind}.</DialogDescription></DialogHeader><div className="my-5"><Field label="Name"><Input value={renameName} onChange={(event) => setRenameName(event.target.value)} required autoFocus /></Field></div><DialogFooter><Button type="button" variant="outline" onClick={() => setRenameTarget(null)}>Cancel</Button><Button type="submit">Rename</Button></DialogFooter></form></DialogContent></Dialog>

    <Dialog open={Boolean(deleteTarget)} onOpenChange={(open) => !open && setDeleteTarget(null)}><DialogContent><DialogHeader><DialogTitle>Delete {deleteTarget?.kind}</DialogTitle><DialogDescription>{deleteTarget?.kind === "machine" ? `Remove ${deleteTarget.name} and all of its agents? Its credential will be revoked, but its local service will not be uninstalled.` : `Delete ${deleteTarget?.name} and stop its process? This agent will not return after reconnecting.`}</DialogDescription></DialogHeader><DialogFooter><Button variant="outline" onClick={() => setDeleteTarget(null)}>Cancel</Button><Button variant="destructive" onClick={confirmDelete}>Delete</Button></DialogFooter></DialogContent></Dialog>

    <Dialog open={membersOpen} onOpenChange={setMembersOpen}><DialogContent className="max-w-xl"><DialogHeader><div className="flex items-center justify-between gap-4 pr-7"><DialogTitle>Organization members</DialogTitle>{canManageMembers && <Button size="sm" onClick={createInvite}><UserRound />Invite member</Button>}</div><DialogDescription>Manage access to {organization?.name ?? "this organization"}.</DialogDescription></DialogHeader><div className="max-h-[55vh] divide-y overflow-auto border-y">{members.map((member) => <div key={member.user_id} className="grid min-h-14 grid-cols-[minmax(0,1fr)_120px_auto] items-center gap-3"><span className="min-w-0"><span className="block truncate text-sm font-medium">{member.preferred_name}</span><span className="mt-0.5 block truncate text-[10px] text-muted-foreground">{member.email}</span></span>{currentRole === "owner" && member.role !== "owner" ? <Select value={member.role} onValueChange={(value: Member["role"]) => updateMemberRole(member.user_id, value)}><SelectTrigger className="h-8 text-xs"><SelectValue /></SelectTrigger><SelectContent><SelectItem value="member">Member</SelectItem><SelectItem value="admin">Admin</SelectItem></SelectContent></Select> : <span className="text-xs capitalize text-muted-foreground">{member.role}</span>}{canManageMembers && member.role !== "owner" ? <IconButton label={`Remove ${member.preferred_name}`} className="text-destructive" onClick={() => removeMember(member.user_id)}><Trash2 /></IconButton> : <span />}</div>)}</div></DialogContent></Dialog>

    <Dialog open={inviteOpen} onOpenChange={setInviteOpen}><DialogContent><DialogHeader><DialogTitle>Invite member</DialogTitle><DialogDescription>This registration link can be used once.</DialogDescription></DialogHeader><Textarea readOnly value={inviteUrl} className="min-h-24 font-mono text-xs" /><DialogFooter><Button variant="outline" onClick={() => setInviteOpen(false)}>Close</Button><Button onClick={() => copy(inviteUrl)}><Copy />Copy link</Button></DialogFooter></DialogContent></Dialog>
    <Dialog open={publishOpen} onOpenChange={setPublishOpen}>
      <DialogContent className="max-w-2xl">
        <DialogHeader><DialogTitle>Published endpoints</DialogTitle><DialogDescription>Expose an HTTP machine service through the configured wildcard HTTPS domain.</DialogDescription></DialogHeader>
        <form onSubmit={createIngress} className="grid gap-3 border-y py-4 sm:grid-cols-[minmax(0,1fr)_minmax(0,1fr)_140px_auto]">
          <Field label="Service"><Select value={publishServiceId} onValueChange={setPublishServiceId} required><SelectTrigger><SelectValue placeholder="Select service" /></SelectTrigger><SelectContent>{services.filter((service) => service.protocol === "http").map((service) => <SelectItem key={service.service_id} value={service.service_id}>{service.name}</SelectItem>)}</SelectContent></Select></Field>
          <Field label="URL slug"><Input value={publishSlug} onChange={(event) => setPublishSlug(event.target.value)} placeholder="service name" /></Field>
          <Field label="Access"><Select value={publishAccess} onValueChange={(value: ServiceIngress["access"]) => setPublishAccess(value)}><SelectTrigger><SelectValue /></SelectTrigger><SelectContent><SelectItem value="public">Public</SelectItem><SelectItem value="workspace">Workspace</SelectItem></SelectContent></Select></Field>
          <Button type="submit" className="self-end" disabled={!publishServiceId}><ExternalLink />Publish</Button>
        </form>
        <div className="max-h-[45vh] divide-y overflow-auto">
          {ingresses.map((ingress) => {
            const service = services.find((item) => item.service_id === ingress.service_id)
            return <div key={ingress.ingress_id} className="grid min-h-16 grid-cols-[minmax(0,1fr)_auto] items-center gap-3 py-2 sm:grid-cols-[minmax(0,1fr)_120px_auto]">
              <span className="min-w-0"><button title={ingress.hostname} className="block max-w-full truncate text-left font-mono text-xs font-medium hover:underline" onClick={() => window.open(ingress.url, "_blank", "noopener,noreferrer")}>{ingress.hostname}</button><span className="mt-1 block truncate text-[10px] text-muted-foreground">{service?.name ?? ingress.service_id}</span></span>
              <Select value={ingress.access} onValueChange={(access: ServiceIngress["access"]) => updateIngress(ingress.ingress_id, { access })}><SelectTrigger className="col-start-1 row-start-2 h-8 w-[120px] text-xs sm:col-start-2 sm:row-start-1"><SelectValue /></SelectTrigger><SelectContent><SelectItem value="public">Public</SelectItem><SelectItem value="workspace">Workspace</SelectItem></SelectContent></Select>
              <span className="col-start-2 row-span-2 row-start-1 flex items-center gap-1 sm:col-start-3 sm:row-span-1"><label className="flex size-8 cursor-pointer items-center justify-center" title={ingress.enabled ? "Disable endpoint" : "Enable endpoint"}><input type="checkbox" className="size-4 accent-foreground" checked={ingress.enabled} onChange={(event) => updateIngress(ingress.ingress_id, { enabled: event.target.checked })} aria-label={`${ingress.enabled ? "Disable" : "Enable"} ${ingress.hostname}`} /></label><IconButton label={`Copy ${ingress.hostname}`} onClick={() => copy(ingress.url)}><Copy /></IconButton><IconButton label={`Open ${ingress.hostname}`} onClick={() => window.open(ingress.url, "_blank", "noopener,noreferrer")} disabled={!ingress.enabled}><ExternalLink /></IconButton><IconButton label={`Delete ${ingress.hostname}`} className="text-destructive hover:text-destructive" onClick={() => deleteIngress(ingress.ingress_id)}><Trash2 /></IconButton></span>
            </div>
          })}
          {!ingresses.length && <EmptyState icon={<ExternalLink />} label="No published endpoints" />}
        </div>
        <DialogFooter><Button variant="outline" onClick={() => setPublishOpen(false)}>Close</Button></DialogFooter>
      </DialogContent>
    </Dialog>
  </TooltipProvider>
}

function Field({ label, children }: { label: string; children: React.ReactNode }) {
  return <div className="space-y-2"><Label>{label}</Label>{children}</div>
}

function SimpleNameDialog({ open, onOpenChange, title, description, label, value, onValueChange, onSubmit }: { open: boolean; onOpenChange: (open: boolean) => void; title: string; description: string; label: string; value: string; onValueChange: (value: string) => void; onSubmit: (event: FormEvent) => void }) {
  return <Dialog open={open} onOpenChange={onOpenChange}><DialogContent><form onSubmit={onSubmit}><DialogHeader><DialogTitle>{title}</DialogTitle><DialogDescription>{description}</DialogDescription></DialogHeader><div className="my-5"><Field label={label}><Input value={value} onChange={(event) => onValueChange(event.target.value)} required autoFocus maxLength={80} /></Field></div><DialogFooter><Button type="button" variant="outline" onClick={() => onOpenChange(false)}>Cancel</Button><Button type="submit">Create</Button></DialogFooter></form></DialogContent></Dialog>
}

function EmptyState({ icon, label }: { icon: React.ReactNode; label: string }) {
  return <div className="flex flex-col items-center gap-2 px-4 py-8 text-center text-[11px] text-muted-foreground"><span className="[&_svg]:size-4 [&_svg]:opacity-50">{icon}</span>{label}</div>
}

function MachineItem({ machine, onRename, onDelete }: { machine: Machine; onRename: () => void; onDelete: () => void }) {
  const builds = `Controller ${buildLabel(machine.controller_build)} · Host ${buildLabel(machine.host_build)}`
  const buildTitle = `Controller ${machine.controller_build.version} (${machine.controller_build.git_commit})\nHost ${machine.host_build.version} (${machine.host_build.git_commit})`
  return <div className="group flex min-h-[68px] items-start gap-2 rounded-[5px] px-2.5 py-2 hover:bg-black/[.045]"><span className={cn("mt-1.5 size-1.5 shrink-0 rounded-full bg-zinc-400", machine.status === "online" && "bg-emerald-500")} /><div className="min-w-0 flex-1"><div className="truncate text-xs font-medium">{machineName(machine)}</div><div className="mt-1 truncate font-mono text-[9px] text-muted-foreground">{machine.root}</div><div className="mt-1 truncate font-mono text-[9px] text-muted-foreground" title={buildTitle}>{builds}</div></div><DropdownMenu><DropdownMenuTrigger asChild><Button size="icon" variant="ghost" className="size-7 shrink-0 opacity-0 group-hover:opacity-100 data-[state=open]:opacity-100" aria-label={`Actions for ${machineName(machine)}`}><MoreHorizontal /></Button></DropdownMenuTrigger><DropdownMenuContent align="end"><DropdownMenuItem onSelect={onRename}><Pencil />Rename</DropdownMenuItem><DropdownMenuSeparator /><DropdownMenuItem className="text-destructive focus:text-destructive" onSelect={onDelete}><Trash2 />Delete</DropdownMenuItem></DropdownMenuContent></DropdownMenu></div>
}

function AgentItem({ agent, machine, selected, onClick }: { agent: Agent; machine?: Machine; selected: boolean; onClick: () => void }) {
  return <button onClick={onClick} className={cn("grid min-h-12 w-full grid-cols-[minmax(0,1fr)_auto] items-center gap-2 rounded-[5px] px-2.5 py-2 text-left hover:bg-black/[.045]", selected && "bg-black/[.075] hover:bg-black/[.075]")}><span className="min-w-0"><span className="block truncate text-xs font-medium">{agent.name}</span><span className="mt-1 block truncate text-[9px] text-muted-foreground">{agent.kind} · {machineName(machine, agent.server_id)}</span></span><Status value={agent.status} /></button>
}

export default function App() {
  return window.location.pathname === "/admin" ? <AdminPanel /> : <WorkspaceApp />
}
