import { useState } from 'react'
import { t, intlLocale } from '@/i18n'
import { formatBytes } from '@/lib/control-plane'
import type { Machine } from '@/lib/api'
import type { MetricKind, MetricSeries, MetricsRange } from '@/lib/machine-metrics'
import { Button } from './ui/button'
import { MachineMetricChart, metricTime } from './machine-metric-chart'
import { useMachineMetrics } from './use-machine-metrics'
export function MachineMetricsView({ machine, workspaceId }: { machine: Machine; workspaceId: string }) {
  const [range, setRange] = useState<MetricsRange>(15)
  const [ramKind, setRamKind] = useState<'ram_used' | 'ram_available' | 'ram_total'>('ram_used')
  const [vramKind, setVramKind] = useState<'gpu_memory_used' | 'gpu_memory_total'>('gpu_memory_used')
  const state = useMachineMetrics(workspaceId, machine.server_id, machine.status === 'online', Boolean(machine.capabilities?.includes('machine.metrics.v1')), range)
  const data = state.data
  const find = (kind: MetricKind, gpuId: string | null = null) => data!.series.find(s => s.kind === kind && s.gpu_id === gpuId)!
  const format = (series: MetricSeries, v: number) => series.unit === 'percent' ? v.toLocaleString(intlLocale(), { maximumFractionDigits: 1 }) + '%' : formatBytes(v)
  const reading = (series: MetricSeries) => {
    const c = series.current, stale = c.status === 'ok' && (c.age_ms ?? 0) + state.elapsed > (data?.stale_after_ms ?? 15000)
    return { value: c.status === 'ok' && !stale && c.value !== null ? format(series, c.value) : '—', reason: stale ? 'stale' as const : c.status }
  }
  const current = (series: MetricSeries) => { const r = reading(series); return <div><p className="text-2xl font-semibold tabular-nums">{r.value}</p><p className="mt-1 text-[11px] text-muted-foreground">{t('metrics.current')}{r.reason !== 'ok' ? ` · ${t(`metrics.status.${r.reason}`)}` : ''}</p><p className="mt-1 break-words text-[11px] text-muted-foreground">{series.current.sampled_at ? metricTime(series.current.sampled_at) : t('metrics.not_observed')}</p></div> }
  const chart = (series: MetricSeries) => <MachineMetricChart series={series} title={t(`metrics.kind.${series.kind}`)} format={v => format(series, v)} />
  return <section aria-label={t('metrics.title')} className="mb-8 min-w-0" data-testid="machine-metrics">
    <div className="mb-4 flex flex-wrap items-start justify-between gap-3"><div><h2 className="text-sm font-semibold">{t('metrics.title')}</h2><p className="mt-1 max-w-xl text-xs leading-5 text-muted-foreground">{t('metrics.local_notice')}</p></div><div className="flex flex-wrap items-center gap-1" role="group" aria-label={t('metrics.range')}>
      {([15, 60, 360, 1440] as MetricsRange[]).map(minutes => <Button key={minutes} variant={range === minutes ? 'default' : 'outline'} aria-pressed={range === minutes} className="min-h-11 min-w-11" onClick={() => setRange(minutes)}>{t(`metrics.range.${minutes}`)}</Button>)}
      <Button variant="outline" className="min-h-11" disabled={state.loading || machine.status !== 'online' || !machine.capabilities?.includes('machine.metrics.v1')} onClick={state.refresh}>{t('metrics.refresh')}</Button>
    </div></div>
    {state.loading && !data && <div role="status" className="grid min-h-[260px] place-items-center rounded-md border p-5 text-sm text-muted-foreground">{t('metrics.loading')}</div>}
    {state.error && <div role={state.error === 'forbidden' || state.error === 'invalid_response' ? 'alert' : 'status'} className="rounded-md border p-5 text-sm">{t(`metrics.error.${state.error}`)}</div>}
    {data && <>
      <p className="mb-3 text-[11px] text-muted-foreground">{t('metrics.aggregation', { seconds: data.bucket_width_ms / 1000 })} · {t('metrics.scope')}{state.loading ? ` · ${t('metrics.refreshing')}` : ''}</p>
      {(data.truncated || data.inventory_truncated || data.series.some(s => s.points.some(p => p.partial))) && <p className="mb-4 rounded-md border bg-muted p-3 text-xs leading-5">{data.truncated ? t('metrics.truncated') + ' ' : ''}{data.inventory_truncated ? t('metrics.inventory_truncated') + ' ' : ''}{data.series.some(s => s.points.some(p => p.partial)) ? t('metrics.gaps') : ''}</p>}
      <div className="grid min-w-0 gap-4 md:grid-cols-2">
        <section className="min-w-0 rounded-md border p-4"><h3 className="mb-3 text-sm font-semibold">CPU</h3>{current(find('cpu_utilization'))}<p className="my-3 text-xs text-muted-foreground">{t('metrics.cpu_scope', { cores: data.logical_cpus ?? '—' })}</p>{chart(find('cpu_utilization'))}</section>
        <section className="min-w-0 rounded-md border p-4"><h3 className="mb-3 text-sm font-semibold">RAM</h3>{current(find('ram_used'))}<dl className="my-3 flex flex-wrap gap-x-5 gap-y-2 text-xs"><div><dt className="text-muted-foreground">{t('metrics.available')}</dt><dd>{reading(find('ram_available')).value}</dd></div><div><dt className="text-muted-foreground">{t('metrics.total')}</dt><dd>{reading(find('ram_total')).value}</dd></div></dl><p className="mb-3 text-[11px] leading-5 text-muted-foreground">{t(`metrics.memory.${data.memory_method}`)}</p><label className="mb-3 block text-xs"><span>{t('metrics.history_metric')}</span><select value={ramKind} onChange={e => setRamKind(e.target.value as typeof ramKind)} className="ml-2 min-h-11 max-w-full rounded-md border bg-background px-2 outline-none focus-visible:ring-2 focus-visible:ring-ring">{(['ram_used', 'ram_available', 'ram_total'] as const).map(k => <option key={k} value={k}>{t(`metrics.kind.${k}`)}</option>)}</select></label>{chart(find(ramKind))}</section>
      </div>
      <div className="mt-4 space-y-4">
        {!data.gpus.length && <div className="rounded-md border p-4"><h3 className="mb-2 text-sm font-semibold">NVIDIA GPU</h3><p className="text-xs text-muted-foreground">{t(`metrics.status.${data.gpu_status}`)}</p></div>}
        {data.gpus.map((gpu, index) => <section key={gpu.id} className="min-w-0 rounded-md border p-4"><h3 className="break-words text-sm font-semibold">GPU {index + 1} · {gpu.name}</h3>{!gpu.present && <p className="mt-1 text-xs text-muted-foreground">{t('metrics.removed')}</p>}<div className="mt-4 grid gap-5 md:grid-cols-2"><div className="min-w-0"><h4 className="mb-2 text-xs font-medium">{t('metrics.kind.gpu_utilization')}</h4>{current(find('gpu_utilization', gpu.id))}<div className="mt-3">{chart(find('gpu_utilization', gpu.id))}</div></div><div className="min-w-0"><h4 className="mb-2 text-xs font-medium">{t('metrics.kind.gpu_memory_used')}</h4>{current(find('gpu_memory_used', gpu.id))}<p className="my-3 text-xs text-muted-foreground">{t('metrics.total')}: {reading(find('gpu_memory_total', gpu.id)).value}</p><label className="mb-3 block text-xs"><span>{t('metrics.history_metric')}</span><select value={vramKind} onChange={e => setVramKind(e.target.value as typeof vramKind)} className="ml-2 min-h-11 max-w-full rounded-md border bg-background px-2 outline-none focus-visible:ring-2 focus-visible:ring-ring">{(['gpu_memory_used', 'gpu_memory_total'] as const).map(k => <option key={k} value={k}>{t(`metrics.kind.${k}`)}</option>)}</select></label>{chart(find(vramKind, gpu.id))}</div></div></section>)}
      </div>
    </>}
  </section>
}
