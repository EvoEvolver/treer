import { useEffect, useState } from 'react'
import { api, ApiError } from '@/lib/api'

// Fail closed while discovery is pending/unavailable. Only a successful older
// config (or its explicit 404) permits the legacy Proxy-origin destination.
export function useAgentOriginCapability(userId: string | undefined, preview: boolean) {
  const scope = `${userId ?? ''}:${preview}`
  const [state, setState] = useState<{ scope: string; mode: 'unknown' | 'legacy' | 'isolated' }>({ scope, mode: 'unknown' })
  useEffect(() => {
    setState({ scope, mode: 'unknown' })
    if (!userId || preview) return
    let active = true
    const controller = new AbortController()
    void api<{ capabilities?: string[] }>('/api/auth/config', { signal: controller.signal, cache: 'no-store' })
      .then((config) => {
        if (active) setState({ scope, mode: Array.isArray(config.capabilities) && config.capabilities.includes('agent.interface-origin.v1') ? 'isolated' : 'legacy' })
      }).catch((error) => {
        if (active) setState({ scope, mode: error instanceof ApiError && error.status === 404 ? 'legacy' : 'unknown' })
      })
    return () => { active = false; controller.abort() }
  }, [scope, userId, preview])
  return state.scope === scope ? state.mode : 'unknown'
}
