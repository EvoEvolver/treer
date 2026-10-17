import { useEffect, useState } from 'react'

// Upstream UIs can imitate Proxy errors: keep observations per Agent. Only
// the selected Agent is probed, after the user opens it (including deep links).
// Bounded in-memory cache; no credentials or response bodies are retained.
const observations = new Map<string, { refused: boolean; until: number }>()
function remember(scope: string, refused: boolean) {
  const now = Date.now()
  for (const [key, value] of observations) if (value.until <= now) observations.delete(key)
  observations.delete(scope)
  observations.set(scope, { refused, until: now + 60_000 })
  while (observations.size > 64) observations.delete(observations.keys().next().value!)
}
export function agentUiIsRefused(scope: string) {
  const cached = observations.get(scope)
  return !!cached?.refused && cached.until > Date.now()
}
export function useAgentUiFallback(scope: string, probeUrl: string | undefined) {
  const [, notify] = useState(0)
  useEffect(() => {
    if (!probeUrl) return
    const cached = observations.get(scope)
    if (cached && cached.until > Date.now()) return
    const controller = new AbortController()
    const timeout = setTimeout(() => controller.abort(), 5000)
    void (async () => {
      try {
        const response = await fetch(probeUrl, { credentials: 'include', cache: 'no-store', redirect: 'error', signal: controller.signal })
        if (controller.signal.aborted) { await response.body?.cancel(); return }
        remember(scope, false)
        if (response.status !== 503 || !response.headers.get('content-type')?.includes('application/json')) {
          await response.body?.cancel()
          return
        }
        // Bound error reads, including chunked responses. Successful UI bytes
        // are cancelled above and never parsed or executed.
        const reader = response.body?.getReader()
        if (!reader) return
        const decoder = new TextDecoder()
        let text = '', size = 0
        try {
          while (true) {
            const { done, value } = await reader.read()
            if (done) break
            size += value.byteLength
            if (size > 4096) return
            text += decoder.decode(value, { stream: true })
          }
          text += decoder.decode()
          if (JSON.parse(text)?.error?.code === 'embedded_ui_requires_separate_app_origin' && !controller.signal.aborted) { remember(scope, true); notify((revision) => revision + 1) }
        } finally { await reader.cancel() }
      } catch {
        // Bound repeated failures too; cancellation on selection changes must
        // not overwrite a completed observation for another request.
        if (!controller.signal.aborted) remember(scope, false)
      }
      finally { clearTimeout(timeout) }
    })()
    return () => { controller.abort(); clearTimeout(timeout) }
  }, [scope, probeUrl])
}
