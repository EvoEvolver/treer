import type { TransferAuth } from './prepared-claude'
import { api, proxyUrl, type Agent, type Machine } from './api'

export interface CopyScope { auth?: TransferAuth; workspace: string; source: string; machine: Machine; entry: string; cwd: string; staging: string }
export interface CopyPlan { scope: CopyScope; sha256: string; input: string; sessionId: string; sourceSessionId: string; directory: string; receipt?: PreparedReceipt }
export interface PreparedReceipt { schema: string; phase: string; inputSha256: string; sourceSessionId: string; sessionId: string; cwd: string; files: number; bytes: number; auth?: TransferAuth; ownerProject?: string; publicationIntent?: string; environment: { CLAUDE_CONFIG_DIR?: string } }
const relative = (v: string) => v.length > 0 && v.length <= 4096 && !/[\\\x00-\x1f\x7f]/.test(v) && v.split('/').every(s => s && s !== '.' && s !== '..')
const absolute = (v: string) => v.startsWith('/') && relative(v.slice(1))
export function copyScopeKey(s: CopyScope) { return JSON.stringify([s.workspace, s.source, s.machine.server_id, s.machine.root, s.entry, s.cwd, s.staging, s.auth ?? 'environment']) }
export function validateCopyScope(s: CopyScope) {
  if (s.auth !== undefined && s.auth !== 'environment' && s.auth !== 'owner-default') throw Error('session_copy_invalid')
  if (!s.workspace || !s.source || !s.machine.server_id || !(s.machine.root === '/' || absolute(s.machine.root)) || !absolute(s.entry) || !relative(s.cwd) || !relative(s.staging)) throw Error('session_copy_invalid')
}
const rootPath = (s: CopyScope, path: string) => `${s.machine.root.replace(/\/$/, '')}/${path}`
const machineBase = (s: CopyScope) => `/api/workspaces/${encodeURIComponent(s.workspace)}/machines/${encodeURIComponent(s.machine.server_id)}`
export const interfaceBase = (workspace: string, agent: string) => `/api/workspaces/${encodeURIComponent(workspace)}/agents/${encodeURIComponent(agent)}/interface/ui`

async function boundedExport(path: string, threadId: string, auth: TransferAuth | undefined) {
  const controller = new AbortController()
  const timer = setTimeout(() => controller.abort(), 35_000)
  try {
    const response = await fetch(proxyUrl(path), { method: 'POST', credentials: 'include', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ threadId, ...(auth === 'owner-default' ? { auth } : {}) }), signal: controller.signal, redirect: 'error', cache: 'no-store' })
    if (!response.ok || !response.body) throw Error('session_copy_export_refused')
    const declared = response.headers.get('content-length')
    if (declared !== null && (!/^[0-9]+$/.test(declared) || !Number.isSafeInteger(Number(declared)) || Number(declared) > 16 * 1024 * 1024)) {
      await response.body.cancel().catch(() => {})
      throw Error('session_copy_too_large')
    }
    const reader = response.body.getReader(); const chunks: Uint8Array[] = []; let size = 0
    try {
      for (;;) {
        const { value, done } = await reader.read(); if (done) break
        size += value.byteLength
        if (size > 16 * 1024 * 1024) throw Error('session_copy_too_large')
        chunks.push(value)
      }
    } finally { await reader.cancel().catch(() => {}) }
    const bytes = new Uint8Array(size); let at = 0
    for (const chunk of chunks) { bytes.set(chunk, at); at += chunk.length }
    return bytes
  } finally { clearTimeout(timer) }
}
export async function exportAndUpload(scope: CopyScope, active: () => boolean = () => true, planned?: (plan: CopyPlan) => void): Promise<CopyPlan> {
  validateCopyScope(scope)
  const base = interfaceBase(scope.workspace, scope.source)
  const beforeStatus = await api<any>(`${base}/v1/status`)
  if (beforeStatus.agent_id !== scope.source || typeof beforeStatus.interface_instance_id !== 'string' || !beforeStatus.interface_instance_id) throw Error('session_copy_identity')
  const before = await api<any>(`${base}/api/state`)
  const sourceSessionId = before.thread?.id
  if (!active()) throw Error('session_copy_scope_changed')
  if (before.agentId !== undefined && before.agentId !== scope.source) throw Error('session_copy_identity')
  if (typeof sourceSessionId !== 'string' || !/^[a-f0-9]{8}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{12}$/.test(sourceSessionId) || before.ready !== true || before.harness?.id !== 'claude' || before.runtime?.state !== 'idle') throw Error('session_copy_source_not_idle')
  const bytes = await boundedExport(`${base}/api/session/export`, sourceSessionId, scope.auth)
  const bundle = JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(bytes))
  if (bundle.schema !== 'treer.agent-session/v1' || bundle.harness !== 'claude' || bundle.runtimeSnapshot?.threadId !== sourceSessionId || bundle.runtimeSnapshot?.agentId !== scope.source || typeof bundle.runtimeSnapshot?.interfaceInstance !== 'string' || bundle.runtimeSnapshot?.interfaceInstance !== beforeStatus.interface_instance_id) throw Error('session_copy_identity')
  if (scope.auth === 'owner-default' ? bundle.runtimeSnapshot.auth !== 'owner-default' : bundle.runtimeSnapshot.auth !== undefined && bundle.runtimeSnapshot.auth !== 'environment') throw Error('session_copy_identity')
  const afterStatus = await api<any>(`${base}/v1/status`)
  if (afterStatus.agent_id !== scope.source || afterStatus.interface_instance_id !== beforeStatus.interface_instance_id) throw Error('session_copy_identity')
  const sha256 = Array.from(new Uint8Array(await crypto.subtle.digest('SHA-256', bytes))).map(x => x.toString(16).padStart(2, '0')).join('')
  const sessionId = crypto.randomUUID(); const file_name = `treer-session-${crypto.randomUUID()}.json`
  let binary = ''; for (let offset = 0; offset < bytes.length; offset += 32768) binary += String.fromCharCode(...bytes.subarray(offset, offset + 32768))
  const input = rootPath(scope, `${scope.staging}/${file_name}`)
  const plan = { scope, sha256, input, sessionId, sourceSessionId, directory: rootPath(scope, scope.cwd) }
  planned?.(plan)
  if (!active()) throw Error('session_copy_scope_changed')
  const upload = await api<any>(`${machineBase(scope)}/files`, { method: 'POST', body: JSON.stringify({ directory: scope.staging, file_name, content_base64: btoa(binary), overwrite: false }) })
  // The Controller returns a machine-root-relative file path, not a new URL.
  if (upload.server_id !== scope.machine.server_id || upload.path !== `${scope.staging}/${file_name}` || upload.bytes_written !== bytes.length || upload.sha256 !== undefined && upload.sha256 !== sha256) throw Error('session_copy_upload_binding')
  return plan
}
export async function inspectOrPrepare(plan: CopyPlan, phase: 'validated' | 'prepared'): Promise<PreparedReceipt> {
  const action = phase === 'validated' ? 'inspect' : 'prepare'
  const args = ['session', action, '--input', plan.input, '--directory', plan.directory, '--session-id', plan.sessionId, '--sha256', plan.sha256, ...(plan.scope.auth === 'owner-default' ? ['--auth', 'owner-default'] : [])]
  const response = await api<any>(`${machineBase(plan.scope)}/exec`, { method: 'POST', body: JSON.stringify({ cwd: plan.scope.staging, command: plan.scope.entry, args, timeout_ms: 30_000 }) })
  if (response.server_id !== plan.scope.machine.server_id || response.cwd !== plan.scope.staging || response.command !== plan.scope.entry || JSON.stringify(response.args) !== JSON.stringify(args) || response.exit_code !== 0 || response.timed_out !== false || response.truncated !== false || typeof response.stdout !== 'string' || response.stdout.length > 65536) throw Error('session_copy_execution')
  const receipt = JSON.parse(response.stdout) as PreparedReceipt
  if (receipt.schema !== 'treer.claude-prepared/v1' || receipt.phase !== phase || receipt.inputSha256 !== plan.sha256 || receipt.sourceSessionId !== plan.sourceSessionId || receipt.sessionId !== plan.sessionId || receipt.cwd !== plan.directory || !Number.isSafeInteger(receipt.files) || receipt.files < 1 || receipt.files > (plan.scope.auth === 'owner-default' ? 1026 : 1025) || !Number.isSafeInteger(receipt.bytes) || receipt.bytes < 1) throw Error('session_copy_receipt')
  const ownerDefault = plan.scope.auth === 'owner-default'
  if (!receipt.environment || typeof receipt.environment !== 'object' || Array.isArray(receipt.environment)) throw Error('session_copy_receipt')
  if (ownerDefault) {
    if (receipt.auth !== 'owner-default' || Object.keys(receipt.environment).length !== 0 || typeof receipt.ownerProject !== 'string' || !absolute(receipt.ownerProject) || typeof receipt.publicationIntent !== 'string' || !absolute(receipt.publicationIntent) || !receipt.ownerProject.endsWith(`/projects/${plan.directory.replace(/[^a-zA-Z0-9]/g, '-')}`) || receipt.publicationIntent !== `${plan.directory.slice(0, plan.directory.lastIndexOf('/'))}/.claude-transfer-${plan.sessionId}.json`) throw Error('session_copy_receipt')
    if (phase === 'prepared' && (receipt.ownerProject !== plan.receipt?.ownerProject || receipt.publicationIntent !== plan.receipt?.publicationIntent)) throw Error('session_copy_receipt')
  } else if ((receipt.auth !== undefined && receipt.auth !== 'environment') || Object.keys(receipt.environment).join(',') !== 'CLAUDE_CONFIG_DIR' || receipt.environment.CLAUDE_CONFIG_DIR !== `${plan.directory}/.harness/claude`) throw Error('session_copy_receipt')
  if (phase === 'prepared' && (!plan.receipt || plan.receipt.phase !== 'validated' || receipt.files !== plan.receipt.files || receipt.bytes !== plan.receipt.bytes)) throw Error('session_copy_receipt')
  return receipt
}
export async function observePreparedAgent(workspace: string, agent: Agent, expectedSession: string, signal: AbortSignal): Promise<boolean> {
  const base = interfaceBase(workspace, agent.agent_id)
  for (let i = 0; i < 20 && !signal.aborted; i++) {
    const attemptSignal = AbortSignal.any([signal, AbortSignal.timeout(3000)])
    try {
      const fresh = await api<Agent>(`/api/workspaces/${encodeURIComponent(workspace)}/agents/${encodeURIComponent(agent.agent_id)}`, { signal: attemptSignal })
      const health = await api<any>(`${base}/api/health`, { signal: attemptSignal })
      const state = await api<any>(`${base}/api/state`, { signal: attemptSignal })
      const status = await api<any>(`${base}/v1/status`, { signal: attemptSignal })
      if (fresh.agent_id === agent.agent_id && fresh.server_id === agent.server_id && typeof fresh.interface?.instance_id === 'string' && fresh.interface.instance_id === status.interface_instance_id && status.agent_id === agent.agent_id && state.harness?.id === 'claude' && health.ready === true && state.ready === true && state.thread?.id === expectedSession && state.runtime?.state === 'idle') return true
    } catch { if (signal.aborted) return false }
    await new Promise<void>(resolve => { const timer = setTimeout(resolve, 1500); signal.addEventListener('abort', () => { clearTimeout(timer); resolve() }, { once: true }) })
  }
  return false
}
