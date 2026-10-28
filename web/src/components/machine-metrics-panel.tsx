import { useEffect, useId, useRef, useState, type ReactNode } from "react"
import { Cpu, Gauge, MemoryStick } from "lucide-react"
import { Button } from "@/components/ui/button"
import { ApiError, type Machine } from "@/lib/api"
import { formatBytes } from "@/lib/control-plane"
import { fetchMachineMetrics, gpuOf, memoryPercent, METRICS_RANGES, segments, series, supportsMachineMetrics, type MachineMetrics, type MetricPoint, type MetricsRange } from "@/lib/machine-metrics"
import { intlLocale, t } from "@/i18n"

const percent = (value: number | undefined) => value === undefined ? "—" : `${value.toLocaleString(intlLocale(), { maximumFractionDigits: 1 })}%`
const clock = (at: number, withSeconds = false) => new Date(at).toLocaleTimeString(intlLocale(), { hour: "2-digit", minute: "2-digit", ...(withSeconds ? { second: "2-digit" } : {}) })

/** Live CPU / RAM / GPU load of one machine with a small history per metric. */
export function MachineMetricsPanel({ machine, workspaceId }: { machine: Machine; workspaceId: string }) {
  const [range, setRange] = useState<MetricsRange>("1h")
  const [metrics, setMetrics] = useState<MachineMetrics | null>(null)
  const [error, setError] = useState<string | null>(null)
  const supported = supportsMachineMetrics(machine)
  const online = machine.status === "online"
  const seconds = METRICS_RANGES.find((item) => item.key === range)!.seconds

  useEffect(() => {
    setMetrics(null)
    setError(null)
    if (!supported || !online) return
    let cancelled = false
    let timer: ReturnType<typeof setTimeout> | undefined
    const load = async () => {
      if (document.visibilityState !== "hidden") {
        try {
          const next = await fetchMachineMetrics(workspaceId, machine.server_id, seconds)
          if (!cancelled) { setMetrics(next); setError(null) }
        } catch (reason) {
          if (!cancelled) setError(reason instanceof ApiError && reason.code === "machine_metrics_unsupported" ? t("metrics.unsupported") : reason instanceof Error ? reason.message : String(reason))
        }
      }
      // Fresh samples arrive every 5 s; minute buckets once a minute.
      if (!cancelled) timer = setTimeout(load, seconds <= 3600 ? 5_000 : 60_000)
    }
    void load()
    return () => { cancelled = true; clearTimeout(timer) }
  }, [workspaceId, machine.server_id, supported, online, seconds])

  const header = <div className="mb-3 flex flex-wrap items-center justify-between gap-2">
    <h2 className="text-sm font-semibold">{t("metrics.title")}</h2>
    {supported && online && <div role="group" aria-label={t("metrics.range")} className="flex gap-1">{METRICS_RANGES.map((item) => <Button key={item.key} size="sm" variant={range === item.key ? "secondary" : "ghost"} aria-pressed={range === item.key} className="h-7 px-2 text-[11px]" onClick={() => setRange(item.key)}>{item.key}</Button>)}</div>}
  </div>

  let body
  if (!supported) body = <p className="text-xs text-muted-foreground">{t("metrics.unsupported")}</p>
  else if (!online) body = <p className="text-xs text-muted-foreground">{t("metrics.offline")}</p>
  else if (!metrics) body = <p role={error ? "alert" : "status"} className="text-xs text-muted-foreground">{error ?? t("metrics.loading")}</p>
  else body = <MetricsBody metrics={metrics} />

  return <section aria-label={t("metrics.title")} data-testid="machine-metrics" className="mb-10 rounded-md border p-4">{header}{body}{metrics && error && <p role="alert" className="mt-2 text-[11px] text-amber-700">{error}</p>}</section>
}

function MetricsBody({ metrics }: { metrics: MachineMetrics }) {
  const latest = metrics.latest
  const gap = metrics.resolution_seconds * 2500
  const windowMs = metrics.window_seconds * 1000
  const ram = memoryPercent(latest?.memory_used_bytes, latest?.memory_total_bytes)
  return <>
    <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-3">
      <MetricTile icon={<Cpu />} label={t("metrics.cpu")} value={percent(latest?.cpu_percent)} detail={t("metrics.cpu_detail", { count: metrics.cpu_count })} points={series(metrics, (sample) => sample.cpu_percent)} gapMs={gap} windowMs={windowMs} />
      <MetricTile icon={<MemoryStick />} label={t("metrics.memory")} value={percent(ram)} detail={latest ? `${formatBytes(latest.memory_used_bytes)} / ${formatBytes(latest.memory_total_bytes)}` : "—"} points={series(metrics, (sample) => memoryPercent(sample.memory_used_bytes, sample.memory_total_bytes))} gapMs={gap} windowMs={windowMs} />
      {metrics.gpus.map((device) => {
        const gpu = gpuOf(latest, device.index)
        const memory = gpu?.memory_used_bytes !== undefined && gpu.memory_total_bytes ? `${formatBytes(gpu.memory_used_bytes)} / ${formatBytes(gpu.memory_total_bytes)} ${t("metrics.vram")}` : t("metrics.vram_unavailable")
        return <MetricTile key={device.index} icon={<Gauge />} label={`GPU ${device.index} · ${device.name}`} value={percent(gpu?.utilization_percent)} detail={memory} points={series(metrics, (sample) => gpuOf(sample, device.index)?.utilization_percent)} gapMs={gap} windowMs={windowMs} />
      })}
    </div>
    <p className="mt-3 text-[11px] text-muted-foreground">
      {metrics.gpus.length === 0 && <>{t(metrics.gpu_status === "unsupported" ? "metrics.gpu_unsupported" : "metrics.gpu_not_detected")} · </>}
      {t(metrics.resolution_seconds > metrics.sample_seconds ? "metrics.resolution_minute" : "metrics.resolution_raw", { seconds: metrics.sample_seconds })}
    </p>
  </>
}

function MetricTile({ icon, label, value, detail, points, gapMs, windowMs }: { icon: ReactNode; label: string; value: string; detail: string; points: MetricPoint[]; gapMs: number; windowMs: number }) {
  return <div className="min-w-0 rounded-md bg-muted/40 p-3">
    <div className="flex items-center gap-1.5 text-[11px] text-muted-foreground [&_svg]:size-3.5"><span aria-hidden="true">{icon}</span><span className="truncate" title={label}>{label}</span></div>
    <div className="mt-1 flex items-baseline justify-between gap-2"><span className="text-xl font-semibold tabular-nums">{value}</span><span className="truncate text-[10px] text-muted-foreground tabular-nums" title={detail}>{detail}</span></div>
    <Sparkline label={label} points={points} gapMs={gapMs} windowMs={windowMs} />
  </div>
}

/** 0..100 % line over the whole window (a young history fills only its right
 * part); gaps longer than `gapMs` break it. Hover/arrow keys read a value. */
function Sparkline({ label, points, gapMs, windowMs }: { label: string; points: MetricPoint[]; gapMs: number; windowMs: number }) {
  const id = useId()
  const ref = useRef<SVGSVGElement>(null)
  const [width, setWidth] = useState(240)
  const [active, setActive] = useState<number | null>(null)
  useEffect(() => {
    const element = ref.current
    if (!element) return
    const observer = new ResizeObserver(([entry]) => setWidth(Math.max(120, Math.floor(entry.contentRect.width))))
    observer.observe(element)
    return () => observer.disconnect()
  }, [])
  const height = 48, top = 3, bottom = height - 2
  const end = points.at(-1)?.at ?? 0, start = end - windowMs
  const x = (at: number) => end === start ? width : (at - start) / (end - start) * width
  const y = (value: number) => bottom - value / 100 * (bottom - top)
  const lines = segments(points, gapMs).map((segment) => segment.length === 1
    ? `M${x(segment[0].at) - 1},${y(segment[0].value)}h2`
    : segment.map((point, index) => `${index ? "L" : "M"}${x(point.at).toFixed(1)},${y(point.value).toFixed(1)}`).join(""))
  const chosen = active === null ? points.at(-1) : points[active]
  const pick = (clientX: number) => {
    const rect = ref.current?.getBoundingClientRect()
    if (!rect || !points.length) return
    const at = start + (clientX - rect.left) / rect.width * (end - start)
    let best = 0
    for (let index = 1; index < points.length; index++) if (Math.abs(points[index].at - at) < Math.abs(points[best].at - at)) best = index
    setActive(best)
  }
  const step = (delta: number) => setActive((current) => Math.max(0, Math.min(points.length - 1, (current ?? points.length - 1) + delta)))
  return <figure className="mt-2">
    <svg ref={ref} role="img" tabIndex={points.length ? 0 : undefined} aria-labelledby={`${id}-title`} viewBox={`0 0 ${width} ${height}`} className="block h-12 w-full overflow-visible rounded-sm outline-none focus-visible:ring-2 focus-visible:ring-ring"
      onMouseMove={(event) => pick(event.clientX)} onMouseLeave={() => setActive(null)} onBlur={() => setActive(null)}
      onKeyDown={(event) => { if (event.key === "ArrowLeft" || event.key === "ArrowRight") { event.preventDefault(); step(event.key === "ArrowLeft" ? -1 : 1) } }}>
      <title id={`${id}-title`}>{t("metrics.chart_title", { label })}</title>
      <line x1={0} x2={width} y1={y(50)} y2={y(50)} className="stroke-border" strokeDasharray="2 4" />
      <line x1={0} x2={width} y1={bottom} y2={bottom} className="stroke-border" />
      {lines.map((d, index) => <path key={index} d={d} fill="none" strokeWidth={2} strokeLinejoin="round" strokeLinecap="round" vectorEffect="non-scaling-stroke" className="stroke-primary" />)}
      {active !== null && chosen && <><line x1={x(chosen.at)} x2={x(chosen.at)} y1={top} y2={bottom} className="stroke-muted-foreground/50" vectorEffect="non-scaling-stroke" /><circle cx={x(chosen.at)} cy={y(chosen.value)} r={3} className="fill-primary stroke-background" strokeWidth={2} vectorEffect="non-scaling-stroke" /></>}
    </svg>
    <figcaption aria-live="polite" className="mt-1 flex justify-between text-[10px] text-muted-foreground tabular-nums">
      {points.length ? <><span>{clock(start)}</span><span>{active !== null && chosen ? `${clock(chosen.at, true)} · ${percent(chosen.value)}` : t("metrics.peak", { value: percent(Math.max(...points.map((point) => point.value))) })}</span><span>{clock(end)}</span></> : <span>{t("metrics.no_samples")}</span>}
    </figcaption>
  </figure>
}
