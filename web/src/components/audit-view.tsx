import { TrafficOverview } from "@/components/traffic-overview"
import { agentPath } from "@/lib/workload-path"
import { EmptyState } from "@/components/control-plane-ui"
import { type Agent, type Machine, type OrganizationAuditEvent } from "@/lib/api"
import { initials } from "@/lib/control-plane"
import {
  Activity,
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
    <section><div className="mb-3 flex items-center justify-between"><h2 className="text-sm font-semibold">{t("org.activity")}</h2><span className="text-[10px] text-muted-foreground">{events.length} events</span></div><div className="border-y divide-y">{events.map((event) => { const actorAgent = agents.find((agent) => agent.agent_id === event.actor_id); const resourceAgent = agents.find((agent) => agent.agent_id === event.resource_id); const actor = event.actor_kind === "agent" ? actorAgent ? agentPath(actorAgent) : event.actor_id ?? t("dialogs.agent_fallback") : event.actor_name ?? event.actor_id ?? event.actor_kind; const resource = event.resource_kind === "agent" ? resourceAgent ? agentPath(resourceAgent) : event.resource_id : event.resource_name ?? event.resource_id; return <div key={event.event_id} className="grid min-h-16 grid-cols-[32px_minmax(0,1fr)] gap-3 py-3 sm:grid-cols-[32px_minmax(0,1fr)_auto] sm:items-center"><span className="grid size-8 place-items-center rounded bg-[#dcebea] text-[10px] font-bold text-[#35645f]">{initials(actor)}</span><div className="min-w-0"><div className="truncate text-xs"><span className="font-medium">{actor}</span> <span className="text-muted-foreground">{auditActionLabels[event.action] ?? event.action}</span></div><div className="mt-1 truncate font-mono text-[9px] text-muted-foreground">{resource}</div></div><time className="col-start-2 text-[10px] text-muted-foreground sm:col-start-auto" dateTime={event.occurred_at}>{new Date(event.occurred_at).toLocaleString(intlLocale())}</time></div>})}{!events.length && <EmptyState icon={loading ? <RotateCw className="animate-spin" /> : <Activity />} label={loading ? t("audit.loading_activity") : t("audit.no_activity")} />}</div></section>
  </div></div>
}
