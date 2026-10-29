import { TrafficOverview } from "@/components/traffic-overview"
import { agentPath } from "@/lib/workload-path"
import { EmptyState } from "@/components/control-plane-ui"
import { type Agent, type Machine, type Member, type OrganizationAuditEvent } from "@/lib/api"
import { cn } from "@/lib/utils"
import { initials } from "@/lib/control-plane"
import {
  Activity,
  AppWindow,
  Bot,
  Server,
  RotateCw,
  ScrollText
} from "lucide-react"
import { t, intlLocale } from "@/i18n"

export const auditActionLabels: Record<string, string> = {
  "organization.created": "created the organization",
  "organization.renamed": "renamed the organization",
  "workspace.created": "created a workspace",
  "workspace.renamed": "renamed a workspace",
  "invitation.created": "created a member invitation",
  "member.role_updated": "changed a member role",
  "member.removed": "removed a member",
  "machine.renamed": "renamed a machine",
  "machine.deleted": "deleted a machine",
  "machine.exec": "ran a machine command",
  "machine.file.uploaded": "uploaded a machine file",
  "agent.created": "created an agent",
  "agent.renamed": "renamed an agent",
  "agent.stopped": "stopped an agent",
  "agent.deleted": "deleted an agent",
  "launch_profile.created": "created a launch profile",
  "launch_profile.updated": "updated a launch profile",
  "launch_profile.deleted": "deleted a launch profile",
}

export function AuditView({ events, workspaceId, refreshRevision, agents, machines, loading }: { events: OrganizationAuditEvent[]; workspaceId: string; refreshRevision: number; agents: Agent[]; machines: Machine[]; loading: boolean }) {

  return <div className="min-h-0 overflow-auto"><div className="mx-auto w-full max-w-[1120px] px-5 py-8 sm:px-8 sm:py-12 lg:px-14">
    <div className="mb-8 flex items-end justify-between gap-4"><div><div className="mb-2 grid size-9 place-items-center rounded-md bg-[#f8d9df] text-[#8b4452]"><ScrollText className="size-4" /></div><h1 className="text-2xl font-semibold">{t("audit.audit")}</h1></div><span className="text-xs text-muted-foreground">{t("audit.organization_activity")}</span></div>
    <TrafficOverview key={workspaceId} workspaceId={workspaceId} agents={agents} machines={machines} refreshRevision={refreshRevision} />
    <AuditEventList events={events} agents={agents} loading={loading} />
  </div></div>
}

// An Agent actor is labelled with the name Core froze when the event was
// written (falling back to its id), never with the Agent's current name or
// path: an Agent can rename itself, but not its past audit entries.
export function auditActorLabel(event: Pick<OrganizationAuditEvent, "actor_kind" | "actor_id" | "actor_name">) {
  if (event.actor_kind === "agent") return agentActorLabel(event.actor_name || event.actor_id)
  return event.actor_name ?? event.actor_id ?? event.actor_kind
}

export const agentActorLabel = (name: string | null | undefined) => t("audit.agent_actor", { name: name || t("dialogs.agent_fallback") })

// Shared path resolution; workspace rows use only the already-visible member directory.
export function AuditEventList({ events, agents = [], members = [], loading, workspace = false }: { events: OrganizationAuditEvent[]; agents?: Agent[]; members?: Member[]; loading: boolean; workspace?: boolean }) {
  return <section><div className="mb-3 flex items-center justify-between"><h2 className="text-sm font-semibold">{t("org.activity")}</h2><span className="text-[10px] text-muted-foreground">{workspace ? t("waudit.count", { count: events.length }) : `${events.length} events`}</span></div><div className="border-y divide-y">{events.map((event) => { const resourceAgent = agents.find((agent) => agent.agent_id === event.resource_id); const actor = auditActorLabel(event); const resource = event.resource_kind === "agent" ? resourceAgent ? agentPath(resourceAgent) : event.resource_id : event.resource_name ?? event.resource_id; if (workspace) {
      const who = event.actor_kind === "agent" ? actor : members.find((member) => member.user_id === event.actor_id)?.preferred_name || event.actor_id || event.actor_kind
      const Icon = event.resource_kind === "agent" ? Bot : event.resource_kind === "app" ? AppWindow : event.resource_kind === "machine" ? Server : ScrollText
      return <div key={event.event_id} className="grid grid-cols-[32px_minmax(0,1fr)] gap-3 py-4 sm:grid-cols-[32px_minmax(0,1fr)_auto] sm:items-start">
        <span className="grid size-8 place-items-center rounded bg-muted text-muted-foreground"><Icon aria-hidden="true" className="size-4" /></span>
        <div className="min-w-0 space-y-1.5">
          <p title={resource} className="break-words text-sm font-medium [overflow-wrap:anywhere]">{resource}</p>
          <p title={who} className="break-words text-xs text-muted-foreground [overflow-wrap:anywhere]">{t("waudit.by", { actor: who })}</p>
          <p className="break-words font-mono text-xs text-muted-foreground [overflow-wrap:anywhere]">{event.action}</p>
          <span className={cn("inline-flex rounded-full border px-2 py-0.5 text-xs font-medium", event.outcome === "failed" ? "border-destructive/30 bg-destructive/10 text-destructive" : "border-status-online/30 bg-status-online/10 text-status-success")}>{t(event.outcome === "failed" ? "waudit.outcome_failed" : "waudit.outcome_succeeded")}</span>
        </div>
        <time className="col-start-2 text-xs text-muted-foreground sm:col-start-auto" dateTime={event.occurred_at}>{new Date(event.occurred_at).toLocaleString(intlLocale())}</time>
      </div>
    }
    return <div key={event.event_id} className="grid min-h-16 grid-cols-[32px_minmax(0,1fr)] gap-3 py-3 sm:grid-cols-[32px_minmax(0,1fr)_auto] sm:items-center"><span className="grid size-8 place-items-center rounded bg-[#dcebea] text-[10px] font-bold text-[#35645f]">{initials(actor)}</span><div className="min-w-0"><div className="truncate text-xs"><span className="font-medium" title={actor}>{actor}</span> <span className="text-muted-foreground">{workspace ? event.action : auditActionLabels[event.action] ?? event.action}</span></div><div title={resource} className="mt-1 truncate font-mono text-[9px] text-muted-foreground">{resource}</div>{workspace && <p className="mt-1 text-[10px] text-muted-foreground">{t(event.outcome === "failed" ? "waudit.outcome_failed" : "waudit.outcome_succeeded")}</p>}</div><time className="col-start-2 text-[10px] text-muted-foreground sm:col-start-auto" dateTime={event.occurred_at}>{new Date(event.occurred_at).toLocaleString(intlLocale())}</time></div>})}{!events.length && <EmptyState icon={loading ? <RotateCw className="animate-spin" /> : <Activity />} label={loading ? t("audit.loading_activity") : t("audit.no_activity")} />}</div></section>
}
