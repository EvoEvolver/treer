import { useEffect, useState } from "react"
import { Bot, Boxes, RefreshCw } from "lucide-react"
import { ApiError, type AppDeployment } from "@/lib/api"
import { APP_TREE_UI_CAPABILITY, knownAppTreeSupport, probeAppTreeSupport, readAppTree, type AppNodeSelection, type AppTreeNode } from "@/lib/app-tree"
import { workloadLinkProps } from "@/lib/workload-link"
import { TreeGroup, TreeRow, treeIndent } from "@/components/tree-row"
import { t } from "@/i18n"

export type AppTreeNavigation = {
  scope: string
  selected: Pick<AppNodeSelection, "ownerId" | "path"> | null
  open: (selection: AppNodeSelection) => void
  href: (selection: AppNodeSelection) => string
}

// App IDs whose UI implements the App tree contract (docs/app-tree.md). Only
// running Apps are asked, once per page session; until an App answers, and for
// every App that does not own a directory, its row stays a plain leaf.
export function useAppTreeSupport(workspaceId: string | null, apps: AppDeployment[]): Set<string> {
  // A declared `app-tree.v1` UI capability answers without a probe.
  const declared = (app: AppDeployment) => app.ui_capabilities?.includes(APP_TREE_UI_CAPABILITY) ?? false
  const running = workspaceId ? apps.filter(app => app.status === "running" && !declared(app)).map(app => app.app_id).sort() : []
  const runningKey = running.join("\n")
  const [, rerender] = useState(0)
  const [retry, setRetry] = useState(0)
  useEffect(() => {
    if (!workspaceId || !runningKey) return
    let active = true, timer = 0
    const pending = runningKey.split("\n").filter(appId => knownAppTreeSupport(workspaceId, appId) === "unknown")
    // Each answer shows its chevron at once; offline or failing Apps are asked again later, never guessed.
    for (const appId of pending) void probeAppTreeSupport(workspaceId, appId).then(result => {
      if (!active) return
      if (result === "yes") rerender(value => value + 1)
      else if (result === "unknown" && !timer) timer = window.setTimeout(() => setRetry(value => value + 1), 30000)
    })
    return () => { active = false; clearTimeout(timer) }
  }, [workspaceId, runningKey, retry])
  return new Set(workspaceId ? apps.filter(app => declared(app) || knownAppTreeSupport(workspaceId, app.app_id) === "yes").map(app => app.app_id) : [])
}

// The App row in the workspace Tree owns the root disclosure; this renders its contents at `depth`.
export function AppOwnedTree({ workspaceId, appId, ownerId, navigation, depth }: { workspaceId: string; appId: string; ownerId: string; navigation: AppTreeNavigation; depth: number }) {
  return <Branch key={`${navigation.scope}/${appId}/${ownerId}`} workspaceId={workspaceId} appId={appId} ownerId={ownerId} navigation={navigation} path={[]} depth={depth} />
}

type BranchProps = { workspaceId: string; appId: string; ownerId: string; navigation: AppTreeNavigation; path: string[]; depth: number }

function Branch({ workspaceId, appId, ownerId, navigation, path, depth }: BranchProps) {
  const pathKey = path.join("/")
  const [nodes, setNodes] = useState<AppTreeNode[] | null>(null)
  const [error, setError] = useState<"denied" | "unavailable" | null>(null)
  const [revision, reload] = useState(0)
  const [loading, setLoading] = useState(false)
  useEffect(() => {
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
  }, [workspaceId, appId, pathKey, revision])
  const note = "py-1.5 pr-2 text-xs text-muted-foreground"
  const indent = { paddingLeft: treeIndent(depth) + 22 }
  // Refresh and retry are rows too, so arrow keys reach them like any other entry.
  const action = (label: string) => <div role="treeitem" aria-level={depth + 1} aria-label={label}>
    <TreeRow depth={depth} treeKey={`app-action:${ownerId}:${pathKey}`} label={label} muted icon={<RefreshCw />} primary={{ kind: "button", onClick: () => reload(value => value + 1) }} />
  </div>
  return <div className="min-w-0" aria-busy={loading}>
    {loading && <p role="status" className={note} style={indent}>{t("app_tree.loading")}</p>}
    {error && <div role="alert" className="break-words"><p className={note} style={indent}>{t(error === "denied" ? "app_tree.denied" : "app_tree.unavailable")}</p>{action(t("app_tree.retry"))}</div>}
    {nodes?.length === 0 && <p role="status" className={note} style={indent}>{t("app_tree.empty")}</p>}
    {nodes?.map(node => <Node key={node.key} {...{ workspaceId, appId, ownerId, navigation, depth }} path={[...path, node.key]} node={node} />)}
    {nodes && action(t("app_tree.refresh"))}
  </div>
}

function Node({ node, ...props }: BranchProps & { node: AppTreeNode }) {
  const { ownerId, navigation, path, depth } = props
  const [expanded, setExpanded] = useState(false)
  const selection = { appId: props.appId, ownerId, path, node }
  const selected = navigation.selected?.ownerId === ownerId && navigation.selected.path.join("/") === path.join("/")
  const expandable = node.capabilities.includes("tree.read") && path.length < 8
  return <div role="treeitem" aria-level={depth + 1} aria-label={node.name} aria-selected={selected} aria-expanded={expandable ? expanded : undefined} data-app-subtree={expandable ? path.join("/") : undefined}>
    <TreeRow depth={depth} expanded={expandable ? expanded : undefined} selected={selected} treeKey={`app-node:${ownerId}:${path.join("/")}`} label={node.name}
      icon={node.kind === "app" ? <Boxes /> : <Bot />}
      trailing={<span className="sr-only">{t(node.kind === "app" ? "app_tree.app" : "app_tree.agent")}</span>}
      toggle={expandable ? { label: t("app_tree.contents"), onClick: () => setExpanded(value => !value) } : undefined}
      primary={{ kind: "link", props: workloadLinkProps(navigation.href(selection), () => navigation.open(selection)) }} />
    {expanded && <TreeGroup depth={depth + 1}><Branch {...props} depth={depth + 1} /></TreeGroup>}
  </div>
}
