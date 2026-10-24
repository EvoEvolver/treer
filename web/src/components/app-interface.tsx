import { useEffect, useRef, useState } from 'react'
import { postEmbedTheme, readEmbedAppearance } from '@/lib/embed-ui'
import type { Theme } from '@/lib/theme'
import { Button } from '@/components/ui/button'
import { t } from '@/i18n'

type Phase = 'checking' | 'ready' | 'failed' | 'auth'

// Managed Apps only. Agent interface isolation/bootstrap is a separate contract.
// The caller keys this component by workspace/App/runtime generation/tab revision.
export function AppInterface({ url, title, theme, visible, workspaceId, appId }: { url: string; title: string; theme: Theme; visible: boolean; workspaceId: string; appId: string }) {
  const frame = useRef<HTMLIFrameElement>(null)
  const loaded = useRef(false)
  const confirmed = useRef(false)
  const budget = useRef({ attempts: 0, deadline: 0 })
  const recover = useRef<() => void>(() => {})
  const initialNavigation = useRef(false)
  const settleTimer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined)
  const [phase, setPhase] = useState<Phase>('checking')
  const [retry, setRetry] = useState(0)
  useEffect(() => {
    if (!visible || confirmed.current || phase === 'auth' || phase === 'failed') return
    let disposed = false
    let timer: ReturnType<typeof setTimeout> | undefined
    let requestTimer: ReturnType<typeof setTimeout> | undefined
    let controller: AbortController | undefined
    if (!budget.current.deadline) budget.current.deadline = Date.now() + 30_000
    const check = async () => {
      if (disposed) return
      if (budget.current.attempts >= 8 || Date.now() >= budget.current.deadline) { setPhase('failed'); return }
      controller = new AbortController()
      requestTimer = setTimeout(() => controller?.abort(), Math.min(3000, budget.current.deadline - Date.now()))
      budget.current.attempts++
      try {
        // HEAD only, on the exact authenticated root including appearance.
        // Do not follow redirects or fall back to GET/health/App mutations.
        const response = await fetch(url, { method: 'HEAD', credentials: 'include', cache: 'no-store', redirect: 'manual', headers: { Accept: 'text/html' }, signal: controller.signal })
        if (disposed) return
        if (response.ok || response.status === 405 || response.type === 'opaqueredirect' || (response.status >= 300 && response.status < 400)) {
          // These mean responsive, not healthy/authorized. The iframe performs
          // its ordinary canonical GET/redirect chain with normal cookies.
          initialNavigation.current = true
          setPhase('ready')
          return
        }
        if (response.status === 401 || response.status === 403) { setPhase('auth'); return }
        if (![502, 503, 504].includes(response.status)) { setPhase('failed'); return }
      } catch {
        // CORS/network failures are unknown, never opaque no-cors success.
      } finally { clearTimeout(requestTimer) }
      if (disposed) return
      loaded.current = false
      setPhase('checking')
      const delay = Math.min(500 * 2 ** (budget.current.attempts - 1), 4000)
      if (budget.current.attempts >= 8 || Date.now() + delay >= budget.current.deadline) { setPhase('failed'); return }
      timer = setTimeout(() => { void check() }, delay)
    }
    recover.current = () => {
      controller?.abort(); clearTimeout(timer); clearTimeout(requestTimer)
      if (budget.current.attempts >= 8 || Date.now() >= budget.current.deadline) { setPhase('failed'); return }
      timer = setTimeout(() => { void check() }, 500)
    }
    void check()
    return () => { disposed = true; recover.current = () => {}; clearTimeout(timer); clearTimeout(requestTimer); controller?.abort() }
    // Outcomes must not restart the budget; retry/generation explicitly do.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [url, visible, retry])
  useEffect(() => {
    if (!visible || phase !== 'ready' || confirmed.current) return
    const timer = setTimeout(() => { if (!confirmed.current) setPhase('failed') }, Math.max(0, Math.min(10_000, budget.current.deadline - Date.now())))
    return () => clearTimeout(timer)
  }, [phase, url, retry, visible])
  useEffect(() => {
    const unavailable = (event: MessageEvent) => {
      if (!initialNavigation.current || !frame.current || event.source !== frame.current.contentWindow || event.origin !== new URL(url).origin) return
      const data = event.data
      if (!data || data.type !== 'treer.app-ui.unavailable' || data.workspaceId !== workspaceId || data.appId !== appId) return
      initialNavigation.current = false
      clearTimeout(settleTimer.current)
      loaded.current = false
      confirmed.current = false
      setPhase('checking')
      recover.current()
    }
    window.addEventListener('message', unavailable)
    return () => { clearTimeout(settleTimer.current); window.removeEventListener('message', unavailable) }
  }, [url, workspaceId, appId])
  useEffect(() => { if (loaded.current) postEmbedTheme(frame.current, theme, readEmbedAppearance()) }, [theme])
  if (phase !== 'ready') return <div className="grid min-h-0 flex-1 place-items-center overflow-auto px-6 py-6 text-center">
    <div className="max-w-sm space-y-3">
      <p role={phase === 'checking' ? 'status' : 'alert'} className="text-sm leading-6">{t(phase === 'checking' ? 'surface.app_connecting' : phase === 'auth' ? 'surface.app_auth_required' : 'surface.app_connection_failed')}</p>
      {phase !== 'checking' && <Button className="min-h-11" variant="outline" onClick={() => { loaded.current = false; confirmed.current = false; budget.current = { attempts: 0, deadline: 0 }; setPhase('checking'); setRetry(value => value + 1) }}>{t('surface.app_retry')}</Button>}
    </div>
  </div>
  return <iframe ref={frame} src={url} title={title} className="block w-full min-h-0 flex-1 border-0 bg-background" style={{ colorScheme: theme }} sandbox="allow-scripts allow-forms allow-same-origin allow-modals allow-downloads allow-popups allow-popups-to-escape-sandbox" onLoad={() => { loaded.current = true; confirmed.current = true; settleTimer.current = setTimeout(() => { initialNavigation.current = false }, 0); postEmbedTheme(frame.current, theme, readEmbedAppearance()) }} />
}
