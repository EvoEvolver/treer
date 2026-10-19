import { useEffect, useState } from 'react'
import { api, ApiError } from '@/lib/api'

export type AgentEmbedMode = 'pending' | 'legacy' | 'isolated' | 'unavailable'

// Fail closed while discovery is pending/unavailable. Only a successful older
// config (or its explicit 404) permits the legacy Proxy-origin destination.
export function useAgentOriginCapability(userId: string | undefined, preview: boolean) {
  const scope = `${userId ?? ''}:${preview}`
  const [state, setState] = useState<{ scope: string; mode: 'unknown' | 'legacy' | 'isolated'; embed: AgentEmbedMode; prewarm: boolean }>({ scope, mode: 'unknown', embed: preview ? 'legacy' : 'pending', prewarm: false })
  useEffect(() => {
    setState({ scope, mode: 'unknown', embed: preview ? 'legacy' : 'pending', prewarm: false })
    if (!userId || preview) return
    let active = true
    const controller = new AbortController()
    void api<{ capabilities?: string[] }>('/api/auth/config', { signal: controller.signal, cache: 'no-store' })
      .then((config) => {
        if (active) setState({ scope,
          mode: Array.isArray(config.capabilities) && config.capabilities.includes('agent.interface-origin.v1') ? 'isolated' : 'legacy',
          embed: Array.isArray(config.capabilities) && config.capabilities.includes('agent.interface-embed.v1') ? 'isolated' : 'legacy',
          prewarm: Array.isArray(config.capabilities) && config.capabilities.includes('agent.interface-embed-prewarm.v1'),
        })
      }).catch((error) => {
        if (active) setState({ scope, mode: error instanceof ApiError && error.status === 404 ? 'legacy' : 'unknown', embed: error instanceof ApiError && error.status === 404 ? 'legacy' : 'unavailable', prewarm: false })
      })
    return () => { active = false; controller.abort() }
  }, [scope, userId, preview])
  return state.scope === scope ? state : { mode: 'unknown' as const, embed: 'pending' as const, prewarm: false }
}
