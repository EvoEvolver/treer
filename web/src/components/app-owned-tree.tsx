import { useEffect, useState } from "react"
import { ChevronDown, ChevronRight, Bot, Boxes } from "lucide-react"
import { ApiError } from "@/lib/api"
import { readAppTree, type AppNodeSelection, type AppTreeNode } from "@/lib/app-tree"
import { workloadLinkProps } from "@/lib/workload-link"
import { Button } from "@/components/ui/button"
import { t } from "@/i18n"

export type AppTreeNavigation = {
  scope: string
  selected: Pick<AppNodeSelection, "ownerId" | "path"> | null
  open: (selection: AppNodeSelection) => void
  href: (selection: AppNodeSelection) => string
}

export function AppOwnedTree({ workspaceId, appId, ownerId, navigation }: { workspaceId: string; appId: string; ownerId: string; navigation: AppTreeNavigation }) {
  return <Branch key={`${navigation.scope}/${appId}/${ownerId}`} workspaceId={workspaceId} appId={appId} ownerId={ownerId} navigation={navigation} path={[]} />
}

function Branch({ workspaceId, appId, ownerId, navigation, path }: { workspaceId: string; appId: string; ownerId: string; navigation: AppTreeNavigation; path: string[] }) {
  const pathKey = path.join("/")
  const [expanded, setExpanded] = useState(false)
  const [nodes, setNodes] = useState<AppTreeNode[] | null>(null)
  const [error, setError] = useState<"denied" | "unavailable" | null>(null)
  const [revision, reload] = useState(0)
  const [loading, setLoading] = useState(false)
  useEffect(() => {
    if (!expanded) return
    const controller = new AbortController()
    let active = true
    const timer = window.setTimeout(() => controller.abort(), 10000)
    setLoading(true); setError(null); setNodes(null)
    readAppTree(workspaceId, appId, pathKey ? pathKey.split("/") : [], controller.signal).then(page => {
      if (active) setNodes(page.nodes)
    }).catch(reason => {
      if (active) setError(reason instanceof ApiError && (reason.status === 401 || reason.status === 403) ? "denied" : "unavailable")
    }).finally(() => { clearTimeout(timer); if (active) setLoading(false) })
    return () => { active = false; clearTimeout(timer); controller.abort() }
  }, [expanded, workspaceId, appId, pathKey, revision])
  return <div className="min-w-0" data-app-subtree={pathKey}>
    <button type="button" aria-expanded={expanded} className="flex min-h-9 w-full items-center gap-1 rounded px-2 text-left text-xs outline-none hover:bg-accent focus-visible:ring-2 focus-visible:ring-ring max-md:min-h-11 [@media(pointer:coarse)]:min-h-11" onClick={() => setExpanded(value => !value)}>{expanded ? <ChevronDown className="size-3" /> : <ChevronRight className="size-3" />}{t("app_tree.contents")}</button>
    {expanded && <div role="group" className="ml-3 min-w-0 border-l pl-1" aria-busy={loading}>
      {loading && <p role="status" className="px-2 py-2 text-xs text-muted-foreground">{t("app_tree.loading")}</p>}
      {error && <div role="alert" className="break-words px-2 py-2 text-xs"><p>{t(error === "denied" ? "app_tree.denied" : "app_tree.unavailable")}</p><Button variant="ghost" size="sm" className="min-h-11" onClick={() => reload(value => value + 1)}>{t("app_tree.retry")}</Button></div>}
      {nodes?.length === 0 && <p role="status" className="px-2 py-2 text-xs text-muted-foreground">{t("app_tree.empty")}</p>}
      {nodes?.map(node => {
        const selection = { appId, ownerId, path: [...path, node.key], node }
        const selected = navigation.selected?.ownerId === ownerId && navigation.selected.path.join("/") === selection.path.join("/")
        return <div role="treeitem" aria-label={node.name} aria-selected={selected} key={node.key}>
          <a {...workloadLinkProps(navigation.href(selection), () => navigation.open(selection))} data-tree-primary="" data-tree-key={`app-node:${ownerId}:${selection.path.join("/")}`} className={`flex min-h-9 min-w-0 items-center gap-2 rounded px-2 text-xs outline-none hover:bg-accent focus-visible:ring-2 focus-visible:ring-ring max-md:min-h-11 [@media(pointer:coarse)]:min-h-11 ${selected ? "bg-accent" : ""}`}>
            {node.kind === "app" ? <Boxes className="size-3.5 shrink-0" /> : <Bot className="size-3.5 shrink-0" />}<span className="truncate">{node.name}</span><span className="sr-only">{t(node.kind === "app" ? "app_tree.app" : "app_tree.agent")}</span>
          </a>
          {node.capabilities.includes("tree.read") && selection.path.length < 8 && <Branch workspaceId={workspaceId} appId={appId} ownerId={ownerId} navigation={navigation} path={selection.path} />}
        </div>
      })}
      {nodes && <Button variant="ghost" size="sm" className="min-h-11 text-xs" onClick={() => reload(value => value + 1)}>{t("app_tree.refresh")}</Button>}
    </div>}
  </div>
}
