import type { Agent, Snapshot } from '@/lib/api'
import { appOwnUrl } from '@/lib/app-url'

export function agentOriginUrl(agent: Agent, blockedOrigins: readonly string[]) {
  const value = agent.interface?.origin_url
  const own = appOwnUrl({ public_url: value }, blockedOrigins)
  if (!own) return null
  const url = new URL(own)
  // The read-model contract is an origin root, never an arbitrary path or a
  // token-bearing link. Do not construct hosts from Controller inventory.
  return url.pathname === '/' && !url.search && !url.hash && /^ag-[0-9a-f]{32}\./.test(url.hostname) ? own : null
}

export function interfaceSignature(agent: Agent) {
  if (!agent.interface) return ''
  const descriptor = agent.interface
  return JSON.stringify([descriptor.protocol, descriptor.instance_id, descriptor.port, descriptor.ui_path, descriptor.registered_at, [...descriptor.capabilities].sort()])
}
export function interfaceInventory(snapshot: Snapshot) {
  return JSON.stringify(snapshot.agents.filter((agent) => agent.interface).map((agent) => [agent.agent_id, interfaceSignature(agent)]).sort(([a], [b]) => a.localeCompare(b)))
}

// Only authenticated read models can supply origin_url. Events cannot grant or
// replace it; keep a known URL only for the exact registered interface.
export function inventoryWithOrigins(snapshot: Snapshot, readAgents: readonly Agent[]): Snapshot {
  const known = new Map(readAgents.map((agent) => [agent.agent_id, agent]))
  return { ...snapshot, agents: snapshot.agents.map((agent) => {
    if (!agent.interface) return agent
    const previous = known.get(agent.agent_id)
    const { origin_url: _untrusted, ...descriptor } = agent.interface
    const origin_url = previous && interfaceSignature(previous) === interfaceSignature(agent) ? previous.interface?.origin_url : undefined
    return { ...agent, interface: { ...descriptor, ...(origin_url ? { origin_url } : {}) } }
  }) }
}
