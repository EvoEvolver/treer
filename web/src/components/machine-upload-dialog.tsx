import { useEffect, useRef, useState, type FormEvent } from 'react'
import { Upload } from 'lucide-react'
import { api, ApiError, machineName, type Machine } from '@/lib/api'
import { formatBytes } from '@/lib/control-plane'
import { MachineUpload, MAX_UPLOAD_BYTES, MIB, UPLOAD_CAPABILITY, type UploadView } from '@/lib/machine-upload'
import { t } from '@/i18n'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Field } from '@/components/control-plane-ui'
import { Dialog, DialogContent, DialogHeader, DialogTitle, DialogDescription, DialogFooter } from '@/components/ui/dialog'

// The parent keys this component by user/workspace/machine and mounts it only
// while open. No file, idempotency key, or upload handle survives that scope.
export function MachineUploadDialog({ workspaceId, machine, onClose }: { workspaceId: string; machine: Machine; onClose: () => void }) {
  const [directory, setDirectory] = useState('.')
  const [file, setFile] = useState<File | null>(null)
  const [overwrite, setOverwrite] = useState(false)
  const [mode, setMode] = useState<'unknown' | 'legacy' | 'resumable'>('unknown')
  const [discoveryError, setDiscoveryError] = useState(false)
  const [view, setView] = useState<UploadView>({ phase: 'idle', loaded: 0, total: 0 })
  const [error, setError] = useState('')
  const [discoverRevision, setDiscoverRevision] = useState(0)
  const engine = useRef<MachineUpload | null>(null)
  const machineCapability = Array.isArray(machine.capabilities) ? machine.capabilities.includes(UPLOAD_CAPABILITY) ? 'supported' : 'absent' : 'unknown'
  const base = `/api/workspaces/${encodeURIComponent(workspaceId)}/machines/${encodeURIComponent(machine.server_id)}`
  useEffect(() => {
    const control = new AbortController()
    setDiscoveryError(false)
    setMode('unknown')
    void api<{ capabilities?: string[] }>('/api/auth/config', { signal: control.signal, cache: 'no-store' }).then((config) => {
      if (control.signal.aborted) return
      const proxySupports = config.capabilities?.includes(UPLOAD_CAPABILITY) === true
      const machineSupports = machineCapability === 'supported'
      if (proxySupports && machineCapability === 'unknown') { setDiscoveryError(true); return }
      // A successful older config/inventory is confirmed lack of support. A
      // network/Policy failure is unknown, never permission to silently fall back.
      setMode(proxySupports && machineSupports ? 'resumable' : 'legacy')
    }).catch((reason) => {
      if (control.signal.aborted) return
      if (reason instanceof ApiError && reason.status === 404) setMode('legacy')
      else setDiscoveryError(true)
    })
    return () => control.abort()
  }, [base, discoverRevision, machineCapability])
  useEffect(() => () => engine.current?.dispose(), [])
  const busy = ['preparing', 'uploading', 'waiting', 'finishing', 'cancelling'].includes(view.phase)
  const retained = view.phase === 'paused' && !view.uncertain && view.resumable
  useEffect(() => {
    if (!busy && !retained) return
    const leave = (event: BeforeUnloadEvent) => { event.preventDefault(); event.returnValue = '' }
    window.addEventListener('beforeunload', leave)
    return () => window.removeEventListener('beforeunload', leave)
  }, [busy, retained])
  const locked = busy || view.phase === 'paused'
  const canCancel = mode === 'resumable' && !view.uncertain && (busy || view.phase === 'paused') && view.phase !== 'finishing' && view.phase !== 'cancelling'
  const max = mode === 'legacy' ? 16 * MIB : MAX_UPLOAD_BYTES
  async function submit(event: FormEvent) {
    event.preventDefault()
    if (busy || !file || mode === 'unknown') return
    setError('')
    if (file.size > max) { setError(t(mode === 'legacy' ? 'upload.upgrade_limit' : 'upload.too_large')); return }
    if (!file.name || /[\\/\x00-\x1f\x7f]/.test(file.name)) { setError(t('upload.invalid_name')); return }
    if (mode === 'resumable' && !crypto.subtle) { setError(t('upload.crypto_required')); return }
    engine.current?.dispose()
    engine.current = new MachineUpload(base, setView)
    await engine.current.start(file, directory, overwrite, mode === 'legacy')
  }
  const message = view.phase === 'succeeded' && view.result
    ? t('app.uploaded', { path: view.result.path, size: formatBytes(view.result.bytes_written) })
    : view.phase === 'cleanup_pending' ? t('upload.cleanup_pending')
    : view.phase === 'cancelled' ? t('upload.cancelled')
    : view.phase === 'finishing' ? t('upload.finishing')
    : view.phase === 'cancelling' ? t('upload.cancelling')
    : view.phase === 'waiting' ? t('upload.waiting')
    : view.phase === 'preparing' ? t('upload.preparing')
    : view.phase === 'uploading' ? t('upload.progress', { sent: formatBytes(view.loaded), total: formatBytes(view.total) }) : ''
  const problem = error || (view.phase === 'paused' ? view.uncertain ? t('upload.outcome_unknown') : uploadErrorMessage(view.error) : '')
  const percentage = view.phase === 'succeeded' ? 100 : view.total ? Math.min(100, Math.floor(view.loaded / view.total * 100)) : 0
  return <Dialog open onOpenChange={(open) => { if (!open && !busy && !retained) onClose() }}>
    <DialogContent className={`max-h-[calc(100dvh-2rem)] overflow-y-auto ${busy || retained ? '[&>button]:hidden' : ''}`}>
      <form onSubmit={submit} className="min-w-0 space-y-4">
        <DialogHeader><DialogTitle>{t('surface.upload_file')}</DialogTitle><DialogDescription>{t('dialogs.upload_to', { machine: machineName(machine) })}</DialogDescription></DialogHeader>
        <Field label={t('dialogs.destination_directory')}><Input aria-label={t('dialogs.destination_directory')} value={directory} onChange={(event) => setDirectory(event.target.value)} required disabled={locked} className="font-mono" /></Field>
        <Field label={t('dialogs.file')}><Input aria-label={t('dialogs.file')} type="file" onChange={(event) => { setFile(event.target.files?.[0] ?? null); setError(''); if (!locked) setView({ phase: 'idle', loaded: 0, total: 0 }) }} disabled={locked} className="min-h-11" /></Field>
        <label className="flex min-h-11 items-center gap-2 text-xs text-muted-foreground"><input type="checkbox" className="size-4 accent-foreground" checked={overwrite} onChange={(event) => setOverwrite(event.target.checked)} disabled={locked} />{t('dialogs.replace_an_existing_file_with')}</label>
        <p className="text-xs text-muted-foreground">{t(mode === 'legacy' ? 'upload.legacy_limit' : 'upload.limit')}</p>
        {mode === 'unknown' && <p role="status" className="text-xs text-muted-foreground">{t(discoveryError ? 'upload.discovery_failed' : 'upload.checking')}</p>}
        {view.phase !== 'idle' && <div className="space-y-2">
          <div role="progressbar" aria-label={t('upload.progress_label')} aria-valuemin={0} aria-valuemax={100} aria-valuenow={percentage} aria-valuetext={`${formatBytes(view.loaded)} / ${formatBytes(view.total)}`} className="h-2 overflow-hidden rounded-full bg-muted"><div className="h-full bg-primary" style={{ width: `${percentage}%` }} /></div>
          <p role="status" className="break-words text-xs text-foreground">{message}</p>
        </div>}
        {problem && <p role="alert" className="break-words text-xs text-status-attention">{problem}</p>}
        {!machine || machine.status !== 'online' ? <p role="status" className="text-xs text-muted-foreground">{t('upload.offline')}</p> : null}
        <DialogFooter className="gap-2">
          <Button type="button" variant="outline" className="min-h-11" onClick={onClose} disabled={busy || retained}>{t('org.close')}</Button>
          {canCancel && <Button type="button" variant="outline" className="min-h-11" onClick={() => void engine.current?.cancel()}>{t('upload.cancel')}</Button>}
          {discoveryError && <Button type="button" className="min-h-11" onClick={() => setDiscoverRevision((value) => value + 1)}>{t('upload.retry_discovery')}</Button>}
          {view.phase === 'paused' ? view.resumable && <Button type="button" className="min-h-11" disabled={busy || machine.status !== 'online'} onClick={() => void engine.current?.resume()}>{t(view.uncertain ? 'upload.check_result' : 'upload.resume')}</Button>
            : <Button type="submit" className="min-h-11" disabled={!file || busy || mode === 'unknown' || machine.status !== 'online'}><Upload />{busy ? t('dialogs.uploading') : t('dialogs.upload')}</Button>}
        </DialogFooter>
      </form>
    </DialogContent>
  </Dialog>
}
function uploadErrorMessage(code?: string) {
  if (code === 'controller_update_required') return t('upload.upgrade_limit')
  if (code === 'machine_file_exists') return t('upload.exists')
  if (code === 'policy_denied' || code === 'forbidden') return t('upload.denied')
  if (code === 'upload_part_hash_mismatch' || code === 'hash_mismatch') return t('upload.hash_failed')
  if (code === 'idle_expired' || code === 'lifetime_expired' || code === 'upload_expired') return t('upload.expired')
  return t('upload.failed')
}
