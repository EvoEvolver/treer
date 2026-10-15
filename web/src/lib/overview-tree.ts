import type { Agent, WorkloadTreeState } from "./api"
import { agentPath, folderPath } from "./workload-path"
import type { attentionGroups } from "./attention"

export type AttentionFilter = "attention" | "working" | "idle"
export type OverviewNode = {
  id: string; name: string; path: string; agent?: Agent; children: OverviewNode[]
  counts: Record<AttentionFilter, number>; matches: number; parent?: OverviewNode
}
const emptyCounts = () => ({ attention: 0, working: 0, idle: 0 })

// Count once from leaves to roots. Invalid parents/cycles become roots, so a
// malformed or temporarily incomplete snapshot cannot hide workloads or loop.
export function overviewForest(tree: WorkloadTreeState, agents: Agent[], groups: ReturnType<typeof attentionGroups>, filter: AttentionFilter | null) {
  const categories = new Map<string, AttentionFilter>()
  for (const key of ["attention", "working", "idle"] as const) for (const agent of groups[key]) categories.set(agent.agent_id, key)
  const folders = new Map<string, OverviewNode>(tree.folders.map((folder) => [folder.folder_id, {
    id: folder.folder_id, name: folder.name, path: folderPath(folder, tree.folders), children: [], counts: emptyCounts(), matches: 0,
  }]))
  const parents = new Map(tree.folders.map((folder) => [folder.folder_id, folder.parent_id]))
  const roots: OverviewNode[] = []
  for (const folder of tree.folders) {
    const node = folders.get(folder.folder_id)!, seen = new Set([folder.folder_id])
    let id = folder.parent_id, cyclic = false
    while (id && folders.has(id)) {
      if (seen.has(id)) { cyclic = true; break }
      seen.add(id); id = parents.get(id) ?? null
    }
    node.parent = !cyclic && folder.parent_id ? folders.get(folder.parent_id) : undefined
    if (node.parent) node.parent.children.push(node)
    else roots.push(node)
  }
  const placements = new Map(tree.placements.map((placement) => [placement.agent_id, placement]))
  const positions = new Map(tree.placements.map((placement) => [placement.agent_id, placement.position]))
  for (const agent of agents) {
    const category = categories.get(agent.agent_id), matched = !filter || category === filter
    const node: OverviewNode = { id: agent.agent_id, name: agent.name, path: agentPath(agent, tree), agent,
      children: [], counts: emptyCounts(), matches: Number(matched), parent: folders.get(placements.get(agent.agent_id)?.folder_id ?? "") }
    if (category && matched) node.counts[category] = 1
    if (node.parent) node.parent.children.push(node)
    else roots.push(node)
  }
  function order(left: OverviewNode, right: OverviewNode) {
    if (Boolean(left.agent) !== Boolean(right.agent)) return Number(Boolean(left.agent)) - Number(Boolean(right.agent))
    if (left.agent && right.agent) {
      const a = positions.get(left.id), b = positions.get(right.id)
      if (a !== undefined && b === undefined) return -1
      if (a === undefined && b !== undefined) return 1
      if (a !== undefined && b !== undefined && a !== b) return a - b
    }
    return left.name.localeCompare(right.name, undefined, { numeric: true, sensitivity: "base" }) || left.id.localeCompare(right.id)
  }
  const all = [...roots]
  for (let index = 0; index < all.length; index++) all.push(...all[index].children)
  for (let index = all.length - 1; index >= 0; index--) {
    const node = all[index]
    node.children.sort(order)
    if (node.agent) continue
    for (const child of node.children) {
      node.matches += child.matches
      for (const key of ["attention", "working", "idle"] as const) node.counts[key] += child.counts[key]
    }
  }
  roots.sort(order)
  return { roots, folders }
}

export function visibleOverviewRows(roots: OverviewNode[], filter: AttentionFilter | null, expanded: Record<string, boolean>) {
  const result: { node: OverviewNode; depth: number; open: boolean }[] = []
  const queue = roots.map((node) => ({ node, depth: 0 })).reverse()
  while (queue.length) {
    const { node, depth } = queue.pop()!
    if (filter && !node.matches) continue
    const open = expanded[node.id] ?? (filter !== null || node.counts.attention > 0 || node.counts.working > 0)
    result.push({ node, depth, open })
    if (!node.agent && open) for (let index = node.children.length - 1; index >= 0; index--) queue.push({ node: node.children[index], depth: depth + 1 })
  }
  return result
}
