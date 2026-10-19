// Mirrors treer-protocol/events.rs at the frozen #191 contract (79d56bc3).
// Validate and project the authorized aggregate; never retain arbitrary payloads.
export const auditCategories = ['agent_lifecycle', 'messages', 'tree', 'policy', 'apps_ingress', 'machines_files', 'access_admin', 'other'] as const
export type AuditCategory = typeof auditCategories[number]
export type AuditDrill = Record<string, string>
export interface AuditCount { events: number; failed: number }
export interface AuditBucket extends AuditCount { bucket_start: string; bucket_end: string; from: string; to: string; by_category: Record<AuditCategory, number> }
export interface AuditActor extends AuditCount { actor_kind: string; actor_id?: string; drillable: boolean }
export interface AuditResource extends AuditCount { resource_id: string; resource_name?: string; drillable: boolean }
export interface AuditRanking<T> { top: T[]; distinct: number; omitted_events: number }
export interface AuditNotable { sequence: number; occurred_at: string; action: string; category: AuditCategory; actor_kind: string; actor_id?: string; outcome: string; resource_kind: string; resource_id: string; resource_name?: string }
export interface AuditSummary {
  workspace_id: string; category_table_version: number
  range: { since: string; until: string; bucket: 'hour' | 'day' | 'week'; tz: string }
  filters: AuditDrill; totals: AuditCount; categories: (AuditCount & { category: AuditCategory })[]; series: AuditBucket[]
  actors: AuditRanking<AuditActor>; resources: Record<'agent' | 'machine' | 'app', AuditRanking<AuditResource>>
  notable_total: number; notable: AuditNotable[]; notable_capped: Record<string, number>
  anomalies: { volume_evaluated: boolean; failure_evaluated: boolean; items: { kind: 'volume_spike' | 'failure_burst'; bucket_start: string; observed: number; baseline: number; threshold: number }[] }
  maintenance: { retention_days: number; deleted_through?: string }
  coverage: { backfill_complete: boolean; complete_since?: string; oldest_in_range?: string }
}
const fail = (): never => { throw new Error('invalid audit summary') }
const obj = (v: unknown): Record<string, unknown> => v !== null && typeof v === 'object' && !Array.isArray(v) ? v as Record<string, unknown> : fail()
const str = (v: unknown, max = 16384): string => typeof v === 'string' && v.length <= max ? v : fail()
const count = (v: unknown): number => typeof v === 'number' && Number.isSafeInteger(v) && v >= 0 ? v : fail()
const bool = (v: unknown): boolean => typeof v === 'boolean' ? v : fail()
const instant = (v: unknown): string => { const s = str(v, 64); return Number.isFinite(Date.parse(s)) ? s : fail() }
const list = <T>(v: unknown, max: number, read: (v: unknown) => T): T[] => Array.isArray(v) && v.length <= max ? v.map(read) : fail()
const category = (v: unknown): AuditCategory => auditCategories.includes(v as AuditCategory) ? v as AuditCategory : fail()
const counts = (v: Record<string, unknown>): AuditCount => { const events = count(v.events), failed = count(v.failed); return failed <= events ? { events, failed } : fail() }
export const auditIdDrillable = (id: string | undefined): boolean => !!id && /^[A-Za-z0-9_.-]{1,128}$/.test(id)
const optionalString = (v: unknown, max?: number): string | undefined => v === undefined || v === null ? undefined : str(v, max)
const optionalInstant = (v: unknown): string | undefined => v === undefined || v === null ? undefined : instant(v)
const drillable = (v: Record<string, unknown>, id: string | undefined) => { const enabled = bool(v.drillable); if (enabled && !auditIdDrillable(id)) fail(); return enabled }
const rank = <T extends AuditCount>(value: unknown, read: (v: unknown) => T): AuditRanking<T> => {
  const v = obj(value), top = list(v.top, 20, read), distinct = count(v.distinct), omitted_events = count(v.omitted_events)
  if (distinct < top.length) fail()
  return { top, distinct, omitted_events }
}

export function readAuditSummary(value: unknown, workspaceId: string, query: URLSearchParams): AuditSummary {
  const v = obj(value), r = obj(v.range)
  if (v.workspace_id !== workspaceId || v.category_table_version !== 1) fail()
  const range = { since: instant(r.since), until: instant(r.until), bucket: str(r.bucket) as AuditSummary['range']['bucket'], tz: str(r.tz, 128) }
  const since = Date.parse(range.since), until = Date.parse(range.until)
  const duration = query.get('range') === '24h' ? 1 : query.get('range') === '7d' ? 7 : query.get('range') === '30d' ? 30 : undefined
  if (duration && until - since !== duration * 86400000) fail()
  if (!(since < until) || until - since > 180 * 86400000 || !['hour', 'day', 'week'].includes(range.bucket) || range.tz !== 'UTC') fail()
  for (const key of ['since', 'until'] as const) if (query.has(key) && Date.parse(query.get(key)!) !== Date.parse(range[key])) fail()
  const rawFilters = obj(v.filters), filters: AuditDrill = {}
  for (const key of ['actor_kind', 'actor_id', 'action', 'category', 'resource_kind', 'resource_id', 'outcome']) {
    const actual = optionalString(rawFilters[key], 128)
    if ((query.get(key) || undefined) !== actual) fail()
    if (actual) filters[key] = actual
  }
  if (bool(rawFilters.notable) !== (query.get('notable') === 'true')) fail()
  if (rawFilters.notable) filters.notable = 'true'
  const total = counts(obj(v.totals))
  const categories = list(v.categories, 8, (value) => { const row = obj(value); return { category: category(row.category), ...counts(row) } })
  if (categories.length !== 8 || categories.some((row, i) => row.category !== auditCategories[i]) || categories.reduce((n, row) => n + row.events, 0) !== total.events || categories.reduce((n, row) => n + row.failed, 0) !== total.failed) fail()
  const series = list(v.series, 744, (value) => {
    const row = obj(value), by = obj(row.by_category), by_category = {} as Record<AuditCategory, number>
    if (Object.keys(by).some((key) => !auditCategories.includes(key as AuditCategory))) fail()
    for (const key of auditCategories) by_category[key] = count(by[key] ?? 0)
    const result = { bucket_start: instant(row.bucket_start), bucket_end: instant(row.bucket_end), from: instant(row.from), to: instant(row.to), by_category, ...counts(row) }
    const start = Date.parse(result.bucket_start), end = Date.parse(result.bucket_end)
    if (!(start < end) || Date.parse(result.from) !== Math.max(since, start) || Date.parse(result.to) !== Math.min(until, end) || Date.parse(result.from) >= Date.parse(result.to) || Object.values(by_category).reduce((a, b) => a + b, 0) !== result.events) fail()
    return result
  })
  if (!series.length || Date.parse(series[0].from) !== since || Date.parse(series.at(-1)!.to) !== until || series.some((row, i) => i > 0 && Date.parse(series[i - 1].bucket_end) !== Date.parse(row.bucket_start)) || series.reduce((n, row) => n + row.events, 0) !== total.events || series.reduce((n, row) => n + row.failed, 0) !== total.failed) fail()
  for (const row of categories) if (series.reduce((n, bucket) => n + bucket.by_category[row.category], 0) !== row.events) fail()
  const actors = rank(v.actors, (value): AuditActor => { const row = obj(value), actor_id = optionalString(row.actor_id); return { actor_kind: str(row.actor_kind, 128), actor_id, drillable: drillable(row, actor_id), ...counts(row) } })
  if (actors.omitted_events + actors.top.reduce((n, row) => n + row.events, 0) !== total.events) fail()
  const resources = {} as AuditSummary['resources'], rawResources = obj(v.resources)
  for (const kind of ['agent', 'machine', 'app'] as const) {
    resources[kind] = rank(rawResources[kind], (value): AuditResource => { const row = obj(value), resource_id = str(row.resource_id); return { resource_id, resource_name: optionalString(row.resource_name, 512), drillable: drillable(row, resource_id), ...counts(row) } })
    if (resources[kind].omitted_events + resources[kind].top.reduce((n, row) => n + row.events, 0) > total.events) fail()
  }
  const notable = list(v.notable, 100, (value): AuditNotable => {
    const row = obj(value), sequence = count(row.sequence), occurred_at = instant(row.occurred_at), outcome = str(row.outcome)
    if (!sequence || Date.parse(occurred_at) < since || Date.parse(occurred_at) >= until || !['succeeded', 'failed'].includes(outcome)) fail()
    return { sequence, occurred_at, action: str(row.action, 512), category: category(row.category), actor_kind: str(row.actor_kind, 128), actor_id: optionalString(row.actor_id), outcome, resource_kind: str(row.resource_kind, 128), resource_id: str(row.resource_id), resource_name: optionalString(row.resource_name, 512) }
  })
  const notable_total = count(v.notable_total), notable_capped: Record<string, number> = {}
  if (notable.length > notable_total || notable_total > total.events) fail()
  for (const [key, n] of Object.entries(obj(v.notable_capped ?? {}))) notable_capped[str(key, 512)] = count(n)
  const a = obj(v.anomalies), anomalies: AuditSummary['anomalies'] = {
    volume_evaluated: bool(a.volume_evaluated), failure_evaluated: bool(a.failure_evaluated), items: list(a.items, 1488, (value) => {
      const row = obj(value), kind = str(row.kind) as 'volume_spike' | 'failure_burst', bucket_start = instant(row.bucket_start), bucket = series.find((b) => b.bucket_start === bucket_start)
      if (!bucket || !['volume_spike', 'failure_burst'].includes(kind)) return fail()
      const observed = count(row.observed), baseline = row.baseline, threshold = row.threshold
      if (typeof baseline !== 'number' || !Number.isFinite(baseline) || baseline < 0 || typeof threshold !== 'number' || !Number.isFinite(threshold) || threshold < 0 || observed <= threshold || observed !== (kind === 'volume_spike' ? bucket.events : bucket.failed)) return fail()
      return { kind, bucket_start, observed, baseline, threshold }
    }),
  }
  if (anomalies.items.some((row) => row.kind === 'volume_spike' ? !anomalies.volume_evaluated : !anomalies.failure_evaluated)) fail()
  const m = obj(v.maintenance), maintenance = { retention_days: count(m.retention_days), deleted_through: optionalInstant(m.deleted_through) }
  const c = obj(v.coverage), coverage = { backfill_complete: bool(c.backfill_complete), complete_since: optionalInstant(c.complete_since), oldest_in_range: optionalInstant(c.oldest_in_range) }
  return { workspace_id: workspaceId, category_table_version: 1, range, filters, totals: total, categories, series, actors, resources, notable_total, notable, notable_capped, anomalies, maintenance, coverage }
}
