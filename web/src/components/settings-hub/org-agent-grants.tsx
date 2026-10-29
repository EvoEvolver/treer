import { FormEvent, useCallback, useEffect, useState } from "react"
import { Pencil, Plus, RotateCw, Trash2 } from "lucide-react"
import { Button } from "@/components/ui/button"
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog"
import { Input } from "@/components/ui/input"
import { Label } from "@/components/ui/label"
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select"
import { api, isUnavailable, type Agent, type AgentGrant, type Snapshot } from "@/lib/api"
import { agentPath } from "@/lib/workload-path"
import { roleLabel, t, type MessageKey } from "@/i18n"
import { ApiEquivalent, Badge, ConfirmDialog, ErrorNote, PageHeader, PermissionNote, Picker, Unavailable, formatDate } from "@/components/settings-hub/ui"
import type { OrgPageContext } from "@/components/settings-hub/org-pages"

// Organization actions an owner/admin can let one Agent perform through the
// API. Core also requires the Agent's workspace Policy allow; Agents can never
// be granted policy.provider.manage or owner/admin-level changes.
export const AGENT_GRANT_ACTIONS = ["org.manage", "org.member.manage", "org.invite.manage", "org.group.manage", "workspace.manage", "audit.read"] as const
const ACTION_TEXT: Record<string, { label: MessageKey; detail: MessageKey }> = {
  "org.manage": { label: "grants.action_org_manage", detail: "grants.action_org_manage_detail" },
  "org.member.manage": { label: "grants.action_member_manage", detail: "grants.action_member_manage_detail" },
  "org.invite.manage": { label: "grants.action_invite_manage", detail: "grants.action_invite_manage_detail" },
  "org.group.manage": { label: "grants.action_group_manage", detail: "grants.action_group_manage_detail" },
  "workspace.manage": { label: "grants.action_workspace_manage", detail: "grants.action_workspace_manage_detail" },
  "audit.read": { label: "grants.action_audit_read", detail: "grants.action_audit_read_detail" },
}
const actionLabel = (action: string) => ACTION_TEXT[action] ? t(ACTION_TEXT[action].label) : action
const message = (reason: unknown) => reason instanceof Error ? reason.message : String(reason)

type Grants = { grants: AgentGrant[]; grantable: string[] } | "unavailable" | null

export function AgentPermissionsPage({ context, supported }: { context: OrgPageContext; supported: boolean | null }) {
  const { organization, workspaces, preview } = context
  const canManage = organization.role === "owner" || organization.role === "admin"
  const base = `/api/organizations/${encodeURIComponent(organization.organization_id)}/agent-grants`
  const [data, setData] = useState<Grants>(null)
  const [error, setError] = useState<string | null>(null)
  const [editing, setEditing] = useState<AgentGrant | "new" | null>(null)
  const [removing, setRemoving] = useState<AgentGrant | null>(null)
  const [removeBusy, setRemoveBusy] = useState(false)
  const [removeError, setRemoveError] = useState<string | null>(null)
  const load = useCallback(async () => {
    if (!canManage || preview || !supported) return
    try {
      const value = await api<{ grants: AgentGrant[]; grantable_actions: string[] }>(base)
      setData({ grants: Array.isArray(value.grants) ? value.grants : [], grantable: Array.isArray(value.grantable_actions) ? value.grantable_actions : [...AGENT_GRANT_ACTIONS] }); setError(null)
    } catch (reason) { if (isUnavailable(reason)) setData("unavailable"); else setError(message(reason)) }
  }, [base, canManage, preview, supported])
  useEffect(() => { setData(null); void load() }, [load])
  async function remove() {
    if (!removing) return
    setRemoveBusy(true); setRemoveError(null)
    try { await api(`${base}/${encodeURIComponent(removing.agent_id)}`, { method: "DELETE" }); setRemoving(null); await load() }
    catch (reason) { setRemoveError(message(reason)) }
    finally { setRemoveBusy(false) }
  }
  const agentLabel = (grant: AgentGrant) => grant.agent_name || grant.agent_id
  const list = data && data !== "unavailable" ? data : null
  const orgId = organization.organization_id
  return <>
    <PageHeader title={t("grants.title")} description={t("grants.description")}
      actions={canManage && list ? <><Button variant="ghost" size="icon" className="max-md:size-11" aria-label={t("hub.refresh")} onClick={() => void load()}><RotateCw /></Button><Button className="max-md:h-11" disabled={preview} onClick={() => setEditing("new")}><Plus />{t("grants.add")}</Button></> : undefined} />
    <div role="note" className="mb-4 space-y-1 rounded-md bg-muted/50 px-3 py-2 text-xs leading-5 text-muted-foreground">
      <p>{t("grants.needs_policy")}</p>
      <p>{t("grants.never_grantable")}</p>
    </div>
    {!canManage ? <PermissionNote>{t("grants.only_managers")}</PermissionNote>
      : supported === false || data === "unavailable" ? <Unavailable>{t("grants.unavailable")}</Unavailable>
      : !list ? <p role="status" className="text-xs text-muted-foreground">{t("hub.loading")}</p>
      : <div className="relative overflow-x-auto rounded-lg border"><table aria-label={t("grants.title")} className="w-full min-w-[680px] text-left text-xs">
        <thead className="border-b bg-muted/40 text-[11px] text-muted-foreground"><tr><th className="px-3 py-2 font-medium">{t("grants.agent")}</th><th className="px-3 py-2 font-medium">{t("grants.permissions")}</th><th className="px-3 py-2 font-medium">{t("grants.granted_by")}</th><th className="px-3 py-2 font-medium">{t("hub.status")}</th><th className="px-3 py-2"><span className="sr-only">{t("hub.actions")}</span></th></tr></thead>
        <tbody className="divide-y">{list.grants.map((grant) => <tr key={grant.agent_id} className="align-top">
          <td className="max-w-56 px-3 py-2"><span className="block truncate font-medium" title={agentLabel(grant)}>{agentLabel(grant)}</span><span className="block truncate font-mono text-[11px] text-muted-foreground" title={grant.agent_id}>{grant.agent_id}</span><span className="block truncate text-muted-foreground" title={grant.workspace_name ?? grant.workspace_id}>{t("grants.in_workspace", { name: grant.workspace_name || grant.workspace_id })}</span></td>
          <td className="px-3 py-2"><div className="flex flex-wrap gap-1">{grant.actions.map((action) => <Badge key={action}>{actionLabel(action)}</Badge>)}</div></td>
          <td className="px-3 py-2"><span className="block truncate">{grant.created_by.kind === "agent" ? t("audit.agent_actor", { name: grant.created_by.name || grant.created_by.id }) : grant.created_by.name || grant.created_by.id}</span><span className="block text-muted-foreground">{roleLabel(grant.created_by_role)} · {formatDate(grant.created_at)}</span></td>
          <td className="px-3 py-2">{grant.active ? <Badge tone="primary">{t("grants.active")}</Badge> : <><Badge tone="warning">{t("grants.inactive")}</Badge><span className="mt-1 block max-w-48 text-[11px] text-muted-foreground">{t("grants.inactive_reason")}</span></>}</td>
          <td className="px-3 py-2"><div className="flex justify-end gap-1">
            <Button size="sm" variant="ghost" className="max-md:h-11" disabled={preview} aria-label={t("grants.edit_for", { name: agentLabel(grant) })} onClick={() => setEditing(grant)}><Pencil />{grant.active ? t("grants.edit") : t("grants.resave")}</Button>
            <Button size="sm" variant="ghost" className="text-destructive max-md:h-11" disabled={preview} aria-label={t("grants.remove_for", { name: agentLabel(grant) })} onClick={() => { setRemoveError(null); setRemoving(grant) }}><Trash2 />{t("grants.remove")}</Button>
          </div></td>
        </tr>)}{!list.grants.length && <tr><td colSpan={5} className="px-3 py-6 text-center text-muted-foreground">{t("grants.empty")}</td></tr>}</tbody>
      </table></div>}
    <ErrorNote message={error} />
    {list && <GrantDialog open={editing !== null} grant={editing === "new" ? null : editing} grantable={list.grantable} workspaces={workspaces} base={base} onOpenChange={(open) => !open && setEditing(null)} onSaved={() => { setEditing(null); void load() }} />}
    <ConfirmDialog open={Boolean(removing)} onOpenChange={(open) => !open && setRemoving(null)} title={t("grants.remove_title", { name: removing ? agentLabel(removing) : "" })} description={t("grants.remove_body")} confirmLabel={t("grants.remove")} busy={removeBusy} error={removeError} onConfirm={() => void remove()} />
    <ApiEquivalent calls={[{ method: "GET", path: `/api/organizations/${orgId}/agent-grants` }, { method: "PUT", path: `/api/organizations/${orgId}/agent-grants/{agent_id}`, note: "{ workspace_id, actions: [...] }" }, { method: "DELETE", path: `/api/organizations/${orgId}/agent-grants/{agent_id}` }]} />
  </>
}

function GrantDialog({ open, grant, grantable, workspaces, base, onOpenChange, onSaved }: { open: boolean; grant: AgentGrant | null; grantable: string[]; workspaces: OrgPageContext["workspaces"]; base: string; onOpenChange: (open: boolean) => void; onSaved: () => void }) {
  const [workspaceId, setWorkspaceId] = useState("")
  const [agentId, setAgentId] = useState("")
  const [actions, setActions] = useState<string[]>([])
  const [agents, setAgents] = useState<{ id: string; list: Agent[] | null; error: boolean } | null>(null)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)
  useEffect(() => {
    if (!open) return
    setWorkspaceId(grant?.workspace_id ?? (workspaces.length === 1 ? workspaces[0].workspace_id : "")); setAgentId(grant?.agent_id ?? ""); setActions(grant?.actions ?? []); setError(null)
  }, [open, grant, workspaces])
  // The picker offers the chosen workspace's Agents; an id can also be pasted.
  useEffect(() => {
    if (!open || !workspaceId || grant) return
    let active = true
    setAgents({ id: workspaceId, list: null, error: false })
    api<Snapshot>(`/api/workspaces/${encodeURIComponent(workspaceId)}/snapshot`)
      .then((snapshot) => { if (active) setAgents({ id: workspaceId, list: snapshot.agents ?? [], error: false }) })
      .catch(() => { if (active) setAgents({ id: workspaceId, list: [], error: true }) })
    return () => { active = false }
  }, [open, workspaceId, grant])
  const options = agents?.id === workspaceId && agents.list ? agents.list.map((agent) => ({ id: agent.agent_id, label: agentPath(agent), detail: agent.agent_id })) : []
  const picked = options.find((option) => option.id === agentId.trim())
  const toggle = (action: string) => setActions((list) => list.includes(action) ? list.filter((value) => value !== action) : [...list, action])
  const valid = Boolean(workspaceId && agentId.trim() && actions.length)
  async function submit(event: FormEvent) {
    event.preventDefault()
    if (!valid) return
    setBusy(true); setError(null)
    try {
      await api(`${base}/${encodeURIComponent(agentId.trim())}`, { method: "PUT", body: JSON.stringify({ workspace_id: workspaceId, actions: grantable.filter((action) => actions.includes(action)) }) })
      onSaved()
    } catch (reason) { setError(message(reason)) }
    finally { setBusy(false) }
  }
  return <Dialog open={open} onOpenChange={onOpenChange}><DialogContent className="max-w-lg max-h-[calc(100dvh-2rem)] overflow-y-auto max-sm:max-h-[100dvh]">
    <form onSubmit={submit} className="space-y-4">
      <DialogHeader><DialogTitle>{grant ? t("grants.edit_title", { name: grant.agent_name || grant.agent_id }) : t("grants.add_title")}</DialogTitle><DialogDescription>{t("grants.dialog_description")}</DialogDescription></DialogHeader>
      <div className="space-y-2"><Label>{t("grants.workspace")}</Label>
        <Select value={workspaceId} onValueChange={(value) => { setWorkspaceId(value); if (!grant) setAgentId("") }}><SelectTrigger aria-label={t("grants.workspace")} className="max-sm:h-11"><SelectValue placeholder={t("grants.choose_workspace")} /></SelectTrigger><SelectContent>{workspaces.map((workspace) => <SelectItem key={workspace.workspace_id} value={workspace.workspace_id}>{workspace.name}</SelectItem>)}{grant && !workspaces.some((workspace) => workspace.workspace_id === grant.workspace_id) && <SelectItem value={grant.workspace_id}>{grant.workspace_name || grant.workspace_id}</SelectItem>}</SelectContent></Select>
        <p className="text-[11px] text-muted-foreground">{t("grants.workspace_hint")}</p>
      </div>
      {!grant && <div className="space-y-2">
        <Label>{t("grants.agent")}</Label>
        <Picker label={t("grants.pick_agent")} placeholder={workspaceId ? t("grants.search_agents") : t("grants.choose_workspace_first")} options={options} disabled={!workspaceId} onPick={(option) => setAgentId(option.id)} />
        {agents?.error && <p className="text-[11px] text-muted-foreground">{t("grants.agents_unavailable")}</p>}
      </div>}
      <div className="space-y-2"><Label htmlFor="grant-agent-id">{t("grants.agent_id")}</Label>
        <Input id="grant-agent-id" value={agentId} onChange={(event) => setAgentId(event.target.value)} disabled={Boolean(grant)} spellCheck={false} autoComplete="off" placeholder={t("grants.agent_id_placeholder")} className="font-mono max-sm:h-11 max-sm:text-base" />
        {picked && <p className="truncate text-[11px] text-muted-foreground">{t("grants.selected_agent", { name: picked.label })}</p>}
      </div>
      <fieldset><legend className="mb-2 text-sm font-medium">{t("grants.permissions")}</legend><ul className="space-y-1">{grantable.map((action) => <li key={action}>
        <label className="flex min-h-11 cursor-pointer items-start gap-3 rounded-md px-2 py-1.5 hover:bg-accent/60">
          <input type="checkbox" className="mt-0.5 size-4 shrink-0 accent-primary" checked={actions.includes(action)} onChange={() => toggle(action)} />
          <span className="min-w-0 text-xs"><span className="block font-medium">{actionLabel(action)} <span className="font-mono font-normal text-muted-foreground">{action}</span></span>{ACTION_TEXT[action] && <span className="block text-muted-foreground">{t(ACTION_TEXT[action].detail)}</span>}</span>
        </label>
      </li>)}</ul>{!actions.length && <p className="mt-1 text-[11px] text-muted-foreground">{t("grants.pick_one")}</p>}</fieldset>
      <ErrorNote message={error} />
      <DialogFooter><Button type="button" variant="outline" className="max-sm:h-11" onClick={() => onOpenChange(false)}>{t("common.cancel")}</Button><Button type="submit" className="max-sm:h-11" disabled={busy || !valid}>{busy ? t("common.saving") : t("common.save")}</Button></DialogFooter>
    </form>
  </DialogContent></Dialog>
}
