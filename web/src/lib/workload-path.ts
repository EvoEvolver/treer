import type { Agent, WorkloadFolder, WorkloadTreeState } from "@/lib/api"

export function folderPath(folder: WorkloadFolder, folders: WorkloadFolder[]): string {
  const names = [folder.name]
  const visited = new Set([folder.folder_id])
  let parentId = folder.parent_id
  while (parentId && !visited.has(parentId)) {
    visited.add(parentId)
    const parent = folders.find((entry) => entry.folder_id === parentId)
    if (!parent) break
    names.unshift(parent.name)
    parentId = parent.parent_id
  }
  return `/${names.join("/")}`
}

// Inventory paths are authoritative until the tree has a persisted placement.
// Afterwards, use the live tree so folder moves/renames are reflected immediately.
export function agentPath(agent: Agent, tree?: WorkloadTreeState): string {
  const placement = tree?.placements.find((entry) => entry.agent_id === agent.agent_id)
  if (!placement) return agent.path || (tree ? `/${agent.name}` : agent.agent_id)
  const folder = tree?.folders.find((entry) => entry.folder_id === placement.folder_id)
  if (placement.folder_id && !folder) return agent.path || agent.agent_id
  return `${folder ? folderPath(folder, tree!.folders) : ""}/${agent.name}`
}

// Runtime kinds describe process mechanics; the console distinguishes only Apps.
export function workloadKind(agent: Pick<Agent, "kind">): "agent" | "app" {
  return agent.kind === "app" ? "app" : "agent"
}
