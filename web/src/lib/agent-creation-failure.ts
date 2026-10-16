export interface AgentCreationFailure {
  agent_id: string
  machine_id: string
  command?: string
  creation_state: "identity_removed" | "reserved_outcome_unknown" | "cleanup_failed"
}

// These fields drive presentation only. Core independently authorizes cleanup.
export function agentCreationFailure(details: unknown, machineId: string): AgentCreationFailure | null {
  if (!details || typeof details !== "object") return null
  const value = details as Record<string, unknown>
  if (value.machine_id !== machineId || typeof value.agent_id !== "string" || !/^[A-Za-z0-9_-]{1,128}$/.test(value.agent_id)) return null
  if (value.creation_state !== "identity_removed" && value.creation_state !== "reserved_outcome_unknown" && value.creation_state !== "cleanup_failed") return null
  const command = typeof value.command === "string" && /^(?!\.{1,2}$)[A-Za-z0-9._-]{1,128}$/.test(value.command) && !value.command.endsWith(".") ? value.command : undefined
  return { agent_id: value.agent_id, machine_id: machineId, creation_state: value.creation_state, command }
}
