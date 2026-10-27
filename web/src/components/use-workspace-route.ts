import { workloadIdFromSplat, workloadPath, workspacePath } from "@/lib/control-plane";
import { useCallback,useRef } from "react";
import { useLocation,useNavigate,useParams } from "react-router";

export function useWorkspaceRoute() {
  const route = useParams<{ organizationId?: string; workspaceId?: string; "*"?: string }>()
  const navigate = useNavigate()
  const location = useLocation()
  const routeSelectionRef = useRef({ organizationId: route.organizationId ?? null, workspaceId: route.workspaceId ?? null })
  routeSelectionRef.current = { organizationId: route.organizationId ?? null, workspaceId: route.workspaceId ?? null }
  const routingRef = useRef({ navigate, search: location.search, hash: location.hash, pathname: location.pathname })
  routingRef.current = { navigate, search: location.search, hash: location.hash, pathname: location.pathname }
  // Canonicalizing the URL must not reset workspace loading or close its event
  // socket when React Router changes the navigate function's identity.
  const replaceWorkspaceRoute = useCallback((organizationId: string | null, workspaceId: string | null) => {
    const routing = routingRef.current
    const parameters = new URLSearchParams(routing.search)
    parameters.delete("invite")
    // Canonicalizing the same workspace must not drop a deep-linked workload; changing workspace does.
    const current = routeSelectionRef.current
    const sameWorkspace = organizationId === current.organizationId && workspaceId === current.workspaceId
    const keep = sameWorkspace ? workloadIdFromSplat(/\/workloads\/[^/]+\/?$/.exec(routing.pathname)?.[0].slice(1)) : null
    if (!sameWorkspace) { parameters.delete("focus"); parameters.delete("app_node") }
    routing.navigate({ pathname: workloadPath(organizationId, workspaceId, keep).split("?")[0], search: parameters.toString(), hash: routing.hash }, { replace: true })
  }, [])
  // Keeps the address bar on the selected workload (replace, no history entry) so the URL can be copied or reloaded.
  const replaceWorkloadRoute = useCallback((workloadId: string | null) => {
    const routing = routingRef.current
    const { organizationId, workspaceId } = routeSelectionRef.current
    const parameters = new URLSearchParams(routing.search)
    if (!workloadId || !routing.pathname.endsWith(`/workloads/${encodeURIComponent(workloadId)}`)) parameters.delete("app_node")
    routing.navigate({ pathname: workloadPath(organizationId, workspaceId, workloadId).split("?")[0], search: parameters.toString(), hash: routing.hash }, { replace: true })
  }, [])
  const routeWorkloadId = workloadIdFromSplat(route["*"])
  const focusMode = new URLSearchParams(location.search).get("focus") === "1"
  return { route, location, routeSelectionRef, routingRef, replaceWorkspaceRoute, replaceWorkloadRoute, routeWorkloadId, focusMode }
}
