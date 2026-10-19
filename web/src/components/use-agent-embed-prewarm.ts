import { useCallback, useLayoutEffect, useRef } from 'react'
import { api } from '@/lib/api'

export type PrewarmTarget = { id: string; origin: string; revision: string }
type Intent = 'pointer' | 'focus'
type Controls = { enter: (id: string, intent: Intent) => void; leave: (id: string) => void; cancel: () => void }
// Local Console budget survives workspace changes. The Proxy independently
// enforces its authoritative per-user limits across tabs and machines.
const budgets = new Map<string, number[]>()
const cooldowns = new Map<string, number>()

export function useAgentEmbedPrewarm(userId: string | undefined, workspaceId: string | null, enabled: boolean, targets: PrewarmTarget[], openIds: string[]) {
  const controls = useRef<Controls | null>(null)
  const cancelledUser = useRef<string | undefined>(undefined)
  const targetKey = JSON.stringify(targets), openKey = JSON.stringify(openIds)
  useLayoutEffect(() => {
    if (!enabled || !userId || !workspaceId || cancelledUser.current === userId) return
    const available = new Map((JSON.parse(targetKey) as PrewarmTarget[]).map(target => [target.id, target]))
    const opened = new Set(JSON.parse(openKey) as string[])
    const connection = (navigator as Navigator & { connection?: EventTarget & { saveData?: boolean } }).connection
    type Attempt = { frame?: HTMLIFrameElement; dwell: ReturnType<typeof setTimeout>; timeout?: ReturnType<typeof setTimeout>; abort: AbortController; receive?: (event: MessageEvent) => void }
    const attempts = new Map<string, Attempt>()
    let lastKey = '', active = true
    const allowed = () => active && !document.hidden && !connection?.saveData
    function remove(id: string) {
      const attempt = attempts.get(id)
      if (!attempt) return
      attempts.delete(id)
      clearTimeout(attempt.dwell); clearTimeout(attempt.timeout); attempt.abort.abort()
      if (attempt.receive) window.removeEventListener('message', attempt.receive)
      attempt.frame?.remove()
    }
    const clear = () => { for (const id of attempts.keys()) remove(id) }
    const keydown = (event: KeyboardEvent) => { lastKey = event.key }
    const pointerdown = () => { lastKey = ''; clear() }
    function enter(id: string, intent: Intent) {
      const target = available.get(id)
      if (!target || opened.has(id) || attempts.has(id) || attempts.size >= 2 || !allowed()) return
      if (intent === 'pointer' ? !matchMedia('(hover: hover) and (pointer: fine)').matches : lastKey !== 'Tab') return
      const key = `${userId}/${workspaceId}/${id}`
      if ((cooldowns.get(key) ?? 0) > Date.now()) return
      const attempt: Attempt = { abort: new AbortController(), dwell: setTimeout(() => start(id, target, key), 400) }
      attempts.set(id, attempt)
    }
    function start(id: string, target: PrewarmTarget, key: string) {
      const attempt = attempts.get(id)
      if (!attempt || !allowed()) { remove(id); return }
      // Any attempted bootstrap has a cooldown, including a failure/cancel.
      cooldowns.set(key, Date.now() + 60_000)
      for (const [entry, until] of cooldowns) if (until <= Date.now()) cooldowns.delete(entry)
      const frame = document.createElement('iframe')
      attempt.frame = frame
      frame.hidden = true; frame.inert = true; frame.tabIndex = -1
      frame.setAttribute('aria-hidden', 'true')
      frame.setAttribute('sandbox', 'allow-scripts allow-same-origin')
      frame.referrerPolicy = 'no-referrer'
      const origin = new URL(target.origin).origin
      let hash: string | undefined
      let state: 'ready' | 'outcome' | 'minting' | 'awaiting-warmed' = 'ready'
      attempt.receive = (event) => {
        if (attempts.get(id) !== attempt || event.source !== frame.contentWindow || event.origin !== origin || !allowed()) return
        const message: unknown = event.data
        if (!message || typeof message !== 'object' || Array.isArray(message)) return
        const data = message as Record<string, unknown>
        if (data.treer_embed === 'cookies_blocked') {
          if (hash ? Object.keys(data).length === 2 && data.nonce_hash === hash : Object.keys(data).length === 1) remove(id)
          return
        }
        if (Object.keys(data).length !== 3 || data.version !== 2 || typeof data.nonce_hash !== 'string' || !/^[0-9a-f]{64}$/.test(data.nonce_hash)) return
        if (state === 'ready' && data.treer_embed === 'ready') {
          hash = data.nonce_hash; state = 'outcome'
          frame.contentWindow!.postMessage({ treer_embed: 'probe', version: 2, nonce_hash: hash }, origin)
          return
        }
        if (!hash || data.nonce_hash !== hash) return
        if (data.treer_embed === 'warm_failed') { remove(id); return }
        if (data.treer_embed === 'warmed' && (state === 'outcome' || state === 'awaiting-warmed')) { remove(id); return }
        if (data.treer_embed !== 'mint' || state !== 'outcome') return
        state = 'minting'
        const now = Date.now(), recent = (budgets.get(userId!) ?? []).filter(at => now - at < 60_000)
        if (recent.length >= 6) { remove(id); return }
        recent.push(now); budgets.set(userId!, recent)
        // Bound retained account keys without keeping any credential or code.
        while (budgets.size > 16) budgets.delete(budgets.keys().next().value!)
        void api<{ code: string; origin: string; expires_in: number }>(`/api/workspaces/${encodeURIComponent(workspaceId!)}/agents/${encodeURIComponent(id)}/interface/embed-authorization`, {
          method: 'POST', body: JSON.stringify({ browser_nonce_hash: hash, purpose: 'prewarm' }),
          cache: 'no-store', redirect: 'error', signal: attempt.abort.signal,
        }).then(result => {
          if (attempts.get(id) !== attempt || !allowed() || attempt.abort.signal.aborted) return
          if (result.origin !== origin || typeof result.code !== 'string' || !/^iec_v1_[0-9a-f]{64}$/.test(result.code) || !Number.isFinite(result.expires_in) || result.expires_in <= 0 || result.expires_in > 30) { remove(id); return }
          state = 'awaiting-warmed'
          frame.contentWindow!.postMessage({ treer_embed: 'code', code: result.code }, origin)
        }).catch(() => { if (attempts.get(id) === attempt) remove(id) }) // Includes 401/403/429: quiet, no retry.
      }
      attempt.timeout = setTimeout(() => remove(id), 15_000)
      window.addEventListener('message', attempt.receive)
      frame.src = new URL('/.treer/embed?reuse=1&prewarm=1', origin).href
      document.body.append(frame)
    }
    controls.current = { enter, leave: remove, cancel: () => { cancelledUser.current = userId; active = false; clear() } }
    window.addEventListener('keydown', keydown, true)
    window.addEventListener('pointerdown', pointerdown, true)
    window.addEventListener('blur', clear)
    document.addEventListener('visibilitychange', clear)
    connection?.addEventListener('change', clear)
    return () => {
      active = false; controls.current = null; clear()
      window.removeEventListener('keydown', keydown, true)
      window.removeEventListener('pointerdown', pointerdown, true)
      window.removeEventListener('blur', clear)
      document.removeEventListener('visibilitychange', clear)
      connection?.removeEventListener('change', clear)
    }
  }, [userId, workspaceId, enabled, targetKey, openKey])
  return {
    enter: useCallback((id: string, intent: Intent) => controls.current?.enter(id, intent), []),
    leave: useCallback((id: string) => controls.current?.leave(id), []),
    cancel: useCallback(() => controls.current?.cancel(), []),
  }
}
