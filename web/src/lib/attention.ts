import { machineName, type Agent, type Machine } from "@/lib/api"

const statuses = new Set(["starting", "working", "idle", "blocked", "exited", "failed", "unknown"])
const statusOf = (agent: Agent) => statuses.has(agent.status) ? agent.status : agent.status === "stopped" ? "exited" : "unknown"
const nameOrder = (left: Agent, right: Agent) => left.name.localeCompare(right.name, undefined, { sensitivity: "base" }) || left.agent_id.localeCompare(right.agent_id)
const updated = (agent: Agent) => Date.parse(agent.updated_at ?? "") || 0
const recentOrder = (left: Agent, right: Agent) => updated(right) - updated(left) || nameOrder(left, right)

// Unknown legacy statuses are
// neutral; stopped workloads remain discoverable in the inactive list.
export function attentionGroups(agents: Agent[], machines: Machine[]) {
  const inventory = new Map(machines.map((machine) => [machine.server_id, machine]))
  const result = { attention: [] as Agent[], working: [] as Agent[], idle: [] as Agent[], inactive: [] as Agent[] }
  for (const agent of agents) {
    if (agent.kind === "app") continue
    const status = statusOf(agent), online = inventory.get(agent.server_id)?.status === "online"
    if (status === "blocked" || status === "failed" || (!online && status !== "exited")) result.attention.push(agent)
    else if (online && (status === "starting" || status === "working")) result.working.push(agent)
    else if (online && (status === "idle" || status === "unknown")) result.idle.push(agent)
    else result.inactive.push(agent)
  }
  result.attention.sort(recentOrder)
  result.working.sort(recentOrder)
  result.idle.sort((left, right) => machineName(inventory.get(left.server_id), left.server_id).localeCompare(machineName(inventory.get(right.server_id), right.server_id), undefined, { sensitivity: "base" }) || nameOrder(left, right))
  result.inactive.sort(nameOrder)
  return result
}
