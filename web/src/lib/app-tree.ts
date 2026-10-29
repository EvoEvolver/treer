import { ApiError, proxyUrl } from "@/lib/api"

export type AppTreeNode = { key: string; name: string; kind: "app" | "agent"; capabilities: string[] }
export type AppTreePage = { schema_version: 1; path: string[]; nodes: AppTreeNode[] }
export const validAppSegment = (value: unknown): value is string => typeof value === "string" && /^[A-Za-z0-9][A-Za-z0-9_-]{0,63}$/.test(value) && value !== "." && value !== ".."
export function decodeAppTree(value: unknown, path: string[]): AppTreePage {
  const page = value as AppTreePage
  if (!page || page.schema_version !== 1 || !Array.isArray(page.path) || JSON.stringify(page.path) !== JSON.stringify(path) || path.length > 8 || !path.every(validAppSegment) || !Array.isArray(page.nodes) || page.nodes.length > 100) throw new Error("invalid_app_tree")
  const seen = new Set<string>()
  for (const node of page.nodes) {
    if (!node || !validAppSegment(node.key) || seen.has(node.key) || (node.kind !== "app" && node.kind !== "agent") || typeof node.name !== "string" || !node.name.trim() || node.name.length > 256 || !Array.isArray(node.capabilities) || node.capabilities.length > 16 || !node.capabilities.every(c => typeof c === "string" && c.length <= 64) || path.length >= 8) throw new Error("invalid_app_tree")
    seen.add(node.key)
  }
  return page
}

// Bound the stream before JSON parsing; an App controls its own directory bytes.
export async function appJson(path: string, signal: AbortSignal, options?: RequestInit, limit = 65536): Promise<unknown> {
  const response = await fetch(proxyUrl(path), { ...options, signal, credentials: "include", cache: "no-store", redirect: "error", headers: { "Content-Type": "application/json", ...options?.headers } })
  if (!response.ok) { await response.body?.cancel(); throw new ApiError(`HTTP ${response.status}`, response.status) }
  const reader = response.body?.getReader()
  if (!reader) throw new Error("invalid_app_response")
  let bytes = 0
  const chunks: Uint8Array[] = []
  try {
    for (;;) {
      const { done, value } = await reader.read()
      if (done) break
      bytes += value.byteLength
      if (bytes > limit) throw new Error("app_response_too_large")
      chunks.push(value)
    }
  } catch (error) { await reader.cancel(); throw error }
  const buffer = new Uint8Array(bytes)
  let offset = 0
  for (const chunk of chunks) { buffer.set(chunk, offset); offset += chunk.byteLength }
  return JSON.parse(new TextDecoder("utf-8", { fatal: true }).decode(buffer))
}

export async function readAppTree(workspaceId: string, appId: string, path: string[], signal: AbortSignal) {
  return decodeAppTree(await appJson(`/api/workspaces/${encodeURIComponent(workspaceId)}/apps/${encodeURIComponent(appId)}/ui/api/tree?path=${encodeURIComponent(path.join("/"))}`, signal), path)
}

export const APP_TREE_UI_CAPABILITY = "app-tree.v1"

export type AppNodeSelection = { appId: string; ownerId: string; path: string[]; node: AppTreeNode }

// Fallback for Proxies that predate AppDeployment.ui_capabilities: the Console asks
// each running App once whether `GET api/tree?path=` is part of its UI API.
// A schema_version 1 JSON root page (even an invalid or empty one) or an auth
// refusal means the App owns a directory; 404/405/501, an HTML SPA fallback or
// any other non-tree answer means it does not. Definitive answers are kept for
// the page session; network errors, timeouts and 5xx are retried later.
export type AppTreeSupport = "yes" | "no" | "unknown"
const support = new Map<string, { value: AppTreeSupport | Promise<AppTreeSupport>; at: number }>()
const RETRY_UNKNOWN_MS = 30000
const PROBE_TIMEOUT_MS = 4000
const supportKey = (workspaceId: string, appId: string) => `${workspaceId}\n${appId}`

export function knownAppTreeSupport(workspaceId: string, appId: string): AppTreeSupport {
  const entry = support.get(supportKey(workspaceId, appId))
  return entry && typeof entry.value === "string" ? entry.value : "unknown"
}

async function requestAppTreeSupport(workspaceId: string, appId: string): Promise<AppTreeSupport> {
  const controller = new AbortController()
  const timer = window.setTimeout(() => controller.abort(), PROBE_TIMEOUT_MS)
  try {
    const page = await appJson(`/api/workspaces/${encodeURIComponent(workspaceId)}/apps/${encodeURIComponent(appId)}/ui/api/tree?path=`, controller.signal, { headers: { Accept: "application/json" } }) as { schema_version?: unknown } | null
    return page && typeof page === "object" && page.schema_version === 1 ? "yes" : "no"
  } catch (reason) {
    if (reason instanceof ApiError) return reason.status === 401 || reason.status === 403 ? "yes" : reason.status >= 500 && reason.status !== 501 ? "unknown" : "no"
    // A body that is not JSON (or not UTF-8) is some other route, e.g. an SPA fallback page.
    if (reason instanceof SyntaxError || reason instanceof TypeError && /encod/i.test(reason.message)) return "no"
    return reason instanceof Error && reason.message === "app_response_too_large" ? "no" : "unknown"
  } finally { clearTimeout(timer) }
}

export function probeAppTreeSupport(workspaceId: string, appId: string): Promise<AppTreeSupport> {
  const key = supportKey(workspaceId, appId)
  const entry = support.get(key)
  if (entry && (entry.value !== "unknown" || Date.now() - entry.at < RETRY_UNKNOWN_MS)) return Promise.resolve(entry.value)
  const pending = requestAppTreeSupport(workspaceId, appId).then(value => { support.set(key, { value, at: Date.now() }); return value })
  support.set(key, { value: pending, at: Date.now() })
  return pending
}
