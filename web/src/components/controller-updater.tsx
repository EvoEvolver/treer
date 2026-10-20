import { useEffect, useRef, useState } from 'react'
import { useT } from '@/i18n'
import { Button } from '@/components/ui/button'
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from '@/components/ui/dialog'
import { controllerUpdateActive, controllerUpdateStatus, sameControllerIdentity, type ControllerUpdateClient, type ControllerUpdatePreview, type ControllerUpdateSnapshot } from '@/lib/controller-update'

// Same prepare/one-use-confirm/read-only-observe interaction as the CLI installer.
// Scope by workspace/machine/session at the call site; nothing is persisted.
// This component is not mounted in MachineView until the Core adapter is reviewed.
export function ControllerUpdater({ machineId, client }: { machineId: string; client: ControllerUpdateClient }) {
  const t = useT()
  const [open, setOpen] = useState(false)
  const [snapshot, setSnapshot] = useState<ControllerUpdateSnapshot | null>(null)
  const [preview, setPreview] = useState<Omit<ControllerUpdatePreview, 'confirmationToken'> | null>(null)
  const [busy, setBusy] = useState(false)
  const [uncertain, setUncertain] = useState(false)
  const [error, setError] = useState(false)
  const [expired, setExpired] = useState(false)
  const [paused, setPaused] = useState(false)
  const token = useRef<string | null>(null)
  const expiry = useRef(0)
  const pending = useRef(false)
  const attempted = useRef(false)
  const expectedOperationId = useRef<string | null>(null)
  const generation = useRef(0)
  const mounted = useRef(true)
  const read = useRef<AbortController | null>(null)
  const trigger = useRef<HTMLButtonElement>(null)
  const heading = useRef<HTMLHeadingElement>(null)
  useEffect(() => { mounted.current = true; return () => { mounted.current = false; generation.current++; token.current = null; read.current?.abort() } }, [])

  async function inspect(signal: AbortSignal) {
    const next = await client.inspect(signal, expectedOperationId.current ?? undefined)
    if (signal.aborted || !mounted.current) return
    if (next.machineId !== machineId || next.operation && (next.operation.machineId !== machineId || expectedOperationId.current && next.operation.id !== expectedOperationId.current)) throw Error('invalid_response')
    setSnapshot(next)
    // After an attempted dispatch, absence is not proof that nothing happened.
    setUncertain(attempted.current && !next.operation)
    return next
  }
  useEffect(() => {
    if (!open || busy || preview) return
    const controller = new AbortController(); read.current = controller
    let timer: number | undefined, polls = 0
    async function poll() {
      try {
        const result = await inspect(controller.signal)
        if (result && (!result.operation || !controllerUpdateActive(result.operation)) && !(attempted.current && !result.operation)) return
      }
      catch { if (!controller.signal.aborted && mounted.current) { setUncertain(true); setError(true) } }
      if (!controller.signal.aborted && ++polls < 300) timer = window.setTimeout(() => void poll(), 2000)
      else if (!controller.signal.aborted) setPaused(true)
    }
    void poll()
    return () => { controller.abort(); if (timer) window.clearTimeout(timer) }
  }, [open, client, machineId, busy, preview])
  useEffect(() => {
    if (!preview) return
    heading.current?.focus()
    const timer = window.setTimeout(() => { token.current = null; setExpired(true) }, Math.max(0, expiry.current - performance.now()))
    return () => window.clearTimeout(timer)
  }, [preview])
  function changeOpen(next: boolean) {
    generation.current++; token.current = null; setPreview(null); setExpired(false); setOpen(next)
  }
  async function prepare(targetId: string) {
    if (pending.current || attempted.current || uncertain || !snapshot?.supported || !snapshot.current || snapshot.blockedByOtherOperation || snapshot.operation) return
    pending.current = true; setBusy(true); setError(false); token.current = null; setPreview(null)
    const epoch = ++generation.current, controller = new AbortController(); read.current?.abort(); read.current = controller
    try {
      const next = await client.prepare(targetId, controller.signal)
      if (!mounted.current || epoch !== generation.current) return
      const target = snapshot.targets.find(v => v.id === targetId)
      if (!target || next.machineId !== machineId || next.current.id !== snapshot.current.id || !sameControllerIdentity(next.current.identity, snapshot.current.identity) || next.target.id !== target.id || !sameControllerIdentity(next.target.identity, target.identity) || next.target.version !== target.version || next.target.platform !== target.platform || !next.confirmationToken || !Number.isFinite(next.expiresInMs) || next.expiresInMs <= 0 || next.expiresInMs > 120000) throw Error('invalid_response')
      const { confirmationToken, ...display } = next
      token.current = confirmationToken; expiry.current = performance.now() + next.expiresInMs; setExpired(false); setPreview(display)
    } catch { if (mounted.current && epoch === generation.current) setError(true) }
    finally { pending.current = false; if (mounted.current) setBusy(false) }
  }
  async function dispatch() {
    if (pending.current || !preview || !token.current || performance.now() >= expiry.current || attempted.current) return
    const confirmation = token.current
    expectedOperationId.current = preview.operationId
    token.current = null; attempted.current = true; pending.current = true; setBusy(true); setPreview(null); setUncertain(true)
    try { await client.dispatch(confirmation) }
    catch { /* Ambiguous refusal or delivery: recover by GET, never resend. */ }
    finally {
      pending.current = false
      if (mounted.current) { setBusy(false); await refresh() }
    }
  }
  async function refresh() {
    if (pending.current) return
    pending.current = true; setBusy(true); setError(false); setPaused(false)
    const controller = new AbortController(); read.current?.abort(); read.current = controller
    try { await inspect(controller.signal) }
    catch { if (mounted.current) { setUncertain(true); setError(true) } }
    finally { pending.current = false; if (mounted.current) setBusy(false) }
  }
  const operation = snapshot?.operation
  const state = operation ? controllerUpdateStatus(operation) : null
  return <>
    <Button ref={trigger} type="button" variant="outline" className="max-md:min-h-11" onClick={() => changeOpen(true)}>{t('controller_update.title')}</Button>
    {!open && (operation || uncertain) && <p className="mt-2 text-sm" role="status">{operation ? t(`controller_update.${state!}`) : t('controller_update.unconfirmed')}</p>}
    <Dialog open={open} onOpenChange={changeOpen}>
      <DialogContent className="max-h-[calc(100dvh-2rem)] max-w-xl overflow-y-auto max-md:[&>button:last-child]:min-h-11 max-md:[&>button:last-child]:min-w-11" onCloseAutoFocus={event => { if (trigger.current) { event.preventDefault(); trigger.current.focus() } }}>
        <DialogHeader><DialogTitle>{t('controller_update.title')}</DialogTitle><DialogDescription>{t('controller_update.disconnect')}</DialogDescription></DialogHeader>
        <div className="min-w-0 space-y-4 text-sm">
          {error && <p role="alert">{t('controller_update.unavailable')}</p>}
          <p className="break-all font-mono text-xs">{machineId}</p>
          {snapshot && <p className="break-words">{t('controller_update.current', { version: snapshot.current?.version ?? t('machine.unknown') })}</p>}
          {snapshot?.blockedByOtherOperation && <p role="status">{t('controller_update.busy')}</p>}
          {snapshot && !snapshot.supported && <p role="status">{t(snapshot.unavailableReason === 'offline' ? 'controller_update.offline' : snapshot.unavailableReason === 'forbidden' ? 'controller_update.forbidden' : snapshot.unavailableReason === 'unavailable' ? 'controller_update.unavailable' : 'controller_update.manual')}</p>}
          {uncertain && <p role="status">{t(operation ? 'controller_update.observation_unavailable' : 'controller_update.unconfirmed')}</p>}
          {paused && <p role="status">{t('controller_update.paused')}</p>}
          {operation && <div className="space-y-2 rounded-md border bg-muted/30 p-3" role="status">
            <p>{t(`controller_update.${state!}`)}</p>
            <p className="break-words">{t('controller_update.target', { version: operation.target.version })}</p>
            <p className="break-all font-mono text-xs">{operation.id}</p>
            {operation.phase && <p>{t(`controller_update.phase_${operation.phase}`)}</p>}
            {operation.observation !== 'available' && <p>{t(operation.observation === 'waiting_for_reconnect' ? 'controller_update.waiting_for_reconnect' : 'controller_update.observation_unavailable')}</p>}
            <p>{t('controller_update.observed', { time: operation.observedAt ?? t('machine.unknown') })}</p>
            {state === 'rolled_back' && <p>{t('controller_update.restored', { version: operation.restored?.version ?? t('machine.unknown') })}</p>}
          </div>}
          {preview ? <>
            <h3 ref={heading} tabIndex={-1} className="font-semibold outline-none">{t('controller_update.preview')}</h3>
            <p>{t('controller_update.current', { version: preview.current.version })}</p>
            <p>{t('controller_update.target', { version: preview.target.version })}</p>
            <details><summary className="min-h-11 cursor-pointer py-3">{t('controller_update.build_details')}</summary>{[preview.current, preview.target].map((release, i) => <div key={i} className="space-y-1 py-2"><p>{t(i ? 'controller_update.target' : 'controller_update.current', { version: release.version })}</p>{Object.entries(release.identity).map(([field, value]) => <p key={field} className="break-all font-mono text-xs">{t(`controller_update.identity_${field as keyof typeof release.identity}`)}: {value}</p>)}</div>)}</details>
            <p className="break-words">{preview.target.platform}</p>
            <ol className="list-inside list-decimal space-y-2">{preview.steps.map(step => <li key={step}>{t(`controller_update.step_${step}`)}</li>)}</ol>
            {expired && <p role="alert">{t('controller_update.expired')}</p>}
            <div className="flex flex-wrap justify-end gap-2"><Button variant="outline" className="max-md:min-h-11" onClick={() => changeOpen(false)}>{t('common.cancel')}</Button><Button className="max-md:min-h-11" disabled={busy || expired} onClick={() => void dispatch()}>{t('controller_update.title')}</Button></div>
          </> : <>
            {snapshot?.supported && snapshot.current && !snapshot.blockedByOtherOperation && !operation && !uncertain && !attempted.current && <div className="space-y-2">
              <p>{t('controller_update.trusted')}</p>
              {snapshot.targets.length === 0 && <p>{t('controller_update.no_targets')}</p>}
              {snapshot.targets.map(target => <Button key={target.id} variant="outline" className="mr-2 max-w-full whitespace-normal break-words max-md:min-h-11" disabled={busy} onClick={() => void prepare(target.id)}>{t('controller_update.choose', { version: target.version })}</Button>)}
            </div>}
            <Button variant="outline" className="max-md:min-h-11" disabled={busy} onClick={() => void refresh()}>{t('controller_update.refresh')}</Button>
          </>}
        </div>
      </DialogContent>
    </Dialog>
  </>
}
