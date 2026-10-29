import { api,type MachineService,type MachineTrafficRecord,type VirtualNetworkHost } from "@/lib/api"
import type { MainView } from "@/lib/control-plane"
import { useCallback,useEffect,useState,type RefObject } from "react"

interface WorkspaceMetricsOptions {
  workspaceId: string | null
  workspaceIdRef: RefObject<string | null>
  mainView: MainView
  showError: (reason: unknown) => void
}

// Network inventory and traffic for the machine page. Audit lives on the settings Audit pages.
export function useWorkspaceMetrics({ workspaceId, workspaceIdRef, mainView, showError }: WorkspaceMetricsOptions) {
  const [traffic, setTraffic] = useState<MachineTrafficRecord[]>([])
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

  useEffect(() => {
    if (mainView !== "machine") return
    loadNetwork().catch(showError)
    loadTraffic().catch(showError)
    const timer = window.setInterval(() => loadTraffic().catch(showError), 10_000)
    return () => window.clearInterval(timer)
  }, [mainView, loadNetwork, loadTraffic, showError])

  return { traffic, virtualHosts, services }
}
