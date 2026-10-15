import { workspacePath } from "@/lib/control-plane";
import { useCallback,useRef } from "react";
import { useLocation,useNavigate,useParams } from "react-router";

export function useWorkspaceRoute() {
  const route = useParams<{ organizationId?: string; workspaceId?: string }>()
  const navigate = useNavigate()
  const location = useLocation()
  const routeSelectionRef = useRef({ organizationId: route.organizationId ?? null, workspaceId: route.workspaceId ?? null })
  routeSelectionRef.current = { organizationId: route.organizationId ?? null, workspaceId: route.workspaceId ?? null }
  const routingRef = useRef({ navigate, search: location.search, hash: location.hash })
  routingRef.current = { navigate, search: location.search, hash: location.hash }
  // Canonicalizing the URL must not reset workspace loading or close its event
  // socket when React Router changes the navigate function's identity.
  const replaceWorkspaceRoute = useCallback((organizationId: string | null, workspaceId: string | null) => {
    const routing = routingRef.current
    const parameters = new URLSearchParams(routing.search)
    parameters.delete("invite")
    routing.navigate({ pathname: workspacePath(organizationId, workspaceId), search: parameters.toString(), hash: routing.hash }, { replace: true })
  }, [])
  return { route, location, routeSelectionRef, routingRef, replaceWorkspaceRoute }
}
