import { FormEvent, useCallback, useEffect, useRef, useState } from "react"
import type * as React from "react"
import { Route, Routes, useLocation, useNavigate, useParams } from "react-router"
import {
  ArrowDown,
  ArrowLeft,
  ArrowRight,
  ArrowUp,
  Activity,
  Building2,
  ChevronDown,
  ChevronLeft,
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
  ListChecks,
  LogOut,
  Mail,
  Maximize2,
  PanelsTopLeft,
  MoreHorizontal,
  MoreVertical,
  Network,
  Pencil,
  Plus,
  RotateCw,
  Rocket,
  Play,
  ScrollText,
  Search,
  Server,
  Settings as SettingsIcon,
  Square,
  ShieldCheck,
  TerminalSquare,
  TriangleAlert,
  Trash2,
  UserRound,
  Users,
  X,
  CircleCheck,
  Download,
} from "lucide-react"
import { api, ApiError, machineName, proxyUrl, websocketUrl, type AdminDashboard, type AdminInvitation, type AdminMachine, type AdminOrganization, type AdminUser, type AdminUserDetail, type Agent, type AgentLaunchProfile, type AppDeployment, type ControlPlaneUpdateStatus, type Machine, type MachineService, type MachineTrafficRecord, type Member, type Organization, type OrganizationAuditEvent, type PlatformAuditEvent, type ServiceIngress, type Snapshot, type User, type VirtualNetworkHost, type Workspace } from "@/lib/api"
import { agentKindFromCommand, availableCatalog, catalogEntry, installThenStartScript, isAgentInstalled, type AgentCatalogEntry } from "@/lib/agents"
import { formatCommandLine, parseCommandLine } from "@/lib/command-line"
import { clearAdminTour, clearFirstRunTour, firstRunTourMode, shouldAutoStartAdminTour, shouldAutoStartFirstRunTour, startAdminTour, startFirstRunTour, stopFirstRunTour, type AdminTourHost, type FirstRunTourHost, type SidebarTab } from "@/lib/first-run-tour"
import { cn } from "@/lib/utils"
import { SettingsDialog } from "@/components/settings"
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
type MainView = "terminal" | "profiles" | "apps" | "network" | "audit" | "machine" | "workspace"
type AuthMode = "login" | "register" | "forgot" | "reset"
type AuthConfig = { github: boolean; google: boolean; invitation_required: boolean }
type RenameTarget = { kind: "machine" | "agent"; id: string; name: string } | null
type DeleteTarget = { kind: "machine" | "agent"; id: string; name: string } | null

const PREVIEW_USER: User = { user_id: "usr_preview", email: "you@example.com", preferred_name: "You" }
const PREVIEW_ORG: Organization = { organization_id: "org_preview", name: "You Personal", role: "owner" }
const PREVIEW_WORKSPACE: Workspace = { workspace_id: "ws_preview", name: "lab" }
const PREVIEW_NOW = "2026-08-25T00:00:00.000Z"
const PREVIEW_PROFILES: AgentLaunchProfile[] = [
  { profile_id: "codex", workspace_id: "ws_preview", name: "Codex", description: "OpenAI Codex", cwd: ".", command: "codex", args: [], created_at: PREVIEW_NOW, created_by: "usr_preview", updated_at: PREVIEW_NOW, updated_by: "usr_preview" },
  { profile_id: "claude", workspace_id: "ws_preview", name: "Claude", description: "Anthropic Claude Code", cwd: ".", command: "claude", args: [], created_at: PREVIEW_NOW, created_by: "usr_preview", updated_at: PREVIEW_NOW, updated_by: "usr_preview" },
  { profile_id: "pi", workspace_id: "ws_preview", name: "Pi", description: "Pi coding agent", cwd: ".", command: "pi", args: [], created_at: PREVIEW_NOW, created_by: "usr_preview", updated_at: PREVIEW_NOW, updated_by: "usr_preview" },
  { profile_id: "opencode", workspace_id: "ws_preview", name: "OpenCode", description: "OpenCode", cwd: ".", command: "opencode", args: [], created_at: PREVIEW_NOW, created_by: "usr_preview", updated_at: PREVIEW_NOW, updated_by: "usr_preview" },
]
const PREVIEW_INSTALL = "curl -fsSL 'https://treer.example/install.sh' | sh"
const PREVIEW_CONNECT = "TREER_ENROLLMENT_KEY='enr_v1_…' treer-agent-server connect --proxy 'https://treer.example/'"

const activeStatuses = new Set(["starting", "working", "idle", "blocked"])

function initials(value: string) {
  return value.trim().slice(0, 2).toUpperCase() || "T"
}

function replaceWorkspace(items: Workspace[], updated: Workspace) {
  return items
    .map((item) => item.workspace_id === updated.workspace_id ? updated : item)
    .sort((left, right) => left.name.localeCompare(right.name) || left.workspace_id.localeCompare(right.workspace_id))
}

function visibleWorkspaceSnapshot(snapshot: Snapshot): Snapshot {
  return { ...snapshot, agents: snapshot.agents.filter((agent) => agent.kind !== "app") }
}

function workspacePath(organizationId: string | null, workspaceId: string | null) {
  if (!organizationId) return "/"
  const organizationPath = `/orgs/${encodeURIComponent(organizationId)}`
  return workspaceId
    ? `${organizationPath}/workspaces/${encodeURIComponent(workspaceId)}`
    : organizationPath
}

function defaultAgentName(kind: string) {
  const now = new Date()
  const month = String(now.getMonth() + 1).padStart(2, "0")
  const day = String(now.getDate()).padStart(2, "0")
  const prefix = kind === "terminal" ? "terminal" : kind === "command" ? "cmd" : kind === "codex" || kind === "claude" || kind === "installer" ? kind : "agent"
  return `${prefix}-${now.getFullYear()}-${month}-${day}`
}

function defaultProfileAgentName(profileName: string) {
  const now = new Date()
  const pad = (value: number) => String(value).padStart(2, "0")
  const slug = profileName.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-+|-+$/g, "").slice(0, 40) || "agent"
  const stamp = `${now.getFullYear()}-${pad(now.getMonth() + 1)}-${pad(now.getDate())}-${pad(now.getHours())}${pad(now.getMinutes())}${pad(now.getSeconds())}`
  return `${slug}-${stamp}`.slice(0, 80)
}

function buildLabel(build: Machine["controller_build"]) {
  const commit = build.git_commit === "unknown" ? build.git_commit : build.git_commit.slice(0, 8)
  return `${build.version}@${commit}`
}

function supervisionLabel(mode: NonNullable<Machine["supervision"]>["mode"]) {
  if (mode === "systemd_user") return "systemd user"
  if (mode === "launchd") return "LaunchAgent"
  if (mode === "nohup") return "nohup"
  return "foreground"
}

function authorizationReturnUrl() {
  const value = new URLSearchParams(window.location.search).get("return_to")
  if (!value) return null
  try {
    const candidate = new URL(value)
    const proxy = new URL(proxyUrl("/"))
    const allowedPaths = new Set(["/.treer/ingress/authorize", "/api/apps/oauth/authorize"])
    return candidate.origin === proxy.origin && allowedPaths.has(candidate.pathname) ? candidate.toString() : null
  } catch {
    return null
  }
}

function machineOnline(machine?: Machine) {
  return machine?.status === "online"
}

function machineListen(machine?: Machine) {
  return machine?.labels?.["treer.listen"]
}

function machineDisplayName(machine: Machine, machines: Machine[] = []) {
  const name = machineName(machine)
  const hostname = machine.hostname
  if (!hostname) return name
  const collisions = machines.filter((item) => item.hostname === hostname)
  if (collisions.length < 2) return name
  const suffix = machine.server_id.replace(/^srv_/, "").slice(0, 6)
  const port = machineListen(machine)?.split(":").pop()
  return port ? `${name} · ${suffix} :${port}` : `${name} · ${suffix}`
}

function machineRecoveryCommands(workspaceId: string) {
  return {
    start: `treer-agent-server service --workspace ${workspaceId} start`,
    restartController: `treer-agent-server service --workspace ${workspaceId} restart-controller`,
  }
}

function MachineRecovery({ workspaceId, onCopy, reason }: { workspaceId: string; onCopy: (value: string) => void; reason?: string }) {
  const commands = machineRecoveryCommands(workspaceId)
  return <div className="mt-3 space-y-2 text-left">
    {reason && <p className="text-xs leading-5 text-muted-foreground">{reason}</p>}
    <p className="text-[10px] uppercase text-muted-foreground">On that machine</p>
    <div className="space-y-2">
      <div className="rounded-md border bg-muted/30 p-2">
        <code className="block break-all font-mono text-[10px] leading-5">{commands.restartController}</code>
        <Button size="sm" variant="outline" className="mt-2 h-7" onClick={() => onCopy(commands.restartController)}><Copy />Copy restart-controller</Button>
      </div>
      <div className="rounded-md border bg-muted/30 p-2">
        <code className="block break-all font-mono text-[10px] leading-5">{commands.start}</code>
        <Button size="sm" variant="outline" className="mt-2 h-7" onClick={() => onCopy(commands.start)}><Copy />Copy start</Button>
      </div>
    </div>
  </div>
}

function agentDisplayStatus(agent: Agent, machine?: Machine) {
  return machineOnline(machine) ? agent.status : "offline"
}

function Status({ value }: { value: string }) {
  return <span className={cn("inline-flex shrink-0 items-center gap-1.5 text-[10px] font-medium capitalize text-zinc-500", value === "idle" && "text-emerald-700", ["working", "starting"].includes(value) && "text-sky-700", value === "blocked" && "text-amber-700", ["failed", "exited", "offline"].includes(value) && "text-red-600")}><span className="size-1.5 rounded-full bg-current opacity-75" />{value}</span>
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
      const returnTo = authorizationReturnUrl()
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

  return <main className="grid min-h-dvh place-items-center bg-sidebar p-4">
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

const PREVIEW_ADMIN_INVITE = "https://treer.example/?invite=inv_preview"
const EMPTY_ADMIN_DASHBOARD: AdminDashboard = { user_count: 0, organization_count: 0, machine_count: 0, agent_count: 0 }
type AdminInventory = "users" | "machines" | "agents" | "organizations" | "invitations" | "activity" | null

function AdminPanel() {
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
        else setError(reason instanceof Error ? reason.message : "Unable to load admin panel")
      })
  }, [preview, loadDashboard])

  async function login(event: FormEvent) {
    event.preventDefault(); setSubmitting(true); setError("")
    try {
      await api("/api/admin/login", { method: "POST", body: JSON.stringify({ password }) })
      setPassword(""); setAuthenticated(true); await loadDashboard()
    } catch (reason) { setError(reason instanceof Error ? reason.message : "Authentication failed") }
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
    } catch (reason) { setError(reason instanceof Error ? reason.message : "Unable to create invitation") }
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

  if (authenticated === undefined) return <div className="grid min-h-dvh place-items-center bg-sidebar text-sm text-muted-foreground">Loading admin...</div>
  if (!authenticated) return <main className="grid min-h-dvh place-items-center bg-sidebar p-4"><form onSubmit={login} className="w-full max-w-[390px] rounded-lg border bg-background p-7 shadow-sm"><div className="mb-6 grid size-9 place-items-center rounded-md bg-[#37352f] text-white"><ShieldCheck className="size-4" /></div><h1 className="text-xl font-semibold">Treer administration</h1><p className="mt-1 text-sm text-muted-foreground">Platform access is separate from user accounts.</p><div className="mt-6 space-y-2"><Label htmlFor="admin-password">Admin password</Label><Input id="admin-password" type="password" autoComplete="current-password" value={password} onChange={(event) => setPassword(event.target.value)} required autoFocus /></div><div className="mt-3 min-h-5 text-xs text-destructive">{error}</div><div className="mt-4 flex justify-end"><Button type="submit" disabled={submitting}>{submitting ? "Please wait" : "Open admin panel"}</Button></div></form></main>

  return <main className="min-h-dvh bg-sidebar">
    {preview && <div className="treer-tour-banner"><span>Tour preview · admin panel, no server writes</span></div>}
    <header className="border-b bg-background"><div className="mx-auto flex h-14 w-full max-w-4xl items-center justify-between px-5"><div className="flex min-w-0 items-center gap-2.5 text-sm font-semibold"><span className="grid size-7 shrink-0 place-items-center rounded bg-[#37352f] text-white"><ShieldCheck className="size-3.5" /></span><span className="truncate">Treer administration</span></div><div className="flex shrink-0 items-center gap-1"><Button variant="ghost" size="sm" asChild><a href="/" data-tour="admin-workspace-link">User workspace</a></Button><Button size="icon" variant="ghost" aria-label="Log out" onClick={logout} disabled={preview}><LogOut /></Button></div></div></header>
    <div className="mx-auto max-w-4xl px-5 py-10">
      <div className="mb-8 flex flex-col items-start gap-4 sm:flex-row sm:items-end sm:justify-between">
        <div><h1 className="text-2xl font-semibold">Platform overview</h1><p className="mt-1 text-sm text-muted-foreground">Current resources across all organizations.</p></div>
        <div className="flex gap-1"><Button size="sm" variant="outline" onClick={replayAdminTour}><ListChecks />Replay tour</Button><Button size="sm" onClick={loadDashboard} disabled={preview}><RotateCw />Refresh</Button></div>
      </div>
      {error && <div className="mb-5 rounded border border-destructive/30 bg-destructive/5 px-3 py-2 text-sm text-destructive">{error}</div>}
      <div className="grid grid-cols-3 border-y" data-tour="admin-overview">
        <AdminCountCard label="Users" icon={<Users className="size-3.5" />} value={dashboard?.user_count} active={inventory === "users"} onClick={() => setInventory(inventory === "users" ? null : "users")} />
        <AdminCountCard label="Machines" icon={<Server className="size-3.5" />} value={dashboard?.machine_count} active={inventory === "machines"} bordered onClick={() => setInventory(inventory === "machines" ? null : "machines")} />
        <AdminCountCard label="Agents" icon={<TerminalSquare className="size-3.5" />} value={dashboard?.agent_count} active={inventory === "agents"} onClick={() => setInventory(inventory === "agents" ? null : "agents")} />
      </div>
      {inventory && <AdminInventoryPanel kind={inventory} preview={preview} onError={setError} onChanged={loadDashboard} />}
      <ControlPlaneUpdate preview={preview} onError={setError} />
      <section className="mt-12" data-tour="admin-invite">
        <h2 className="text-sm font-semibold">User invitations</h2>
        <div className="mt-3 grid gap-4 border-y py-4 sm:grid-cols-[minmax(0,1fr)_auto] sm:items-center">
          <div><div className="text-sm font-medium">Invite a new user</div><div className="mt-1 text-xs text-muted-foreground">Registration creates a personal organization owned by that user.</div></div>
          <div className="flex flex-wrap gap-2">
            <Button size="sm" variant={inventory === "invitations" ? "secondary" : "outline"} onClick={() => setInventory(inventory === "invitations" ? null : "invitations")}>Pending invitations</Button>
            <Button size="sm" onClick={createInvite}><KeyRound />Create invitation</Button>
          </div>
        </div>
      </section>
      <section className="mt-12">
        <h2 className="text-sm font-semibold">Organizations</h2>
        <div className="mt-3 grid gap-4 border-y py-4 sm:grid-cols-[minmax(0,1fr)_auto] sm:items-center">
          <div><div className="text-sm font-medium">All organizations</div><div className="mt-1 text-xs text-muted-foreground">{dashboard?.organization_count ?? "—"} personal and shared organizations on this deployment.</div></div>
          <Button size="sm" variant={inventory === "organizations" ? "secondary" : "outline"} onClick={() => setInventory(inventory === "organizations" ? null : "organizations")}><Building2 />View organizations</Button>
        </div>
      </section>
      <section className="mt-12">
        <h2 className="text-sm font-semibold">Admin activity</h2>
        <div className="mt-3 grid gap-4 border-y py-4 sm:grid-cols-[minmax(0,1fr)_auto] sm:items-center">
          <div><div className="text-sm font-medium">Recent operator actions</div><div className="mt-1 text-xs text-muted-foreground">Password resets, session revokes, and invitation changes.</div></div>
          <Button size="sm" variant={inventory === "activity" ? "secondary" : "outline"} onClick={() => setInventory(inventory === "activity" ? null : "activity")}><ScrollText />View activity</Button>
        </div>
      </section>
    </div>
    <Dialog open={Boolean(inviteUrl)} onOpenChange={(open) => !open && setInviteUrl("")}><DialogContent data-tour="admin-invite-dialog"><DialogHeader><DialogTitle>User invitation</DialogTitle><DialogDescription>This one-time registration link creates the user's personal organization.</DialogDescription></DialogHeader><Textarea readOnly value={inviteUrl} className="min-h-24 font-mono text-xs" /><DialogFooter><Button variant="outline" onClick={() => setInviteUrl("")}>Close</Button><Button onClick={() => navigator.clipboard.writeText(inviteUrl)}><Copy />Copy link</Button></DialogFooter></DialogContent></Dialog>
  </main>
}

function AdminCountCard({ label, icon, value, active, bordered, onClick }: { label: string; icon: React.ReactNode; value?: number; active: boolean; bordered?: boolean; onClick: () => void }) {
  return <button type="button" onClick={onClick} className={cn("py-6 text-left transition-colors hover:bg-accent/40", bordered && "border-x px-6", !bordered && "px-0", active && "bg-accent/50")}>
    <div className="flex items-center gap-2 text-xs text-muted-foreground">{icon}{label}</div>
    <div className="mt-2 text-3xl font-semibold tabular-nums">{value ?? "-"}</div>
  </button>
}

function AdminInventoryPanel({ kind, preview, onError, onChanged }: { kind: Exclude<AdminInventory, null>; preview: boolean; onError: (message: string) => void; onChanged: () => Promise<void> }) {
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
      onError(reason instanceof Error ? reason.message : "Unable to load admin inventory")
    } finally {
      setLoading(false)
    }
  }, [kind, preview, query, onError])

  useEffect(() => { void load() }, [load])

  async function openUser(userId: string) {
    try { setUserDetail((await api<{ user: AdminUserDetail }>(`/api/admin/users/${userId}`)).user) }
    catch (reason) { onError(reason instanceof Error ? reason.message : "Unable to load user") }
  }

  async function resetPassword(userId: string) {
    try {
      const data = await api<{ url: string; emailed: boolean }>(`/api/admin/users/${userId}/password-reset`, { method: "POST", body: "{}" })
      setResetUrl(data.url)
      setResetEmailed(data.emailed)
      await load()
    } catch (reason) { onError(reason instanceof Error ? reason.message : "Unable to issue password reset") }
  }

  async function revokeSessions(userId: string) {
    try {
      await api(`/api/admin/users/${userId}/revoke-sessions`, { method: "POST", body: "{}" })
      await load()
    } catch (reason) { onError(reason instanceof Error ? reason.message : "Unable to sign the user out") }
  }

  async function toggleMachine(serverId: string) {
    if (openMachine === serverId) { setOpenMachine(null); setMachineDetail(null); return }
    setOpenMachine(serverId)
    try { setMachineDetail((await api<{ machine: AdminMachine }>(`/api/admin/machines/${serverId}`)).machine) }
    catch (reason) { onError(reason instanceof Error ? reason.message : "Unable to load machine") }
  }

  async function revokeInvite(token: string) {
    try {
      await api(`/api/admin/invitations/${encodeURIComponent(token)}`, { method: "DELETE" })
      await load()
      await onChanged()
    } catch (reason) { onError(reason instanceof Error ? reason.message : "Unable to revoke invitation") }
  }

  const title = kind === "users" ? "Users" : kind === "machines" ? "Machines" : kind === "agents" ? "Agents" : kind === "organizations" ? "Organizations" : kind === "invitations" ? "Pending invitations" : "Admin activity"

  return <div className="border-b">
    <div className="flex items-center justify-between gap-3 py-3">
      <div className="text-sm font-medium">{title}</div>
      {kind === "users" && <Input value={query} onChange={(event) => setQuery(event.target.value)} placeholder="Search name or email" className="h-8 max-w-xs text-xs" />}
    </div>
    {loading && <div className="py-6 text-xs text-muted-foreground">Loading…</div>}
    {!loading && kind === "users" && (users.length ? users.map((user) => (
      <div key={user.user_id} className="grid gap-3 border-t py-3 sm:grid-cols-[minmax(0,1fr)_auto] sm:items-center">
        <div className="min-w-0"><div className="truncate text-sm font-medium">{user.preferred_name}</div><div className="mt-0.5 truncate text-[11px] text-muted-foreground">{user.email}</div></div>
        <div className="flex flex-wrap gap-1">
          <Button size="sm" variant="outline" onClick={() => void openUser(user.user_id)}>Details</Button>
          <Button size="sm" variant="outline" onClick={() => void resetPassword(user.user_id)}>Reset password</Button>
          <Button size="sm" variant="outline" onClick={() => void revokeSessions(user.user_id)}>Sign out everywhere</Button>
        </div>
      </div>
    )) : <EmptyState icon={<Users />} label="No users match this search" />)}
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
              <span className="min-w-0 truncate">{agent.name}<span className="ml-2 text-[11px] text-muted-foreground">{agent.kind}{agent.interface ? " · AIS" : ""}</span></span>
              <Status value={agent.status} />
            </div>
          )) : <div className="py-2 text-[11px] text-muted-foreground">No live agents on this machine.</div>}
        </div>}
      </div>
    )) : <EmptyState icon={<Server />} label="No enrolled machines" />)}
    {!loading && kind === "agents" && (agents.length ? Object.entries(agents.reduce<Record<string, Agent[]>>((groups, agent) => {
      const key = agent.server_id
      groups[key] = groups[key] ? [...groups[key], agent] : [agent]
      return groups
    }, {})).map(([serverId, group]) => (
      <div key={serverId} className="border-t py-3">
        <div className="text-[11px] text-muted-foreground">{serverId}</div>
        {group.map((agent) => (
          <div key={agent.agent_id} className="mt-1 flex items-center justify-between gap-3 text-sm">
            <span className="truncate">{agent.name}<span className="ml-2 text-[11px] text-muted-foreground">{agent.kind}{agent.interface ? " · AIS" : ""}</span></span>
            <Status value={agent.status} />
          </div>
        ))}
      </div>
    )) : <EmptyState icon={<TerminalSquare />} label="No live agents" />)}
    {!loading && kind === "organizations" && (organizations.length ? organizations.map((organization) => (
      <div key={organization.organization_id} className="grid gap-1 border-t py-3 sm:grid-cols-[minmax(0,1fr)_auto] sm:items-center">
        <div><div className="text-sm font-medium">{organization.name}</div><div className="mt-0.5 text-[11px] text-muted-foreground">{organization.owner_email || "No owner"} · {organization.workspace_count} workspaces · {organization.machine_count} machines</div></div>
      </div>
    )) : <EmptyState icon={<Building2 />} label="No organizations" />)}
    {!loading && kind === "invitations" && (invitations.length ? invitations.map((invitation) => (
      <div key={invitation.token} className="grid gap-3 border-t py-3 sm:grid-cols-[minmax(0,1fr)_auto] sm:items-center">
        <div className="min-w-0 font-mono text-[11px] text-muted-foreground">{invitation.created_at}</div>
        <div className="flex gap-1">
          <Button size="sm" variant="outline" onClick={() => navigator.clipboard.writeText(invitation.url)}><Copy />Copy</Button>
          <Button size="sm" variant="outline" onClick={() => void revokeInvite(invitation.token)}>Revoke</Button>
        </div>
      </div>
    )) : <EmptyState icon={<KeyRound />} label="No pending invitations" />)}
    {!loading && kind === "activity" && (events.length ? events.map((event) => (
      <div key={event.event_id} className="border-t py-3 text-sm">
        <div className="font-medium">{event.action}</div>
        <div className="mt-0.5 text-[11px] text-muted-foreground">{event.occurred_at} · {event.resource_kind} {event.resource_name || event.resource_id}</div>
      </div>
    )) : <EmptyState icon={<ScrollText />} label="No admin activity yet" />)}
    <Dialog open={Boolean(userDetail)} onOpenChange={(open) => !open && setUserDetail(null)}>
      <DialogContent className="max-w-lg">
        <DialogHeader><DialogTitle>{userDetail?.preferred_name}</DialogTitle><DialogDescription>{userDetail?.email}</DialogDescription></DialogHeader>
        {userDetail && <div className="space-y-3 text-sm">
          <div className="text-xs text-muted-foreground">Verified {userDetail.email_verified ? "yes" : "no"} · OAuth {userDetail.oauth_providers.join(", ") || "none"}</div>
          <div>
            <div className="text-xs font-medium text-muted-foreground">Organizations</div>
            {userDetail.organizations.map((organization) => <div key={organization.organization_id}>{organization.name} · {organization.role}</div>)}
          </div>
          <div>
            <div className="text-xs font-medium text-muted-foreground">Workspaces</div>
            {userDetail.workspaces.map((workspace) => <div key={workspace.workspace_id}>{workspace.name}</div>)}
            {userDetail.workspaces.length === 0 && <div className="text-muted-foreground">None</div>}
          </div>
        </div>}
      </DialogContent>
    </Dialog>
    <Dialog open={Boolean(resetUrl)} onOpenChange={(open) => !open && setResetUrl("")}>
      <DialogContent>
        <DialogHeader><DialogTitle>Password reset</DialogTitle><DialogDescription>{resetEmailed ? "A reset email was sent. The link also works if you copy it." : "Email sending is not configured. Copy this one-time link."}</DialogDescription></DialogHeader>
        <Textarea readOnly value={resetUrl} className="min-h-24 font-mono text-xs" />
        <DialogFooter><Button variant="outline" onClick={() => setResetUrl("")}>Close</Button><Button onClick={() => navigator.clipboard.writeText(resetUrl)}><Copy />Copy link</Button></DialogFooter>
      </DialogContent>
    </Dialog>
  </div>
}

function ControlPlaneUpdate({ preview, onError }: { preview?: boolean; onError: (message: string) => void }) {
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
    loadStatus().catch((reason) => onError(reason instanceof Error ? reason.message : "Unable to load control-plane status"))
  }, [preview, loadStatus, onError])

  async function checkForUpdates() {
    setBusy(true)
    try {
      const next = await api<ControlPlaneUpdateStatus>("/api/admin/update/check")
      setConfigured(true)
      setStatus(next)
    } catch (reason) {
      onError(reason instanceof Error ? reason.message : "Unable to check for updates")
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
            onError(next.job.error || "Control-plane update failed")
            return
          }
          return
        } catch {
          // Proxy and updater bounce while Compose recreates them.
        }
      }
      onError("The update is still running. Refresh this page after the control plane returns.")
    } catch (reason) {
      onError(reason instanceof Error ? reason.message : "Unable to apply the update")
    } finally {
      setBusy(false)
    }
  }

  if (configured === null) return null
  if (!configured) {
    return <section className="mt-12">
      <h2 className="text-sm font-semibold">Control plane</h2>
      <p className="mt-3 max-w-2xl text-sm leading-6 text-muted-foreground">Control-plane updates are not configured on this deployment. Hosted Railway uses its own release promotion. Self-hosted Compose exposes this panel through the updater sidecar.</p>
    </section>
  }

  const jobRunning = status?.job?.state === "running" || busy
  const digestLabel = (value?: string | null) => value ? value.replace(/^sha256:/, "").slice(0, 12) : "unknown"

  return <section className="mt-12">
    <h2 className="text-sm font-semibold">Control plane</h2>
    <p className="mt-1 text-xs text-muted-foreground">Updates pull immutable GHCR tags for Proxy, App, and the updater sidecar. Enrolled machines still run <span className="font-mono">treer-agent-server update</span> on each host.</p>
    <div className="mt-3 grid gap-4 border-y py-4 sm:grid-cols-[minmax(0,1fr)_auto] sm:items-start">
      <div>
        <div className="text-sm font-medium">Channel {status?.channel ?? "—"}</div>
        <div className="mt-2 space-y-1">
          {(status?.services ?? []).map((service) => (
            <div key={service.name} className="flex flex-wrap items-baseline gap-x-3 text-xs">
              <span className="w-16 font-medium">{service.name}</span>
              <span className="text-muted-foreground">{service.present ? service.version || "running" : "missing"}</span>
              <span className="font-mono text-[10px] text-muted-foreground">{digestLabel(service.digest)}</span>
              {service.update_available && <span className="text-emerald-700">update available</span>}
            </div>
          ))}
        </div>
        {status?.job?.state === "failed" && <div className="mt-2 text-xs text-destructive">{status.job.error}</div>}
        {jobRunning && <div className="mt-2 text-xs text-muted-foreground">Applying images. This page may briefly lose the Proxy connection.</div>}
      </div>
      <div className="flex flex-wrap gap-2">
        <Button size="sm" variant="outline" onClick={() => { void checkForUpdates() }} disabled={jobRunning}><RotateCw />Check for updates</Button>
        <Button size="sm" onClick={() => { void applyUpdate() }} disabled={jobRunning || !(status?.update_available || (status?.services ?? []).some((service) => service.update_available))}><Download />Apply update</Button>
      </div>
    </div>
  </section>
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
  "workspace.renamed": "renamed a workspace",
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
  const totalBytes = traffic.reduce((sum, item) => sum + (item.billable_bytes ?? item.payload_bytes), 0)
  const totalFrames = traffic.reduce((sum, item) => sum + item.payload_frames, 0)
  const routes = Array.from(traffic.reduce((items, item) => {
    const trafficClass = item.traffic_class ?? "virtual_network"
    const sourceType = item.source_type ?? "machine"
    const destinationType = item.destination_type ?? "machine"
    const key = `${trafficClass}\u0000${sourceType}\u0000${item.source_server_id}\u0000${destinationType}\u0000${item.destination_server_id}`
    const current = items.get(key) ?? { trafficClass, sourceType, source: item.source_server_id, destinationType, destination: item.destination_server_id, bytes: 0, frames: 0 }
    current.bytes += item.billable_bytes ?? item.payload_bytes
    current.frames += item.payload_frames
    items.set(key, current)
    return items
  }, new Map<string, { trafficClass: NonNullable<MachineTrafficRecord["traffic_class"]>; sourceType: NonNullable<MachineTrafficRecord["source_type"]>; source: string; destinationType: NonNullable<MachineTrafficRecord["destination_type"]>; destination: string; bytes: number; frames: number }>()).values()).sort((left, right) => right.bytes - left.bytes)
  const resolveEndpoint = (type: NonNullable<MachineTrafficRecord["source_type"]>, serverId: string) => type === "client" || serverId === "browser"
    ? "Browser / ingress"
    : machineName(machines.find((machine) => machine.server_id === serverId), serverId)
  const trafficClassLabel: Record<NonNullable<MachineTrafficRecord["traffic_class"]>, string> = { virtual_network: "Machine", service_ingress: "Ingress", virtual_host: "Virtual host", agent_interface: "Agent UI" }

  return <div className="min-h-0 overflow-auto"><div className="mx-auto w-full max-w-[1120px] px-5 py-8 sm:px-8 sm:py-12 lg:px-14">
    <div className="mb-8 flex items-end justify-between gap-4"><div><div className="mb-2 grid size-9 place-items-center rounded-md bg-[#f8d9df] text-[#8b4452]"><ScrollText className="size-4" /></div><h1 className="text-2xl font-semibold">Audit</h1></div><span className="text-xs text-muted-foreground">Organization activity</span></div>
    <section className="mb-11"><div className="mb-3 flex items-center justify-between"><h2 className="text-sm font-semibold">Traffic</h2><span className="text-[10px] uppercase text-muted-foreground">Last 24 hours</span></div><div className="grid grid-cols-3 border-y">
      <div className="py-5 pr-3"><div className="text-[10px] text-muted-foreground">Billable usage</div><div className="mt-1 text-xl font-semibold tabular-nums sm:text-2xl">{formatBytes(totalBytes)}</div></div>
      <div className="border-x px-3 py-5 sm:px-6"><div className="text-[10px] text-muted-foreground">Data frames</div><div className="mt-1 text-xl font-semibold tabular-nums sm:text-2xl">{totalFrames.toLocaleString()}</div></div>
      <div className="py-5 pl-3 sm:pl-6"><div className="text-[10px] text-muted-foreground">Routes</div><div className="mt-1 text-xl font-semibold tabular-nums sm:text-2xl">{routes.length}</div></div>
    </div>{routes.length > 0 && <div className="mt-3 divide-y border-b">{routes.slice(0, 5).map((route) => <div key={`${route.trafficClass}:${route.source}:${route.destination}`} className="grid min-h-10 grid-cols-[minmax(0,1fr)_auto] items-center gap-4 text-xs"><span className="flex min-w-0 items-center gap-2"><span className="w-16 shrink-0 text-[9px] uppercase text-muted-foreground">{trafficClassLabel[route.trafficClass]}</span><span className="truncate">{resolveEndpoint(route.sourceType, route.source)}</span><ArrowRight className="size-3 shrink-0 text-muted-foreground" /><span className="truncate">{resolveEndpoint(route.destinationType, route.destination)}</span></span><span className="font-mono text-[10px] text-muted-foreground">{formatBytes(route.bytes)}</span></div>)}</div>}</section>
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

function AppsView({ apps, machines, loading, onOpen, onAction, onDelete }: { apps: AppDeployment[]; machines: Machine[]; loading: boolean; onOpen: (app: AppDeployment) => void; onAction: (app: AppDeployment, action: "start" | "stop" | "restart") => void; onDelete: (app: AppDeployment) => void }) {
  return <div className="min-h-0 overflow-auto"><div className="mx-auto w-full max-w-[1120px] px-5 py-8 sm:px-8 sm:py-12 lg:px-14">
    <div className="mb-8 flex items-end justify-between gap-4"><div><div className="mb-2 grid size-9 place-items-center rounded-md bg-emerald-100 text-emerald-800"><PanelsTopLeft className="size-4" /></div><h1 className="text-2xl font-semibold">Apps</h1></div><span className="text-xs text-muted-foreground">{apps.length} deployments</span></div>
    <div className="border-y">
      <div className="hidden h-9 grid-cols-[minmax(150px,1fr)_minmax(220px,1.3fr)_minmax(150px,1fr)_auto] items-center gap-4 border-b text-[10px] font-medium uppercase text-muted-foreground sm:grid"><span>App</span><span>URL</span><span>Machine</span><span className="w-36" /></div>
      {apps.map((app) => { const machine = machines.find((item) => item.server_id === app.server_id); const interfaceBase = app.public_url?.replace(/\/$/, "") ?? app.hostname; return <div key={app.app_id} className="grid min-h-[72px] grid-cols-[minmax(0,1fr)_auto] items-center gap-x-3 gap-y-1 border-b py-3 last:border-b-0 sm:grid-cols-[minmax(150px,1fr)_minmax(220px,1.3fr)_minmax(150px,1fr)_auto] sm:gap-4">
        <div className="col-start-1 row-start-1 min-w-0 sm:col-start-auto sm:row-start-auto"><div className="flex items-center gap-2"><span className="truncate text-xs font-medium">{app.name}</span><Status value={app.status} /></div><div className="mt-1 truncate font-mono text-[9px] text-muted-foreground" title={formatCommandLine(app.command, app.args)}>{formatCommandLine(app.command, app.args)}</div>{app.last_error && <div className="mt-1 truncate text-[9px] text-red-600" title={app.last_error}>{app.last_error}</div>}</div>
        <button type="button" className="col-start-1 row-start-2 min-w-0 truncate text-left font-mono text-[10px] hover:underline sm:col-start-auto sm:row-start-auto" aria-label={`Open ${app.name} interface`} onClick={() => onOpen(app)}>{interfaceBase}/</button>
        <span className="col-start-1 row-start-3 min-w-0 truncate text-[10px] text-muted-foreground sm:col-start-auto sm:row-start-auto">{machineName(machine, app.server_id)} · restarts {app.restart_count}</span>
        <span className="col-start-2 row-span-3 row-start-1 flex items-center justify-end gap-1 sm:col-start-auto sm:row-span-1 sm:row-start-auto"><IconButton label={`Open ${app.name}`} onClick={() => onOpen(app)} disabled={app.status !== "running"}><ExternalLink /></IconButton>{app.desired_state === "stopped" ? <IconButton label={`Start ${app.name}`} onClick={() => onAction(app, "start")} disabled={machine?.status !== "online"}><Play /></IconButton> : <IconButton label={`Stop ${app.name}`} onClick={() => onAction(app, "stop")} disabled={machine?.status !== "online"}><Square /></IconButton>}<IconButton label={`Restart ${app.name}`} onClick={() => onAction(app, "restart")} disabled={machine?.status !== "online"}><RotateCw /></IconButton><IconButton label={`Delete ${app.name}`} className="text-destructive hover:text-destructive" onClick={() => onDelete(app)}><Trash2 /></IconButton></span>
      </div>})}
      {!apps.length && <EmptyState icon={<PanelsTopLeft />} label={loading ? "Loading Apps" : "No Apps yet"} />}
    </div>
  </div></div>
}

function WorkspaceSettingsView({ workspace, organization, name, machineCount, agentCount, canDelete, preview, onNameChange, onRename, onDelete, onClose }: { workspace?: Workspace; organization?: Organization; name: string; machineCount?: number; agentCount?: number; canDelete: boolean; preview: boolean; onNameChange: (value: string) => void; onRename: (event: FormEvent) => void; onDelete: () => void; onClose: () => void }) {
  if (!workspace) return <div className="grid min-h-0 flex-1 place-items-center p-8 text-sm text-muted-foreground">Workspace not found. <Button variant="outline" className="mt-3" onClick={onClose}>Back</Button></div>
  const deleteBlocked = machineCount === undefined || machineCount > 0

  return <div className="min-h-0 overflow-auto"><div className="mx-auto w-full max-w-[960px] px-5 py-8 sm:px-8 sm:py-12 lg:px-14">
    <div className="mb-10 flex items-start justify-between gap-4">
      <div className="min-w-0">
        <div className="mb-3 flex items-center gap-3"><span className="grid size-9 shrink-0 place-items-center rounded-md bg-[#e8deee] text-[#694a73]"><SettingsIcon className="size-4" /></span><h1 className="truncate text-2xl font-semibold">{workspace.name}</h1></div>
        <p className="font-mono text-xs text-muted-foreground">{workspace.workspace_id}</p>
        {organization && <p className="mt-1 text-xs text-muted-foreground">{organization.name}</p>}
      </div>
      <Button variant="outline" className="shrink-0" onClick={onClose}>Close</Button>
    </div>

    <section className="border-y py-6">
      <div className="grid gap-5 md:grid-cols-[220px_minmax(0,1fr)]">
        <div><h2 className="text-sm font-semibold">General</h2><p className="mt-1 text-xs leading-5 text-muted-foreground">The display name shown to organization members.</p></div>
        <form onSubmit={onRename} className="grid gap-2 sm:grid-cols-[minmax(0,1fr)_auto] sm:items-end"><div className="min-w-0"><Field label="Workspace name"><Input aria-label="Workspace name" value={name} onChange={(event) => onNameChange(event.target.value)} required maxLength={80} /></Field></div><Button type="submit" className="w-full sm:w-auto" disabled={!name.trim() || name.trim() === workspace.name}>Save</Button></form>
      </div>
    </section>

    <section className="border-b py-6">
      <div className="grid gap-5 md:grid-cols-[220px_minmax(0,1fr)]">
        <div><h2 className="text-sm font-semibold">Inventory</h2><p className="mt-1 text-xs leading-5 text-muted-foreground">Resources currently attached to this workspace.</p></div>
        <dl className="grid grid-cols-2 border-y text-xs"><div className="py-4 pr-4"><dt className="text-muted-foreground">Machines</dt><dd className="mt-1 text-2xl font-semibold tabular-nums">{machineCount ?? "-"}</dd></div><div className="border-l py-4 pl-4"><dt className="text-muted-foreground">Agents</dt><dd className="mt-1 text-2xl font-semibold tabular-nums">{agentCount ?? "-"}</dd></div></dl>
      </div>
    </section>

    {canDelete && <section className="border-b py-6">
      <div className="grid gap-5 md:grid-cols-[220px_minmax(0,1fr)]">
        <div><h2 className="text-sm font-semibold text-destructive">Danger zone</h2><p className="mt-1 text-xs leading-5 text-muted-foreground">Deleted workspaces disappear from active views while historical traffic and messages remain.</p></div>
        <div className="grid gap-3 border border-destructive/30 p-4 sm:grid-cols-[minmax(0,1fr)_auto] sm:items-center"><p className="text-xs leading-5 text-muted-foreground">{machineCount === undefined ? "Checking machine inventory..." : machineCount > 0 ? `Delete all ${machineCount} ${machineCount === 1 ? "machine" : "machines"} before deleting this workspace.` : "This action cannot be undone."}</p><Button variant="destructive" className="w-full sm:w-auto" disabled={preview || deleteBlocked} onClick={onDelete}><Trash2 />Delete workspace</Button></div>
      </div>
    </section>}
  </div></div>
}

function WorkspaceApp() {
  const route = useParams<{ organizationId?: string; workspaceId?: string }>()
  const navigate = useNavigate()
  const location = useLocation()
  const routeSelectionRef = useRef({ organizationId: route.organizationId ?? null, workspaceId: route.workspaceId ?? null })
  routeSelectionRef.current = { organizationId: route.organizationId ?? null, workspaceId: route.workspaceId ?? null }
  const replaceWorkspaceRoute = useCallback((organizationId: string | null, workspaceId: string | null) => {
    navigate({ pathname: workspacePath(organizationId, workspaceId), search: location.search, hash: location.hash }, { replace: true })
  }, [navigate, location.search, location.hash])
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
  const [sidebarTab, setSidebarTab] = useState<SidebarTab>("agents")
  const [selectedAgentId, setSelectedAgentId] = useState<string | null>(null)
  const [selectedMachineId, setSelectedMachineId] = useState<string | null>(null)
  const [connection, setConnection] = useState<ConnectionState>("connecting")
  const [terminalStatus, setTerminalStatus] = useState<TerminalState>("not attached")
  const [interfaceUiRevision, setInterfaceUiRevision] = useState(0)
  const [mainView, setMainView] = useState<MainView>("terminal")
  const [settingsOpen, setSettingsOpen] = useState(false)
  const [mobileTerminalOpen, setMobileTerminalOpen] = useState(false)
  const [isMobile, setIsMobile] = useState(() => window.matchMedia("(max-width: 767px)").matches)
  const [ctrlArmed, setCtrlArmed] = useState(false)
  const terminalPaneRef = useRef<TerminalPaneHandle>(null)
  const ctrlArmedRef = useRef(false)
  const [error, setError] = useState<string | null>(null)
  const [createOrganizationOpen, setCreateOrganizationOpen] = useState(false)
  const [createWorkspaceOpen, setCreateWorkspaceOpen] = useState(false)
  const [deleteWorkspaceOpen, setDeleteWorkspaceOpen] = useState(false)
  const [createAgentOpen, setCreateAgentOpen] = useState(false)
  const [creatingAgent, setCreatingAgent] = useState(false)
  const [installingAgentKind, setInstallingAgentKind] = useState<string | null>(null)
  const [createAppOpen, setCreateAppOpen] = useState(false)
  const [deletingApp, setDeletingApp] = useState<AppDeployment | null>(null)
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
  const [renameOrganizationOpen, setRenameOrganizationOpen] = useState(false)
  const [renameTarget, setRenameTarget] = useState<RenameTarget>(null)
  const [deleteTarget, setDeleteTarget] = useState<DeleteTarget>(null)
  const [organizationName, setOrganizationName] = useState("")
  const [createWorkspaceName, setCreateWorkspaceName] = useState("")
  const [workspaceName, setWorkspaceName] = useState("")
  const [agentName, setAgentName] = useState(defaultAgentName("terminal"))
  const [agentNameCustomized, setAgentNameCustomized] = useState(false)
  const [agentProfileId, setAgentProfileId] = useState("terminal")
  const [agentServerId, setAgentServerId] = useState("")
  const [agentCwd, setAgentCwd] = useState(".")
  const [agentCommandLine, setAgentCommandLine] = useState("codex")
  const [agentRecipeUrl, setAgentRecipeUrl] = useState("")
  const [agentRecipeKind, setAgentRecipeKind] = useState("")
  const [launchProfiles, setLaunchProfiles] = useState<AgentLaunchProfile[]>(preview ? PREVIEW_PROFILES : [])
  const [apps, setApps] = useState<AppDeployment[]>([])
  const [appsLoading, setAppsLoading] = useState(false)
  const [appName, setAppName] = useState("")
  const [appServerId, setAppServerId] = useState("")
  const [appCwd, setAppCwd] = useState(".")
  const [appCommandLine, setAppCommandLine] = useState("")
  const [appPort, setAppPort] = useState("")
  const [appHostname, setAppHostname] = useState("")
  const createAgentOpenRef = useRef(false)
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
  const [serviceTargetAgentId, setServiceTargetAgentId] = useState<string | null>(null)
  const [serviceTargetHost, setServiceTargetHost] = useState("127.0.0.1")
  const [serviceTargetPort, setServiceTargetPort] = useState("")
  const [serviceProtocol, setServiceProtocol] = useState<"tcp" | "http">("http")

  const showError = useCallback((reason: unknown) => setError(reason instanceof Error ? reason.message : "Something went wrong"), [])

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
      setConnection("no workspace")
      return
    }
    const data = await api<{ workspaces: Workspace[] }>(`/api/workspaces?organization_id=${encodeURIComponent(organizationId)}`)
    if (organizationIdRef.current !== organizationId) return
    setWorkspaces(data.workspaces)
    const routeSelection = routeSelectionRef.current
    const selected = routeSelection.organizationId === organizationId
      && routeSelection.workspaceId
      && data.workspaces.some((item) => item.workspace_id === routeSelection.workspaceId)
      ? routeSelection.workspaceId
      : data.workspaces[0]?.workspace_id ?? null
    workspaceIdRef.current = selected
    setWorkspaceId(selected)
    replaceWorkspaceRoute(organizationId, selected)
    if (!data.workspaces.length) setConnection("no workspace")
  }, [organizationId, preview, replaceWorkspaceRoute])

  useEffect(() => {
    if (preview) {
      setConnection("no workspace")
      return
    }
    workspaceIdRef.current = null
    setWorkspaceId(null)
    setSnapshot(null)
    void syncWorkspaces().catch(showError)
  }, [organizationId, preview, syncWorkspaces, showError])

  const refreshSnapshot = useCallback(async () => {
    if (preview || !workspaceId) return
    const requestedWorkspaceId = workspaceId
    const data = visibleWorkspaceSnapshot(await api<Snapshot>(`/api/workspaces/${encodeURIComponent(requestedWorkspaceId)}/snapshot`))
    if (workspaceIdRef.current !== requestedWorkspaceId || data.workspace.workspace_id !== requestedWorkspaceId) return
    setSnapshot(data)
    setWorkspaces((items) => replaceWorkspace(items, data.workspace))
  }, [preview, workspaceId])

  useEffect(() => {
    if (preview) return
    if (!workspaceId) {
      setSnapshot(null)
      setSelectedAgentId(null)
      setConnection("no workspace")
      return
    }
    let disposed = false
    let socket: WebSocket | null = null
    let timer: number | undefined
    setSnapshot((current) => current?.workspace.workspace_id === workspaceId ? current : null)
    refreshSnapshot().catch(showError)
    const connect = (initial = false) => {
      if (disposed || workspaceIdRef.current !== workspaceId) return
      if (initial) setConnection("connecting")
      socket = new WebSocket(websocketUrl(`/api/workspaces/${encodeURIComponent(workspaceId)}/events`))
      socket.onopen = () => { if (!disposed && workspaceIdRef.current === workspaceId) setConnection("live") }
      socket.onmessage = (event) => {
        if (disposed || workspaceIdRef.current !== workspaceId) return
        const message = JSON.parse(event.data) as { event: string; data?: Snapshot | Workspace }
        if (message.event === "workspace.snapshot" && message.data) {
          const next = visibleWorkspaceSnapshot(message.data as Snapshot)
          if (next.workspace.workspace_id !== workspaceId) return
          setSnapshot(next)
          setWorkspaces((items) => replaceWorkspace(items, next.workspace))
        } else if (message.event === "workspace.renamed" && message.data) {
          const updated = message.data as Workspace
          setWorkspaces((items) => replaceWorkspace(items, updated))
          setSnapshot((current) => current?.workspace.workspace_id === updated.workspace_id ? { ...current, workspace: updated } : current)
        } else if (message.event === "workspace.deleted" && message.data) {
          const removed = message.data as Workspace
          setWorkspaces((items) => items.filter((item) => item.workspace_id !== removed.workspace_id))
          if (removed.workspace_id === workspaceId) {
            workspaceIdRef.current = null
            setSnapshot(null)
            setSelectedAgentId(null)
            setSelectedMachineId(null)
            void syncWorkspaces().catch(showError)
          }
        }
        else refreshSnapshot().catch(showError)
      }
      socket.onclose = () => {
        if (disposed || workspaceIdRef.current !== workspaceId) return
        setConnection("reconnecting")
        timer = window.setTimeout(() => connect(false), 1200)
      }
    }
    connect(true)
    return () => { disposed = true; window.clearTimeout(timer); socket?.close() }
  }, [preview, workspaceId, refreshSnapshot, syncWorkspaces, showError])

  useEffect(() => {
    const agents = snapshot?.agents ?? []
    setSelectedAgentId((current) => current && agents.some((agent) => agent.agent_id === current) ? current : agents[0]?.agent_id ?? null)
  }, [snapshot])

  const selectedAgent = snapshot?.agents.find((agent) => agent.agent_id === selectedAgentId)
  const selectedAgentMachine = snapshot?.servers.find((machine) => machine.server_id === selectedAgent?.server_id)
  const selectedAgentMachineOnline = machineOnline(selectedAgentMachine)
  const selectedAgentInterface = selectedAgentMachineOnline && selectedAgent?.interface?.ui_path ? selectedAgent.interface : undefined
  const onlineMachines = snapshot?.servers.filter((machine) => machine.status === "online") ?? []
  const selectedCreateProfile = launchProfiles.find((profile) => profile.profile_id === agentProfileId)
  const selectedCreateMachine = onlineMachines.find((machine) => machine.server_id === agentServerId)
  const selectedProfileKind = selectedCreateProfile ? agentKindFromCommand(selectedCreateProfile.command) : null
  const selectedProfileInstalled = selectedProfileKind ? isAgentInstalled(selectedCreateMachine, selectedProfileKind) : null
  const selectedProfileInstall = selectedProfileKind ? catalogEntry(selectedProfileKind) : undefined
  const recipeInstallers = availableCatalog(selectedCreateMachine)
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
  const interfaceUiUrl = workspaceId && selectedAgent && selectedAgentInterface
    ? proxyUrl(`/api/workspaces/${encodeURIComponent(workspaceId)}/agents/${encodeURIComponent(selectedAgent.agent_id)}/interface/ui/`)
    : null
  const setTerminalState = useCallback((value: TerminalState) => setTerminalStatus(value), [])
  const currentRole = organization?.role ?? "member"
  const canManageMembers = ["owner", "admin"].includes(currentRole)

  useEffect(() => {
    if (mainView === "workspace") setWorkspaceName(workspace?.name ?? "")
  }, [mainView, workspace?.workspace_id, workspace?.name])
  const mobileTerminalIdle = isMobile && mainView === "terminal" && !mobileTerminalOpen
  const mobileSidebarHidden = isMobile && mainView !== "terminal"
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

  function closeMobileSurface() {
    setMobileTerminalOpen(false)
    setCtrlModifier(false)
  }

  function refreshAgentView() {
    if (selectedAgentInterface) {
      setInterfaceUiRevision((value) => value + 1)
      return
    }
    setSelectedAgentId(null)
    requestAnimationFrame(() => setSelectedAgentId(selectedAgent?.agent_id ?? null))
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

  useEffect(() => {
    if (isMobile || !mobileTerminalOpen) return
    setMobileTerminalOpen(false)
    setCtrlModifier(false)
  }, [isMobile, mobileTerminalOpen])

  function showAgentTerminal(agentId: string) {
    setSelectedAgentId(agentId)
    setMainView("terminal")
    if (isMobile) setMobileTerminalOpen(true)
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
    replaceWorkspaceRoute(organizationId, value)
  }

  useEffect(() => {
    if (createAgentOpen && !onlineMachines.some((machine) => machine.server_id === agentServerId)) setAgentServerId(onlineMachines[0]?.server_id ?? "")
  }, [createAgentOpen, onlineMachines, agentServerId])

  const availableAgentKey = (selectedCreateMachine?.available_agents ?? []).join(",")
  useEffect(() => {
    const kinds = availableCatalog(selectedCreateMachine).map((entry) => entry.kind)
    setAgentRecipeKind((current) => (current && kinds.includes(current) ? current : (kinds[0] ?? "")))
  }, [agentServerId, availableAgentKey, selectedCreateMachine])

  useEffect(() => {
    if (createAppOpen && !onlineMachines.some((machine) => machine.server_id === appServerId)) setAppServerId(onlineMachines[0]?.server_id ?? "")
  }, [createAppOpen, onlineMachines, appServerId])

  async function createOrganization(event: FormEvent) {
    event.preventDefault()
    try {
      const data = await api<{ organization: Organization }>("/api/organizations", { method: "POST", body: JSON.stringify({ name: organizationName }) })
      setCreateOrganizationOpen(false); setOrganizationName(""); await loadOrganizations(data.organization.organization_id)
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
      setRenameOrganizationOpen(false); await loadOrganizations(organizationId)
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
    if (!workspaceId) return
    setCreatingAgent(true)
    try {
      let agent
      if (agentProfileId === "terminal") {
        agent = await api<Agent>(`/api/workspaces/${encodeURIComponent(workspaceId)}/agents`, { method: "POST", body: JSON.stringify({ server_id: agentServerId, kind: "command", name: agentName, cwd: agentCwd, args: [], cols: 120, rows: 36 }) })
      } else if (agentProfileId === "recipe") {
        const kind = agentRecipeKind || recipeInstallers[0]?.kind || "auto"
        agent = await api<Agent>(`/api/workspaces/${encodeURIComponent(workspaceId)}/agents`, { method: "POST", body: JSON.stringify({ server_id: agentServerId, kind, name: agentName, cwd: ".", args: [], recipe: agentRecipeUrl.trim(), cols: 120, rows: 36 }) })
      } else if (agentProfileId === "manual") {
        const parsed = parseCommandLine(agentCommandLine)
        const kind = parsed.command === "codex" || parsed.command === "claude" ? parsed.command : "command"
        const args = kind === "command" ? [parsed.command, ...parsed.args] : parsed.args
        agent = await api<Agent>(`/api/workspaces/${encodeURIComponent(workspaceId)}/agents`, { method: "POST", body: JSON.stringify({ server_id: agentServerId, kind, name: agentName, cwd: agentCwd, args, cols: 120, rows: 36 }) })
      } else {
        agent = await api<Agent>(`/api/workspaces/${encodeURIComponent(workspaceId)}/launch-profiles/${encodeURIComponent(agentProfileId)}/launch`, { method: "POST", body: JSON.stringify({ server_id: agentServerId, agent_name: agentName, cols: 120, rows: 36 }) })
      }
      const agentId = agent.agent_id
      if (!agentId) throw new Error("Create agent did not return an id")
      setCreateAgentOpen(false)
      showAgentTerminal(agentId)
      await refreshSnapshot()
    } catch (reason) { showError(reason) }
    finally { setCreatingAgent(false) }
  }

  function openCreateAgent(reset = true) {
    if (reset || !createAgentOpenRef.current) {
      setAgentProfileId("terminal")
      setAgentName(defaultAgentName("terminal"))
      setAgentNameCustomized(false)
      setAgentCommandLine("codex")
      setAgentRecipeUrl("")
      setAgentRecipeKind("")
    }
    setCreateAgentOpen(true)
    if (!preview) void loadLaunchProfiles().catch(showError)
  }

  async function installAgent(entry: AgentCatalogEntry, name: string) {
    if (!workspaceId || !entry.install || !agentServerId || installingAgentKind) return
    const script = installThenStartScript(entry)
    if (!script) return
    setInstallingAgentKind(entry.kind)
    try {
      const agent = await api<Agent>(`/api/workspaces/${encodeURIComponent(workspaceId)}/agents`, { method: "POST", body: JSON.stringify({ server_id: agentServerId, kind: "shell", name, cwd: ".", args: ["bash", "-lc", script], cols: 120, rows: 36 }) })
      if (!agent.agent_id) throw new Error("Create agent did not return an id")
      setCreateAgentOpen(false)
      showAgentTerminal(agent.agent_id)
      await refreshSnapshot()
    } catch (reason) { showError(reason) }
    finally { setInstallingAgentKind(null) }
  }

  async function installSelectedAgent() {
    if (!selectedProfileInstall) return
    await installAgent(selectedProfileInstall, agentName || defaultProfileAgentName(selectedProfileInstall.label))
  }

  function selectAgentProfile(profileId: string) {
    changeAgentProfile(profileId)
    const profile = launchProfiles.find((item) => item.profile_id === profileId)
    const kind = profile ? agentKindFromCommand(profile.command) : null
    const entry = kind ? catalogEntry(kind) : undefined
    if (entry?.install && isAgentInstalled(selectedCreateMachine, entry.kind) === false) {
      void installAgent(entry, defaultProfileAgentName(profile?.name ?? entry.label))
    }
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
    if (profileId === "recipe") {
      setAgentName(defaultAgentName("installer"))
      setAgentNameCustomized(false)
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
    try {
      const data = await api<{ profiles: AgentLaunchProfile[] }>(`/api/workspaces/${encodeURIComponent(workspaceId)}/launch-profiles`)
      setLaunchProfiles(data.profiles)
    } finally {
      setLaunchProfilesLoading(false)
    }
  }, [preview, workspaceId])

  useEffect(() => {
    if (mainView === "profiles") loadLaunchProfiles().catch(showError)
  }, [mainView, loadLaunchProfiles, showError])

  const loadApps = useCallback(async () => {
    if (preview || !workspaceId) {
      setApps([])
      return
    }
    setAppsLoading(true)
    try {
      const data = await api<{ apps: AppDeployment[] }>(`/api/workspaces/${encodeURIComponent(workspaceId)}/apps`)
      setApps(data.apps)
    } finally {
      setAppsLoading(false)
    }
  }, [preview, workspaceId])

  useEffect(() => {
    if (mainView === "apps") loadApps().catch(showError)
  }, [mainView, loadApps, showError])

  function openApps() {
    setMainView("apps")
    setMobileTerminalOpen(false)
  }

  function openCreateApp() {
    setAppName("")
    setAppServerId(onlineMachines[0]?.server_id ?? "")
    setAppCwd(".")
    setAppCommandLine("")
    setAppPort("")
    setAppHostname("")
    setCreateAppOpen(true)
  }

  async function createAppDeployment(event: FormEvent) {
    event.preventDefault()
    if (!workspaceId) return
    try {
      const parsed = parseCommandLine(appCommandLine)
      await api(`/api/workspaces/${encodeURIComponent(workspaceId)}/apps`, {
        method: "POST",
        body: JSON.stringify({
          server_id: appServerId,
          name: appName,
          command: parsed.command,
          args: parsed.args,
          cwd: appCwd,
          port: Number(appPort),
          hostname: appHostname,
        }),
      })
      setCreateAppOpen(false)
      await loadApps()
    } catch (reason) { showError(reason) }
  }

  async function appLifecycle(app: AppDeployment, action: "start" | "stop" | "restart") {
    if (!workspaceId) return
    try {
      await api(`/api/workspaces/${encodeURIComponent(workspaceId)}/apps/${encodeURIComponent(app.app_id)}/${action}`, { method: "POST", body: "{}" })
      await loadApps()
    } catch (reason) { showError(reason) }
  }

  async function deleteAppDeployment() {
    if (!workspaceId || !deletingApp) return
    try {
      await api(`/api/workspaces/${encodeURIComponent(workspaceId)}/apps/${encodeURIComponent(deletingApp.app_id)}`, { method: "DELETE" })
      setDeletingApp(null)
      await loadApps()
    } catch (reason) { showError(reason) }
  }

  function openLaunchProfiles() {
    setMainView("profiles")
  }

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
    setMainView("terminal")
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
      setConnectCommand(preview ? PREVIEW_CONNECT : `TREER_ENROLLMENT_KEY='enr_v1_…' treer-agent-server connect --proxy '${origin}/'`)
      setInstallOpen(true)
      return
    }
    try {
      const data = await api<{ install_command: string; connect_command: string }>(`/api/workspaces/${encodeURIComponent(workspaceId)}/bootstrap`, { method: "POST", body: "{}" })
      setInstallCommand(data.install_command); setConnectCommand(data.connect_command); setInstallOpen(true)
    } catch (reason) { showError(reason) }
  }

  useEffect(() => {
    createAgentOpenRef.current = createAgentOpen
  }, [createAgentOpen])

  const tourHostRef = useRef<FirstRunTourHost | null>(null)
  tourHostRef.current = {
    setSidebarTab,
    openCreateWorkspace: () => { setCreateWorkspaceName(""); setCreateWorkspaceOpen(true) },
    closeCreateWorkspace: () => setCreateWorkspaceOpen(false),
    prepareWorkspaceForMachineSteps: () => {
      if (!preview) return
      setWorkspaces([PREVIEW_WORKSPACE])
      setWorkspaceId(PREVIEW_WORKSPACE.workspace_id)
      setSnapshot({ revision: 0, workspace: PREVIEW_WORKSPACE, servers: [], agents: [] })
      setConnection("no workspace")
    },
    openInstall,
    closeInstall: () => setInstallOpen(false),
    openCreateAgent,
    closeCreateAgent: () => setCreateAgentOpen(false),
    setAgentLaunch: (kind) => {
      if (kind === "ui-profile") {
        const profile = launchProfiles.find((item) => item.name === "Codex") ?? launchProfiles[0]
        changeAgentProfile(profile?.profile_id ?? "terminal")
        return
      }
      changeAgentProfile(kind)
    },
  }

  function replayFirstRunTour() {
    if (!user) return
    clearFirstRunTour(user.user_id)
    if (tourHostRef.current) startFirstRunTour(tourHostRef.current, { userId: user.user_id, persist: !preview })
  }

  useEffect(() => {
    if (!user) return
    if (!shouldAutoStartFirstRunTour(user.user_id)) return
    const timer = window.setTimeout(() => {
      if (!tourHostRef.current) return
      startFirstRunTour(tourHostRef.current, { userId: user.user_id, persist: !preview })
    }, 450)
    return () => {
      window.clearTimeout(timer)
      stopFirstRunTour()
    }
  }, [user, preview])

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

  async function stopAgent(agentId = selectedAgentId) {
    if (!workspaceId || !agentId) return
    try { await api(`/api/workspaces/${encodeURIComponent(workspaceId)}/agents/${encodeURIComponent(agentId)}/stop`, { method: "POST", body: "{}" }) }
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

  function showMachineOverview(serverId: string) {
    setSelectedMachineId(serverId)
    setMainView("machine")
  }

  function openNetwork() {
    setMainView("network")
  }

  const loadAudit = useCallback(async () => {
    if (!organizationId || !workspaceId || !canManageMembers) {
      setAuditEvents([])
      return
    }
    setAuditLoading(true)
    try {
      const auditData = await api<{ events: OrganizationAuditEvent[] }>(`/api/organizations/${encodeURIComponent(organizationId)}/audit-events?workspace_id=${encodeURIComponent(workspaceId)}&limit=100`)
      setAuditEvents(auditData.events)
    } finally {
      setAuditLoading(false)
    }
  }, [organizationId, workspaceId, canManageMembers])

  const loadTraffic = useCallback(async () => {
    const requestedWorkspaceId = workspaceId
    if (!requestedWorkspaceId) {
      setTraffic([])
      return
    }
    const data = await api<{ traffic: MachineTrafficRecord[] }>(`/api/workspaces/${encodeURIComponent(requestedWorkspaceId)}/traffic?hours=24`)
    if (workspaceIdRef.current === requestedWorkspaceId) setTraffic(data.traffic)
  }, [workspaceId])

  const refreshAudit = useCallback(() => {
    void Promise.all([loadAudit(), loadTraffic()]).catch(showError)
  }, [loadAudit, loadTraffic, showError])

  useEffect(() => {
    if (mainView === "audit") loadAudit().catch(showError)
    if (mainView === "machine") loadNetwork().catch(showError)
    if (mainView !== "audit" && mainView !== "machine") return
    loadTraffic().catch(showError)
    const timer = window.setInterval(() => loadTraffic().catch(showError), 10_000)
    return () => window.clearInterval(timer)
  }, [mainView, loadAudit, loadNetwork, loadTraffic, showError])

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
    setServiceTargetAgentId(null)
    setServiceServerId((current) => current || snapshot?.servers[0]?.server_id || "")
    setCreateServiceOpen(true)
  }

  function openEditService(service: MachineService) {
    setEditingService(service)
    setServiceName(service.name)
    setServiceServerId(service.server_id)
    setServiceTargetAgentId(service.target_agent_id ?? null)
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

  function openApp(app: AppDeployment) {
    if (!app.public_url) {
      openVirtualHost(app.hostname)
      return
    }
    window.open(app.public_url, "_blank", "noopener,noreferrer")
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
          ...(!editingService && serviceTargetAgentId ? { target_agent_id: serviceTargetAgentId } : {}),
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
    if (preview) return
    try { await api("/api/auth/logout", { method: "POST", body: "{}" }) }
    finally { window.location.href = "/" }
  }

  if (user === undefined) return <div className="grid min-h-dvh place-items-center bg-sidebar text-sm text-muted-foreground">Loading Treer...</div>
  if (!user) return <AuthScreen onAuthenticated={setUser} />

  return <TooltipProvider delayDuration={350}>
      <main className={cn("grid h-dvh min-h-0 bg-background md:grid-cols-[272px_minmax(0,1fr)] md:grid-rows-1 md:overflow-hidden", mobileTerminalIdle || mobileSidebarHidden ? "grid-rows-1 overflow-hidden" : "grid-rows-[374px_minmax(620px,1fr)] overflow-auto")}>
        <aside className={cn("flex min-h-0 flex-col border-b bg-sidebar md:border-b-0 md:border-r", mobileSidebarHidden && "hidden md:flex")}>
        {preview && <div className="treer-tour-banner"><span>Tour preview · empty first-run account, no server writes</span></div>}
        <div className="grid min-h-[58px] grid-cols-[32px_minmax(0,1fr)_32px] items-center gap-2 px-3 py-2">
          <div className="grid size-8 place-items-center rounded-[5px] bg-[#e8deee] text-[10px] font-bold text-[#694a73]">{initials(organization?.name ?? "Treer")}</div>
          <div className="min-w-0"><div className="mb-0.5 px-1 text-[9px] font-semibold uppercase text-muted-foreground">Organization</div><Select value={organizationId ?? undefined} onValueChange={selectOrganization}><SelectTrigger aria-label="Organization" className="h-7 border-0 bg-transparent px-1 shadow-none hover:bg-accent"><SelectValue placeholder="No organization" /></SelectTrigger><SelectContent>{organizations.map((item) => <SelectItem key={item.organization_id} value={item.organization_id}>{item.name}</SelectItem>)}</SelectContent></Select></div>
          <DropdownMenu><DropdownMenuTrigger asChild><Button size="icon" variant="ghost" className="size-8" aria-label="Organization actions"><MoreHorizontal /></Button></DropdownMenuTrigger><DropdownMenuContent align="end"><DropdownMenuItem onSelect={() => setCreateOrganizationOpen(true)}><Plus />Create organization</DropdownMenuItem><DropdownMenuItem onSelect={() => { setCreateWorkspaceName(""); setCreateWorkspaceOpen(true) }} disabled={!organizationId}><FolderKanban />Create workspace</DropdownMenuItem>{canManageMembers && organization && <DropdownMenuItem onSelect={() => { setOrganizationName(organization.name); setRenameOrganizationOpen(true) }}><Pencil />Rename organization</DropdownMenuItem>}<DropdownMenuSeparator /><DropdownMenuItem onSelect={openMembers} disabled={!organizationId}><Users />Members</DropdownMenuItem>{canManageMembers && <DropdownMenuItem onSelect={openAudit} disabled={!organizationId}><ScrollText />Audit</DropdownMenuItem>}</DropdownMenuContent></DropdownMenu>
        </div>
        <div className="grid grid-cols-[20px_minmax(0,1fr)_32px] items-center gap-2 px-3 pb-3 pl-5">
          <FolderKanban className="size-3.5 text-muted-foreground" />
          <div data-tour="workspace-select">
            <Select value={workspaceId ?? undefined} onValueChange={selectWorkspace} disabled={!organizationId}><SelectTrigger aria-label="Workspace" className="h-7 border-0 bg-transparent px-1 text-xs shadow-none hover:bg-accent"><SelectValue placeholder="No workspace" /></SelectTrigger><SelectContent>{workspaces.map((item) => <SelectItem key={item.workspace_id} value={item.workspace_id}>{item.name}</SelectItem>)}</SelectContent></Select>
          </div>
          <span data-tour="create-workspace"><IconButton label={workspace ? "Workspace settings" : "Create workspace"} disabled={!organizationId} className={cn(mainView === "workspace" && "bg-accent")} onClick={openWorkspaceSettings}>{workspace ? <SettingsIcon /> : <Plus />}</IconButton></span>
        </div>
        <div className="px-2 pb-2">
          <DropdownMenu><DropdownMenuTrigger asChild><Button variant="ghost" className={cn("h-8 w-full justify-start gap-2 px-2 text-xs font-normal", (mainView === "profiles" || mainView === "apps") && "bg-accent")} aria-label="Workspace views"><ListChecks className="size-3.5" />{mainView === "profiles" ? "Profiles" : mainView === "apps" ? "Apps" : "Workspace"}<ChevronDown className="ml-auto size-3.5 text-muted-foreground" /></Button></DropdownMenuTrigger>
            <DropdownMenuContent align="start" className="w-56">
              <DropdownMenuItem onSelect={openLaunchProfiles} disabled={!workspaceId}><Rocket />Profiles</DropdownMenuItem>
              <DropdownMenuItem onSelect={openApps} disabled={!workspaceId}><PanelsTopLeft />Apps</DropdownMenuItem>
            </DropdownMenuContent></DropdownMenu>
        </div>

        <Tabs value={sidebarTab} onValueChange={(value) => setSidebarTab(value as SidebarTab)} className="flex min-h-0 flex-1 flex-col overflow-hidden">
          <TabsList className="mx-2 grid h-auto grid-cols-2 bg-accent p-0.5">
            <TabsTrigger value="machines" data-tour="machines-tab" className="h-8 gap-2 text-xs"><Server className="size-3.5" />Machines <span className="rounded-full bg-background px-1.5 text-[9px]">{snapshot?.servers.length ?? 0}</span></TabsTrigger>
            <TabsTrigger value="agents" data-tour="agents-tab" className="h-8 gap-2 text-xs"><TerminalSquare className="size-3.5" />Agents <span className="rounded-full bg-background px-1.5 text-[9px]">{snapshot?.agents.length ?? 0}</span></TabsTrigger>
          </TabsList>
          <TabsContent value="machines" className="mt-0 min-h-0 flex-1 overflow-hidden data-[state=inactive]:hidden">
            <div className="flex h-full min-h-0 flex-col">
              <div className="flex h-10 shrink-0 items-center justify-between px-4 text-[11px] font-medium text-muted-foreground"><span>Machines</span><span data-tour="add-machine"><Button variant="ghost" size="sm" className="h-7 px-2" onClick={openInstall} disabled={!workspaceId}><CirclePlus className="size-3.5" />Add</Button></span></div>
              <div className="min-h-0 flex-1 overflow-auto px-2 pb-2">
                {snapshot?.servers.map((machine) => <MachineItem key={machine.server_id} machine={machine} machines={snapshot.servers} workspaceId={workspaceId} selected={mainView === "machine" && machine.server_id === selectedMachineId} onClick={() => showMachineOverview(machine.server_id)} onRename={() => openRename({ kind: "machine", id: machine.server_id, name: machineName(machine) })} onDelete={() => setDeleteTarget({ kind: "machine", id: machine.server_id, name: machineName(machine) })} onCopy={copy} />)}
                {snapshot && !snapshot.servers.length && <EmptyState icon={<Server />} label="No machines connected" />}
              </div>
            </div>
          </TabsContent>
          <TabsContent value="agents" className="mt-0 min-h-0 flex-1 overflow-hidden data-[state=inactive]:hidden">
            <div className="flex h-full min-h-0 flex-col">
              <div className="flex h-10 shrink-0 items-center justify-between px-4 text-[11px] font-medium text-muted-foreground"><span>Agents {snapshot && <span className="ml-1 font-mono text-[9px] text-zinc-400">rev {snapshot.revision}</span>}</span><span data-tour="create-agent"><Button variant="ghost" size="sm" className="h-7 px-2" onClick={() => openCreateAgent()} disabled={!workspaceId || (!onlineMachines.length && !preview)}><Plus className="size-3.5" />New</Button></span></div>
              <div className="min-h-0 flex-1 overflow-auto px-2 pb-2">
                {snapshot?.agents.map((agent) => <AgentItem key={agent.agent_id} agent={agent} machine={snapshot.servers.find((item) => item.server_id === agent.server_id)} selected={mainView === "terminal" && agent.agent_id === selectedAgentId} onClick={() => showAgentTerminal(agent.agent_id)} onRename={() => openRename({ kind: "agent", id: agent.agent_id, name: agent.name })} onStop={() => void stopAgent(agent.agent_id)} onDelete={() => setDeleteTarget({ kind: "agent", id: agent.agent_id, name: agent.name })} />)}
                {snapshot && !snapshot.agents.length && <EmptyState icon={<TerminalSquare />} label="No agents in this workspace" />}
              </div>
            </div>
          </TabsContent>
        </Tabs>

        <div className="shrink-0 border-t p-2">
          <DropdownMenu>
            <DropdownMenuTrigger asChild><button type="button" aria-label="User menu" className="grid h-11 w-full grid-cols-[28px_minmax(0,1fr)_20px] items-center gap-2 rounded-[5px] px-2 text-left hover:bg-accent"><span className="grid size-7 place-items-center rounded bg-[#e8deee] text-[10px] font-bold text-[#694a73]">{initials(user.preferred_name)}</span><span className="min-w-0"><span className="block truncate text-xs font-medium">{user.preferred_name}</span><span className="block truncate text-[9px] text-muted-foreground">{user.email}</span></span><MoreHorizontal className="size-4 text-muted-foreground" /></button></DropdownMenuTrigger>
            <DropdownMenuContent side="top" align="start" className="w-60"><DropdownMenuLabel><span className="block truncate">{user.preferred_name}</span><span className="mt-0.5 block truncate text-[10px] font-normal text-muted-foreground">{user.email} · {currentRole}</span></DropdownMenuLabel><DropdownMenuSeparator /><DropdownMenuItem onSelect={openSettings}><SettingsIcon />Settings</DropdownMenuItem><DropdownMenuItem onSelect={replayFirstRunTour}><ListChecks />Replay product tour</DropdownMenuItem><DropdownMenuSeparator /><DropdownMenuItem onSelect={logout} disabled={preview}><LogOut />Log out</DropdownMenuItem></DropdownMenuContent>
          </DropdownMenu>
        </div>
      </aside>

      <section className={cn("min-h-0 min-w-0 grid-rows-[48px_minmax(0,1fr)]", mobileTerminalIdle ? "hidden md:grid" : "grid")}>
        <header className="flex min-w-0 items-center justify-between gap-4 border-b px-3 sm:px-5">
          <div className="flex min-w-0 items-center gap-1.5 overflow-hidden text-xs text-muted-foreground">
            {isMobile && mainView !== "terminal" && <IconButton label="Back" className="mr-1 md:hidden" onClick={closeMainView}><ChevronLeft /></IconButton>}
            <span className="hidden truncate sm:block">{workspace?.name ?? "Workspace"}</span><ChevronRight className="hidden size-3 shrink-0 sm:block" /><strong className="truncate font-medium text-foreground">{mainView === "workspace" ? "Settings" : mainView === "profiles" ? "Profiles" : mainView === "apps" ? "Apps" : mainView === "network" ? "Network" : mainView === "audit" ? "Audit" : selectedAgent?.name ?? "Terminal"}</strong></div>
          {mainView === "terminal" ? <div className="flex shrink-0 items-center gap-0.5">
            <IconButton label={selectedAgentInterface ? "Open full-screen interface" : "Open full-screen terminal"} className="md:hidden" disabled={!selectedAgent} onClick={openMobileTerminal}><Maximize2 /></IconButton>
            <IconButton label="Rename agent" disabled={!selectedAgent} onClick={() => selectedAgent && openRename({ kind: "agent", id: selectedAgent.agent_id, name: selectedAgent.name })}><Pencil /></IconButton>
            <IconButton label={selectedAgentInterface ? "Reload interface" : "Reconnect terminal"} disabled={!selectedAgent} onClick={refreshAgentView}><RotateCw /></IconButton>
            <IconButton label="Stop agent" disabled={!selectedAgent || !terminalActive} onClick={() => void stopAgent()}><Square /></IconButton>
            <IconButton label="Delete agent" disabled={!selectedAgent} className="text-destructive hover:text-destructive" onClick={() => selectedAgent && setDeleteTarget({ kind: "agent", id: selectedAgent.agent_id, name: selectedAgent.name })}><Trash2 /></IconButton>
          </div> : mainView === "profiles" ? <div className="flex shrink-0 items-center gap-1"><IconButton label="Refresh profiles" onClick={loadLaunchProfiles} disabled={launchProfilesLoading}><RotateCw /></IconButton><Button size="sm" className="h-8" onClick={openNewLaunchProfile}><Plus />New profile</Button></div> : mainView === "apps" ? <div className="flex shrink-0 items-center gap-1"><IconButton label="Refresh Apps" onClick={loadApps} disabled={appsLoading}><RotateCw /></IconButton><Button size="sm" className="h-8" onClick={openCreateApp} disabled={!onlineMachines.length}><Plus />New App</Button></div> : mainView === "audit" ? <IconButton label="Refresh audit" onClick={refreshAudit} disabled={auditLoading}><RotateCw /></IconButton> : mainView === "network" ? <div className="flex shrink-0 items-center gap-1"><IconButton label="Refresh network" onClick={refreshNetwork}><RotateCw /></IconButton><Button size="sm" variant="outline" className="h-8" onClick={openCreateService} disabled={!snapshot?.servers.length}><Server />Add service</Button><Button size="sm" variant="outline" className="h-8" onClick={openCreateVirtualHost} disabled={!services.length}><Plus />Add host</Button><Button size="sm" className="h-8" onClick={openPublish} disabled={!services.some((service) => service.protocol === "http")}><ExternalLink />Publish</Button></div> : null}
        </header>
        {mainView === "terminal" && selectedAgent && !selectedAgentMachineOnline ? <div className={cn("grid min-h-0 place-items-center bg-sidebar px-6 text-center", mobileTerminalOpen && "fixed inset-0 z-[100] bg-sidebar pt-[env(safe-area-inset-top)]")}>
          <div className="max-w-sm">
            <p className="text-sm font-medium text-zinc-800">Machine is offline</p>
            <p className="mt-2 text-xs leading-5 text-muted-foreground">{machineName(selectedAgentMachine, selectedAgent.server_id)} is not connected to the control plane. It may be stopped, waking from sleep, or fenced as a duplicate.</p>
            {workspaceId && <MachineRecovery workspaceId={workspaceId} onCopy={copy} reason="Copy a recovery command and run it on that machine. restart-controller keeps Agents; start launches a stopped Host." />}
          </div>
        </div> : mainView === "terminal" && selectedAgentInterface && interfaceUiUrl ? <div className={cn("min-h-0 min-w-0 overflow-hidden bg-white", mobileTerminalOpen && "fixed inset-0 z-[100] grid h-[100dvh] grid-rows-[44px_minmax(0,1fr)] bg-[#0f1215] pt-[env(safe-area-inset-top)]")}>
          {mobileTerminalOpen && <div className="flex min-w-0 items-center justify-between gap-3 border-b border-zinc-800 bg-[#191d20] px-3.5"><span className="truncate text-xs font-semibold text-zinc-200">{selectedAgent?.name ?? "Interface"}</span><button type="button" className="grid size-8 place-items-center rounded-[5px] text-zinc-400 hover:bg-white/10 hover:text-zinc-100" aria-label="Close full-screen interface" onClick={closeMobileSurface}><X className="size-4" /></button></div>}
          <iframe key={`${selectedAgentInterface.instance_id}:${selectedAgentInterface.registered_at}:${interfaceUiRevision}`} src={interfaceUiUrl} title={`${selectedAgent?.name ?? "Agent"} interface`} className="block size-full min-h-0 border-0 bg-white" sandbox="allow-scripts allow-forms allow-same-origin allow-modals allow-downloads" />
        </div> : mainView === "terminal" ? mobileTerminalIdle ? null : <div className="flex min-h-0 justify-center overflow-hidden px-3 pb-4 pt-4 sm:px-8 sm:pb-7 sm:pt-6 lg:px-16">
          <div className={cn("grid h-full min-h-0 w-full max-w-[1120px] grid-rows-[42px_minmax(0,1fr)] overflow-hidden rounded-md border border-zinc-800 bg-[#0f1215] shadow-[0_8px_28px_rgba(15,18,21,.14)]", mobileTerminalOpen && "fixed inset-0 z-[100] h-[100dvh] max-w-none grid-rows-[44px_minmax(0,1fr)_auto] rounded-none border-0 shadow-none")}>
            <div className="flex min-w-0 items-center justify-between gap-3 border-b border-zinc-800 bg-[#191d20] px-3.5"><div className="flex min-w-0 items-baseline gap-2"><span className="truncate text-xs font-semibold text-zinc-200">{selectedAgent?.name ?? "Terminal"}</span>{selectedAgent && <span className="hidden truncate font-mono text-[9px] text-zinc-500 sm:block">{selectedAgent.agent_id} · {machineName(snapshot?.servers.find((item) => item.server_id === selectedAgent.server_id))}</span>}</div><div className="flex shrink-0 items-center gap-2"><span className="inline-flex items-center gap-1.5 text-[9px] uppercase text-zinc-500"><span className="size-1.5 rounded-full bg-current" />{terminalStatus}</span>{mobileTerminalOpen && <button type="button" className="grid size-8 place-items-center rounded-[5px] text-zinc-400 hover:bg-white/10 hover:text-zinc-100" aria-label="Close full-screen terminal" onClick={closeMobileSurface}><X className="size-4" /></button>}</div></div>
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
        </div> : mainView === "workspace" ? <WorkspaceSettingsView workspace={workspace} organization={organization} name={workspaceName} machineCount={workspaceMachineCount} agentCount={snapshot?.agents.length} canDelete={canManageMembers} preview={preview} onNameChange={setWorkspaceName} onRename={renameWorkspace} onDelete={() => setDeleteWorkspaceOpen(true)} onClose={closeMainView} /> : mainView === "profiles" ? <LaunchProfilesView profiles={launchProfiles} loading={launchProfilesLoading} onEdit={openEditLaunchProfile} onLaunch={openLaunchProfile} onDelete={setDeletingProfile} /> : mainView === "apps" ? <AppsView apps={apps} machines={snapshot?.servers ?? []} loading={appsLoading} onOpen={openApp} onAction={appLifecycle} onDelete={setDeletingApp} /> : mainView === "machine" ? <MachineOverviewView machine={selectedMachine} agents={snapshot?.agents.filter((agent) => agent.server_id === selectedMachineId) ?? []} services={services.filter((service) => service.server_id === selectedMachineId)} virtualHosts={virtualHosts.filter((host) => host.destination_server_id === selectedMachineId)} traffic={traffic} machines={snapshot?.servers ?? []} workspaceId={workspaceId} onOpenAgent={showAgentTerminal} onClose={closeMainView} onCopy={copy} /> : mainView === "audit" ? <AuditView events={auditEvents} traffic={traffic} machines={snapshot?.servers ?? []} loading={auditLoading} /> : <div className="min-h-0 overflow-auto"><div className="mx-auto w-full max-w-[1120px] px-5 py-8 sm:px-8 sm:py-12 lg:px-14"><div className="mb-8 flex items-end justify-between gap-4"><div><div className="mb-2 grid size-9 place-items-center rounded-md bg-[#e8deee] text-[#694a73]"><Network className="size-4" /></div><h1 className="text-2xl font-semibold">Network</h1></div><span className="text-xs text-muted-foreground">{services.length} services · {virtualHosts.length} hosts</span></div><section className="mb-10"><h2 className="mb-3 text-sm font-semibold">Machine services</h2><div className="border-y"><div className="hidden h-9 grid-cols-[minmax(150px,1fr)_minmax(180px,1fr)_minmax(140px,1fr)_auto] items-center gap-4 border-b text-[10px] font-medium uppercase text-muted-foreground sm:grid"><span>Service</span><span>Target</span><span>Machine</span><span className="w-24" /></div>{services.map((service) => { const machine = snapshot?.servers.find((item) => item.server_id === service.server_id); const health = serviceHealth[service.service_id]; return <div key={service.service_id} className="grid min-h-16 grid-cols-[minmax(0,1fr)_auto] items-center gap-x-3 gap-y-1 border-b py-3 last:border-b-0 sm:grid-cols-[minmax(150px,1fr)_minmax(180px,1fr)_minmax(140px,1fr)_auto] sm:gap-4"><span className="col-start-1 row-start-1 min-w-0 truncate text-xs font-medium sm:col-start-auto sm:row-start-auto">{service.name}<span className="ml-2 font-mono text-[9px] uppercase text-muted-foreground">{service.protocol}</span>{health && <span className={cn("ml-2 text-[9px]", health === "healthy" ? "text-emerald-700" : "text-red-600")}>{health}</span>}</span><span className="col-start-1 row-start-2 min-w-0 truncate font-mono text-[10px] text-muted-foreground sm:col-start-auto sm:row-start-auto">{service.target_host}:{service.target_port}</span><span className="col-start-1 row-start-3 min-w-0 truncate text-[10px] text-muted-foreground sm:col-start-auto sm:row-start-auto">{machineName(machine, service.server_id)}</span><span className="col-start-2 row-span-3 row-start-1 flex items-center justify-end gap-1 sm:col-start-auto sm:row-span-1 sm:row-start-auto"><IconButton label={`Probe ${service.name}`} onClick={() => probeService(service.service_id)} disabled={machine?.status !== "online"}><RotateCw /></IconButton><IconButton label={`Edit ${service.name}`} onClick={() => openEditService(service)}><Pencil /></IconButton><IconButton label={`Delete ${service.name}`} className="text-destructive hover:text-destructive" onClick={() => deleteService(service.service_id)}><Trash2 /></IconButton></span></div>})}{!services.length && <EmptyState icon={<Server />} label="No machine services" />}</div></section><section><h2 className="mb-3 text-sm font-semibold">Virtual hosts</h2><div className="border-y"><div className="hidden h-9 grid-cols-[minmax(150px,1.2fr)_minmax(180px,1fr)_minmax(140px,1fr)_auto] items-center gap-4 border-b text-[10px] font-medium uppercase text-muted-foreground sm:grid"><span>Hostname</span><span>Service</span><span>Machine</span><span className="w-24" /></div>{virtualHosts.map((host) => { const machine = snapshot?.servers.find((item) => item.server_id === host.destination_server_id); const service = services.find((item) => item.service_id === host.service_id); return <div key={host.hostname} className="grid min-h-16 grid-cols-[minmax(0,1fr)_auto] items-center gap-x-3 gap-y-1 border-b py-3 last:border-b-0 sm:grid-cols-[minmax(150px,1.2fr)_minmax(180px,1fr)_minmax(140px,1fr)_auto] sm:gap-4"><button className="col-start-1 row-start-1 min-w-0 truncate text-left font-mono text-xs font-medium hover:underline sm:col-start-auto sm:row-start-auto" onClick={() => openVirtualHost(host.hostname)}>{host.hostname}</button><span className="col-start-1 row-start-2 min-w-0 truncate text-[10px] text-muted-foreground sm:col-start-auto sm:row-start-auto">{service?.name ?? host.service_id}</span><span className="col-start-1 row-start-3 min-w-0 truncate text-[10px] text-muted-foreground sm:col-start-auto sm:row-start-auto">{machineName(machine, host.destination_server_id)}</span><span className="col-start-2 row-span-3 row-start-1 flex items-center justify-end gap-1 sm:col-start-auto sm:row-span-1 sm:row-start-auto"><IconButton label={`Open ${host.hostname}`} onClick={() => openVirtualHost(host.hostname)} disabled={machine?.status !== "online" || service?.protocol !== "http"}><ExternalLink /></IconButton><IconButton label={`Delete ${host.hostname}`} className="text-destructive hover:text-destructive" onClick={() => deleteVirtualHost(host.hostname)}><Trash2 /></IconButton></span></div>})}{!virtualHosts.length && <EmptyState icon={<Network />} label="No virtual hosts" />}</div></section></div></div>}
      </section>
    </main>

    {error && <div className="fixed bottom-4 left-1/2 z-[90] flex max-w-[calc(100vw-2rem)] -translate-x-1/2 items-center gap-3 rounded-md border bg-background px-4 py-3 text-sm shadow-lg"><span className="truncate">{error}</span><Button size="sm" variant="ghost" onClick={() => setError(null)}>Dismiss</Button></div>}

    <SettingsDialog open={settingsOpen} onOpenChange={setSettingsOpen} user={user} onUserChange={setUser} onError={showError} />

    <SimpleNameDialog open={createOrganizationOpen} onOpenChange={setCreateOrganizationOpen} title="Create organization" description="Organizations contain members and workspaces." label="Organization name" value={organizationName} onValueChange={setOrganizationName} onSubmit={createOrganization} />
    <SimpleNameDialog open={createWorkspaceOpen} onOpenChange={setCreateWorkspaceOpen} title="Create workspace" description={`Add a workspace to ${organization?.name ?? "this organization"}.`} label="Workspace name" value={createWorkspaceName} onValueChange={setCreateWorkspaceName} onSubmit={createWorkspace} dataTour="create-workspace-dialog" />

    <Dialog open={renameOrganizationOpen} onOpenChange={setRenameOrganizationOpen}><DialogContent><form onSubmit={renameOrganization}><DialogHeader><DialogTitle>Rename organization</DialogTitle><DialogDescription>Update the organization name shown to its members.</DialogDescription></DialogHeader><div className="my-5"><Field label="Organization name"><Input value={organizationName} onChange={(event) => setOrganizationName(event.target.value)} required autoFocus maxLength={80} /></Field></div><DialogFooter><Button type="button" variant="outline" onClick={() => setRenameOrganizationOpen(false)}>Cancel</Button><Button type="submit">Save</Button></DialogFooter></form></DialogContent></Dialog>

    <Dialog open={profileEditorOpen} onOpenChange={(open) => { setProfileEditorOpen(open); if (!open) setEditingLaunchProfile(null) }}><DialogContent className="max-w-xl"><form onSubmit={saveLaunchProfile} className="grid gap-4 sm:grid-cols-2"><DialogHeader className="sm:col-span-2"><DialogTitle>{editingLaunchProfile ? "Edit launch profile" : "New launch profile"}</DialogTitle><DialogDescription>Reusable Agent process settings for this workspace.</DialogDescription></DialogHeader><Field label="Profile name"><Input value={launchProfileName} onChange={(event) => setLaunchProfileName(event.target.value)} required autoFocus maxLength={80} /></Field><Field label="Working directory"><Input className="font-mono" value={launchProfileCwd} onChange={(event) => setLaunchProfileCwd(event.target.value)} required /></Field><div className="sm:col-span-2"><Field label="Description"><Input value={launchProfileDescription} onChange={(event) => setLaunchProfileDescription(event.target.value)} maxLength={1000} /></Field></div><div className="sm:col-span-2"><Field label="Command"><Input className="font-mono" value={launchProfileCommandLine} onChange={(event) => setLaunchProfileCommandLine(event.target.value)} placeholder="codex review --base main" required /></Field></div><DialogFooter className="sm:col-span-2"><Button type="button" variant="outline" onClick={() => setProfileEditorOpen(false)}>Cancel</Button><Button type="submit"><Rocket />{editingLaunchProfile ? "Save profile" : "Create profile"}</Button></DialogFooter></form></DialogContent></Dialog>

    <Dialog open={Boolean(launchingProfile)} onOpenChange={(open) => !open && setLaunchingProfile(null)}><DialogContent><form onSubmit={launchFromProfile} className="space-y-4"><DialogHeader><DialogTitle>Run {launchingProfile?.name}</DialogTitle><DialogDescription>Choose where to start this Agent.</DialogDescription></DialogHeader><Field label="Machine"><Select value={launchMachineId} onValueChange={setLaunchMachineId} required><SelectTrigger><SelectValue placeholder="Select an online machine" /></SelectTrigger><SelectContent>{onlineMachines.map((machine) => <SelectItem key={machine.server_id} value={machine.server_id}>{machineName(machine)}</SelectItem>)}</SelectContent></Select></Field><Field label="Agent name"><Input value={launchAgentName} onChange={(event) => setLaunchAgentName(event.target.value)} required maxLength={80} /></Field><DialogFooter><Button type="button" variant="outline" onClick={() => setLaunchingProfile(null)}>Cancel</Button><Button type="submit" disabled={!launchMachineId}><Play />Run Agent</Button></DialogFooter></form></DialogContent></Dialog>

    <Dialog open={Boolean(deletingProfile)} onOpenChange={(open) => !open && setDeletingProfile(null)}><DialogContent><DialogHeader><DialogTitle>Delete launch profile</DialogTitle><DialogDescription>Delete {deletingProfile?.name}? Existing Agents are not affected.</DialogDescription></DialogHeader><DialogFooter><Button variant="outline" onClick={() => setDeletingProfile(null)}>Cancel</Button><Button variant="destructive" onClick={deleteLaunchProfile}>Delete profile</Button></DialogFooter></DialogContent></Dialog>

    <Dialog open={createAgentOpen} onOpenChange={setCreateAgentOpen}>
      <DialogContent data-tour="create-agent-dialog">
        <form onSubmit={createAgent} className="min-w-0 space-y-4">
          <DialogHeader>
            <DialogTitle>Create agent</DialogTitle>
            <DialogDescription>Start a terminal or agent on an online machine in this workspace.</DialogDescription>
          </DialogHeader>
          <Field label="Machine">
            <Select value={agentServerId} onValueChange={setAgentServerId} required>
              <SelectTrigger aria-label="Machine"><SelectValue placeholder="Select a machine" /></SelectTrigger>
              <SelectContent>{onlineMachines.map((machine) => <SelectItem key={machine.server_id} value={machine.server_id}>{machineName(machine)}</SelectItem>)}</SelectContent>
            </Select>
          </Field>
          <Field label="Launch">
            <Select value={agentProfileId} onValueChange={selectAgentProfile}>
              <SelectTrigger aria-label="Launch" data-tour="agent-launch"><SelectValue /></SelectTrigger>
              <SelectContent>
                <SelectItem value="terminal">Terminal</SelectItem>
                <SelectItem value="manual">Custom command</SelectItem>
                <SelectItem value="recipe">Install recipe</SelectItem>
                {launchProfiles.map((profile) => {
                  const kind = agentKindFromCommand(profile.command)
                  const installed = kind ? isAgentInstalled(selectedCreateMachine, kind) : null
                  const installable = installed === false && Boolean(kind && catalogEntry(kind)?.install)
                  const installing = Boolean(kind && installingAgentKind === kind)
                  return (
                    <SelectItem
                      key={profile.profile_id}
                      value={profile.profile_id}
                      className={installed === false ? "text-muted-foreground" : undefined}
                      trailing={installed === true
                        ? <CircleCheck className="size-3.5 text-emerald-700" />
                        : installable
                          ? installing ? <RotateCw className="size-3.5 animate-spin" /> : <Download className="size-3.5" />
                          : null}
                    >
                      {installable ? `Install ${profile.name}` : profile.name}
                    </SelectItem>
                  )
                })}
              </SelectContent>
            </Select>
          </Field>
          {agentProfileId === "terminal" || agentProfileId === "manual"
            ? <Field label="Working directory"><Input value={agentCwd} onChange={(event) => setAgentCwd(event.target.value)} /></Field>
            : selectedCreateProfile
              ? <div className="min-w-0 max-w-full overflow-hidden rounded-md border bg-muted/30 px-3 py-2"><code className="block max-w-full truncate text-xs" title={formatCommandLine(selectedCreateProfile.command, selectedCreateProfile.args)}>{formatCommandLine(selectedCreateProfile.command, selectedCreateProfile.args)}</code><span className="mt-1 block max-w-full truncate text-[10px] text-muted-foreground">{selectedCreateProfile.cwd || "."}</span></div>
              : null}
          {agentProfileId === "manual" && <Field label="Command"><Input className="font-mono" value={agentCommandLine} onChange={(event) => changeAgentCommandLine(event.target.value)} placeholder="codex" required /></Field>}
          {agentProfileId === "recipe" && <div data-tour="agent-recipe"><Field label="Installer"><Select value={agentRecipeKind} onValueChange={setAgentRecipeKind} disabled={!recipeInstallers.length}><SelectTrigger><SelectValue placeholder={recipeInstallers.length ? "Select an installed agent" : "No installed agent on this machine"} /></SelectTrigger><SelectContent>{recipeInstallers.map((entry) => <SelectItem key={entry.kind} value={entry.kind} trailing={<CircleCheck className="size-3.5 text-emerald-700" />}>{entry.label}</SelectItem>)}</SelectContent></Select><span className="mt-1 block text-[10px] text-muted-foreground">{recipeInstallers.length ? "Only agents already installed on the selected machine can run a recipe." : "Install Claude, Cursor, Codex, OpenCode, or Pi on this machine first."}</span></Field><Field label="Recipe URL"><Input className="font-mono" value={agentRecipeUrl} onChange={(event) => setAgentRecipeUrl(event.target.value)} placeholder="https://github.com/example/recipe.git" required /></Field></div>}
          {selectedCreateProfile && selectedProfileInstalled === false && selectedProfileInstall?.install
            ? <div className="rounded-md border bg-muted/30 px-3 py-2 text-[11px] text-muted-foreground">Installation runs on {machineName(selectedCreateMachine)} and opens a terminal for setup and login.</div>
            : selectedCreateProfile
              ? <span className="block text-[10px] text-muted-foreground">Creates another Agent. Each Agent is one thread. A running same-type UI is reused when this process can reach it.</span>
              : null}
          <Field label="Name"><Input value={agentName} onChange={(event) => { setAgentName(event.target.value); setAgentNameCustomized(true) }} required /></Field>
          <DialogFooter>
            <Button type="button" variant="outline" onClick={() => setCreateAgentOpen(false)}>Cancel</Button>
            {selectedProfileInstalled === false && selectedProfileInstall?.install
              ? <Button type="button" onClick={() => { void installSelectedAgent() }} disabled={!agentServerId || Boolean(installingAgentKind)}>{installingAgentKind ? <RotateCw className="animate-spin" /> : <Download />}{installingAgentKind ? "Starting installer…" : `Install ${selectedProfileInstall.label}`}</Button>
              : <Button type="submit" disabled={creatingAgent || Boolean(installingAgentKind) || !agentServerId || (agentProfileId === "recipe" && (!agentRecipeUrl.trim() || !agentRecipeKind))}>{creatingAgent ? "Starting…" : agentProfileId === "terminal" ? "Create terminal" : agentProfileId === "recipe" ? "Install recipe" : "Create agent"}</Button>}
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
    <Dialog open={createAppOpen} onOpenChange={setCreateAppOpen}><DialogContent className="max-w-xl"><form onSubmit={createAppDeployment} className="grid gap-4 sm:grid-cols-2"><DialogHeader className="sm:col-span-2"><DialogTitle>New App</DialogTitle><DialogDescription>Run a managed HTTP App with a stable workspace hostname.</DialogDescription></DialogHeader><Field label="Name"><Input aria-label="Name" value={appName} onChange={(event) => setAppName(event.target.value)} required autoFocus maxLength={80} /></Field><Field label="Machine"><Select value={appServerId} onValueChange={setAppServerId} required><SelectTrigger aria-label="Machine"><SelectValue placeholder="Select an online machine" /></SelectTrigger><SelectContent>{onlineMachines.map((machine) => <SelectItem key={machine.server_id} value={machine.server_id}>{machineName(machine)}</SelectItem>)}</SelectContent></Select></Field><div className="sm:col-span-2"><Field label="Command"><Input aria-label="Command" className="font-mono" value={appCommandLine} onChange={(event) => setAppCommandLine(event.target.value)} placeholder="python3 -m http.server 8080" required /></Field></div><Field label="Working directory"><Input aria-label="Working directory" className="font-mono" value={appCwd} onChange={(event) => setAppCwd(event.target.value)} required /></Field><Field label="UI port"><Input aria-label="UI port" className="font-mono" type="number" min="1" max="65535" value={appPort} onChange={(event) => setAppPort(event.target.value)} placeholder="8080" required /></Field><div className="sm:col-span-2"><Field label="Virtual hostname"><Input aria-label="Virtual hostname" className="font-mono" value={appHostname} onChange={(event) => setAppHostname(event.target.value)} placeholder="my-app.internal" required /></Field></div><DialogFooter className="sm:col-span-2"><Button type="button" variant="outline" onClick={() => setCreateAppOpen(false)}>Cancel</Button><Button type="submit" disabled={!appServerId}><Play />Create App</Button></DialogFooter></form></DialogContent></Dialog>

    <Dialog open={Boolean(deletingApp)} onOpenChange={(open) => !open && setDeletingApp(null)}><DialogContent><DialogHeader><DialogTitle>Delete App</DialogTitle><DialogDescription>Delete {deletingApp?.name}, stop its process, and remove its service and virtual hostname?</DialogDescription></DialogHeader><DialogFooter><Button variant="outline" onClick={() => setDeletingApp(null)}>Cancel</Button><Button variant="destructive" onClick={deleteAppDeployment}>Delete App</Button></DialogFooter></DialogContent></Dialog>


    <Dialog open={installOpen} onOpenChange={setInstallOpen}><DialogContent className="max-w-xl" data-tour="add-machine-dialog"><DialogHeader><DialogTitle>Add machine</DialogTitle><DialogDescription>Install Treer, then connect this workspace.</DialogDescription></DialogHeader><div className="space-y-4"><Field label="1. Install Treer"><div className="space-y-2"><Textarea readOnly value={installCommand} className="min-h-20 font-mono text-xs" /><Button size="sm" variant="outline" onClick={() => copy(installCommand)}><Copy />Copy install command</Button></div></Field><Field label="2. Connect workspace"><div className="space-y-2"><Textarea readOnly value={connectCommand} className="min-h-24 font-mono text-xs" /><Button size="sm" onClick={() => copy(connectCommand)}><Copy />Copy connection command</Button></div></Field></div><DialogFooter><Button variant="outline" onClick={() => setInstallOpen(false)}>Close</Button></DialogFooter></DialogContent></Dialog>

    <Dialog open={createServiceOpen} onOpenChange={(open) => { setCreateServiceOpen(open); if (!open) setEditingService(null) }}>
      <DialogContent className="max-w-xl">
        <form onSubmit={createService} className="grid gap-4 sm:grid-cols-2">
          <DialogHeader className="sm:col-span-2">
            <DialogTitle>{editingService ? "Edit service" : "Register service"}</DialogTitle>
            <DialogDescription>{editingService ? "Update this service without changing its scope." : "Register a machine service or a service on an Agent's private loopback."}</DialogDescription>
          </DialogHeader>
          <Field label="Service name"><Input value={serviceName} onChange={(event) => setServiceName(event.target.value)} placeholder="API server" required autoFocus /></Field>
          <Field label="Scope">
            <Select value={serviceTargetAgentId ?? "machine"} disabled={Boolean(editingService)} onValueChange={(value) => {
              if (value === "machine") { setServiceTargetAgentId(null); return }
              setServiceTargetAgentId(value)
              const agent = snapshot?.agents.find((item) => item.agent_id === value)
              if (agent) setServiceServerId(agent.server_id)
              setServiceTargetHost("127.0.0.1")
            }}>
              <SelectTrigger><SelectValue /></SelectTrigger>
              <SelectContent><SelectItem value="machine">Machine</SelectItem>{snapshot?.agents.map((agent) => <SelectItem key={agent.agent_id} value={agent.agent_id}>{agent.name} · {machineName(snapshot?.servers.find((machine) => machine.server_id === agent.server_id), agent.server_id)}</SelectItem>)}</SelectContent>
            </Select>
          </Field>
          <Field label="Machine"><Select value={serviceServerId} onValueChange={setServiceServerId} disabled={Boolean(serviceTargetAgentId)} required><SelectTrigger><SelectValue placeholder="Select machine" /></SelectTrigger><SelectContent>{snapshot?.servers.map((machine) => <SelectItem key={machine.server_id} value={machine.server_id}>{machineName(machine)}</SelectItem>)}</SelectContent></Select></Field>
          <Field label="Target host"><Input className="font-mono" value={serviceTargetHost} onChange={(event) => setServiceTargetHost(event.target.value)} disabled={Boolean(serviceTargetAgentId)} required /></Field>
          <Field label="Target port"><Input type="number" min="1" max="65535" value={serviceTargetPort} onChange={(event) => setServiceTargetPort(event.target.value)} required /></Field>
          <Field label="Protocol"><Select value={serviceProtocol} onValueChange={(value: "tcp" | "http") => setServiceProtocol(value)}><SelectTrigger><SelectValue /></SelectTrigger><SelectContent><SelectItem value="http">HTTP</SelectItem><SelectItem value="tcp">TCP</SelectItem></SelectContent></Select></Field>
          <DialogFooter className="sm:col-span-2"><Button type="button" variant="outline" onClick={() => setCreateServiceOpen(false)}>Cancel</Button><Button type="submit" disabled={!serviceName || !serviceServerId || !serviceTargetPort}><Server />{editingService ? "Save service" : "Register service"}</Button></DialogFooter>
        </form>
      </DialogContent>
    </Dialog>

    <Dialog open={createVirtualHostOpen} onOpenChange={setCreateVirtualHostOpen}><DialogContent><form onSubmit={createVirtualHost} className="space-y-4"><DialogHeader><DialogTitle>Add virtual host</DialogTitle><DialogDescription>Map a workspace hostname to a registered machine service.</DialogDescription></DialogHeader><Field label="Virtual hostname"><Input className="font-mono" value={virtualHostname} onChange={(event) => setVirtualHostname(event.target.value)} placeholder="app.internal" required autoFocus /></Field><Field label="Service"><Select value={virtualServiceId} onValueChange={setVirtualServiceId} required><SelectTrigger><SelectValue placeholder="Select service" /></SelectTrigger><SelectContent>{services.map((service) => <SelectItem key={service.service_id} value={service.service_id}>{service.name} · {service.target_host}:{service.target_port}</SelectItem>)}</SelectContent></Select></Field><DialogFooter><Button type="button" variant="outline" onClick={() => setCreateVirtualHostOpen(false)}>Cancel</Button><Button type="submit" disabled={!virtualHostname || !virtualServiceId}><Plus />Add host</Button></DialogFooter></form></DialogContent></Dialog>

    <Dialog open={Boolean(renameTarget)} onOpenChange={(open) => !open && setRenameTarget(null)}><DialogContent><form onSubmit={submitRename}><DialogHeader><DialogTitle>Rename {renameTarget?.kind}</DialogTitle><DialogDescription>Choose a clear name for this {renameTarget?.kind}.</DialogDescription></DialogHeader><div className="my-5"><Field label="Name"><Input value={renameName} onChange={(event) => setRenameName(event.target.value)} required autoFocus /></Field></div><DialogFooter><Button type="button" variant="outline" onClick={() => setRenameTarget(null)}>Cancel</Button><Button type="submit">Rename</Button></DialogFooter></form></DialogContent></Dialog>

    <Dialog open={Boolean(deleteTarget)} onOpenChange={(open) => !open && setDeleteTarget(null)}><DialogContent><DialogHeader><DialogTitle>Delete {deleteTarget?.kind}</DialogTitle><DialogDescription>{deleteTarget?.kind === "machine" ? `Remove ${deleteTarget.name} and all of its agents? Its credential will be revoked, but its local service will not be uninstalled.` : `Delete ${deleteTarget?.name} and stop its process? This agent will not return after reconnecting.`}</DialogDescription></DialogHeader><DialogFooter><Button variant="outline" onClick={() => setDeleteTarget(null)}>Cancel</Button><Button variant="destructive" onClick={confirmDelete}>Delete</Button></DialogFooter></DialogContent></Dialog>
    <Dialog open={deleteWorkspaceOpen} onOpenChange={(open) => !open && setDeleteWorkspaceOpen(false)}><DialogContent><DialogHeader><DialogTitle>Delete workspace</DialogTitle><DialogDescription>{workspaceMachineCount === undefined ? "Checking the workspace machine inventory..." : workspaceMachineCount > 0 ? `Delete all ${workspaceMachineCount} ${workspaceMachineCount === 1 ? "machine" : "machines"} from ${workspace?.name ?? "this workspace"} first.` : `Delete ${workspace?.name}? It will disappear from active views while historical traffic and messages are retained. This cannot be undone.`}</DialogDescription></DialogHeader><DialogFooter><Button variant="outline" onClick={() => setDeleteWorkspaceOpen(false)}>Cancel</Button><Button variant="destructive" disabled={workspaceMachineCount !== 0} onClick={() => void confirmDeleteWorkspace()}>Delete workspace</Button></DialogFooter></DialogContent></Dialog>

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

function SimpleNameDialog({ open, onOpenChange, title, description, label, value, onValueChange, onSubmit, dataTour }: { open: boolean; onOpenChange: (open: boolean) => void; title: string; description: string; label: string; value: string; onValueChange: (value: string) => void; onSubmit: (event: FormEvent) => void; dataTour?: string }) {
  return <Dialog open={open} onOpenChange={onOpenChange}><DialogContent data-tour={dataTour}><form onSubmit={onSubmit}><DialogHeader><DialogTitle>{title}</DialogTitle><DialogDescription>{description}</DialogDescription></DialogHeader><div className="my-5"><Field label={label}><Input value={value} onChange={(event) => onValueChange(event.target.value)} required autoFocus maxLength={80} /></Field></div><DialogFooter><Button type="button" variant="outline" onClick={() => onOpenChange(false)}>Cancel</Button><Button type="submit">Create</Button></DialogFooter></form></DialogContent></Dialog>
}

function EmptyState({ icon, label }: { icon: React.ReactNode; label: string }) {
  return <div className="flex flex-col items-center gap-2 px-4 py-8 text-center text-[11px] text-muted-foreground"><span className="[&_svg]:size-4 [&_svg]:opacity-50">{icon}</span>{label}</div>
}

function MachineOverviewView({ machine, agents, services, virtualHosts, traffic, machines, workspaceId, onOpenAgent, onClose, onCopy }: { machine?: Machine; agents: Agent[]; services: MachineService[]; virtualHosts: VirtualNetworkHost[]; traffic: MachineTrafficRecord[]; machines: Machine[]; workspaceId?: string | null; onOpenAgent: (agentId: string) => void; onClose: () => void; onCopy: (value: string) => void }) {
  const [localHealth, setLocalHealth] = useState<Record<string, "healthy" | "unreachable">>({})
  const outBytes = traffic.filter((t) => (t.source_type ?? "machine") === "machine" && t.source_server_id === machine?.server_id).reduce((sum, t) => sum + (t.billable_bytes ?? t.payload_bytes), 0)
  const inBytes = traffic.filter((t) => (t.destination_type ?? "machine") === "machine" && t.destination_server_id === machine?.server_id).reduce((sum, t) => sum + (t.billable_bytes ?? t.payload_bytes), 0)
  const peers = Array.from(new Set(
    traffic
      .filter((t) => t.source_server_id === machine?.server_id || t.destination_server_id === machine?.server_id)
      .map((t) => (t.source_server_id === machine?.server_id ? t.destination_server_id : t.source_server_id)),
  )).map((id) => machines.find((item) => item.server_id === id)).filter((item): item is Machine => Boolean(item))

  async function probe(serviceId: string) {
    try {
      await api(`/api/services/${encodeURIComponent(serviceId)}/probe`, { method: "POST", body: "{}" })
      setLocalHealth((current) => ({ ...current, [serviceId]: "healthy" }))
    } catch {
      setLocalHealth((current) => ({ ...current, [serviceId]: "unreachable" }))
    }
  }

  if (!machine) return <div className="grid min-h-0 flex-1 place-items-center p-8 text-sm text-muted-foreground">Machine not found (or disconnected). <Button variant="outline" className="mt-3" onClick={onClose}>Back</Button></div>
  const controller = buildLabel(machine.controller_build)
  const host = buildLabel(machine.host_build)
  const supervision = machine.supervision ? supervisionLabel(machine.supervision.mode) : "Unknown"

  return <div className="min-h-0 overflow-auto"><div className="mx-auto w-full max-w-[1120px] px-5 py-8 sm:px-8 sm:py-12 lg:px-14">
    <div className="mb-8 flex items-start justify-between gap-4">
      <div>
        <div className="mb-3 flex items-center gap-3">
          <span className={cn("inline-flex size-2.5 rounded-full", machine.status === "online" ? "bg-emerald-500" : "bg-zinc-400")} />
          <h1 className="truncate text-2xl font-semibold">{machineDisplayName(machine, machines)}</h1>
          <span className="text-xs uppercase text-muted-foreground">{machine.status}</span>
        </div>
        <p className="font-mono text-xs text-muted-foreground">{machine.server_id}</p>
        {machine.hostname && <p className="mt-1 font-mono text-xs text-muted-foreground">{machine.hostname}{machineListen(machine) ? ` · ${machineListen(machine)}` : ""}</p>}
        <p className="mt-2 max-w-2xl break-all font-mono text-[11px] text-muted-foreground" title={machine.root}>{machine.root}</p>
        {machine.status !== "online" && workspaceId && <MachineRecovery workspaceId={workspaceId} onCopy={onCopy} reason="This machine is not connected to the control plane. It may be stopped, waking from sleep, or fenced as a duplicate." />}
      </div>
      <Button variant="outline" className="shrink-0" onClick={onClose}>Close</Button>
    </div>

    <div className="grid gap-10 md:grid-cols-2">
      <section className="space-y-3 rounded-md border p-4">
        <h2 className="text-sm font-semibold">Build</h2>
        <dl className="grid gap-2 text-xs">
          <div className="flex justify-between gap-3"><dt className="text-muted-foreground">Controller</dt><dd className="truncate font-mono">{controller}</dd></div>
          <div className="flex justify-between gap-3"><dt className="text-muted-foreground">Host</dt><dd className="truncate font-mono">{host}</dd></div>
          <div className="flex justify-between gap-3"><dt className="text-muted-foreground">Supervision</dt><dd className="truncate font-mono">{supervision}</dd></div>
          <div className="flex justify-between gap-3"><dt className="text-muted-foreground">Controller commit</dt><dd className="truncate font-mono">{machine.controller_build.git_commit.slice(0, 10)}</dd></div>
          <div className="flex justify-between gap-3"><dt className="text-muted-foreground">Host commit</dt><dd className="truncate font-mono">{machine.host_build.git_commit.slice(0, 10)}</dd></div>
        </dl>
        {machine.supervision?.fallback_reason && <div className="flex gap-2 border-t pt-3 text-[11px] leading-5 text-amber-700"><TriangleAlert className="mt-0.5 size-3.5 shrink-0" /><p><span className="font-medium">{supervisionLabel(machine.supervision.mode)} fallback.</span> {machine.supervision.fallback_reason}</p></div>}
      </section>

      <section className="space-y-3 rounded-md border p-4">
        <h2 className="text-sm font-semibold">Network (last 24h)</h2>
        <dl className="grid gap-2 text-xs">
          <div className="flex justify-between gap-3"><dt className="text-muted-foreground">Routed out</dt><dd className="font-mono">{formatBytes(outBytes)}</dd></div>
          <div className="flex justify-between gap-3"><dt className="text-muted-foreground">Routed in</dt><dd className="font-mono">{formatBytes(inBytes)}</dd></div>
          <div className="flex justify-between gap-3"><dt className="text-muted-foreground">Peers</dt><dd className="font-mono">{peers.length}</dd></div>
        </dl>
        {peers.length > 0 && <div className="flex flex-wrap gap-1.5 pt-2">{peers.map((peer) => <span key={peer.server_id} className="rounded-full bg-accent px-2 py-1 text-[10px] font-medium">{machineName(peer)}</span>)}</div>}
      </section>
    </div>

    <section className="mt-10">
      <h2 className="mb-3 text-sm font-semibold">Agents on this machine</h2>
      {agents.length ? <div className="border-y">{agents.map((agent) => <div key={agent.agent_id} className="grid min-h-14 grid-cols-[minmax(0,1fr)_auto] items-center gap-3 border-b py-3 last:border-b-0">
        <div className="min-w-0">
          <div className="truncate text-xs font-medium">{agent.name}</div>
          <div className="mt-1 truncate font-mono text-[9px] text-muted-foreground">{agent.kind} · {agent.agent_id}</div>
        </div>
        <div className="flex items-center gap-2"><Status value={agentDisplayStatus(agent, machine)} /><Button size="sm" variant="outline" onClick={() => onOpenAgent(agent.agent_id)}>Terminal</Button></div>
      </div>)}</div> : <EmptyState icon={<TerminalSquare />} label="No agents running on this machine" />}
    </section>

    <section className="mt-10">
      <div className="mb-3 flex items-center justify-between"><h2 className="text-sm font-semibold">Registered services</h2><span className="text-[10px] text-muted-foreground">HTTP / TCP endpoints</span></div>
      {services.length ? <div className="border-y">{services.map((service) => { const health = localHealth[service.service_id]; return <div key={service.service_id} className="grid min-h-14 grid-cols-[minmax(0,1fr)_auto] items-center gap-3 border-b py-3 last:border-b-0">
        <div className="min-w-0">
          <div className="truncate text-xs font-medium">{service.name} <span className="ml-2 font-mono text-[9px] uppercase text-muted-foreground">{service.protocol}</span>{health && <span className={cn("ml-2 text-[9px]", health === "healthy" ? "text-emerald-700" : "text-red-600")}>{health}</span>}</div>
          <div className="mt-1 truncate font-mono text-[10px] text-muted-foreground">{service.target_host}:{service.target_port} · updated {new Date(service.updated_at).toLocaleString()}</div>
        </div>
        <Button size="sm" variant="ghost" onClick={() => probe(service.service_id)} disabled={machine.status !== "online"}>Probe</Button>
      </div>})}</div> : <EmptyState icon={<Server />} label="No services registered for this machine" />}
    </section>

    <section className="mt-10">
      <div className="mb-3 flex items-center justify-between"><h2 className="text-sm font-semibold">Virtual hosts</h2><span className="text-[10px] text-muted-foreground">Public hostnames targeting this machine</span></div>
      {virtualHosts.length ? <div className="border-y">{virtualHosts.map((host) => { const service = services.find((item) => item.service_id === host.service_id); return <div key={host.hostname} className="grid min-h-14 grid-cols-[minmax(0,1fr)_auto] items-center gap-3 border-b py-3 last:border-b-0">
        <div className="min-w-0">
          <div className="truncate font-mono text-xs font-medium">{host.hostname}</div>
          <div className="mt-1 truncate font-mono text-[10px] text-muted-foreground">{service?.name ?? host.service_id} · {host.target_host}{host.target_port ? `:${host.target_port}` : ""}</div>
        </div>
        <Button size="icon" variant="ghost" aria-label={`Open ${host.hostname}`} onClick={() => window.open(`https://${host.hostname}`, "_blank", "noopener")} disabled={service?.protocol !== "http"}><ExternalLink /></Button>
      </div>})}</div> : <EmptyState icon={<Network />} label="No virtual hosts routed to this machine" />}
    </section>
  </div></div>
}

function MachineItem({ machine, machines = [], workspaceId, selected, onClick, onRename, onDelete, onCopy }: { machine: Machine; machines?: Machine[]; workspaceId?: string | null; selected?: boolean; onClick?: () => void; onRename: () => void; onDelete: () => void; onCopy: (value: string) => void }) {
  const builds = `Controller ${buildLabel(machine.controller_build)} · Host ${buildLabel(machine.host_build)}`
  const buildTitle = `Controller ${machine.controller_build.version} (${machine.controller_build.git_commit})\nHost ${machine.host_build.version} (${machine.host_build.git_commit})`
  const commands = workspaceId ? machineRecoveryCommands(workspaceId) : null
  return <div className={cn("group flex min-h-[68px] items-start gap-2 rounded-[5px] px-2.5 py-2 hover:bg-accent", selected && "bg-accent hover:bg-accent")}><button type="button" onClick={onClick} className="flex min-w-0 flex-1 items-start gap-2 text-left"><span className={cn("mt-1.5 size-1.5 shrink-0 rounded-full bg-zinc-400", machine.status === "online" && "bg-emerald-500")} /><div className="min-w-0 flex-1"><div className="truncate text-xs font-medium">{machineDisplayName(machine, machines)}</div><div className="mt-1 truncate font-mono text-[9px] text-muted-foreground">{machine.root}</div><div className="mt-1 truncate font-mono text-[9px] text-muted-foreground" title={buildTitle}>{builds}</div>{machine.status !== "online" && <div className="mt-1 text-[9px] uppercase text-red-600">offline</div>}</div></button><DropdownMenu><DropdownMenuTrigger asChild><Button size="icon" variant="ghost" className="size-7 shrink-0 opacity-0 group-hover:opacity-100 data-[state=open]:opacity-100" aria-label={`Actions for ${machineName(machine)}`}><MoreHorizontal /></Button></DropdownMenuTrigger><DropdownMenuContent align="end"><DropdownMenuItem onSelect={onRename}><Pencil />Rename</DropdownMenuItem>{commands && machine.status !== "online" && <><DropdownMenuItem onSelect={() => onCopy(commands.restartController)}><Copy />Copy restart-controller</DropdownMenuItem><DropdownMenuItem onSelect={() => onCopy(commands.start)}><Copy />Copy start</DropdownMenuItem><DropdownMenuSeparator /></>}<DropdownMenuItem className="text-destructive focus:text-destructive" onSelect={onDelete}><Trash2 />Delete</DropdownMenuItem></DropdownMenuContent></DropdownMenu></div>
}

function AgentItem({ agent, machine, selected, onClick, onRename, onStop, onDelete }: { agent: Agent; machine?: Machine; selected: boolean; onClick: () => void; onRename: () => void; onStop: () => void; onDelete: () => void }) {
  const running = machineOnline(machine) && activeStatuses.has(agent.status)
  return <div className={cn("group flex min-h-12 items-center gap-2 rounded-[5px] px-2.5 py-2 hover:bg-accent", selected && "bg-accent hover:bg-accent")}><button type="button" onClick={onClick} className="min-w-0 flex-1 text-left"><span className="block truncate text-xs font-medium">{agent.name}</span><span className="mt-1 block truncate text-[9px] text-muted-foreground">{agent.kind}{agent.interface ? " · AIS" : ""} · {machineName(machine, agent.server_id)}</span></button><div className="flex shrink-0 flex-col items-end gap-0.5"><Status value={agentDisplayStatus(agent, machine)} /><DropdownMenu><DropdownMenuTrigger asChild><Button size="icon" variant="ghost" className={cn("size-6 opacity-0 group-hover:opacity-100 group-focus-within:opacity-100 data-[state=open]:opacity-100 max-md:opacity-100", selected && "opacity-100")} aria-label={`Actions for ${agent.name}`}><MoreVertical className="size-3.5" /></Button></DropdownMenuTrigger><DropdownMenuContent align="end"><DropdownMenuItem onSelect={onRename}><Pencil />Rename</DropdownMenuItem><DropdownMenuItem disabled={!running} onSelect={onStop}><Square />Stop</DropdownMenuItem><DropdownMenuSeparator /><DropdownMenuItem className="text-destructive focus:text-destructive" onSelect={onDelete}><Trash2 />Delete</DropdownMenuItem></DropdownMenuContent></DropdownMenu></div></div>
}

export default function App() {
  return <Routes>
    <Route path="/admin" element={<AdminPanel />} />
    <Route path="/orgs/:organizationId/workspaces/:workspaceId" element={<WorkspaceApp />} />
    <Route path="/orgs/:organizationId" element={<WorkspaceApp />} />
    <Route path="*" element={<WorkspaceApp />} />
  </Routes>
}
