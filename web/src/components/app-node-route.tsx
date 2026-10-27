import { useEffect, useState } from "react"
import { readAppTree, validAppSegment, type AppNodeSelection } from "@/lib/app-tree"
import { AppNodeView } from "@/components/app-node-view"
import { t } from "@/i18n"
import { ApiError } from "@/lib/api"

export function AppNodeRoute({ workspaceId, appId, ownerId, endpoint }: { workspaceId: string; appId: string; ownerId: string; endpoint: string }) {
  const [selection, setSelection] = useState<AppNodeSelection | null>(null)
  const [failed, setFailed] = useState<"unknown" | "denied" | "unavailable" | null>(null)
  useEffect(() => {
    const abort = new AbortController()
    let active = true
    const timer = window.setTimeout(() => abort.abort(), 15000)
    setSelection(null); setFailed(null)
    ;(async () => {
      const segments = endpoint.split("/")
      if (segments.length > 8 || !segments.every(validAppSegment)) throw new Error("invalid_endpoint")
      for (let index = 0; index < segments.length; index++) {
        const page = await readAppTree(workspaceId, appId, segments.slice(0, index), abort.signal)
        const node = page.nodes.find(node => node.key === segments[index])
        if (!node || (index < segments.length - 1 && !node.capabilities.includes("tree.read"))) throw new Error("missing_node")
        if (index === segments.length - 1 && active) setSelection({ appId, ownerId, path: segments, node })
      }
    })().catch(reason => { if (active) setFailed(reason instanceof ApiError && (reason.status === 401 || reason.status === 403) ? "denied" : reason instanceof Error && reason.message === "missing_node" ? "unknown" : "unavailable") }).finally(() => clearTimeout(timer))
    return () => { active = false; clearTimeout(timer); abort.abort() }
  }, [workspaceId, appId, ownerId, endpoint])
  return failed ? <p role="alert" className="p-4">{t(failed === "denied" ? "app_tree.denied" : failed === "unknown" ? "app_tree.unknown" : "app_tree.unavailable")}</p> : selection ? <AppNodeView key={`${ownerId}/${endpoint}`} workspaceId={workspaceId} selection={selection} /> : <p role="status" className="p-4">{t("app_tree.loading")}</p>
}
