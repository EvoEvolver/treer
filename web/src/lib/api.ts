export interface User {
  username: string
  is_admin?: boolean
}

export interface Organization {
  organization_id: string
  name: string
}

export interface Workspace {
  workspace_id: string
  name: string
}

export interface Machine {
  server_id: string
  name?: string
  hostname?: string
  root: string
  status: string
}

export interface Agent {
  agent_id: string
  server_id: string
  name: string
  kind: string
  status: string
}

export interface Snapshot {
  revision: number
  servers: Machine[]
  agents: Agent[]
}

export interface Member {
  username: string
  role: "owner" | "admin" | "member"
}

export interface ApiErrorBody {
  error?: { message?: string }
}

export class ApiError extends Error {
  status: number

  constructor(message: string, status: number) {
    super(message)
    this.status = status
  }
}

export async function api<T>(path: string, options?: RequestInit): Promise<T> {
  const response = await fetch(path, {
    ...options,
    headers: { "content-type": "application/json", ...options?.headers },
  })
  const body = (await response.json()) as T & ApiErrorBody
  if (!response.ok) throw new ApiError(body.error?.message ?? `HTTP ${response.status}`, response.status)
  return body
}

export function machineName(machine: Machine | undefined, fallback = "Unknown machine") {
  return machine?.name || machine?.hostname || machine?.server_id || fallback
}

export function websocketUrl(path: string) {
  const protocol = window.location.protocol === "https:" ? "wss" : "ws"
  return `${protocol}://${window.location.host}${path}`
}
