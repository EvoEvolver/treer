// UI view models only, not a wire protocol. The Core adapter must validate and
// bind every response before supplying this interface. Runtime support is gated.
export interface ControllerIdentity { release_sha: string; artifact_source_sha: string; controller_sha256: string; cli_sha256: string }
export interface ControllerRelease { id: string; version: string; commit: string; platform: string; identity: ControllerIdentity }
export const sameControllerIdentity = (a: ControllerIdentity | undefined, b: ControllerIdentity) => !!a && a.release_sha === b.release_sha && a.artifact_source_sha === b.artifact_source_sha && a.controller_sha256 === b.controller_sha256 && a.cli_sha256 === b.cli_sha256
export type ControllerUpdateStatus = 'preparing' | 'queued' | 'dispatched' | 'updating' | 'waiting_for_reconnect' | 'succeeded' | 'failed' | 'rolled_back' | 'rollback_failed' | 'partial' | 'unconfirmed'
export type ControllerUpdateStep = 'verify_artifacts' | 'stage' | 'replace_controller_and_cli' | 'verify_health' | 'reconcile_launcher' | 'finalize'
export interface ControllerUpdateOperation {
  id: string
  machineId: string
  current: ControllerRelease
  target: ControllerRelease
  status: ControllerUpdateStatus
  observedIdentity?: ControllerIdentity
  health: 'passed' | 'failed' | 'unknown'
  receiptVerified: boolean
  observedAt: string | null
  restored: ControllerRelease | null
  observation: 'available' | 'waiting_for_reconnect' | 'unavailable'
  phase: 'preparing' | 'prepared' | 'activating' | 'core_healthy' | 'launcher_installing' | 'rolling_back' | 'complete' | 'failed' | 'rolled_back' | 'rollback_failed' | 'partial' | null
}
export interface ControllerUpdateSnapshot {
  machineId: string
  supported: boolean
  unavailableReason?: 'unsupported' | 'offline' | 'forbidden' | 'unavailable'
  current: ControllerRelease | null
  targets: ControllerRelease[]
  operation: ControllerUpdateOperation | null
  // Adapter proof: fresh ready catalog/current identity, resolved prior claim,
  // and server cooldown permit explicit new prepare. Never infer from age.
  prepareAllowed: boolean
  blockedByOtherOperation: boolean
}
export interface ControllerUpdatePreview {
  machineId: string
  current: ControllerRelease
  target: ControllerRelease
  confirmationToken: string
  expiresInMs: number
  operationId: string
  steps: ControllerUpdateStep[]
}
export interface ControllerUpdateClient {
  // Read-only discovery must find the existing operation after refresh or a lost
  // dispatch response. Never implement this by replaying a prepare/dispatch.
  inspect(signal: AbortSignal, operationId?: string): Promise<ControllerUpdateSnapshot>
  prepare(targetId: string, signal: AbortSignal): Promise<ControllerUpdatePreview>
  dispatch(confirmationToken: string): Promise<void>
}
export function controllerUpdateStatus(op: ControllerUpdateOperation): ControllerUpdateStatus {
  // Reconnect alone is not success, even if an adapter labels it succeeded.
  if (op.status === 'succeeded' && (!op.receiptVerified || op.phase !== 'complete' || op.health !== 'passed' || !sameControllerIdentity(op.observedIdentity, op.target.identity))) return 'unconfirmed'
  if (op.status === 'rolled_back' && (!op.receiptVerified || op.phase !== 'rolled_back' || op.health !== 'passed' || !sameControllerIdentity(op.restored?.identity, op.current.identity) || !sameControllerIdentity(op.observedIdentity, op.current.identity))) return 'unconfirmed'
  return op.status
}
export const controllerUpdateActive = (op: ControllerUpdateOperation) => ['preparing', 'queued', 'dispatched', 'updating', 'waiting_for_reconnect'].includes(controllerUpdateStatus(op))

export function controllerUpdateCanPrepare(snapshot: ControllerUpdateSnapshot): boolean {
  const status = snapshot.operation && controllerUpdateStatus(snapshot.operation)
  return snapshot.prepareAllowed === true && snapshot.supported && !!snapshot.current && !snapshot.blockedByOtherOperation
    && (!status || ['succeeded', 'rolled_back', 'failed'].includes(status))
}
