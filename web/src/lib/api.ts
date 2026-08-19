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

export interface MailAddress {
  agent_id: string
  name: string
}

export interface HumanMailAddress {
  user_id: string
  preferred_name: string
}

export interface MailMessage {
  message_id: string
  workspace_id: string
  sender: MailAddress
  recipients: MailAddress[]
  human_recipients: HumanMailAddress[]
  context_ids: string[]
  body: string
  created_at: string
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
  const body = (await response.json()) as T & ApiErrorBody
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
