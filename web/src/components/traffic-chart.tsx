import { useEffect, useId, useRef, useState } from "react"
import { formatBytes } from "@/lib/control-plane"
import { trafficTime, type TrafficBucket, type TrafficHours, type TrafficMetric } from "@/lib/traffic"

export function TrafficChart({ buckets, hours, metric }: { buckets: TrafficBucket[]; hours: TrafficHours; metric: TrafficMetric }) {
  const id = useId()
  const svgRef = useRef<SVGSVGElement>(null)
  const [width, setWidth] = useState(640)
  useEffect(() => {
    const element = svgRef.current
    if (!element) return
    const observer = new ResizeObserver(([entry]) => setWidth(Math.max(240, Math.floor(entry.contentRect.width))))
    observer.observe(element)
    return () => observer.disconnect()
  }, [])
  const [active, setActive] = useState<number | null>(null)
  const peak = Math.max(0, ...buckets.map((bucket) => bucket[metric]))
  const maximum = Math.max(1, peak)
  const format = (value: number) => metric === "bytes" ? formatBytes(value) : value.toLocaleString()
  const label = metric === "bytes" ? "Billable usage trend" : "Data frames trend"
  const chosen = active === null ? null : buckets[active]
  const height = 156, bottom = 136, left = 44, usable = width - left - 4
  const step = usable / buckets.length
  return <figure aria-label={label} className="mt-5 min-w-0 rounded-md border bg-background p-3 sm:p-4">
    <div className="mb-3 flex flex-wrap items-center justify-between gap-2 text-xs"><figcaption className="font-medium">{label}</figcaption><span className="text-[10px] text-muted-foreground">{hours <= 24 ? "Hourly" : "Daily"} · UTC</span></div>
    <svg ref={svgRef} role="img" aria-labelledby={`${id}-title ${id}-description`} viewBox={`0 0 ${width} ${height}`} className="block w-full overflow-visible">
      <title id={`${id}-title`}>{label}</title><desc id={`${id}-description`}>Recorded usage by {hours <= 24 ? "hour" : "UTC day"}. Focus a bar for its value, or open the data table below.</desc>
      {(peak ? [0, 0.5, 1] : [0]).map((fraction) => { const y = bottom - fraction * 116; return <g key={fraction}><line x1={left} x2={width} y1={y} y2={y} className="stroke-border" strokeDasharray={fraction ? "3 4" : undefined} /><text x={left - 7} y={y + 3} textAnchor="end" className="fill-muted-foreground text-[9px]">{format(maximum * fraction)}</text></g> })}
      {buckets.map((bucket, index) => {
        const value = bucket[metric], barHeight = value / maximum * 116
        const description = `${trafficTime(bucket.start, hours, true)} UTC: ${format(value)}`
        return <g key={bucket.start}>
          <rect role="img" tabIndex={0} aria-label={description} x={left + index * step + 1} y={20} width={Math.max(1, step - 2)} height={116} fill="transparent" className="outline-none focus:stroke-ring focus:stroke-2" onFocus={() => setActive(index)} onBlur={() => setActive(null)} onMouseEnter={() => setActive(index)} onMouseLeave={() => setActive(null)}><title>{description}</title></rect>
          <rect aria-hidden="true" pointerEvents="none" x={left + index * step + 2} y={bottom - barHeight} width={Math.max(1, step - 4)} height={barHeight} rx={2} className={active === index ? "fill-primary" : "fill-primary/70"} />
        </g>
      })}
      {[0, Math.floor((buckets.length - 1) / 2), buckets.length - 1].filter((index, position, items) => items.indexOf(index) === position).map((index) => <text key={index} x={left + (index + 0.5) * step} y={height - 1} textAnchor={index === 0 ? "start" : index === buckets.length - 1 ? "end" : "middle"} className="fill-muted-foreground text-[9px]">{trafficTime(buckets[index].start, hours)}</text>)}
    </svg>
    <p aria-live="polite" className="mt-3 min-h-4 text-[11px] text-muted-foreground">{chosen ? `${trafficTime(chosen.start, hours, true)} UTC · ${format(chosen[metric])}` : "Focus or hover a bar to inspect recorded usage."}</p>
    <details className="mt-3 border-t pt-3 text-xs"><summary className="cursor-pointer rounded-sm text-muted-foreground outline-none focus-visible:ring-2 focus-visible:ring-ring">View traffic data table</summary><div className="mt-3 max-h-64 overflow-auto"><table className="w-full text-left tabular-nums"><caption className="sr-only">Traffic by {hours <= 24 ? "hour" : "day"}, UTC</caption><thead><tr><th scope="col" className="py-2">UTC window</th><th scope="col" className="py-2 text-right">Billable usage</th><th scope="col" className="py-2 text-right">Data frames</th></tr></thead><tbody>{buckets.map((bucket) => <tr key={bucket.start} className="border-t"><th scope="row" className="py-2 font-normal">{trafficTime(bucket.start, hours)}</th><td className="text-right">{formatBytes(bucket.bytes)}</td><td className="text-right">{bucket.frames.toLocaleString()}</td></tr>)}</tbody></table></div></details>
  </figure>
}
