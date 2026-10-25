// UI-only argv construction. Native revalidates the physical plan and private identity.
export type TransferAuth = 'environment' | 'owner-default'
export interface PreparedClaude { auth?: TransferAuth; entry: string; directory: string; sessionId: string; acknowledgedFor: string }
export const emptyPreparedClaude = (): PreparedClaude => ({ entry: '', directory: '', sessionId: '', acknowledgedFor: '' })
const absolute = (v: string) => v.startsWith('/') && v !== '/' && v.length <= 4096 && !/[\\\x00-\x1f\x7f]/.test(v) && v.split('/').slice(1).every(s => s && s !== '.' && s !== '..')
export function preparedClaudeBinding(machineId: string, root: string, cwd: string, v: PreparedClaude) {
  return JSON.stringify([machineId, root, cwd, v.entry, v.directory, v.sessionId, v.auth ?? 'environment'])
}
export function preparedClaudeArgs(machineId: string, root: string, cwd: string, v: PreparedClaude): string[] | null {
  const relative = cwd.length > 0 && cwd.length <= 4096 && !/[\\\x00-\x1f\x7f]/.test(cwd) && cwd.split('/').every(s => s && s !== '.' && s !== '..')
  const base = root.endsWith('/') ? root.slice(0, -1) : root
  if (v.auth !== undefined && v.auth !== 'environment' && v.auth !== 'owner-default') return null
  if (!machineId || !relative || !(absolute(base) || root === '/') || !absolute(v.entry) || !absolute(v.directory)
    || v.directory !== `${base}/${cwd}` || !/^[a-f0-9]{8}-[a-f0-9]{4}-4[a-f0-9]{3}-[89ab][a-f0-9]{3}-[a-f0-9]{12}$/.test(v.sessionId)
    || v.acknowledgedFor !== preparedClaudeBinding(machineId, root, cwd, v)) return null
  return [v.entry, 'session', 'run', '--directory', v.directory, '--session-id', v.sessionId, '--auth', v.auth ?? 'environment']
}
