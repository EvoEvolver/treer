export interface User {
  user_id: string
  email: string
  preferred_name: string
}

export interface Organization {
  organization_id: string
  name: string
  role: "owner" | "admin" | "member"
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
  user_id: string
  email: string
  preferred_name: string
  role: "owner" | "admin" | "member"
}

export interface AdminDashboard {
  machine_count: number
  agent_count: number
}

export interface VirtualNetworkHost {
  hostname: string
  service_id: string
  service_protocol: "tcp" | "http"
  destination_server_id: string
  target_host: string
  target_port?: number
}

export interface MachineService {
  service_id: string
  name: string
  server_id: string
  target_host: string
  target_port: number
  protocol: "tcp" | "http"
  updated_at: string
  updated_by: string
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
