import { api,type MachineService,type MachineTrafficRecord,type OrganizationAuditEvent,type VirtualNetworkHost } from "@/lib/api"
import type { MainView } from "@/lib/control-plane"
import { useCallback,useEffect,useState,type RefObject } from "react"

interface WorkspaceMetricsOptions {
  workspaceId: string | null
  workspaceIdRef: RefObject<string | null>
  organizationId: string | null
  canManageMembers: boolean
  mainView: MainView
  showError: (reason: unknown) => void
}

export function useWorkspaceMetrics({ workspaceId, workspaceIdRef, organizationId, canManageMembers, mainView, showError }: WorkspaceMetricsOptions) {
  const [auditEvents, setAuditEvents] = useState<OrganizationAuditEvent[]>([])
  const [traffic, setTraffic] = useState<MachineTrafficRecord[]>([])
  const [auditRefreshRevision, setAuditRefreshRevision] = useState(0)
  const [auditLoading, setAuditLoading] = useState(false)
  const [virtualHosts, setVirtualHosts] = useState<VirtualNetworkHost[]>([])
  const [services, setServices] = useState<MachineService[]>([])
  const loadNetwork = useCallback(async () => {
    if (!workspaceId) {
      setVirtualHosts([])
      setServices([])
      return
    }
    const [hostData, serviceData] = await Promise.all([
      api<{ hosts: VirtualNetworkHost[] }>(`/api/workspaces/${encodeURIComponent(workspaceId)}/virtual-hosts`),
      api<{ services: MachineService[] }>(`/api/workspaces/${encodeURIComponent(workspaceId)}/services`),
    ])
    setVirtualHosts(hostData.hosts)
    setServices(serviceData.services)
  }, [workspaceId])

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
    if (workspaceIdRef.current === requestedWorkspaceId) {
      setTraffic(data.traffic)
    }
  }, [workspaceId])

  const refreshAudit = useCallback(() => {
    setAuditRefreshRevision((value) => value + 1)
    void loadAudit().catch(showError)
  }, [loadAudit, showError])

  useEffect(() => {
    if (mainView === "audit") loadAudit().catch(showError)
    if (mainView === "machine") loadNetwork().catch(showError)
    if (mainView !== "machine") return
    loadTraffic().catch(showError)
    const timer = window.setInterval(() => loadTraffic().catch(showError), 10_000)
    return () => window.clearInterval(timer)
  }, [mainView, loadAudit, loadNetwork, loadTraffic, showError])

  return { auditEvents, traffic, auditRefreshRevision, auditLoading, virtualHosts, services, refreshAudit }
}
