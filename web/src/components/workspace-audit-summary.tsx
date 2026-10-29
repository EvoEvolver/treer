import { useEffect, useId, useRef, useState, type ReactNode } from 'react'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { api, ApiError, type Agent, type Member } from '@/lib/api'
import { auditCategories, AuditSummaryVersionError, readAuditSummary, type AuditCategory, type AuditDrill, type AuditSummary } from '@/lib/audit-summary'
import { agentActorLabel } from '@/components/audit-view'
import { intlLocale, t } from '@/i18n'

const categoryLabel = (category: AuditCategory) => t(`waudit.category_${category}`)
const date = (value: string) => new Date(value).toLocaleString(intlLocale(), { timeZone: 'UTC', year: 'numeric', month: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit' })
const number = (value: number) => value.toLocaleString(intlLocale())
const tones = auditCategories.map((_, i) => `--chart-${i + 1}`)
const patterns = ['M0 6L6 0', 'M0 3h6', 'M3 0v6', 'M0 0L6 6', 'M0 3h6M3 0v6', 'M0 0L6 6M0 6L6 0', 'M0 1h6M0 5h6', 'M1 0v6M5 0v6']
function Card({ title, children }: { title: string; children: ReactNode }) {
  return <section aria-label={title} className="min-w-0 space-y-4 rounded-md border bg-card p-4"><h3 className="font-semibold">{title}</h3>{children}</section>
}
function Table({ children, caption }: { children: ReactNode; caption: string }) {
  return <details className="text-xs"><summary className="min-h-11 cursor-pointer py-3 focus-visible:outline focus-visible:outline-2 focus-visible:outline-ring">{t('as.data_table')}</summary><div role="region" aria-label={t('as.table_region', { caption })} tabIndex={0} className="overflow-x-auto focus-visible:outline focus-visible:outline-2 focus-visible:outline-ring"><table className="w-full border-collapse text-left [&_th]:p-2 [&_td]:border-t [&_td]:p-2"><caption className="py-3 text-left">{caption}</caption>{children}</table></div></details>
}

export function WorkspaceAuditSummary({ workspaceId, filters, revision, preview, agents, members, selected, onReset, onDrill }: {
  workspaceId: string; filters: AuditDrill; revision: number; preview: boolean; agents: Agent[]; members: Member[]; selected?: string; onReset: () => void; onDrill: (filters: AuditDrill, label: string) => void
}) {
  const [bucketFocus, setBucketFocus] = useState(0)
  const combinedRef = useRef<HTMLDivElement>(null)
  const [range, setRange] = useState('7d'), [from, setFrom] = useState(''), [until, setUntil] = useState('')
  const [data, setData] = useState<AuditSummary>(), [error, setError] = useState<string>(), [loading, setLoading] = useState(true)
  const [supported, setSupported] = useState(false)
  const [retry, setRetry] = useState(0), [custom, setCustom] = useState<{ since: string; until: string }>()
  const patternId = useId().replaceAll(':', '')
  const filterKey = JSON.stringify(filters)
  useEffect(() => {
    const controller = new AbortController()
    setData(undefined); setError(undefined); setLoading(true); setSupported(false)
    async function load() {
      if (preview) { setLoading(false); return }
      try {
        const config = await api<{ capabilities?: string[] }>('/api/auth/config', { signal: controller.signal, cache: 'no-store' })
        if (controller.signal.aborted || !config.capabilities?.includes('workspace.audit-summary.v1')) return
        setSupported(true)
        if (range === 'custom' && !custom) return
        const query = new URLSearchParams({ tz: 'UTC' })
        const active = JSON.parse(filterKey) as AuditDrill
        for (const [key, value] of Object.entries(active)) if (value) query.set(key, value)
        if (range === 'custom' && custom) { query.set('since', custom.since); query.set('until', custom.until) }
        else if (!query.has('since') && !query.has('until')) query.set('range', range)
        if (query.has('since') && !query.has('until')) query.set('until', new Date().toISOString())
        if (query.has('until') && !query.has('since')) query.set('since', new Date(Date.parse(query.get('until')!) - 7 * 86400000).toISOString())
        if (query.has('since') && query.has('until')) {
          const start = Date.parse(query.get('since')!), end = Date.parse(query.get('until')!)
          if (!Number.isFinite(start) || !Number.isFinite(end) || start >= end || end - start > 180 * 86400000) { setError(t('as.invalid_range')); return }
        }
        const response = await api<unknown>(`/api/workspaces/${encodeURIComponent(workspaceId)}/audit-summary?${query}`, { signal: controller.signal, cache: 'no-store' })
        if (!controller.signal.aborted) setData(readAuditSummary(response, workspaceId, query))
      } catch (reason) {
        if (!controller.signal.aborted) setError(reason instanceof AuditSummaryVersionError ? t('as.version_update') : reason instanceof ApiError && reason.status === 403 ? t('waudit.denied') : reason instanceof ApiError && [404, 405].includes(reason.status) ? t('as.unsupported') : reason instanceof ApiError && reason.status === 429 ? t('as.rate_limited') : t('as.failed'))
      } finally { if (!controller.signal.aborted) setLoading(false) }
    }
    void load()
    return () => controller.abort()
  }, [workspaceId, filterKey, range, custom, revision, retry, preview])
  useEffect(() => {
    if (combinedRef.current) combinedRef.current.scrollLeft = combinedRef.current.scrollWidth
    setBucketFocus(Math.max(0, (data?.series.length ?? 1) - 1))
  }, [data])
  // Agents are labelled by id: the summary carries no stored name and an Agent's live name is not proof of who acted.
  const actorName = (kind: string, id?: string) => kind === 'agent' ? agentActorLabel(id) : members.find((m) => m.user_id === id)?.preferred_name || id || kind
  const drill = (extra: AuditDrill, label: string) => {
    if (!data) return
    onDrill({ ...data.filters, since: data.range.since, until: data.range.until, ...extra }, label)
  }
  if (!supported && !loading && !error) return <p className="text-xs text-muted-foreground">{t('as.unsupported')}</p>
  const partial = data && !data.coverage.backfill_complete
  const max = Math.max(1, ...data?.series.map((row) => row.events) ?? [])
  const skewed = !!data?.totals.events && data.categories.some((row) => row.events / data.totals.events > .7)
  const chart = (category?: AuditCategory) => {
    if (!data) return null
    const scale = category ? Math.max(1, ...data.series.map((row) => row.by_category[category])) : max
    if (category) return <div className="space-y-1">
      <p className="flex justify-between text-xs text-muted-foreground"><span>{t('as.events')}</span><span>{t('as.scale', { count: number(scale) })}</span></p>
      <svg role="img" aria-label={t('as.chart_summary', { category: categoryLabel(category), count: number(data.categories.find((row) => row.category === category)!.events), peak: number(scale) })} viewBox="0 0 400 170" preserveAspectRatio="none" className="h-44 w-full border-b border-foreground/60">
        {data.series.map((row, i) => {
          const value = row.by_category[category], width = 400 / data.series.length
          return <g key={row.bucket_start}><title>{date(row.from)} – {date(row.to)} · {number(value)}</title><rect x={i * width + 1} y={170 - 150 * value / scale} width={Math.max(.1, width - 2)} height={150 * value / scale} fill={`url(#${patternId}-${auditCategories.indexOf(category)})`} />{!value && <path d={`M${i * width + width / 2 - 1} 168h2`} stroke="hsl(var(--foreground))" />}</g>
        })}
      </svg>
      <p className="flex justify-between text-xs text-muted-foreground"><span>{new Date(data.range.since).toLocaleDateString(intlLocale(), { timeZone: 'UTC', month: 'numeric', day: 'numeric' })}</span><span>{new Date(data.series.at(-1)!.from).toLocaleDateString(intlLocale(), { timeZone: 'UTC', month: 'numeric', day: 'numeric' })}</span></p>
    </div>
    return <div className="min-w-0 space-y-1">
      <p className="flex justify-between text-xs text-muted-foreground"><span>{t('as.events')}</span><span>{t('as.scale', { count: number(scale) })}</span></p>
      <div role="region" ref={combinedRef} aria-label={t('as.buckets')} tabIndex={0} className="overflow-x-auto rounded px-2 focus-visible:outline focus-visible:outline-2 focus-visible:outline-ring" onKeyDown={(event) => {
        if (!['ArrowRight', 'ArrowLeft', 'Home', 'End'].includes(event.key)) return
        event.preventDefault()
        const buttons = Array.from(event.currentTarget.querySelectorAll('button'))
        const current = buttons.findIndex((button) => button === document.activeElement)
        const index = event.key === 'Home' ? 0 : event.key === 'End' ? buttons.length - 1 : Math.max(0, Math.min(buttons.length - 1, current + (event.key === 'ArrowRight' ? 1 : -1)))
        buttons[index]?.focus()
        buttons[index]?.scrollIntoView({ block: 'nearest', inline: 'nearest', behavior: 'instant' })
      }}>
        <div className="flex min-w-full items-end gap-1" style={{ width: Math.max(data.series.length * 48, 240) }}>
          {data.series.map((row, bucketIndex) => {
            const value = row.events
            const unusual = data.anomalies.items.some((item) => item.bucket_start === row.bucket_start)
            const notable = data.notable.some((item) => Date.parse(item.occurred_at) >= Date.parse(row.from) && Date.parse(item.occurred_at) < Date.parse(row.to))
            const label = `${date(row.from)} – ${date(row.to)} · ${t('as.events')} ${number(value)}`
            return <button key={row.bucket_start} type="button" tabIndex={bucketFocus === bucketIndex ? 0 : -1} onFocus={() => setBucketFocus(bucketIndex)} aria-label={label} title={label} className="group min-h-11 min-w-11 flex-1 rounded px-0.5 text-center text-xs hover:bg-accent focus-visible:outline focus-visible:outline-2 focus-visible:outline-ring" onClick={() => drill({ since: row.from, until: row.to, ...(selected ? { category: selected } : {}) }, label)}>
              <span className="block h-8 whitespace-nowrap" aria-hidden="true"><span className="block tabular-nums">{number(value)}</span>{notable ? '◆' : ''}{unusual ? '▲' : ''}</span>
              <svg aria-hidden="true" viewBox="0 0 40 160" className="h-40 w-full border-b border-foreground/60" preserveAspectRatio="none">
                {auditCategories.map((cat, i) => {
                  const lower = auditCategories.slice(0, i).reduce((n, key) => n + row.by_category[key], 0)
                  return <rect key={cat} x="2" y={160 - 150 * (lower + row.by_category[cat]) / scale} width="36" height={150 * row.by_category[cat] / scale} fill={`url(#${patternId}-${i})`} stroke="hsl(var(--card))" strokeWidth="1.2" />
                })}
                {!value && <path d="M18 158h4" stroke="hsl(var(--foreground))" strokeWidth="2" />}
              </svg>
              <span className="block whitespace-nowrap pt-2">{data.range.bucket === 'hour' ? new Date(row.from).toLocaleTimeString(intlLocale(), { timeZone: 'UTC', hour: '2-digit', minute: '2-digit', hour12: false }) : new Date(row.from).toLocaleDateString(intlLocale(), { timeZone: 'UTC', month: 'numeric', day: 'numeric' })}</span>
            </button>
          })}
        </div>
      </div>
      <p className="text-xs text-muted-foreground">{date(data.range.since)} – {date(data.range.until)} · UTC</p>
    </div>
  }
  return <section aria-label={t('as.title')} aria-busy={loading} className="min-w-0 space-y-4">
    {range !== 'custom' && (filters.since || filters.until) && <p className="text-xs text-muted-foreground">{t('as.applied_dates')}</p>}
    <div className="flex flex-wrap items-end justify-between gap-3"><h3 className="text-lg font-semibold">{t('as.title')}</h3><label className="grid gap-1 text-xs">{t('as.range')}<select value={range} onChange={(event) => { setRange(event.target.value); setCustom(undefined); onReset() }} className="min-h-11 rounded-md border bg-background px-3 text-sm"><option value="24h">{t('traffic.range_24h')}</option><option value="7d">{t('traffic.range_7d')}</option><option value="30d">{t('traffic.range_30d')}</option><option value="custom">{t('as.custom')}</option></select></label></div>
    {range === 'custom' && <form className="grid gap-3 sm:grid-cols-[1fr_1fr_auto]" onSubmit={(event) => {
      event.preventDefault()
      const start = Date.parse(from), end = Date.parse(until)
      if (!Number.isFinite(start) || !Number.isFinite(end) || start >= end || end - start > 180 * 86400000) { setError(t('as.invalid_range')); return }
      onReset(); setCustom({ since: new Date(start).toISOString(), until: new Date(end).toISOString() })
    }}><label className="grid gap-1 text-xs">{t('as.from')}<Input type="datetime-local" required value={from} onChange={(event) => setFrom(event.target.value)} className="min-h-11" /></label><label className="grid gap-1 text-xs">{t('as.until')}<Input type="datetime-local" required value={until} onChange={(event) => setUntil(event.target.value)} className="min-h-11" /></label><Button type="submit" className="min-h-11 self-end">{t('waudit.apply')}</Button></form>}
    {loading && <p className="min-h-32 py-5 text-sm text-muted-foreground">{t('as.loading')}</p>}
    {error && <div role="alert" className="space-y-2 rounded-md border p-4 text-sm"><p>{error}</p><Button variant="outline" className="min-h-11" onClick={() => setRetry((n) => n + 1)}>{t('as.retry')}</Button></div>}
    {data && <>
      <svg aria-hidden="true" className="absolute size-0"><defs>{auditCategories.map((_, i) => <pattern key={i} id={`${patternId}-${i}`} width="6" height="6" patternUnits="userSpaceOnUse"><rect width="6" height="6" fill={`hsl(var(${tones[i]}))`} /><path d={patterns[i]} stroke="hsl(var(--card))" strokeWidth="1" /></pattern>)}</defs></svg>
      <p className="text-xs text-muted-foreground">{t('as.window', { since: date(data.range.since), until: date(data.range.until) })}</p>
      <p className="text-xs text-muted-foreground">{t('as.retention', { days: data.maintenance.retention_days })}{data.maintenance.deleted_through && ` ${t('as.deleted', { date: date(data.maintenance.deleted_through) })}`}</p>
      {partial && <p className="rounded-md border p-3 text-sm">{t('as.partial', { since: data.coverage.complete_since ? date(data.coverage.complete_since) : t('as.unknown') })}</p>}
      {!partial && <dl className="grid grid-cols-2 gap-3 sm:grid-cols-4">{[[t('as.events'), data.totals.events], [t('as.failed_count'), data.totals.failed], [t('as.actors'), data.actors.distinct], [t('as.notable'), data.notable_total]].map(([label, value]) => <div key={label} className="rounded-md border p-3"><dt className="text-xs text-muted-foreground">{label}</dt><dd className="mt-1 text-2xl font-semibold tabular-nums">{number(value as number)}</dd></div>)}</dl>}
      {!data.totals.events && <p className="rounded-md border p-4 text-sm">{t(partial ? 'as.empty_partial' : 'as.empty')}</p>}
      <div className="space-y-2 rounded-md border p-4"><h4 className="font-semibold">{t('as.signals')}</h4>{(!data.anomalies.volume_evaluated || !data.anomalies.failure_evaluated) && <p className="text-sm">{t('as.insufficient')}</p>}{data.anomalies.items.map((item) => <p key={`${item.kind}-${item.bucket_start}`} className="break-words text-sm">{t(item.kind === 'volume_spike' ? 'as.volume_spike' : 'as.failure_burst', { date: date(item.bucket_start), count: number(item.observed), median: number(item.baseline), threshold: number(item.threshold) })}</p>)}{!data.anomalies.items.length && data.anomalies.volume_evaluated && data.anomalies.failure_evaluated && <p className="text-sm">{t('as.no_signal')}</p>}<details className="text-xs text-muted-foreground"><summary className="min-h-11 cursor-pointer py-3 focus-visible:outline focus-visible:outline-2 focus-visible:outline-ring">{t('as.signal_explanation')}</summary><p>{t('as.signal_rule')}</p></details></div>
      <Card title={t('as.activity')}>
        <p className="text-xs text-muted-foreground">{t('as.chart_hint')}</p>
        <div className="grid grid-cols-1 gap-2 sm:grid-cols-2 lg:grid-cols-4">{data.categories.map((row, i) => <button key={row.category} type="button" aria-pressed={selected === row.category} className="flex min-h-11 items-center justify-between gap-2 rounded border px-3 py-2 text-left text-xs aria-pressed:bg-accent aria-pressed:ring-1 aria-pressed:ring-foreground hover:bg-accent focus-visible:outline focus-visible:outline-2 focus-visible:outline-ring" onClick={() => drill({ category: row.category }, categoryLabel(row.category))}><span><svg aria-hidden="true" viewBox="0 0 16 16" className="mr-2 inline-block size-4"><rect width="16" height="16" fill={`url(#${patternId}-${i})`} /></svg>{categoryLabel(row.category)}</span><span className="tabular-nums">{number(row.events)}</span></button>)}</div>
        {skewed && <><p className="text-sm">{t('as.skew')}</p><div className="grid gap-4 lg:grid-cols-4">{auditCategories.filter((category) => data.categories.find((row) => row.category === category)!.events > 0).map((category) => <section key={category} className="min-w-0 space-y-2 rounded border p-3"><h4 className="text-xs font-medium">{categoryLabel(category)}</h4>{chart(category)}</section>)}</div><p className="text-xs text-muted-foreground">{data.categories.some((row) => row.events === 0) && t('as.zero_categories', { categories: data.categories.filter((row) => row.events === 0).map((row) => categoryLabel(row.category)).join(' · ') })}</p><h4 className="text-sm font-medium">{t('as.combined')}</h4></>}
        {chart()}
        <Table caption={t('as.activity')}><thead><tr><th>{t(data.range.bucket === 'hour' ? 'as.utc_hours' : 'as.utc_days')}</th>{auditCategories.map((category) => <th key={category}>{categoryLabel(category)}</th>)}<th>{t('as.events')}</th><th>{t('as.failed_count')}</th></tr></thead><tbody>{data.series.map((row) => <tr key={row.bucket_start}><th scope="row" title={`${date(row.from)} – ${date(row.to)} · UTC`}>{new Date(row.from).toLocaleString(intlLocale(), { timeZone: 'UTC', month: 'short', day: 'numeric', ...(data.range.bucket === 'hour' ? { hour: '2-digit', minute: '2-digit', hour12: false } : {}) })}</th>{auditCategories.map((cat) => <td key={cat}>{number(row.by_category[cat])}</td>)}<td>{number(row.events)}</td><td>{number(row.failed)}</td></tr>)}</tbody></Table>
      </Card>
      <div className="grid gap-4 xl:grid-cols-2">
        <Card title={t('as.top_actors')}><p className="text-xs text-muted-foreground">{t('as.current_names')}</p>{data.actors.top.map((row, i) => <div key={`${row.actor_kind}-${row.actor_id ?? i}`} className="space-y-1"><Button variant="ghost" disabled={!row.drillable} className="h-auto min-h-11 w-full justify-between gap-3 px-0 text-left" onClick={() => drill({ actor_kind: row.actor_kind, actor_id: row.actor_id! }, actorName(row.actor_kind, row.actor_id))}><span className="min-w-0 break-all whitespace-normal">{actorName(row.actor_kind, row.actor_id)} <span className="text-xs text-muted-foreground">· {row.actor_kind}</span></span><span>{number(row.events)}</span></Button><div aria-hidden="true" className="h-2 rounded bg-muted"><div className="h-full rounded bg-foreground" style={{ width: `${100 * row.events / Math.max(1, data.actors.top[0]?.events ?? 0)}%` }} /></div></div>)}<p className="text-xs text-muted-foreground">{t('as.rank_note', { count: data.actors.top.length, distinct: number(data.actors.distinct), omitted: number(data.actors.omitted_events) })}</p><Table caption={t('as.top_actors')}><thead><tr><th>{t('as.current_name')}</th><th>{t('as.id')}</th><th>{t('as.events')}</th><th>{t('as.failed_count')}</th></tr></thead><tbody>{data.actors.top.map((row, i) => <tr key={i}><th scope="row">{actorName(row.actor_kind, row.actor_id)}</th><td className="break-all">{row.actor_kind} · {row.actor_id ?? '—'}</td><td>{number(row.events)}</td><td>{number(row.failed)}</td></tr>)}</tbody></Table></Card>
        {(['machine', 'agent', 'app'] as const).map((kind) => <Card key={kind} title={t(`waudit.kind_${kind}`)}><p className="text-xs text-muted-foreground">{t('as.current_names')}</p>{data.resources[kind].top.map((row) => <div key={row.resource_id} className="space-y-1"><Button variant="ghost" disabled={!row.drillable} className="h-auto min-h-11 w-full justify-between gap-3 px-0 text-left" onClick={() => drill({ resource_kind: kind, resource_id: row.resource_id }, row.resource_name || row.resource_id)}><span className="min-w-0 break-all whitespace-normal">{row.resource_name || row.resource_id}</span><span>{number(row.events)}</span></Button><div aria-hidden="true" className="h-2 rounded bg-muted"><div className="h-full rounded bg-foreground" style={{ width: `${100 * row.events / Math.max(1, data.resources[kind].top[0]?.events ?? 0)}%` }} /></div></div>)}<p className="text-xs text-muted-foreground">{t('as.rank_note', { count: data.resources[kind].top.length, distinct: number(data.resources[kind].distinct), omitted: number(data.resources[kind].omitted_events) })}</p><Table caption={t(`waudit.kind_${kind}`)}><thead><tr><th>{t('as.current_name')}</th><th>{t('as.id')}</th><th>{t('as.events')}</th><th>{t('as.failed_count')}</th></tr></thead><tbody>{data.resources[kind].top.map((row) => <tr key={row.resource_id}><th scope="row">{row.resource_name || '—'}</th><td className="break-all">{row.resource_id}</td><td>{number(row.events)}</td><td>{number(row.failed)}</td></tr>)}</tbody></Table></Card>)}
      </div>
      <Card title={t('as.notable')}><p className="text-xs text-muted-foreground">{Object.keys(data.notable_capped).length ? t('as.capped', { count: data.notable.length, total: number(data.notable_total) }) : t('as.notable_note', { count: data.notable.length, total: number(data.notable_total) })}</p>{Object.entries(data.notable_capped).map(([action, omitted]) => <p key={action} className="break-all text-xs text-muted-foreground">{action} · {t('as.omitted', { count: number(omitted) })}</p>)}<p className="text-xs text-muted-foreground">{t('as.current_names')}</p><ol className="divide-y">{data.notable.map((event) => <li key={event.sequence} className="py-3"><button type="button" className="grid min-h-11 w-full gap-1 rounded text-left focus-visible:outline focus-visible:outline-2 focus-visible:outline-ring sm:grid-cols-[10rem_1fr]" onClick={() => drill({ sequence: String(event.sequence) }, `${event.action} · ${date(event.occurred_at)}`)}><time dateTime={event.occurred_at} className="text-xs text-muted-foreground">{date(event.occurred_at)} · UTC</time><span className="min-w-0 break-words [overflow-wrap:anywhere]"><span className="text-sm font-medium">{event.action}</span><span className="block text-xs">{actorName(event.actor_kind, event.actor_id)} · {event.resource_name || event.resource_id}</span><span className="text-xs">{t(event.outcome === 'failed' ? 'waudit.outcome_failed' : 'waudit.outcome_succeeded')}</span></span></button></li>)}</ol><Button className="min-h-11" variant="outline" onClick={() => drill({ notable: 'true' }, t('as.notable'))}>{t('as.view_notable')}</Button></Card>
      <Button variant="outline" className="min-h-11" onClick={() => drill({}, t('as.all_range'))}>{t('as.view_events')}</Button>
    </>}
  </section>
}
