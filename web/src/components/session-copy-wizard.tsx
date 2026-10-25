import { useEffect, useRef, useState } from 'react'
import { t } from '@/i18n'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Field } from '@/components/control-plane-ui'
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select'
import type { Agent, Machine } from '@/lib/api'
import { copyScopeKey, validateCopyScope, exportAndUpload, inspectOrPrepare, type CopyPlan, type CopyScope } from '@/lib/session-orchestration'

// Parent calls this hook outside DialogContent: closing the dialog cannot erase
// an ambiguous write or make it eligible for automatic resubmission.
export function useSessionCopy() {
  const [source, setSource] = useState('')
  const [staging, setStaging] = useState('')
  const [stage, setStage] = useState<'idle' | 'reading' | 'validated' | 'preparing' | 'prepared' | 'unknown'>('idle')
  const [plan, setPlan] = useState<CopyPlan>()
  const latch = useRef(false)
  const prepareLatch = useRef(false)
  const currentKey = useRef('')
  useEffect(() => () => { currentKey.current = '' }, [])
  return { source, setSource, staging, setStaging, stage, setStage, plan, setPlan, latch, prepareLatch, currentKey, busy: stage === 'reading' || stage === 'preparing' }
}
type CopyState = ReturnType<typeof useSessionCopy>
export function SessionCopyWizard({ flow, workspace, machine, agents, cwd, entry, onPrepared }: { flow: CopyState; workspace: string; machine?: Machine; agents: Agent[]; cwd: string; entry: string; onPrepared: (plan: CopyPlan) => void }) {
  const [confirmed, setConfirmed] = useState('')
  const [stagingAck, setStagingAck] = useState('')
  const scope: CopyScope | undefined = machine && { workspace, source: flow.source, machine, cwd, entry, staging: flow.staging }
  const key = scope ? copyScopeKey(scope) : ''
  let validScope = false
  try { if (scope) { validateCopyScope(scope); validScope = true } } catch { /* Inline input guidance, no request. */ }
  const same = !!flow.plan && copyScopeKey(flow.plan.scope) === key
  async function preview() {
    if (!scope || !validScope || stagingAck !== key || flow.latch.current) return
    flow.latch.current = true; flow.setStage('reading')
    try {
      const plan = await exportAndUpload(scope, () => flow.currentKey.current === key, flow.setPlan)
      if (flow.currentKey.current !== key) throw Error('scope_changed')
      plan.receipt = await inspectOrPrepare(plan, 'validated')
      if (flow.currentKey.current !== key) throw Error('scope_changed')
      flow.setPlan(plan); flow.setStage('validated')
    } catch { flow.setStage('unknown') }
  }
  async function prepare() {
    if (!same || !flow.plan || confirmed !== key || flow.prepareLatch.current) return
    flow.prepareLatch.current = true; flow.setStage('preparing')
    const plan = flow.plan
    try {
      plan.receipt = await inspectOrPrepare(plan, 'prepared')
      if (flow.currentKey.current !== key) throw Error('scope_changed')
      flow.setPlan({ ...plan }); flow.setStage('prepared'); onPrepared(plan)
    } catch { flow.setStage('unknown') }
  }
  return <section className="min-w-0 space-y-3 border-y py-3" aria-label={t('sessionCopy.title')}>
    <h3 className="text-sm font-medium">{t('sessionCopy.title')}</h3>
    <p className="text-xs text-muted-foreground">{t('sessionCopy.description')}</p>
    <fieldset className="min-w-0 space-y-3" disabled={flow.latch.current}>
      <Field label={t('sessionCopy.source')}><Select value={flow.source} onValueChange={flow.setSource}><SelectTrigger className="max-md:min-h-11" aria-label={t('sessionCopy.source')}><SelectValue /></SelectTrigger><SelectContent>{agents.filter(a => a.status === 'running' || a.status === 'idle').map(a => <SelectItem key={a.agent_id} value={a.agent_id}>{a.name} · {a.agent_id}</SelectItem>)}</SelectContent></Select></Field>
      <Field label={t('sessionCopy.staging')}><Input className="max-md:min-h-11" aria-label={t('sessionCopy.staging')} value={flow.staging} onChange={e => flow.setStaging(e.target.value)} /></Field>
      <label className="flex min-h-11 gap-3 text-sm"><input type="checkbox" className="mt-1 size-5 shrink-0" checked={stagingAck === key} onChange={e => setStagingAck(e.target.checked ? key : '')} />{t('sessionCopy.stagingAck')}</label>
      <Button type="button" variant="outline" className="max-md:min-h-11" disabled={!validScope || stagingAck !== key || !scope || !flow.source || !flow.staging || !entry || !cwd} onClick={() => void preview()}>{t('sessionCopy.preview')}</Button>
    </fieldset>
    {!validScope && <p className="text-xs text-muted-foreground">{t('sessionCopy.invalid')}</p>}
    {flow.plan && <div className="space-y-1 break-all text-xs"><p>{t('sessionCopy.plan', { session: flow.plan.sessionId, directory: flow.plan.directory })}</p><p>SHA-256: {flow.plan.sha256}</p><p>{t('sessionCopy.retained', { path: flow.plan.input })}</p></div>}
    {flow.stage === 'validated' && same && <><label className="flex min-h-11 gap-3 text-sm"><input className="mt-1 size-5 shrink-0" type="checkbox" checked={confirmed === key} onChange={e => setConfirmed(e.target.checked ? key : '')} />{t('sessionCopy.confirm')}</label><Button type="button" className="max-md:min-h-11" disabled={confirmed !== key} onClick={() => void prepare()}>{t('sessionCopy.prepare')}</Button></>}
    {flow.stage !== 'idle' && <p role={flow.stage === 'unknown' ? 'alert' : 'status'} className="break-words text-sm">{t(`sessionCopy.${flow.stage}`)}</p>}
    {flow.plan && !same && <p role="alert" className="text-sm">{t('sessionCopy.changed')}</p>}
  </section>
}
