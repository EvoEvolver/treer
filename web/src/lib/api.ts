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

export interface BuildInfo {
  version: string
  git_commit: string
}

export interface Machine {
  server_id: string
  name?: string
  hostname?: string
  root: string
  controller_build: BuildInfo
  host_build: BuildInfo
  status: string
  available_agents?: string[]
}

export interface Agent {
  agent_id: string
  server_id: string
  name: string
  kind: string
  status: string
  interface?: AgentInterface
}

export interface AgentInterface {
  protocol: string
  instance_id: string
  port: number
  capabilities: string[]
  ui_path?: string
  registered_at: string
}

export interface AgentLaunchProfile {
  profile_id: string
  workspace_id: string
  name: string
  description: string
  cwd: string
  command: string
  args: string[]
  created_at: string
  created_by: string
  updated_at: string
  updated_by: string
}

export interface Snapshot {
  revision: number
  workspace: Workspace
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
  user_count: number
  organization_count: number
  machine_count: number
  agent_count: number
}

export interface ControlPlaneService {
  name: string
  present: boolean
  image?: string
  digest?: string | null
  version?: string | null
  revision?: string | null
  channel_digest?: string
  update_available?: boolean
}

export interface ControlPlaneJob {
  id: string
  state: "running" | "succeeded" | "failed"
  error?: string | null
}

export interface ControlPlaneUpdateStatus {
  channel: string
  services: ControlPlaneService[]
  job?: ControlPlaneJob | null
  update_available?: boolean
}

export interface AdminUser {
  user_id: string
  email: string
  preferred_name: string
  email_verified: boolean
  created_at: string
}

export interface AdminUserDetail extends AdminUser {
  password_login: boolean
  oauth_providers: string[]
  organizations: Array<{ organization_id: string; name: string; role: string }>
  workspaces: Array<{ workspace_id: string; name: string; organization_id: string }>
}

export interface AdminMachine {
  server_id: string
  name: string
  hostname: string
  workspace_id: string
  workspace_name: string
  created_at: string
  enrolled_by: string
  status: string
  last_seen_at?: string | null
  root?: string | null
  agents?: Agent[]
}

export interface AdminOrganization {
  organization_id: string
  name: string
  created_at: string
  owner_id?: string | null
  owner_name?: string | null
  owner_email?: string | null
  workspace_count: number
  machine_count: number
}

export interface AdminInvitation {
  token: string
  created_at: string
  url: string
}

export interface PlatformAuditEvent {
  sequence: number
  event_id: string
  occurred_at: string
  action: string
  resource_kind: string
  resource_id: string
  resource_name?: string | null
  payload: Record<string, unknown>
}

export interface VirtualNetworkHost {
  hostname: string
  service_id: string
  service_protocol: "tcp" | "http"
  destination_server_id: string
  destination_agent_id?: string
  target_host: string
  target_port?: number
}

export interface MachineService {
  service_id: string
  name: string
  server_id: string
  target_agent_id?: string
  target_host: string
  target_port: number
  protocol: "tcp" | "http"
  updated_at: string
  updated_by: string
}

export interface ServiceIngress {
  ingress_id: string
  service_id: string
  hostname: string
  url: string
  access: "public" | "workspace"
  enabled: boolean
  updated_at: string
  updated_by: string
}

export interface MachineTrafficRecord {
  window_start: string
  source_server_id: string
  destination_server_id: string
  payload_bytes: number
  payload_frames: number
}

export interface OrganizationAuditEvent {
  sequence: number
  event_id: string
  organization_id: string
  workspace_id?: string
  occurred_at: string
  actor_kind: string
  actor_id?: string
  actor_name?: string
  source: string
  action: string
  outcome: "succeeded" | "failed"
  resource_kind: string
  resource_id: string
  resource_name?: string
  correlation_id?: string
  payload: Record<string, unknown>
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

interface RuntimeConfig {
  proxy_url?: string
}

let proxyOrigin = window.location.origin

export async function loadRuntimeConfig() {
  if (import.meta.env.DEV) return
  const response = await fetch("/config.json", { cache: "no-store" })
  if (!response.ok) {
    throw new Error(`Unable to load Treer runtime configuration (HTTP ${response.status})`)
  }
  const config = (await response.json()) as RuntimeConfig
  if (!config.proxy_url) throw new Error("Treer runtime configuration has no proxy URL")
  const url = new URL(config.proxy_url)
  if (!(["http:", "https:"] as string[]).includes(url.protocol) || url.username || url.password) {
    throw new Error("Treer runtime configuration has an invalid proxy URL")
  }
  proxyOrigin = url.origin
}

export function proxyUrl(path: string) {
  return new URL(path, `${proxyOrigin}/`).toString()
}

export async function api<T>(path: string, options?: RequestInit): Promise<T> {
  const response = await fetch(proxyUrl(path), {
    ...options,
    credentials: "include",
    headers: { "content-type": "application/json", ...options?.headers },
  })
  const text = await response.text()
  let body: T & ApiErrorBody
  try {
    body = JSON.parse(text) as T & ApiErrorBody
  } catch {
    const fallback =
      response.status === 502 || response.status === 503
        ? "control-plane updater sidecar is unreachable"
        : `HTTP ${response.status}`
    throw new ApiError(fallback, response.status)
  }
  if (!response.ok) throw new ApiError(body.error?.message ?? `HTTP ${response.status}`, response.status)
  return body
}

export function machineName(machine: Machine | undefined, fallback = "Unknown machine") {
  return machine?.name || machine?.hostname || machine?.server_id || fallback
}

export function websocketUrl(path: string) {
  const url = new URL(proxyUrl(path))
  url.protocol = url.protocol === "https:" ? "wss:" : "ws:"
  return url.toString()
}
