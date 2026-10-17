import type { Agent, AppDeployment } from './api'

export type PinKind = 'agent' | 'app' | 'folder'
export interface WorkspacePin {
  kind: PinKind
  id: string
  pinned_at: string
  pinned_by: string | null
}
export interface WorkspacePins {
  pins: WorkspacePin[]
  limit: number
  can_pin: boolean
  revision: number
}
export const PINS_CAPABILITY = 'workspace.pins.v1'
export const PINS_EVENT = 'treer:workspace-pins-changed'
const idPattern = /^[A-Za-z0-9_.:-]{1,128}$/
const timestampPattern = /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d{1,9})?(?:Z|[+-]\d{2}:\d{2})$/

export function parseWorkspacePins(value: unknown): WorkspacePins {
  const data = value as WorkspacePins
  if (!data || !Array.isArray(data.pins) || data.pins.length > 50 || data.limit !== 50
    || typeof data.can_pin !== 'boolean' || !Number.isSafeInteger(data.revision) || data.revision < 0) {
    throw new Error('Invalid pins response')
  }
  const seen = new Set<string>()
  for (const pin of data.pins) {
    if (!pin || !['agent', 'app', 'folder'].includes(pin.kind)
      || typeof pin.id !== 'string' || !idPattern.test(pin.id)
      || typeof pin.pinned_at !== 'string' || !timestampPattern.test(pin.pinned_at) || !Number.isFinite(Date.parse(pin.pinned_at))
      || (pin.pinned_by !== null && (typeof pin.pinned_by !== 'string' || !idPattern.test(pin.pinned_by)))
      || seen.has(`${pin.kind}:${pin.id}`)) {
      throw new Error('Invalid pin')
    }
    seen.add(`${pin.kind}:${pin.id}`)
  }
  return {
    pins: data.pins.map(({ kind, id, pinned_at, pinned_by }) => ({ kind, id, pinned_at, pinned_by })),
    limit: 50,
    can_pin: data.can_pin,
    revision: data.revision,
  }
}

export function workloadPin(agent: Agent, apps: AppDeployment[]): Pick<WorkspacePin, 'kind' | 'id'> | null {
  const app = apps.find((item) => (item.runtime_agent_id ?? item.app_id) === agent.agent_id || item.app_id === agent.agent_id)
  if (app) return { kind: 'app', id: app.app_id }
  // Wait for App metadata instead of pinning an App runtime as an ordinary Agent.
  return agent.kind === 'app' ? null : { kind: 'agent', id: agent.agent_id }
}
