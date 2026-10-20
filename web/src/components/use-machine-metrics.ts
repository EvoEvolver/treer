import { useEffect, useState } from 'react'
import { fetchMachineMetrics, MetricsError, type MachineMetrics, type MetricsFailure, type MetricsRange } from '@/lib/machine-metrics'
export function useMachineMetrics(workspaceId: string, serverId: string, online: boolean, supported: boolean, range: MetricsRange) {
  const key = `${workspaceId}/${serverId}/${online}/${supported}/${range}`
  const [state, setState] = useState<{ key: string; data?: MachineMetrics; error?: MetricsFailure; loading: boolean; received: number; requestDuration?: number }>({ key, loading: true, received: 0 })
  const [clock, setClock] = useState(0)
  const [revision, setRevision] = useState(0)
  useEffect(() => {
    if (!online || !supported) { setState({ key, loading: false, error: online ? 'unsupported' : 'offline', received: 0 }); return }
    let active = true, busy = false, errors = 0
    let request: AbortController | undefined, timer: ReturnType<typeof setTimeout> | undefined
    const run = async () => {
      if (!active || busy || document.hidden || errors >= 3) return
      busy = true; request = new AbortController()
      setState(s => s.key === key ? { ...s, loading: true } : { key, loading: true, received: 0 })
      const started = performance.now()
      const end = new Date(), from = new Date(end.getTime() - range * 60000)
      const deadline = setTimeout(() => request?.abort(), 10000)
      try {
        const data = await fetchMachineMetrics(workspaceId, { serverId, from: from.toISOString(), to: end.toISOString(), maxPoints: 360 }, request.signal)
        if (active && !request.signal.aborted) { errors = 0; setState({ key, data, loading: false, received: performance.now(), requestDuration: Math.max(0, performance.now() - started) }) }
      } catch (error) {
        if (active && !document.hidden) {
          errors++
          const reason = error instanceof MetricsError ? error.reason : request.signal.aborted ? 'timeout' : 'unavailable'
          // Refusal/offline/unsupported require an explicit new query or identity/inventory change.
          if (['forbidden', 'offline', 'unsupported', 'disabled', 'invalid_response'].includes(reason)) errors = 3
          setState({ key, error: reason, loading: false, received: 0 })
        }
      } finally {
        clearTimeout(deadline); busy = false
        if (active && !document.hidden && errors < 3) timer = setTimeout(run, Math.min(60000, 15000 * 2 ** errors) + Math.random() * 1000)
      }
    }
    const visible = () => { clearTimeout(timer); if (document.hidden) request?.abort(); else void run() }
    document.addEventListener('visibilitychange', visible)
    const tick = setInterval(() => { if (!document.hidden) setClock(performance.now()) }, 1000)
    void run()
    return () => { active = false; clearTimeout(timer); clearInterval(tick); request?.abort(); document.removeEventListener('visibilitychange', visible) }
  }, [key, workspaceId, serverId, online, supported, range, revision])
  // Never render a previous machine/range while the replacement effect starts.
  const current = state.key === key ? state : { key, loading: true, received: 0 }
  return { ...current, elapsed: (current.requestDuration ?? 0) + Math.max(0, clock - current.received), refresh: () => setRevision(n => n + 1) }
}
