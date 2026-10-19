import { useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react'
import { api } from '@/lib/api'
import { postEmbedTheme, readEmbedAppearance, withEmbedTheme } from '@/lib/embed-ui'
import type { Theme } from '@/lib/theme'
import { t } from '@/i18n'

type Phase = 'bootstrap' | 'framed' | 'checking-fallback' | 'offer-compatibility' | 'compatibility' | 'refused' | 'unavailable' | 'timed-out'

// Probe only the fixed Proxy tunnel. Never execute its bytes while deciding
// whether the deployment permits the weaker compatibility view.
async function fallbackAllowed(url: string, signal: AbortSignal): Promise<boolean> {
  const response = await fetch(url, { credentials: 'include', cache: 'no-store', redirect: 'error', signal })
  if (response.ok) { await response.body?.cancel(); return true }
  if (response.status !== 503 || !response.headers.get('content-type')?.includes('application/json')) {
    await response.body?.cancel(); throw new Error('unavailable')
  }
  const reader = response.body?.getReader()
  if (!reader) throw new Error('unavailable')
  let size = 0, text = ''
  const decoder = new TextDecoder()
  try {
    while (true) {
      const { done, value } = await reader.read()
      if (done) break
      size += value.byteLength
      if (size > 4096) throw new Error('unavailable')
      text += decoder.decode(value, { stream: true })
    }
    text += decoder.decode()
    if (JSON.parse(text)?.error?.code === 'embedded_ui_requires_isolated_origin') return false
    throw new Error('unavailable')
  } finally { await reader.cancel() }
}

// The parent keys this component by workspace, registered interface, revision,
// capability mode and the authenticated origin. A new generation cancels every
// old listener/request; tab hiding and appearance changes do not mint again.
export function IsolatedAgentInterface({ workspaceId, agentId, origin, fallbackUrl, title, theme, pending = false }: {
  workspaceId: string; agentId: string; origin?: string; fallbackUrl: string
  title: string; theme: Theme; pending?: boolean
}) {
  const frame = useRef<HTMLIFrameElement>(null)
  const noticeRegion = useRef<HTMLDivElement>(null)
  const loaded = useRef<() => void>(() => {})
  const [phase, setPhase] = useState<Phase>(origin ? 'bootstrap' : 'unavailable')
  // Appearance is an initial public hint; later changes use exact-origin messages.
  const bootstrap = useMemo(() => origin ? withEmbedTheme(new URL('/.treer/embed', origin).href, theme, readEmbedAppearance()) : null, [origin]) // eslint-disable-line react-hooks/exhaustive-deps
  useEffect(() => { postEmbedTheme(frame.current, theme, readEmbedAppearance()) }, [theme])
  useLayoutEffect(() => { if (phase === 'compatibility') noticeRegion.current?.focus() }, [phase])
  useLayoutEffect(() => {
    if (!origin || pending) return
    const expectedOrigin = new URL(origin).origin
    let active = true, ended = false, acceptedHash: string | undefined, sent = false, loads = 0
    const deadline = Date.now() + 20_000
    const mint = new AbortController(), fallback = new AbortController()
    let fallbackTimer: ReturnType<typeof setTimeout> | undefined
    const stop = () => { ended = true; clearTimeout(timer); mint.abort() }
    const unavailable = () => { if (active) { stop(); setPhase('unavailable') } }
    // Give the bootstrap's own 15 s failure report time to reach the Console.
    const timer = setTimeout(() => { if (active) { stop(); setPhase('timed-out') } }, 20_000)
    const compatibility = async () => {
      stop()
      setPhase('checking-fallback')
      fallbackTimer = setTimeout(() => { fallback.abort(); if (active) setPhase('unavailable') }, 5000)
      try {
        const allowed = await fallbackAllowed(fallbackUrl, fallback.signal)
        if (active && !fallback.signal.aborted) setPhase(allowed ? 'offer-compatibility' : 'refused')
      } catch { if (active && !fallback.signal.aborted) setPhase('unavailable') }
      finally { clearTimeout(fallbackTimer) }
    }
    const receive = (event: MessageEvent) => {
      if (!active || ended || event.origin !== expectedOrigin || event.source !== frame.current?.contentWindow || Date.now() >= deadline) return
      const data: unknown = event.data
      if (!data || typeof data !== 'object' || Array.isArray(data)) return
      const message = data as Record<string, unknown>
      if (message.treer_embed === 'cookies_blocked') {
        // WindowProxy/origin survive bootstrap -> Agent navigation. Only the
        // bootstrap knows its accepted hash; Agent HTML must not force fallback.
        if (acceptedHash ? Object.keys(message).length === 2 && message.nonce_hash === acceptedHash : Object.keys(message).length === 1) void compatibility()
        return
      }
      if (acceptedHash || message.treer_embed !== 'ready' || Object.keys(message).length !== 2 || typeof message.nonce_hash !== 'string' || !/^[0-9a-f]{64}$/.test(message.nonce_hash)) return
      acceptedHash = message.nonce_hash
      void api<{ code: string; origin: string; expires_in: number }>(`/api/workspaces/${encodeURIComponent(workspaceId)}/agents/${encodeURIComponent(agentId)}/interface/embed-authorization`, {
        method: 'POST', body: JSON.stringify({ browser_nonce_hash: message.nonce_hash }), cache: 'no-store', redirect: 'error', signal: mint.signal,
      }).then((result) => {
        if (!active || ended || mint.signal.aborted || Date.now() >= deadline || event.source !== frame.current?.contentWindow) return
        if (result.origin !== expectedOrigin || typeof result.code !== 'string' || !/^iec_v1_[0-9a-f]{64}$/.test(result.code) || !Number.isFinite(result.expires_in) || result.expires_in <= 0 || result.expires_in > 30) { unavailable(); return }
        sent = true
        // No code in a URL, storage, DOM attribute, error string or log.
        frame.current!.contentWindow!.postMessage({ treer_embed: 'code', code: result.code }, expectedOrigin)
      }).catch(() => { if (!mint.signal.aborted) unavailable() })
    }
    loaded.current = () => {
      loads++
      // The bootstrap document loads first, then replaces itself with the UI.
      if (active && !ended && sent && loads >= 2) { stop(); setPhase('framed') }
    }
    window.addEventListener('message', receive)
    return () => { active = false; stop(); fallback.abort(); clearTimeout(fallbackTimer); loaded.current = () => {}; window.removeEventListener('message', receive) }
  }, [workspaceId, agentId, origin, fallbackUrl, pending])

  const waiting = pending || phase === 'bootstrap' || phase === 'checking-fallback'
  const error = !pending && (phase === 'unavailable' || phase === 'refused' || phase === 'timed-out')
  const notice = pending || phase === 'bootstrap' ? 'surface.embed_checking' : phase === 'checking-fallback' ? 'surface.embed_fallback_checking' : phase === 'offer-compatibility' ? 'surface.embed_offer' : phase === 'compatibility' ? 'surface.embed_compatibility' : phase === 'refused' ? 'surface.embed_refused' : phase === 'timed-out' ? 'surface.embed_timeout' : !origin ? 'surface.embed_unknown' : 'surface.embed_unavailable'
  const src = phase === 'compatibility' ? fallbackUrl : bootstrap
  return <div className="flex size-full min-h-0 min-w-0 flex-col overflow-hidden">
    {(waiting || error || phase === 'compatibility' || phase === 'offer-compatibility') && <div ref={noticeRegion} tabIndex={-1} role={error || phase === 'compatibility' || phase === 'offer-compatibility' ? 'alert' : 'status'} className="flex shrink-0 flex-wrap items-center justify-between outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring gap-x-4 gap-y-1 border-b bg-sidebar px-3 py-2 text-xs leading-5 text-foreground">
      <p className="w-full min-w-0 sm:w-auto sm:flex-1">{t(notice)}</p>
      {!pending && origin && <a href={origin} target="_blank" rel="noopener noreferrer" className="font-medium inline-flex min-h-11 items-center rounded-sm underline outline-none focus-visible:ring-2 focus-visible:ring-ring">{t('app.open_in_new_tab')}</a>}
      {phase === 'offer-compatibility' && <button type="button" onClick={() => setPhase('compatibility')} className="-ml-2 inline-flex min-h-11 items-center rounded-sm px-2 underline outline-none focus-visible:ring-2 focus-visible:ring-ring">{t('surface.embed_load_compatibility')}</button>}
    </div>}
    {!pending && !error && phase !== 'checking-fallback' && phase !== 'offer-compatibility' && src && <iframe ref={frame} src={src} title={title} className="block min-h-0 w-full flex-1 border-0 bg-background" style={{ colorScheme: theme }} sandbox="allow-scripts allow-forms allow-same-origin allow-modals allow-downloads allow-popups allow-popups-to-escape-sandbox" onLoad={() => { postEmbedTheme(frame.current, theme, readEmbedAppearance()); loaded.current() }} />}
  </div>
}
