import type { Machine } from "@/lib/api"

export interface AgentCatalogEntry {
  kind: string
  command: string
  label: string
  install: string | null
  start: string
}

export const AGENT_CATALOG: AgentCatalogEntry[] = [
  { kind: "claude", command: "claude", label: "Claude", install: "curl -fsSL https://claude.ai/install.sh | bash", start: "claude --dangerously-skip-permissions" },
  { kind: "cursor", command: "cursor-agent", label: "Cursor", install: "curl https://cursor.com/install -fsS | bash", start: "cursor-agent" },
  { kind: "grok", command: "grok", label: "Grok", install: null, start: "grok --always-approve" },
  { kind: "opencode", command: "opencode", label: "OpenCode", install: "curl -fsSL https://opencode.ai/install | bash", start: "opencode" },
  { kind: "pi", command: "pi", label: "Pi", install: "curl -fsSL https://pi.dev/install.sh | sh", start: "pi" },
  { kind: "codex", command: "codex", label: "Codex", install: "curl -fsSL https://chatgpt.com/codex/install.sh | sh", start: "codex --dangerously-bypass-approvals-and-sandbox" },
]

export function agentKindFromCommand(command: string): string | null {
  const file = command.split(/[/\\]/).pop() ?? command
  const name = file.replace(/\.exe$/i, "")
  if (name === "cursor-agent") return "cursor"
  return AGENT_CATALOG.find((entry) => entry.kind === name || entry.command === name)?.kind ?? null
}

export function catalogEntry(kind: string): AgentCatalogEntry | undefined {
  const normalized = kind === "cursor-agent" ? "cursor" : kind
  return AGENT_CATALOG.find((entry) => entry.kind === normalized)
}

export function machineReportsAgents(machine?: Machine | null): boolean {
  return machine?.available_agents != null
}

export function isAgentInstalled(machine: Machine | undefined | null, kind: string): boolean | null {
  if (!machineReportsAgents(machine)) return null
  const normalized = kind === "cursor-agent" ? "cursor" : kind
  return (machine?.available_agents ?? []).includes(normalized)
}

export function availableCatalog(machine?: Machine | null): AgentCatalogEntry[] {
  if (!machineReportsAgents(machine)) return AGENT_CATALOG
  const installed = new Set(machine?.available_agents ?? [])
  return AGENT_CATALOG.filter((entry) => installed.has(entry.kind))
}

export function installThenStartScript(entry: AgentCatalogEntry): string | null {
  if (!entry.install) return null
  return `${entry.install} && echo && echo 'treer: install finished; starting ${entry.label} for login' && exec ${entry.start}`
}

export interface AcpLaunchOption {
  id: string
  harness: string
  label: string
  catalogKind: string
}

export const ACP_LAUNCH_OPTIONS: AcpLaunchOption[] = [
  { id: "acp-grok", harness: "grok", label: "Grok thread", catalogKind: "grok" },
  { id: "acp-cursor", harness: "cursor", label: "Cursor thread", catalogKind: "cursor" },
  { id: "acp-codex", harness: "codex", label: "Codex thread", catalogKind: "codex" },
  { id: "acp-claude", harness: "claude", label: "Claude thread", catalogKind: "claude" },
  { id: "acp-opencode", harness: "opencode", label: "OpenCode thread", catalogKind: "opencode" },
]

export function acpOption(profileId: string): AcpLaunchOption | undefined {
  return ACP_LAUNCH_OPTIONS.find((item) => item.id === profileId)
}

export function isAcpLaunch(profileId: string): boolean {
  return profileId === "import" || Boolean(acpOption(profileId))
}

export function acpLaunchArgs(harness: string, sessionId?: string): string[] {
  const args = ["--harness", harness]
  if (sessionId) args.push("--session-id", sessionId)
  return args
}

export const TREER_EMBED_UI_QUERY = "presentation=workspace&explorer=1&shell=0&permissions=0&nav=0"
