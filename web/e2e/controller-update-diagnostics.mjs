import { lstatSync, readFileSync, renameSync, writeFileSync } from 'node:fs'
import { join } from 'node:path'

// Only fixed enums are published. Never serialize Playwright errors, URLs,
// credentials, locator call logs, attachments, test titles or page contents.
export const phases = Object.freeze([
  'runner_starting', 'browser_setup', 'waiting_for_ready', 'validating_binding',
  'opening_login', 'normal_login', 'opening_machine', 'preparing_update',
  'waiting_for_preview_choice', 'waiting_for_preview_review', 'waiting_for_preview_binding',
  'preview_binding_body_unavailable', 'preview_binding_target_mismatch', 'preview_binding_operation_invalid',
  'confirming_update', 'waiting_for_reconnect', 'refreshing',
  'observing_final_operation', 'writing_result', 'waiting_for_core_completion',
])
const statuses = new Set(['running', 'passed', 'failed', 'timedOut', 'interrupted', 'skipped', 'runner_error'])
function privateRoot(root) {
  if (!root) throw Error('private fixture root required')
  const info = lstatSync(root)
  if (!info.isDirectory() || info.isSymbolicLink() || (info.mode & 0o077)) throw Error('private fixture root required')
}
function save(root, phase, status) {
  privateRoot(root)
  if (!phases.includes(phase) || !statuses.has(status)) throw Error('invalid diagnostic enum')
  const temporary = join(root, 'browser-diagnostic.tmp')
  writeFileSync(temporary, JSON.stringify({ schema_version: 1, phase, status }) + '\n', { mode: 0o600, flag: 'wx' })
  renameSync(temporary, join(root, 'browser-diagnostic.json'))
}
export function recordBrowserPhase(root, phase) { save(root, phase, 'running') }
export function finishBrowserProcess(root) {
  const value = JSON.parse(readFileSync(join(root, 'browser-diagnostic.json'), 'utf8'))
  if (value.status === 'running') save(root, value.phase, 'runner_error')
}
export default class SafeBrowserReporter {
  root = process.env.TREER_CONTROLLER_BROWSER_ROOT
  runnerError = false
  onTestBegin() { recordBrowserPhase(this.root, 'browser_setup') }
  onError() { this.runnerError = true; this.publish('runner_error') }
  onTestEnd(_test, result) { this.publish(this.runnerError ? 'runner_error' : result.status) }
  publish(status) {
    let phase = 'runner_starting'
    try {
      const value = JSON.parse(readFileSync(join(this.root, 'browser-diagnostic.json'), 'utf8'))
      if (phases.includes(value.phase)) phase = value.phase
    } catch { /* Missing phase remains explicitly runner_starting. */ }
    save(this.root, phase, status)
  }
}
