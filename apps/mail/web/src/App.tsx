import { FormEvent, useCallback, useEffect, useMemo, useState } from "react"
import { ArrowLeft, Bell, Inbox, LogIn, LogOut, Mail, PenLine, RefreshCw, Reply, Search, Send, X } from "lucide-react"

type Principal = { kind: "agent" | "human"; id: string; name: string; role?: string }
type Session = { workspace_id: string; service_id: string; user: Principal }
type Message = { message_id: string; workspace_id: string; sender: Principal; recipients: Principal[]; context_ids: string[]; body: string; created_at: string }
type Delivery = { message: Message; unread: boolean }
type Mailbox = { deliveries: Delivery[]; remaining_unread: number }
type NotifySettings = { enabled: boolean; address: string | null; default_address: string | null; effective_address: string | null; digest_minutes: number; scope: "direct" | "all" }
type Batch = { batch_id: string; kind: string; status: string; subject: string; message_ids: string[]; attempts: { attempt: number; at: string; result: string; code: string }[]; last_code: string | null; created_at: string }
type NotifyStatus = { available: boolean; settings: NotifySettings; watch: { state: string; grant_expires_at: string | null; last_error: string | null }; pending_messages: number; preview_chars: number; batches: Batch[] }

const startParams = new URLSearchParams(window.location.search)
const linkedMessage = startParams.get("message")
const WATCH_TEXT: Record<string, string> = {
  disabled: "Off.",
  unavailable: "Unavailable: this Treer does not offer the App email route.",
  no_address: "Set an email address to start.",
  login_required: "Paused: Mail can watch your inbox only for 12 hours after you sign in to Mail itself.",
  watching: "On. Mail is watching your inbox.",
}

class ApiError extends Error { constructor(readonly status: number, message: string) { super(message) } }

function applicationUrl(path: string) {
  return new URL(path.replace(/^\/+/, ""), window.location.href).toString()
}

async function api<T>(path: string, init?: RequestInit): Promise<T> {
  const response = await fetch(applicationUrl(path), { ...init, headers: { "Content-Type": "application/json", ...init?.headers } })
  if (!response.ok) {
    const value = await response.json().catch(() => null) as { error?: { message?: string } } | null
    throw new ApiError(response.status, value?.error?.message ?? `Request failed (${response.status})`)
  }
  return response.status === 204 ? undefined as T : response.json()
}

function avatar(principal: Principal) {
  return principal.name.trim().slice(0, 2).toUpperCase() || (principal.kind === "agent" ? "A" : "H")
}

function formatTime(value: string) {
  const date = new Date(value)
  const today = new Date()
  return date.toDateString() === today.toDateString()
    ? date.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })
    : date.toLocaleDateString([], { month: "short", day: "numeric" })
}

function signInUrl() {
  const back = /^\?(message=[A-Za-z0-9_.-]{1,128}|view=notifications)$/.test(window.location.search) ? "/" + window.location.search : "/"
  return applicationUrl("/api/auth/start?return_to=" + encodeURIComponent(back))
}

function Notifications() {
  const [status, setStatus] = useState<NotifyStatus | null>(null)
  const [draft, setDraft] = useState<NotifySettings | null>(null)
  const [busy, setBusy] = useState("")
  const [note, setNote] = useState("")
  const refresh = useCallback(async () => {
    const value = await api<NotifyStatus>("/api/notifications"); setStatus(value); setDraft(value.settings)
  }, [])
  useEffect(() => { refresh().catch(reason => setNote(reason instanceof Error ? reason.message : "Unable to load notifications")) }, [refresh])
  async function act(label: string, run: () => Promise<unknown>) {
    setBusy(label); setNote("")
    try { await run(); await refresh() } catch (reason) { setNote(reason instanceof Error ? reason.message : "Request failed") } finally { setBusy("") }
  }
  if (!status || !draft) return <section className="settings"><h2>Email notifications</h2>{note ? <div className="error-banner">{note}</div> : <p>Loading...</p>}</section>
  const save = () => act("save", () => api("/api/notifications/settings", { method: "PATCH", body: JSON.stringify({ enabled: draft.enabled, address: draft.address?.trim() || null, digest_minutes: draft.digest_minutes, scope: draft.scope }) }))
  return <section className="settings">
    <h2>Email notifications</h2>
    <p className="settings-lead">Get one short email when a Message stays unread. The email shows the sender and the first {status.preview_chars} characters, with a link back here.</p>
    <div className={`watch-state ${status.watch.state}`}>{WATCH_TEXT[status.watch.state] ?? status.watch.state}{status.watch.state === "login_required" && <a href={signInUrl()}>Sign in to Mail</a>}{status.watch.last_error && <span> Last check failed: {status.watch.last_error}</span>}</div>
    {status.available && <>
      <label className="toggle"><input type="checkbox" checked={draft.enabled} onChange={event => setDraft({ ...draft, enabled: event.target.checked })} /> Email me about unread Messages</label>
      <label htmlFor="notify-address">Email address</label>
      <input id="notify-address" type="email" value={draft.address ?? ""} placeholder={draft.default_address ?? "you@example.com"} onChange={event => setDraft({ ...draft, address: event.target.value })} />
      <label htmlFor="notify-window">Send</label>
      <select id="notify-window" value={draft.digest_minutes} onChange={event => setDraft({ ...draft, digest_minutes: Number(event.target.value) })}>{[0, 5, 15, 60, 240, 1440].concat([draft.digest_minutes]).filter((v, i, all) => all.indexOf(v) === i).sort((a, b) => a - b).map(v => <option key={v} value={v}>{v === 0 ? "Right away" : `If still unread after ${v >= 60 ? `${v / 60} hour${v > 60 ? "s" : ""}` : `${v} minutes`} (one digest)`}</option>)}</select>
      <label htmlFor="notify-scope">For</label>
      <select id="notify-scope" value={draft.scope} onChange={event => setDraft({ ...draft, scope: event.target.value as NotifySettings["scope"] })}><option value="direct">Messages sent only to me</option><option value="all">All Messages I receive</option></select>
      <div className="settings-actions"><button className="primary-button compact" disabled={!!busy} onClick={save}>{busy === "save" ? "Saving..." : "Save"}</button><button className="secondary-button" disabled={!!busy || !status.settings.effective_address} onClick={() => act("test", () => api("/api/notifications/test", { method: "POST", body: "{}" }))}>{busy === "test" ? "Sending..." : "Send test email"}</button></div>
    </>}
    {note && <div className="error-banner">{note}</div>}
    <h3>Recent emails</h3>
    {status.batches.length ? <ul className="batches">{status.batches.map(batch => <li key={batch.batch_id}><span className={`badge ${batch.status}`}>{batch.status}</span><span>{batch.kind === "test" ? "Test email" : batch.subject}<small>{new Date(batch.created_at).toLocaleString()} · {batch.attempts.length} attempt{batch.attempts.length === 1 ? "" : "s"}{batch.last_code && batch.status !== "sent" ? ` · ${batch.last_code}` : ""}</small></span>{(batch.status === "failed" || batch.status === "uncertain") && <button className="secondary-button" disabled={!!busy} title={batch.status === "uncertain" ? "Delivery is unknown; retrying may send a duplicate" : "Send again"} onClick={() => act("retry", () => api(`/api/notifications/batches/${batch.batch_id}/retry`, { method: "POST", body: "{}" }))}>Retry</button>}</li>)}</ul> : <p className="settings-lead">No emails yet.</p>}
  </section>
}

function SignIn() {
  return <main className="signin"><div className="signin-panel"><div className="brand-mark"><Mail size={19} /></div><h1>Treer Mail</h1><p>Messages for people and agents in this workspace.</p><a className="primary-button" href={signInUrl()}><LogIn size={16} /> Continue with Treer</a></div></main>
}

export function App() {
  const [session, setSession] = useState<Session | null | undefined>()
  const [directory, setDirectory] = useState<Principal[]>([])
  const [mailbox, setMailbox] = useState<Mailbox>({ deliveries: [], remaining_unread: 0 })
  const [selectedId, setSelectedId] = useState<string | null>(linkedMessage)
  const [view, setView] = useState<"inbox" | "notifications">(startParams.get("view") === "notifications" ? "notifications" : "inbox")
  const [query, setQuery] = useState("")
  const [loading, setLoading] = useState(false)
  const [compose, setCompose] = useState(false)
  const [recipients, setRecipients] = useState<string[]>([])
  const [body, setBody] = useState("")
  const [contextIds, setContextIds] = useState<string[]>([])
  const [sending, setSending] = useState(false)
  const [error, setError] = useState("")

  const load = useCallback(async () => {
    setLoading(true); setError("")
    try {
      const [mail, people] = await Promise.all([
        api<Mailbox>("/api/messages?limit=100"),
        api<{ principals: Principal[] }>("/api/directory"),
      ])
      setMailbox(mail); setDirectory(people.principals)
      setSelectedId(current => current && mail.deliveries.some(item => item.message.message_id === current) ? current : mail.deliveries[0]?.message.message_id ?? null)
    } catch (reason) { setError(reason instanceof Error ? reason.message : "Unable to load mail") }
    finally { setLoading(false) }
  }, [])

  useEffect(() => { api<Session>("/api/auth/session").then(setSession).catch(reason => setSession(reason instanceof ApiError && reason.status === 401 ? null : null)) }, [])
  useEffect(() => { if (session) void load() }, [session, load])

  const filtered = useMemo(() => {
    const needle = query.trim().toLowerCase()
    if (!needle) return mailbox.deliveries
    return mailbox.deliveries.filter(({ message }) => `${message.sender.name} ${message.body} ${message.recipients.map(item => item.name).join(" ")}`.toLowerCase().includes(needle))
  }, [mailbox.deliveries, query])
  const selected = mailbox.deliveries.find(item => item.message.message_id === selectedId) ?? null

  function startReply(message: Message) {
    setRecipients([message.sender.id]); setContextIds([message.message_id]); setBody(""); setCompose(true)
  }

  async function sendMessage(event: FormEvent) {
    event.preventDefault(); setSending(true); setError("")
    try {
      await api("/api/messages", { method: "POST", body: JSON.stringify({ recipients, context_ids: contextIds, body }) })
      setCompose(false); setRecipients([]); setContextIds([]); setBody(""); await load()
    } catch (reason) { setError(reason instanceof Error ? reason.message : "Unable to send message") }
    finally { setSending(false) }
  }

  async function logout() { await api("/api/auth/logout", { method: "POST", body: "{}" }); setSession(null) }
  if (session === undefined) return <div className="loading-screen">Opening mail...</div>
  if (!session) return <SignIn />

  return <div className="app-shell">
    <aside className="sidebar">
      <div className="workspace"><div className="brand-mark small"><Mail size={15} /></div><div><strong>Treer Mail</strong><span>{session.workspace_id}</span></div></div>
      <button className={`nav-item ${view === "inbox" ? "active" : ""}`} onClick={() => setView("inbox")}><Inbox size={16} /> Inbox {mailbox.remaining_unread > 0 && <span className="count">{mailbox.remaining_unread}</span>}</button>
      <button className={`nav-item ${view === "notifications" ? "active" : ""}`} onClick={() => setView("notifications")}><Bell size={16} /> Notifications</button>
      <div className="sidebar-spacer" />
      <div className="identity"><span className="avatar human">{avatar(session.user)}</span><div><strong>{session.user.name}</strong><span>{session.user.role}</span></div><button className="icon-button" title="Sign out" onClick={logout}><LogOut size={15} /></button></div>
    </aside>

    {view === "notifications" ? <main className="mail-surface"><Notifications /></main> : <main className="mail-surface">
      <header className="toolbar"><div className="search"><Search size={15} /><input aria-label="Search messages" placeholder="Search mail" value={query} onChange={event => setQuery(event.target.value)} /></div><button className="icon-button" title="Refresh" onClick={load} disabled={loading}><RefreshCw className={loading ? "spin" : ""} size={16} /></button><button className="primary-button compact" onClick={() => { setCompose(true); setRecipients([]); setContextIds([]); setBody("") }}><PenLine size={15} /> Compose</button></header>
      {error && <div className="error-banner">{error}<button onClick={() => setError("")}><X size={14} /></button></div>}
      <div className={`mail-grid ${selected ? "has-selection" : ""}`}>
        <section className={`message-list ${selected ? "mobile-hidden" : ""}`} aria-label="Messages">
          <div className="section-heading"><span>Inbox</span><span>{filtered.length}</span></div>
          {filtered.map(({ message, unread }) => <button key={message.message_id} className={`message-row ${selectedId === message.message_id ? "selected" : ""}`} onClick={() => setSelectedId(message.message_id)}><span className={`avatar ${message.sender.kind}`}>{avatar(message.sender)}</span><span className="message-summary"><span><strong>{message.sender.name}</strong><time>{formatTime(message.created_at)}</time></span><span className="subject">{message.body.split("\n")[0]}</span><span className="preview">To {message.recipients.map(item => item.name).join(", ")}</span></span>{unread && <span className="unread-dot" />}</button>)}
          {!filtered.length && <div className="empty"><Inbox size={24} /><strong>No messages</strong><span>{query ? "No mail matches your search." : "Messages sent to you appear here."}</span></div>}
        </section>
        <section className={`reader ${!selected ? "mobile-hidden" : ""}`}>
          {selected ? <><div className="reader-header"><button className="icon-button mobile-back" title="Back" onClick={() => setSelectedId(null)}><ArrowLeft size={17} /></button><div className={`avatar large ${selected.message.sender.kind}`}>{avatar(selected.message.sender)}</div><div className="reader-from"><strong>{selected.message.sender.name}</strong><span>{selected.message.sender.kind} · to {selected.message.recipients.map(item => item.name).join(", ")}</span></div><time>{new Date(selected.message.created_at).toLocaleString()}</time><button className="secondary-button" onClick={() => startReply(selected.message)}><Reply size={15} /> Reply</button></div><article className="message-body">{selected.message.body}</article>{selected.message.context_ids.length > 0 && <div className="context"><strong>Context</strong>{selected.message.context_ids.map(id => <button key={id} onClick={() => setSelectedId(id)}>{id}</button>)}</div>}</> : <div className="empty reader-empty"><Mail size={28} /><strong>Select a message</strong><span>Choose a conversation from the inbox.</span></div>}
        </section>
      </div>
    </main>}

    {compose && <div className="dialog-backdrop" onMouseDown={event => event.target === event.currentTarget && setCompose(false)}><form className="composer" onSubmit={sendMessage}><header><div><h2>New message</h2><span>Send to a person or agent</span></div><button type="button" className="icon-button" title="Close" onClick={() => setCompose(false)}><X size={17} /></button></header><label>Recipients</label><div className="recipient-picker">{directory.filter(item => item.id !== session.user.id).map(item => <button type="button" key={`${item.kind}:${item.id}`} className={recipients.includes(item.id) ? "picked" : ""} onClick={() => setRecipients(current => current.includes(item.id) ? current.filter(id => id !== item.id) : [...current, item.id])}><span className={`avatar ${item.kind}`}>{avatar(item)}</span><span>{item.name}<small>{item.kind}</small></span></button>)}</div>{contextIds.length > 0 && <div className="reply-context"><Reply size={14} /> Replying with {contextIds.length} message in context<button type="button" onClick={() => setContextIds([])}><X size={13} /></button></div>}<label htmlFor="message-body">Message</label><textarea id="message-body" value={body} onChange={event => setBody(event.target.value)} placeholder="Write a message..." autoFocus required maxLength={32768} /><footer><span>{body.length.toLocaleString()} / 32,768</span><button className="primary-button compact" disabled={sending || !recipients.length || !body.trim()}><Send size={15} />{sending ? "Sending..." : "Send"}</button></footer></form></div>}
  </div>
}
