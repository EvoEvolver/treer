import { useEffect, useRef, useState, type FormEvent } from "react"
import { ChevronDown, RotateCw } from "lucide-react"
import { WorkspaceAuditSummary } from "@/components/workspace-audit-summary"
import { type AuditDrill } from "@/lib/audit-summary"
import { AuditEventList } from "@/components/audit-view"
import { Field } from "@/components/control-plane-ui"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { api, ApiError, type Agent, type Member, type OrganizationAuditEvent } from "@/lib/api"
import { agentPath } from "@/lib/workload-path"
import { WorkspaceAuditFilter, auditFilterOptions, type AuditFilterKey } from "@/components/workspace-audit-filter"
import { cn } from "@/lib/utils"
import { t } from "@/i18n"

interface Filters { since: string; until: string; actor_id: string; action: string; resource_kind: string }
const emptyFilters: Filters = { since: "", until: "", actor_id: "", action: "", resource_kind: "" }
interface Page { events: OrganizationAuditEvent[]; next_cursor: number | null }

// Project only fields used for display. Never render arbitrary audit payloads,
// names or server error bodies, including those returned by an older Proxy.
// The one stored name kept is an Agent actor's (bounded text, shown as
// "agent: <name>"), because an Agent's current name is not who acted then.
function auditPage(value: unknown, workspaceId: string): Page {
  if (!value || typeof value !== "object") throw new Error("invalid audit page")
  const page = value as Page
  if (!Array.isArray(page.events) || page.events.length > 50 || !(page.next_cursor === null || (Number.isSafeInteger(page.next_cursor) && page.next_cursor > 0))) throw new Error("invalid audit page")
  const text = (value: unknown): value is string => typeof value === "string" && value.length <= 512
  const events = page.events.map((event) => {
    if (!event || event.workspace_id !== workspaceId || !Number.isSafeInteger(event.sequence) || event.sequence <= 0 || !text(event.event_id) || !text(event.occurred_at) || !Number.isFinite(Date.parse(event.occurred_at)) || !text(event.actor_kind) || (event.actor_id !== undefined && event.actor_id !== null && !text(event.actor_id)) || !text(event.action) || !text(event.resource_kind) || !text(event.resource_id) || !["succeeded", "failed"].includes(event.outcome)) throw new Error("invalid audit event")
    return { sequence: event.sequence, event_id: event.event_id, organization_id: "", workspace_id: workspaceId, occurred_at: event.occurred_at, actor_kind: event.actor_kind, actor_id: event.actor_id ?? undefined, ...(event.actor_kind === "agent" && text(event.actor_name) && event.actor_name.trim() ? { actor_name: event.actor_name } : {}), source: "", action: event.action, outcome: event.outcome, resource_kind: event.resource_kind, resource_id: event.resource_id, payload: {} }
  })
  return { events, next_cursor: page.next_cursor }
}

export function WorkspaceAudit({ workspaceId, agents, members, preview }: { workspaceId: string; agents: Agent[]; members: Member[]; preview: boolean }) {
  const [drill, setDrill] = useState<AuditDrill>({})
  const [selection, setSelection] = useState<string>()
  const listHeading = useRef<HTMLHeadingElement>(null)
  const [filtersOpen, setFiltersOpen] = useState(false)
  const [draft, setDraft] = useState<Filters>(emptyFilters)
  const [filters, setFilters] = useState<Filters>(emptyFilters)
  const [cursors, setCursors] = useState<(number | undefined)[]>([undefined])
  const [page, setPage] = useState<Page>()
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string>()
  const [validation, setValidation] = useState<string>()
  const [revision, setRevision] = useState(0)
  const cursor = cursors.at(-1)
  // Catalog suggestions are not an authorization or completeness claim. Current
  // page codes come only from the validated feed; identities are already visible.
  const actions = ["agent.created", "agent.renamed", "agent.deleted", "agent.stopped", "app.created", "machine.exec", "machine.renamed", "machine.deleted", "workspace.created", "workspace.renamed", "launch_profile.created", "launch_profile.updated", "launch_profile.deleted"]
  const kinds = ["agent", "app", "machine", "workspace", "launch_profile"]
  const kindLabel = (kind: string) => kinds.includes(kind) ? t(`waudit.kind_${kind}` as "waudit.kind_agent") : kind
  const actionLabel = (action: string) => {
    const [kind, verb] = action.split(".")
    return actions.includes(action) ? `${kindLabel(kind)} · ${t(`waudit.verb_${verb}` as "waudit.verb_created")}` : action
  }
  const choices = {
    actor_id: auditFilterOptions([
      ...agents.map((agent) => ({ value: agent.agent_id, label: agentPath(agent) })),
      ...members.map((member) => ({ value: member.user_id, label: member.preferred_name || member.user_id })),
    ]),
    action: auditFilterOptions([...new Set([...actions, ...(page?.events.map((event) => event.action) ?? [])])].map((action) => ({ value: action, label: actionLabel(action) }))),
    resource_kind: auditFilterOptions([...new Set([...kinds, ...(page?.events.map((event) => event.resource_kind) ?? [])])].map((kind) => ({ value: kind, label: kindLabel(kind) }))),
  }

  useEffect(() => {
    const controller = new AbortController()
    setPage(undefined); setError(undefined); setLoading(true)
    async function load() {
      if (preview) { setPage({ events: [], next_cursor: null }); setLoading(false); return }
      try {
        // Capability gate fails closed before requesting audit data from an old Proxy.
        const config = await api<{ capabilities?: string[] }>("/api/auth/config", { signal: controller.signal, cache: "no-store" })
        if (controller.signal.aborted) return
        if (!config.capabilities?.includes("workspace.audit.v1")) { setError(t("waudit.unsupported")); return }
        const query = new URLSearchParams({ limit: "50" })
        if (cursor !== undefined) query.set("before", String(cursor))
        for (const [key, value] of Object.entries({ ...filters, ...drill })) if (value) query.set(key, value)
        const data = await api<unknown>(`/api/workspaces/${encodeURIComponent(workspaceId)}/audit-events?${query}`, { signal: controller.signal, cache: "no-store" })
        if (!controller.signal.aborted) setPage(auditPage(data, workspaceId))
      } catch (reason) {
        if (controller.signal.aborted) return
        setError(reason instanceof ApiError && reason.status === 403 ? t("waudit.denied") : reason instanceof ApiError && [404, 405].includes(reason.status) ? t("waudit.unsupported") : t("waudit.failed"))
      } finally { if (!controller.signal.aborted) setLoading(false) }
    }
    void load()
    return () => controller.abort()
  }, [workspaceId, filters, drill, cursor, revision, preview])

  function apply(event: FormEvent) {
    event.preventDefault()
    const next = { ...draft }
    for (const key of ["actor_id", "action", "resource_kind"] as const) {
      next[key] = next[key].trim()
      if (next[key] && !/^[A-Za-z0-9_.-]{1,128}$/.test(next[key])) { setValidation(t("waudit.invalid_filter")); return }
    }
    for (const key of ["since", "until"] as const) {
      if (!next[key]) continue
      const time = Date.parse(next[key])
      if (!Number.isFinite(time)) { setValidation(t("waudit.invalid_time")); return }
      next[key] = new Date(time).toISOString()
    }
    if (next.since && next.until && (Date.parse(next.until) <= Date.parse(next.since) || Date.parse(next.until) - Date.parse(next.since) > 366 * 86_400_000)) { setValidation(t("waudit.invalid_time")); return }
    setDrill({}); setSelection(undefined); setValidation(undefined); setPage(undefined); setCursors([undefined]); setFilters(next)
  }

  return <section aria-label={t("waudit.title")} className="min-w-0 space-y-5">
    <div className="flex flex-wrap items-center justify-between gap-3"><h2 className="text-lg font-semibold">{t("waudit.title")}</h2><Button variant="outline" className="min-h-11" onClick={() => { setPage(undefined); setRevision((value) => value + 1) }} disabled={loading}><RotateCw className={loading ? "animate-spin" : ""} />{t("waudit.refresh")}</Button></div>
    <p className="text-xs leading-5 text-muted-foreground">{t("waudit.scope")}</p>
    <Button variant="outline" className="min-h-11 w-full justify-between sm:hidden" aria-expanded={filtersOpen} aria-controls="workspace-audit-filters" onClick={() => setFiltersOpen((open) => !open)}>{t("waudit.filters")}<ChevronDown aria-hidden="true" className={cn("size-4", filtersOpen && "rotate-180")} /></Button>
    <form id="workspace-audit-filters" onSubmit={apply} aria-label={t("waudit.filters")} className={cn("min-w-0 gap-3 rounded-md border bg-muted/20 p-4 sm:grid sm:grid-cols-2 lg:grid-cols-3", filtersOpen ? "grid" : "hidden")}>
      {(["since", "until"] as const).map((key) => <Field key={key} label={t(`waudit.${key}`)}><Input aria-label={t(`waudit.${key}`)} className="min-h-11 min-w-0 w-full" type="datetime-local" maxLength={128} value={draft[key]} onChange={(event) => setDraft((current) => ({ ...current, [key]: event.target.value }))} /></Field>)}
      {(["actor_id", "action", "resource_kind"] satisfies AuditFilterKey[]).map((key) => <WorkspaceAuditFilter key={key} field={key} value={draft[key]} options={choices[key]} onChange={(value) => setDraft((current) => ({ ...current, [key]: value }))} />)}
      <div className="flex flex-wrap items-end gap-2"><Button className="min-h-11" type="submit">{t("waudit.apply")}</Button><Button className="min-h-11" variant="outline" type="button" onClick={() => { setDrill({}); setSelection(undefined); setDraft(emptyFilters); setFilters({ ...emptyFilters }); setPage(undefined); setValidation(undefined); setCursors([undefined]) }}>{t("waudit.clear")}</Button></div>
      <p className="text-xs leading-5 text-muted-foreground sm:col-span-2 lg:col-span-3">{t("waudit.filter_hint")}</p>
      {validation && <p role="alert" className="text-xs text-destructive sm:col-span-2 lg:col-span-3">{validation}</p>}
    </form>
    <WorkspaceAuditSummary workspaceId={workspaceId} agents={agents} members={members} filters={{ ...filters }} revision={revision} preview={preview} selected={drill.category} onReset={() => { setDrill({}); setSelection(undefined); setPage(undefined); setCursors([undefined]) }} onDrill={(next, label) => {
      setDrill(next); setSelection(label); setPage(undefined); setCursors([undefined])
      requestAnimationFrame(() => { listHeading.current?.focus(); listHeading.current?.scrollIntoView({ block: "start" }) })
    }} />
    <div className="flex flex-wrap items-center justify-between gap-2"><h3 ref={listHeading} tabIndex={-1} className="break-words text-sm font-semibold focus-visible:outline focus-visible:outline-2 focus-visible:outline-ring">{selection ? t("as.feed_selected", { selection }) : t("as.feed_all")}</h3>{selection && <Button variant="outline" className="min-h-11" onClick={() => { setDrill({}); setSelection(undefined); setPage(undefined); setCursors([undefined]) }}>{t("as.reset_selection")}</Button>}</div>
    {selection && <p className="break-words text-xs text-muted-foreground">{drill.since} – {drill.until} · UTC</p>}
    {error ? <p role="alert" className="rounded-md border p-4 text-sm">{error}</p> : <div aria-busy={loading}><AuditEventList events={page?.events ?? []} agents={agents} members={members} loading={loading} workspace /></div>}
    <nav aria-label={t("waudit.pagination")} className="flex flex-wrap items-center justify-between gap-3">
      <Button className="min-h-11" variant="outline" disabled={loading || cursors.length <= 1} onClick={() => { setPage(undefined); setCursors((current) => current.slice(0, -1)) }}>{t("waudit.newer")}</Button>
      <span role="status" className="text-xs text-muted-foreground">{t("waudit.page", { page: cursors.length })}</span>
      <Button className="min-h-11" variant="outline" disabled={loading || !page?.next_cursor} onClick={() => { if (page?.next_cursor) { setPage(undefined); setCursors((current) => [...current, page.next_cursor!]) } }}>{t("waudit.older")}</Button>
    </nav>
  </section>
}
