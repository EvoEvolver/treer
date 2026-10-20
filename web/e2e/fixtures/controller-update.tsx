import { createRoot } from 'react-dom/client'
import { ControllerUpdater } from '../../src/components/controller-updater'
import type { ControllerUpdateClient, ControllerUpdateOperation, ControllerUpdateSnapshot } from '../../src/lib/controller-update'
export function mountControllerUpdate(mode = 'normal') {
  const identity = (c: string) => ({ release_sha: c.repeat(40), artifact_source_sha: c.repeat(40), controller_sha256: c.repeat(64), cli_sha256: c.repeat(64) })
  const current = { id: 'release-current', version: '1.0.0', commit: 'a'.repeat(40), platform: 'macos-arm64', identity: identity('a') }
  const target = { id: 'release-target', version: '1.1.0', commit: 'b'.repeat(40), platform: 'macos-arm64', identity: identity('b') }
  const snapshot: ControllerUpdateSnapshot = { machineId: 'test-machine', supported: true, current, targets: [target], operation: null, blockedByOtherOperation: false }
  const counts = { inspect: 0, prepare: 0, dispatch: 0 }
  const operation = (status: ControllerUpdateOperation['status']): ControllerUpdateOperation => ({ id: 'update-one', machineId: snapshot.machineId, current, target, status, receiptVerified: false, health: 'unknown', observedAt: null, restored: null, observation: 'available', phase: null })
  if (['unsupported', 'offline', 'forbidden', 'unavailable'].includes(mode)) { snapshot.supported = false; snapshot.unavailableReason = mode as never }
  if (mode === 'unverified') snapshot.operation = operation('succeeded')
  if (mode === 'success') snapshot.operation = { ...operation('succeeded'), observedIdentity: target.identity, phase: 'complete', receiptVerified: true, health: 'passed', observedAt: '2026-10-20T19:00:00Z' }
  if (mode === 'busy') snapshot.blockedByOtherOperation = true
  if (mode === 'known-offline') { snapshot.supported = false; snapshot.unavailableReason = 'offline' }
  if (mode === 'known-offline') snapshot.operation = { ...operation('succeeded'), observedIdentity: target.identity, phase: 'complete', receiptVerified: true, health: 'passed', observation: 'unavailable' }
  if (mode === 'core-healthy') snapshot.operation = { ...operation('updating'), phase: 'core_healthy', health: 'passed' }
  if (mode === 'rollback-failed') snapshot.operation = operation('rollback_failed')
  if (mode === 'partial') snapshot.operation = operation('partial')
  if (mode === 'rollback') snapshot.operation = { ...operation('rolled_back'), restored: current, observedIdentity: current.identity, receiptVerified: true, phase: 'rolled_back', health: 'passed' }
  if (mode === 'long') { target.version = '<img src=x onerror=alert(1)>'.repeat(4); target.platform = 'macos-arm64' }
  const client: ControllerUpdateClient = {
    async inspect(signal) { counts.inspect++; if (signal.aborted) throw Error('aborted'); return structuredClone(snapshot) },
    async prepare(id) { counts.prepare++; if (id !== target.id) throw Error('wrong_target'); return { machineId: mode === 'wrong-machine' ? 'other' : snapshot.machineId, current, target, confirmationToken: 'PRIVATE-CONFIRMATION-TOKEN', expiresInMs: mode === 'expired' ? 100 : 120000, operationId: 'update-one', steps: ['verify_artifacts', 'stage', 'replace_controller_and_cli', 'verify_health', 'reconcile_launcher', 'finalize'] } },
    async dispatch(token) {
      counts.dispatch++; if (token !== 'PRIVATE-CONFIRMATION-TOKEN') throw Error('wrong_token')
      if (mode !== 'lost-unknown') snapshot.operation = operation('waiting_for_reconnect')
      if (mode.startsWith('lost')) throw Error('PRIVATE RAW DIAGNOSTIC')
    },
  }
  const node = document.createElement('div'); node.id = 'controller-update-test'; document.body.replaceChildren(node)
  let root = createRoot(node); root.render(<ControllerUpdater machineId={snapshot.machineId} client={client} />)
  return { counts, snapshot, unmount: () => root.unmount(), remount: () => { root.unmount(); root = createRoot(node); root.render(<ControllerUpdater machineId={snapshot.machineId} client={client} />) } }
}
