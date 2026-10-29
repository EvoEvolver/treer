import { useEffect, useMemo, useRef, useState } from "react"
import { appJson, APP_PAGE_CAPABILITY, type AppNodeSelection } from "@/lib/app-tree"
import { proxyUrl } from "@/lib/api"
import { readEmbedAppearance, withEmbedTheme } from "@/lib/embed-ui"
import type { Theme } from "@/lib/theme"
import { AppInterface } from "@/components/app-interface"
import { Button } from "@/components/ui/button"
import { t } from "@/i18n"

type Principal = { id: string; name: string; app_endpoint?: string }
type Message = { message_id: string; sender: Principal; recipients: Principal[]; body: string; created_at: string }

export function AppNodeView({ workspaceId, selection, theme, visible }: { workspaceId: string; selection: AppNodeSelection; theme: Theme; visible: boolean }) {
  const [draft, setDraft] = useState("")
  const [messages, setMessages] = useState<Message[] | null>(null)
  const [historyError, setHistoryError] = useState(false)
  const [sending, setSending] = useState(false)
  const [notice, setNotice] = useState<"sent" | "failed" | null>(null)
  const [revision, refresh] = useState(0)
  const active = useRef(true)
  const controller = useRef<AbortController | null>(null)
  const operation = useRef<{ body: string; idempotency_key: string; recipients: string[]; context_ids: string[] } | null>(null)
  const endpoint = selection.path.join("/")
  const base = `/api/workspaces/${encodeURIComponent(workspaceId)}/messages`
  const canRead = selection.node.capabilities.includes("message.read")
  const canSend = selection.node.capabilities.includes("message.send")
  // `page.read`: the App renders this node itself at `ui/?node=PATH`, through the
  // same authenticated App UI tunnel and iframe isolation as the App's own UI.
  const hasPage = selection.node.capabilities.includes(APP_PAGE_CAPABILITY)
  const [view, setView] = useState<"page" | "messages">(hasPage ? "page" : "messages")
  const pageUrl = useMemo(() => withEmbedTheme(proxyUrl(`/api/workspaces/${encodeURIComponent(workspaceId)}/apps/${encodeURIComponent(selection.appId)}/ui/?node=${encodeURIComponent(endpoint)}`), theme, readEmbedAppearance()),
    // Theme changes travel by postMessage, never by replacing the frame.
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [workspaceId, selection.appId, endpoint])
  useEffect(() => { active.current = true; return () => { active.current = false; controller.current?.abort() } }, [])
  useEffect(() => {
    if (!canRead || view !== "messages") return
    const abort = new AbortController()
    let active = true
    const timer = window.setTimeout(() => abort.abort(), 10000)
    let poll: number | undefined
    setHistoryError(false)
    appJson(`${base}?app_owner_id=${encodeURIComponent(selection.ownerId)}&app_endpoint=${encodeURIComponent(endpoint)}&limit=100`, abort.signal, undefined, 1048576).then(value => {
      const page = value as { messages: Message[] }
      if (!Array.isArray(page.messages) || page.messages.length > 100 || page.messages.some(message => !message || typeof message.message_id !== "string" || typeof message.body !== "string" || !message.sender || !Array.isArray(message.recipients))) throw new Error("invalid_messages")
      const matches = (principal: Principal) => principal.id === selection.ownerId && principal.app_endpoint === endpoint
      if (active) setMessages(page.messages.filter(message => matches(message.sender) || message.recipients.some(matches)).reverse())
      if (active) poll = window.setTimeout(() => { if (!document.hidden) refresh(value => value + 1) }, 5000)
    }).catch(() => { if (active) setHistoryError(true) }).finally(() => clearTimeout(timer))
    return () => { active = false; clearTimeout(timer); clearTimeout(poll); abort.abort() }
  }, [base, canRead, selection.ownerId, endpoint, revision, view])
  async function send() {
    if (!canSend || sending || !draft.trim()) return
    if (!operation.current || operation.current.body !== draft) operation.current = {
      body: draft, idempotency_key: crypto.randomUUID(), recipients: [`${selection.ownerId}/${endpoint}`],
      context_ids: messages?.length ? [messages[messages.length - 1].message_id] : [],
    }
    const request = operation.current
    const abort = new AbortController(); controller.current = abort
    const timer = window.setTimeout(() => abort.abort(), 15000)
    setSending(true); setNotice(null)
    try {
      const result = await appJson(base, abort.signal, { method: "POST", body: JSON.stringify(request) }, 1048576)
      if (!result || typeof result !== "object" || !("message" in result)) throw new Error("invalid_send")
      if (abort.signal.aborted || !active.current) return
      setDraft(""); operation.current = null; setNotice("sent"); refresh(value => value + 1)
    } catch { if (active.current) setNotice("failed") }
    finally { clearTimeout(timer); if (active.current) setSending(false) }
  }
  const conversation = <section className="mx-auto flex w-full max-w-3xl flex-col gap-4 p-4" aria-label={hasPage ? t("app_tree.messages") : selection.node.name}>
    {!hasPage && <div><h2 className="break-words text-lg font-medium">{selection.node.name}</h2><p className="break-all text-xs text-muted-foreground">{endpoint}</p><p className="mt-1 text-xs text-muted-foreground">{t("app_tree.managed")}</p></div>}
    <div className="flex items-center justify-between gap-2"><h3 className="text-sm font-medium">{t("app_tree.messages")}</h3>{canRead && <Button variant="outline" className="min-h-11" onClick={() => refresh(value => value + 1)}>{t("app_tree.refresh_messages")}</Button>}</div>
    {!canRead ? <p>{t("app_tree.no_read")}</p> : historyError ? <p role="alert">{t("app_tree.history_failed")}</p> : messages === null ? <p role="status">{t("app_tree.loading")}</p> : messages.length === 0 ? <p>{t("app_tree.no_messages")}</p> : <ol className="space-y-3">{messages.map(message => <li key={message.message_id} className="rounded-md border p-3"><p className="break-words text-xs text-muted-foreground"><span data-app-message-sender="">{message.sender.name}{message.sender.app_endpoint ? ` / ${message.sender.app_endpoint}` : ""}</span> · <time>{message.created_at}</time></p><p className="mt-1 whitespace-pre-wrap break-words text-sm [overflow-wrap:anywhere]">{message.body}</p></li>)}</ol>}
    {canSend ? <form className="flex flex-col gap-2" onSubmit={event => { event.preventDefault(); void send() }}><label htmlFor="app-node-message" className="text-sm">{t("app_tree.body")}</label><textarea id="app-node-message" value={draft} onChange={event => setDraft(event.target.value)} disabled={sending} maxLength={16384} className="min-h-28 w-full rounded-md border bg-background p-2 text-sm focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring" /><Button type="submit" className="min-h-11 self-start" disabled={sending || !draft.trim()}>{t(sending ? "app_tree.sending" : "app_tree.send")}</Button></form> : <p>{t("app_tree.no_send")}</p>}
    {notice && <p role={notice === "failed" ? "alert" : "status"}>{t(notice === "failed" ? "app_tree.failed" : "app_tree.sent")}</p>}
  </section>
  if (!hasPage) return conversation
  return <div className="flex h-full min-h-0 flex-col" aria-label={selection.node.name} role="region">
    <div className="flex shrink-0 flex-wrap items-center gap-x-3 gap-y-1 border-b px-3 py-2">
      <div className="min-w-0"><h2 className="truncate text-sm font-medium">{selection.node.name}</h2><p className="truncate text-xs text-muted-foreground">{endpoint}</p></div>
      {(canRead || canSend) && <div className="ml-auto flex gap-1" role="group" aria-label={t("app_tree.view")}>
        {(["page", "messages"] as const).map(option => <Button key={option} type="button" variant={view === option ? "secondary" : "ghost"} className="min-h-11 md:min-h-9" aria-pressed={view === option} onClick={() => setView(option)}>{t(option === "page" ? "app_tree.page" : "app_tree.messages")}</Button>)}
      </div>}
    </div>
    {view === "page" ? <>
      <p role="note" className="shrink-0 border-b bg-sidebar px-3 py-2 text-xs leading-5 text-foreground">{t("surface.app_shared_origin")}</p>
      <AppInterface key={pageUrl} workspaceId={workspaceId} appId={selection.appId} url={pageUrl} title={`${selection.node.name} page`} theme={theme} visible={visible} />
    </> : <div className="min-h-0 flex-1 overflow-auto">{conversation}</div>}
  </div>
}
