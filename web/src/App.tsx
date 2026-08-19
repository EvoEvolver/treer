import { FormEvent, useCallback, useEffect, useState } from "react"
import type * as React from "react"
import {
  ChevronRight,
  CirclePlus,
  Copy,
  ExternalLink,
  FolderKanban,
  GitBranch,
  KeyRound,
  LogOut,
  Mail,
  MoreHorizontal,
  Network,
  Pencil,
  Plus,
  RotateCw,
  Search,
  Server,
  Square,
  ShieldCheck,
  TerminalSquare,
  Trash2,
  UserRound,
  Users,
} from "lucide-react"
import { api, ApiError, machineName, proxyUrl, websocketUrl, type AdminDashboard, type Agent, type Machine, type MachineService, type MailDelivery, type MailMessage, type MailboxResponse, type Member, type Organization, type ServiceIngress, type Snapshot, type User, type VirtualNetworkHost, type Workspace } from "@/lib/api"
import { cn } from "@/lib/utils"
import { TerminalPane } from "@/components/terminal-pane"
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
type MainView = "terminal" | "inbox" | "network"
type AuthMode = "login" | "register" | "forgot" | "reset"
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
  const prefix = kind === "command" ? "cmd" : kind === "codex" || kind === "claude" ? kind : "agent"
  return `${prefix}-${now.getFullYear()}-${month}-${day}`
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

function AuthScreen({ onAuthenticated }: { onAuthenticated: (user: User) => void }) {
  const parameters = new URLSearchParams(window.location.search)
  const invite = parameters.get("invite")
  const resetToken = parameters.get("reset")
  const [mode, setMode] = useState<AuthMode>(resetToken ? "reset" : invite ? "register" : "login")
  const [email, setEmail] = useState("")
  const [preferredName, setPreferredName] = useState("")
  const [password, setPassword] = useState("")
  const [passwordConfirmation, setPasswordConfirmation] = useState("")
  const [error, setError] = useState("")
  const [notice, setNotice] = useState("")
  const [submitting, setSubmitting] = useState(false)

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

  const title = mode === "register" ? "Join Treer" : mode === "forgot" ? "Reset your password" : mode === "reset" ? "Choose a new password" : "Sign in to Treer"
  const description = mode === "register" ? "Create your account from this invitation." : mode === "forgot" ? "Enter the email associated with your account." : mode === "reset" ? "Use at least 8 characters for your new password." : "Open your agent workspace."

  return <main className="grid min-h-dvh place-items-center bg-[#f7f7f5] p-4">
    <form onSubmit={submit} className="w-full max-w-[390px] rounded-lg border bg-background p-7 shadow-sm">
      <div className="mb-6 grid size-9 place-items-center rounded-md bg-[#37352f] font-serif text-lg font-bold text-white">T</div>
      <h1 className="text-xl font-semibold">{title}</h1>
      <p className="mt-1 text-sm text-muted-foreground">{description}</p>
      {mode !== "reset" && <div className="mt-6 space-y-2"><Label htmlFor="email">Email</Label><Input id="email" type="email" autoComplete="email" value={email} maxLength={254} onChange={(event) => setEmail(event.target.value)} required autoFocus /></div>}
      {mode === "register" && <div className="mt-4 space-y-2"><Label htmlFor="preferred-name">Preferred name</Label><Input id="preferred-name" autoComplete="name" value={preferredName} maxLength={80} onChange={(event) => setPreferredName(event.target.value)} required /></div>}
      {(mode === "login" || mode === "register" || mode === "reset") && <div className={cn("space-y-2", mode === "reset" ? "mt-6" : "mt-4")}><Label htmlFor="password">{mode === "reset" ? "New password" : "Password"}</Label><Input id="password" type="password" autoComplete={mode === "login" ? "current-password" : "new-password"} value={password} minLength={mode === "login" ? undefined : 8} maxLength={1024} onChange={(event) => setPassword(event.target.value)} required autoFocus={mode === "reset"} /></div>}
      {mode === "reset" && <div className="mt-4 space-y-2"><Label htmlFor="password-confirmation">Confirm new password</Label><Input id="password-confirmation" type="password" autoComplete="new-password" value={passwordConfirmation} minLength={8} maxLength={1024} onChange={(event) => setPasswordConfirmation(event.target.value)} required /></div>}
      {mode === "login" && <div className="mt-2 text-right"><Button type="button" variant="ghost" size="sm" className="h-auto px-0 py-1 text-xs text-muted-foreground" onClick={() => { setMode("forgot"); setError(""); setNotice("") }}>Forgot password?</Button></div>}
      <div className="mt-3 min-h-5 text-xs text-destructive">{error}</div>
      {notice && <div className="mt-2 text-xs leading-5 text-emerald-700">{notice}</div>}
      <div className="mt-4 flex items-center justify-between gap-3">
        {(mode === "register" || mode === "forgot" || mode === "reset") && <Button type="button" variant="ghost" className="px-0 text-primary" onClick={() => showSignIn()}>Back to sign in</Button>}
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

type MailTraceEntry = { id: string; message?: MailMessage; current: boolean }

function mailSubject(message: MailMessage) {
  return message.body.split("\n", 1)[0]?.trim() || "Untitled message"
}

function formatMailTime(value: string, compact = false) {
  const date = new Date(value)
  if (!compact) return date.toLocaleString([], { dateStyle: "medium", timeStyle: "short" })
  const today = new Date()
  if (date.toDateString() === today.toDateString()) return date.toLocaleTimeString([], { hour: "numeric", minute: "2-digit" })
  return date.toLocaleDateString([], { month: "short", day: "numeric" })
}

function buildMailTrace(messages: MailMessage[], selected: MailMessage | undefined): MailTraceEntry[] {
  if (!selected) return []
  const byId = new Map(messages.map((message) => [message.message_id, message]))
  const visited = new Set<string>()
  const entries: MailTraceEntry[] = []
  const visit = (id: string) => {
    if (visited.has(id)) return
    visited.add(id)
    const message = byId.get(id)
    if (message) message.context_ids.forEach(visit)
    entries.push({ id, message, current: id === selected.message_id })
  }
  selected.context_ids.forEach(visit)
  visit(selected.message_id)
  return entries
}

function InboxView({ deliveries, selectedMessageId, query, loading, remainingUnread, onQueryChange, onSelect, onCopy }: {
  deliveries: MailDelivery[]
  selectedMessageId: string | null
  query: string
  loading: boolean
  remainingUnread: number
  onQueryChange: (value: string) => void
  onSelect: (messageId: string) => void
  onCopy: (value: string) => void
}) {
  const messages = deliveries.map((delivery) => delivery.message)
  const normalizedQuery = query.trim().toLowerCase()
  const visibleDeliveries = deliveries.filter(({ message }) => !normalizedQuery || [message.message_id, message.sender.name, message.sender.id, message.body, ...message.recipients.flatMap((recipient) => [recipient.name, recipient.id])].some((value) => value.toLowerCase().includes(normalizedQuery)))
  const selected = messages.find((message) => message.message_id === selectedMessageId)
  const trace = buildMailTrace(messages, selected)

  return <div className="grid min-h-0 grid-rows-[minmax(230px,42%)_minmax(360px,1fr)] bg-background lg:grid-cols-[340px_minmax(0,1fr)] lg:grid-rows-1">
    <section className="flex min-h-0 flex-col border-b bg-[#fbfbfa] lg:border-b-0 lg:border-r" aria-label="Message list">
      <div className="shrink-0 border-b px-3 py-3">
        <div className="relative"><Search className="pointer-events-none absolute left-2.5 top-1/2 size-3.5 -translate-y-1/2 text-muted-foreground" /><Input value={query} onChange={(event) => onQueryChange(event.target.value)} placeholder="Search messages" className="h-8 bg-background pl-8 text-xs" /></div>
        <div className="mt-2 flex items-center justify-between px-0.5 text-[10px] text-muted-foreground"><span>{visibleDeliveries.length} messages</span>{remainingUnread > 0 && <span>{remainingUnread} older unread</span>}</div>
      </div>
      <div className="min-h-0 flex-1 overflow-auto">
        {visibleDeliveries.map(({ message, unread }) => <button key={message.message_id} onClick={() => onSelect(message.message_id)} className={cn("grid w-full grid-cols-[8px_minmax(0,1fr)_auto] gap-x-2 border-b px-3 py-3 text-left hover:bg-black/[.035]", selectedMessageId === message.message_id && "bg-[#eef4f8] hover:bg-[#eef4f8]")}>
          <span className={cn("mt-1.5 size-1.5 rounded-full", unread ? "bg-sky-500" : "bg-transparent")} />
          <span className="min-w-0"><span className="flex min-w-0 items-center gap-2"><span className={cn("truncate text-xs", unread ? "font-semibold" : "font-medium")}>{message.sender.name}</span><span className="shrink-0 text-[9px] uppercase text-muted-foreground">{message.sender.kind}</span></span><span className="mt-1 block truncate text-[11px] font-medium text-foreground/90">{mailSubject(message)}</span><span className="mt-1 block truncate text-[10px] text-muted-foreground">{message.body.replace(/\s+/g, " ")}</span></span>
          <span className="pt-0.5 text-[9px] text-muted-foreground">{formatMailTime(message.created_at, true)}</span>
        </button>)}
        {!loading && !visibleDeliveries.length && <EmptyState icon={<Mail />} label={query ? "No messages match this search" : "No messages in this workspace"} />}
        {loading && <div className="px-4 py-8 text-center text-xs text-muted-foreground">Loading messages...</div>}
      </div>
    </section>

    <section className="min-h-0 overflow-auto" aria-label="Message reader">
      {selected ? <article className="mx-auto w-full max-w-[900px] px-5 py-7 sm:px-8 sm:py-10 lg:px-12">
        <header className="border-b pb-6">
          <div className="flex items-start justify-between gap-4"><div className="min-w-0"><h1 className="break-words text-xl font-semibold sm:text-2xl">{mailSubject(selected)}</h1><div className="mt-4 flex items-center gap-3"><span className={cn("grid size-8 shrink-0 place-items-center rounded-[5px] text-[10px] font-bold", selected.sender.kind === "agent" ? "bg-[#dcecf8] text-[#315f7d]" : "bg-[#f7dfea] text-[#824e67]")}>{initials(selected.sender.name)}</span><span className="min-w-0"><span className="block truncate text-xs font-medium">{selected.sender.name}</span><span className="block truncate font-mono text-[9px] text-muted-foreground">{selected.sender.id}</span></span></div></div><time className="shrink-0 pt-1 text-[10px] text-muted-foreground">{formatMailTime(selected.created_at)}</time></div>
          <div className="mt-4 grid grid-cols-[34px_minmax(0,1fr)] gap-2 text-[10px] text-muted-foreground"><span>To</span><span className="min-w-0 break-words">{selected.recipients.map((recipient) => `${recipient.name} (${recipient.kind})`).join(", ")}</span><span>ID</span><button className="min-w-0 truncate text-left font-mono hover:text-foreground" onClick={() => onCopy(selected.message_id)} title="Copy message ID">{selected.message_id}</button></div>
        </header>
        <div className="whitespace-pre-wrap break-words py-8 text-sm leading-7 text-foreground">{selected.body}</div>
        <section className="border-t pt-6"><div className="mb-4 flex items-center gap-2 text-xs font-semibold"><GitBranch className="size-3.5 text-muted-foreground" />Message trace</div><div className="ml-1.5 border-l pl-5">{trace.map((entry) => <button key={entry.id} disabled={!entry.message} onClick={() => entry.message && onSelect(entry.id)} className={cn("relative block w-full py-2.5 text-left", entry.message && !entry.current && "hover:text-primary")}><span className={cn("absolute -left-[24px] top-4 size-1.5 rounded-full ring-4 ring-background", entry.current ? "bg-sky-500" : entry.message ? "bg-zinc-400" : "bg-amber-400")} />{entry.message ? <><span className="flex min-w-0 items-center gap-2"><span className="truncate text-[11px] font-medium">{entry.message.sender.name}</span><span className="shrink-0 text-[9px] text-muted-foreground">{formatMailTime(entry.message.created_at, true)}</span>{entry.current && <span className="text-[9px] font-medium text-sky-700">Current</span>}</span><span className="mt-1 block truncate text-[10px] text-muted-foreground">{mailSubject(entry.message)}</span></> : <><span className="block text-[11px] font-medium text-amber-700">Referenced message unavailable</span><span className="mt-1 block truncate font-mono text-[9px] text-muted-foreground">{entry.id}</span></>}</button>)}</div></section>
      </article> : <div className="grid h-full min-h-[300px] place-items-center"><EmptyState icon={<Mail />} label={deliveries.length ? "Select a message to read" : "Your inbox is empty"} /></div>}
    </section>
  </div>
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
  const [error, setError] = useState<string | null>(null)
  const [createOrganizationOpen, setCreateOrganizationOpen] = useState(false)
  const [createWorkspaceOpen, setCreateWorkspaceOpen] = useState(false)
  const [createAgentOpen, setCreateAgentOpen] = useState(false)
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
  const [agentName, setAgentName] = useState(defaultAgentName("codex"))
  const [agentNameCustomized, setAgentNameCustomized] = useState(false)
  const [agentKind, setAgentKind] = useState("codex")
  const [agentServerId, setAgentServerId] = useState("")
  const [agentCwd, setAgentCwd] = useState(".")
  const [agentArgs, setAgentArgs] = useState("")
  const [renameName, setRenameName] = useState("")
  const [installCommand, setInstallCommand] = useState("")
  const [connectCommand, setConnectCommand] = useState("")
  const [inviteUrl, setInviteUrl] = useState("")
  const [members, setMembers] = useState<Member[]>([])
  const [mailDeliveries, setMailDeliveries] = useState<MailDelivery[]>([])
  const [selectedMessageId, setSelectedMessageId] = useState<string | null>(null)
  const [inboxLoading, setInboxLoading] = useState(false)
  const [inboxQuery, setInboxQuery] = useState("")
  const [remainingUnread, setRemainingUnread] = useState(0)
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
    setMailDeliveries([])
    setSelectedMessageId(null)
    setInboxQuery("")
    setRemainingUnread(0)
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
  const organization = organizations.find((item) => item.organization_id === organizationId)
  const workspace = workspaces.find((item) => item.workspace_id === workspaceId)
  const terminalActive = Boolean(selectedAgent && activeStatuses.has(selectedAgent.status))
  const setTerminalState = useCallback((value: TerminalState) => setTerminalStatus(value), [])
  const currentRole = organization?.role ?? "member"
  const canManageMembers = ["owner", "admin"].includes(currentRole)

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
      const agent = await api<Agent>(`/api/workspaces/${encodeURIComponent(workspaceId)}/agents`, { method: "POST", body: JSON.stringify({ server_id: agentServerId, kind: agentKind, name: agentName, cwd: agentCwd, args: agentArgs.split("\n").map((value) => value.trim()).filter(Boolean), cols: 120, rows: 36 }) })
      setCreateAgentOpen(false); setSelectedAgentId(agent.agent_id); await refreshSnapshot()
    } catch (reason) { showError(reason) }
  }

  function openCreateAgent() {
    setAgentName(defaultAgentName(agentKind))
    setAgentNameCustomized(false)
    setCreateAgentOpen(true)
  }

  function changeAgentKind(kind: string) {
    setAgentKind(kind)
    if (!agentNameCustomized) setAgentName(defaultAgentName(kind))
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

  async function loadInbox() {
    if (!workspaceId) return
    setInboxLoading(true)
    try {
      const data = await api<MailboxResponse>(`/api/workspaces/${encodeURIComponent(workspaceId)}/inbox`, { method: "POST", body: JSON.stringify({ limit: 100 }) })
      setMailDeliveries(data.deliveries)
      setRemainingUnread(data.remaining_unread)
      setSelectedMessageId((current) => current && data.deliveries.some((delivery) => delivery.message.message_id === current) ? current : data.deliveries[0]?.message.message_id ?? null)
    } catch (reason) { showError(reason) }
    finally { setInboxLoading(false) }
  }

  function openInbox() {
    setMainView("inbox")
    void loadInbox()
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
    <main className="grid h-dvh min-h-0 grid-rows-[310px_minmax(620px,1fr)] overflow-auto bg-background md:grid-cols-[272px_minmax(0,1fr)] md:grid-rows-1 md:overflow-hidden">
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
          <Button variant={mainView === "inbox" ? "secondary" : "ghost"} className="h-8 w-full justify-start px-2 text-xs font-normal" onClick={openInbox} disabled={!workspaceId}><Mail className="size-3.5" />Inbox</Button>
          <Button variant={mainView === "network" ? "secondary" : "ghost"} className="h-8 w-full justify-start px-2 text-xs font-normal" onClick={openNetwork} disabled={!workspaceId}><Network className="size-3.5" />Network</Button>
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
          <div className="flex min-w-0 items-center gap-1.5 overflow-hidden text-xs text-muted-foreground"><span className="hidden truncate sm:block">{workspace?.name ?? "Workspace"}</span><ChevronRight className="hidden size-3 shrink-0 sm:block" /><strong className="truncate font-medium text-foreground">{mainView === "inbox" ? "Inbox" : mainView === "network" ? "Network" : selectedAgent?.name ?? "Terminal"}</strong></div>
          {mainView === "terminal" ? <div className="flex shrink-0 items-center gap-0.5">
            <IconButton label="Rename agent" disabled={!selectedAgent} onClick={() => selectedAgent && openRename({ kind: "agent", id: selectedAgent.agent_id, name: selectedAgent.name })}><Pencil /></IconButton>
            <IconButton label="Reconnect terminal" disabled={!selectedAgent} onClick={() => { setSelectedAgentId(null); requestAnimationFrame(() => setSelectedAgentId(selectedAgent?.agent_id ?? null)) }}><RotateCw /></IconButton>
            <IconButton label="Stop agent" disabled={!selectedAgent || !terminalActive} onClick={stopAgent}><Square /></IconButton>
            <IconButton label="Delete agent" disabled={!selectedAgent} className="text-destructive hover:text-destructive" onClick={() => selectedAgent && setDeleteTarget({ kind: "agent", id: selectedAgent.agent_id, name: selectedAgent.name })}><Trash2 /></IconButton>
          </div> : mainView === "inbox" ? <div className="flex shrink-0 items-center gap-2"><span className="hidden text-[10px] text-muted-foreground sm:inline">{mailDeliveries.length} messages</span><IconButton label="Refresh inbox" onClick={loadInbox} disabled={inboxLoading}><RotateCw /></IconButton></div> : <div className="flex shrink-0 items-center gap-1"><IconButton label="Refresh network" onClick={refreshNetwork}><RotateCw /></IconButton><Button size="sm" variant="outline" className="h-8" onClick={openCreateService} disabled={!snapshot?.servers.length}><Server />Add service</Button><Button size="sm" variant="outline" className="h-8" onClick={openCreateVirtualHost} disabled={!services.length}><Plus />Add host</Button><Button size="sm" className="h-8" onClick={openPublish} disabled={!services.some((service) => service.protocol === "http")}><ExternalLink />Publish</Button></div>}
        </header>
        {mainView === "terminal" ? <div className="flex min-h-0 justify-center overflow-hidden px-3 pb-4 pt-4 sm:px-8 sm:pb-7 sm:pt-6 lg:px-16">
          <div className="grid h-full min-h-0 w-full max-w-[1120px] grid-rows-[42px_minmax(0,1fr)] overflow-hidden rounded-md border border-zinc-800 bg-[#0f1215] shadow-[0_8px_28px_rgba(15,18,21,.14)]">
            <div className="flex min-w-0 items-center justify-between gap-4 border-b border-zinc-800 bg-[#191d20] px-3.5"><div className="flex min-w-0 items-baseline gap-2"><span className="truncate text-xs font-semibold text-zinc-200">{selectedAgent?.name ?? "Terminal"}</span>{selectedAgent && <span className="hidden truncate font-mono text-[9px] text-zinc-500 sm:block">{selectedAgent.agent_id} · {machineName(snapshot?.servers.find((item) => item.server_id === selectedAgent.server_id))}</span>}</div><span className="inline-flex shrink-0 items-center gap-1.5 text-[9px] uppercase text-zinc-500"><span className="size-1.5 rounded-full bg-current" />{terminalStatus}</span></div>
            <div className="min-h-0 min-w-0 overflow-hidden"><TerminalPane key={`${workspaceId}:${selectedAgentId}`} workspaceId={workspaceId} agentId={selectedAgentId} active={terminalActive} onStatusChange={setTerminalState} /></div>
          </div>
        </div> : mainView === "inbox" ? <InboxView deliveries={mailDeliveries} selectedMessageId={selectedMessageId} query={inboxQuery} loading={inboxLoading} remainingUnread={remainingUnread} onQueryChange={setInboxQuery} onSelect={setSelectedMessageId} onCopy={copy} /> : <div className="min-h-0 overflow-auto"><div className="mx-auto w-full max-w-[1120px] px-5 py-8 sm:px-8 sm:py-12 lg:px-14"><div className="mb-8 flex items-end justify-between gap-4"><div><div className="mb-2 grid size-9 place-items-center rounded-md bg-[#e8deee] text-[#694a73]"><Network className="size-4" /></div><h1 className="text-2xl font-semibold">Network</h1></div><span className="text-xs text-muted-foreground">{services.length} services · {virtualHosts.length} hosts</span></div><section className="mb-10"><h2 className="mb-3 text-sm font-semibold">Machine services</h2><div className="border-y"><div className="hidden h-9 grid-cols-[minmax(150px,1fr)_minmax(180px,1fr)_minmax(140px,1fr)_auto] items-center gap-4 border-b text-[10px] font-medium uppercase text-muted-foreground sm:grid"><span>Service</span><span>Target</span><span>Machine</span><span className="w-24" /></div>{services.map((service) => { const machine = snapshot?.servers.find((item) => item.server_id === service.server_id); const health = serviceHealth[service.service_id]; return <div key={service.service_id} className="grid min-h-16 grid-cols-[minmax(0,1fr)_auto] items-center gap-x-3 gap-y-1 border-b py-3 last:border-b-0 sm:grid-cols-[minmax(150px,1fr)_minmax(180px,1fr)_minmax(140px,1fr)_auto] sm:gap-4"><span className="col-start-1 row-start-1 min-w-0 truncate text-xs font-medium sm:col-start-auto sm:row-start-auto">{service.name}<span className="ml-2 font-mono text-[9px] uppercase text-muted-foreground">{service.protocol}</span>{health && <span className={cn("ml-2 text-[9px]", health === "healthy" ? "text-emerald-700" : "text-red-600")}>{health}</span>}</span><span className="col-start-1 row-start-2 min-w-0 truncate font-mono text-[10px] text-muted-foreground sm:col-start-auto sm:row-start-auto">{service.target_host}:{service.target_port}</span><span className="col-start-1 row-start-3 min-w-0 truncate text-[10px] text-muted-foreground sm:col-start-auto sm:row-start-auto">{machineName(machine, service.server_id)}</span><span className="col-start-2 row-span-3 row-start-1 flex items-center justify-end gap-1 sm:col-start-auto sm:row-span-1 sm:row-start-auto"><IconButton label={`Probe ${service.name}`} onClick={() => probeService(service.service_id)} disabled={machine?.status !== "online"}><RotateCw /></IconButton><IconButton label={`Edit ${service.name}`} onClick={() => openEditService(service)}><Pencil /></IconButton><IconButton label={`Delete ${service.name}`} className="text-destructive hover:text-destructive" onClick={() => deleteService(service.service_id)}><Trash2 /></IconButton></span></div>})}{!services.length && <EmptyState icon={<Server />} label="No machine services" />}</div></section><section><h2 className="mb-3 text-sm font-semibold">Virtual hosts</h2><div className="border-y"><div className="hidden h-9 grid-cols-[minmax(150px,1.2fr)_minmax(180px,1fr)_minmax(140px,1fr)_auto] items-center gap-4 border-b text-[10px] font-medium uppercase text-muted-foreground sm:grid"><span>Hostname</span><span>Service</span><span>Machine</span><span className="w-24" /></div>{virtualHosts.map((host) => { const machine = snapshot?.servers.find((item) => item.server_id === host.destination_server_id); const service = services.find((item) => item.service_id === host.service_id); return <div key={host.hostname} className="grid min-h-16 grid-cols-[minmax(0,1fr)_auto] items-center gap-x-3 gap-y-1 border-b py-3 last:border-b-0 sm:grid-cols-[minmax(150px,1.2fr)_minmax(180px,1fr)_minmax(140px,1fr)_auto] sm:gap-4"><button className="col-start-1 row-start-1 min-w-0 truncate text-left font-mono text-xs font-medium hover:underline sm:col-start-auto sm:row-start-auto" onClick={() => openVirtualHost(host.hostname)}>{host.hostname}</button><span className="col-start-1 row-start-2 min-w-0 truncate text-[10px] text-muted-foreground sm:col-start-auto sm:row-start-auto">{service?.name ?? host.service_id}</span><span className="col-start-1 row-start-3 min-w-0 truncate text-[10px] text-muted-foreground sm:col-start-auto sm:row-start-auto">{machineName(machine, host.destination_server_id)}</span><span className="col-start-2 row-span-3 row-start-1 flex items-center justify-end gap-1 sm:col-start-auto sm:row-span-1 sm:row-start-auto"><IconButton label={`Open ${host.hostname}`} onClick={() => openVirtualHost(host.hostname)} disabled={machine?.status !== "online" || service?.protocol !== "http"}><ExternalLink /></IconButton><IconButton label={`Delete ${host.hostname}`} className="text-destructive hover:text-destructive" onClick={() => deleteVirtualHost(host.hostname)}><Trash2 /></IconButton></span></div>})}{!virtualHosts.length && <EmptyState icon={<Network />} label="No virtual hosts" />}</div></section></div></div>}
      </section>
    </main>

    {error && <div className="fixed bottom-4 left-1/2 z-[90] flex max-w-[calc(100vw-2rem)] -translate-x-1/2 items-center gap-3 rounded-md border bg-background px-4 py-3 text-sm shadow-lg"><span className="truncate">{error}</span><Button size="sm" variant="ghost" onClick={() => setError(null)}>Dismiss</Button></div>}

    <SimpleNameDialog open={createOrganizationOpen} onOpenChange={setCreateOrganizationOpen} title="Create organization" description="Organizations contain members and workspaces." label="Organization name" value={organizationName} onValueChange={setOrganizationName} onSubmit={createOrganization} />
    <SimpleNameDialog open={createWorkspaceOpen} onOpenChange={setCreateWorkspaceOpen} title="Create workspace" description={`Add a workspace to ${organization?.name ?? "this organization"}.`} label="Workspace name" value={workspaceName} onValueChange={setWorkspaceName} onSubmit={createWorkspace} />

    <Dialog open={renameOrganizationOpen} onOpenChange={setRenameOrganizationOpen}><DialogContent><form onSubmit={renameOrganization}><DialogHeader><DialogTitle>Rename organization</DialogTitle><DialogDescription>Update the organization name shown to its members.</DialogDescription></DialogHeader><div className="my-5"><Field label="Organization name"><Input value={organizationName} onChange={(event) => setOrganizationName(event.target.value)} required autoFocus maxLength={80} /></Field></div><DialogFooter><Button type="button" variant="outline" onClick={() => setRenameOrganizationOpen(false)}>Cancel</Button><Button type="submit">Save</Button></DialogFooter></form></DialogContent></Dialog>

    <Dialog open={profileOpen} onOpenChange={setProfileOpen}><DialogContent><form onSubmit={updateProfile}><DialogHeader><DialogTitle>Edit profile</DialogTitle><DialogDescription>Your preferred name is visible to other organization members.</DialogDescription></DialogHeader><div className="my-5 space-y-4"><Field label="Preferred name"><Input value={preferredName} onChange={(event) => setPreferredName(event.target.value)} required autoFocus maxLength={80} /></Field><Field label="Email"><Input type="email" value={profileEmail} onChange={(event) => setProfileEmail(event.target.value)} required maxLength={254} /></Field></div><DialogFooter><Button type="button" variant="outline" onClick={() => setProfileOpen(false)}>Cancel</Button><Button type="submit">Save</Button></DialogFooter></form></DialogContent></Dialog>

    <Dialog open={createAgentOpen} onOpenChange={setCreateAgentOpen}><DialogContent><form onSubmit={createAgent} className="space-y-4"><DialogHeader><DialogTitle>Create agent</DialogTitle><DialogDescription>Start an agent on an online machine in this workspace.</DialogDescription></DialogHeader><Field label="Machine"><Select value={agentServerId} onValueChange={setAgentServerId} required><SelectTrigger><SelectValue placeholder="Select a machine" /></SelectTrigger><SelectContent>{onlineMachines.map((machine) => <SelectItem key={machine.server_id} value={machine.server_id}>{machineName(machine)}</SelectItem>)}</SelectContent></Select></Field><Field label="Kind"><Select value={agentKind} onValueChange={changeAgentKind}><SelectTrigger><SelectValue /></SelectTrigger><SelectContent><SelectItem value="codex">codex</SelectItem><SelectItem value="claude">claude</SelectItem><SelectItem value="command">command</SelectItem></SelectContent></Select></Field><Field label="Name"><Input value={agentName} onChange={(event) => { setAgentName(event.target.value); setAgentNameCustomized(true) }} required /></Field><Field label="Working directory"><Input value={agentCwd} onChange={(event) => setAgentCwd(event.target.value)} /></Field><Field label="Arguments, one per line"><Textarea rows={3} value={agentArgs} onChange={(event) => setAgentArgs(event.target.value)} /></Field><DialogFooter><Button type="button" variant="outline" onClick={() => setCreateAgentOpen(false)}>Cancel</Button><Button type="submit" disabled={!agentServerId}>Create agent</Button></DialogFooter></form></DialogContent></Dialog>

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
  return <div className="group flex min-h-14 items-start gap-2 rounded-[5px] px-2.5 py-2 hover:bg-black/[.045]"><span className={cn("mt-1.5 size-1.5 shrink-0 rounded-full bg-zinc-400", machine.status === "online" && "bg-emerald-500")} /><div className="min-w-0 flex-1"><div className="truncate text-xs font-medium">{machineName(machine)}</div><div className="mt-1 truncate font-mono text-[9px] text-muted-foreground">{machine.root}</div></div><DropdownMenu><DropdownMenuTrigger asChild><Button size="icon" variant="ghost" className="size-7 shrink-0 opacity-0 group-hover:opacity-100 data-[state=open]:opacity-100" aria-label={`Actions for ${machineName(machine)}`}><MoreHorizontal /></Button></DropdownMenuTrigger><DropdownMenuContent align="end"><DropdownMenuItem onSelect={onRename}><Pencil />Rename</DropdownMenuItem><DropdownMenuSeparator /><DropdownMenuItem className="text-destructive focus:text-destructive" onSelect={onDelete}><Trash2 />Delete</DropdownMenuItem></DropdownMenuContent></DropdownMenu></div>
}

function AgentItem({ agent, machine, selected, onClick }: { agent: Agent; machine?: Machine; selected: boolean; onClick: () => void }) {
  return <button onClick={onClick} className={cn("grid min-h-12 w-full grid-cols-[minmax(0,1fr)_auto] items-center gap-2 rounded-[5px] px-2.5 py-2 text-left hover:bg-black/[.045]", selected && "bg-black/[.075] hover:bg-black/[.075]")}><span className="min-w-0"><span className="block truncate text-xs font-medium">{agent.name}</span><span className="mt-1 block truncate text-[9px] text-muted-foreground">{agent.kind} · {machineName(machine, agent.server_id)}</span></span><Status value={agent.status} /></button>
}

export default function App() {
  return window.location.pathname === "/admin" ? <AdminPanel /> : <WorkspaceApp />
}
