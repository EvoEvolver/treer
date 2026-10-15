import { machineName, proxyUrl, type Agent, type AgentLaunchProfile, type Machine, type Organization, type Snapshot, type User, type Workspace } from "@/lib/api"
import { t } from "@/i18n"


export type TerminalState = "not attached" | "connecting" | "live" | "reconnecting" | "closed" | "error"

export type MainView = "overview" | "terminal" | "apps" | "app" | "audit" | "machine" | "organization" | "workspace"

export type RenameTarget = { kind: "machine" | "agent"; id: string; name: string } | null

export type DeleteTarget = { kind: "machine" | "agent"; id: string; name: string } | null

export const PREVIEW_USER: User = { user_id: "usr_preview", email: "you@example.com", preferred_name: "You" }

export const PREVIEW_ORG: Organization = { organization_id: "org_preview", name: "You Personal", role: "owner" }

export const PREVIEW_WORKSPACE: Workspace = { workspace_id: "ws_preview", name: "lab" }

export const PREVIEW_NOW = "2026-08-25T00:00:00.000Z"

export const PREVIEW_PROFILES: AgentLaunchProfile[] = [
  { profile_id: "codex", workspace_id: "ws_preview", name: "Codex", description: "OpenAI Codex", cwd: ".", command: "codex", args: [], created_at: PREVIEW_NOW, created_by: "usr_preview", updated_at: PREVIEW_NOW, updated_by: "usr_preview" },
  { profile_id: "claude", workspace_id: "ws_preview", name: "Claude", description: "Anthropic Claude Code", cwd: ".", command: "claude", args: [], created_at: PREVIEW_NOW, created_by: "usr_preview", updated_at: PREVIEW_NOW, updated_by: "usr_preview" },
  { profile_id: "pi", workspace_id: "ws_preview", name: "Pi", description: "Pi coding agent", cwd: ".", command: "pi", args: [], created_at: PREVIEW_NOW, created_by: "usr_preview", updated_at: PREVIEW_NOW, updated_by: "usr_preview" },
  { profile_id: "opencode", workspace_id: "ws_preview", name: "OpenCode", description: "OpenCode", cwd: ".", command: "opencode", args: [], created_at: PREVIEW_NOW, created_by: "usr_preview", updated_at: PREVIEW_NOW, updated_by: "usr_preview" },
]

export const PREVIEW_INSTALL = "curl -fsSL 'https://treer.example/install.sh' | sh"

export const PREVIEW_CONNECT = "treer-agent-server connect --key 'enr_v1_…' --proxy 'https://treer.example/'"

export const activeStatuses = new Set(["running", "starting", "working", "idle", "blocked"])

export function initials(value: string) {
  return value.trim().slice(0, 2).toUpperCase() || "T"
}

export function replaceWorkspace(items: Workspace[], updated: Workspace) {
  return items
    .map((item) => item.workspace_id === updated.workspace_id ? updated : item)
    .sort((left, right) => left.name.localeCompare(right.name) || left.workspace_id.localeCompare(right.workspace_id))
}

export function visibleWorkspaceSnapshot(snapshot: Snapshot): Snapshot {
  return snapshot
}

export function workspacePath(organizationId: string | null, workspaceId: string | null) {
  if (!organizationId) return "/"
  const organizationPath = `/orgs/${encodeURIComponent(organizationId)}`
  return workspaceId
    ? `${organizationPath}/workspaces/${encodeURIComponent(workspaceId)}`
    : organizationPath
}

export function defaultAgentName(kind: string) {
  const now = new Date()
  const month = String(now.getMonth() + 1).padStart(2, "0")
  const day = String(now.getDate()).padStart(2, "0")
  const prefix = kind === "terminal" ? "terminal" : kind === "command" ? "cmd" : kind === "codex" || kind === "claude" || kind === "installer" ? kind : "agent"
  return `${prefix}-${now.getFullYear()}-${month}-${day}`
}

export function defaultProfileAgentName(profileName: string) {
  const now = new Date()
  const pad = (value: number) => String(value).padStart(2, "0")
  const slug = profileName.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-+|-+$/g, "").slice(0, 40) || "agent"
  const stamp = `${now.getFullYear()}-${pad(now.getMonth() + 1)}-${pad(now.getDate())}-${pad(now.getHours())}${pad(now.getMinutes())}${pad(now.getSeconds())}`
  return `${slug}-${stamp}`.slice(0, 80)
}

export function buildLabel(build: Machine["controller_build"]) {
  const commit = build.git_commit === "unknown" ? build.git_commit : build.git_commit.slice(0, 8)
  return `${build.version}@${commit}`
}

export function supervisionLabel(mode: NonNullable<Machine["supervision"]>["mode"]) {
  if (mode === "systemd_user") return "systemd user"
  if (mode === "launchd") return "LaunchAgent"
  if (mode === "nohup") return "nohup"
  if (mode === "detached") return t("machine.supervision_windows")
  return "foreground"
}

export function authorizationReturnUrl() {
  const value = new URLSearchParams(window.location.search).get("return_to")
  if (!value) return null
  try {
    const candidate = new URL(value)
    const proxy = new URL(proxyUrl("/"))
    const allowedPaths = new Set(["/.treer/ingress/authorize", "/api/apps/oauth/authorize"])
    return !candidate.username && !candidate.password && !candidate.hash && candidate.origin === proxy.origin && allowedPaths.has(candidate.pathname) ? candidate.toString() : null
  } catch {
    return null
  }
}

export function machineOnline(machine?: Machine) {
  return machine?.status === "online"
}

export function machineListen(machine?: Machine) {
  return machine?.labels?.["treer.listen"]
}

export function machineDisplayName(machine: Machine, machines: Machine[] = []) {
  const name = machineName(machine)
  const hostname = machine.hostname
  if (!hostname) return name
  const collisions = machines.filter((item) => item.hostname === hostname)
  if (collisions.length < 2) return name
  const suffix = machine.server_id.replace(/^srv_/, "").slice(0, 6)
  const port = machineListen(machine)?.split(":").pop()
  return port ? `${name} · ${suffix} :${port}` : `${name} · ${suffix}`
}

export function machineRecoveryCommands(workspaceId: string) {
  return {
    start: `treer-agent-server restore --workspace ${workspaceId}`,
    restartController: `treer-agent-server service --workspace ${workspaceId} restart-controller`,
  }
}

export function agentDisplayStatus(agent: Agent, machine?: Machine) {
  return machineOnline(machine) ? agent.status : "offline"
}

export function controlCharacter(value: string) {
  const character = value[0]
  if (!character) return null
  const upper = character.toUpperCase()
  if (upper >= "A" && upper <= "Z") return String.fromCharCode(upper.charCodeAt(0) - 64)
  const special: Record<string, string> = { "@": "\x00", " ": "\x00", "[": "\x1b", "\\": "\x1c", "]": "\x1d", "^": "\x1e", "_": "\x1f", "?": "\x7f" }
  return special[character] ?? null
}

export function formatBytes(value: number) {
  if (value < 1024) return `${value} B`
  const units = ["KB", "MB", "GB", "TB"]
  let amount = value / 1024
  let unit = units[0]
  for (let index = 1; index < units.length && amount >= 1024; index += 1) {
    amount /= 1024
    unit = units[index]
  }
  return `${amount >= 10 ? amount.toFixed(1) : amount.toFixed(2)} ${unit}`
}

export function arrayBufferToBase64(buffer: ArrayBuffer) {
  const bytes = new Uint8Array(buffer)
  const chunks: string[] = []
  for (let offset = 0; offset < bytes.length; offset += 32_768) {
    chunks.push(String.fromCharCode(...bytes.subarray(offset, offset + 32_768)))
  }
  return window.btoa(chunks.join(""))
}
