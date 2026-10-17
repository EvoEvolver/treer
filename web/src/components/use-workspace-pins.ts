import { useEffect, useRef, useState } from 'react'
import { api, ApiError } from '@/lib/api'
import { PINS_CAPABILITY, PINS_EVENT, parseWorkspacePins, type WorkspacePin, type WorkspacePins } from '@/lib/workspace-pins'
import { t } from '@/i18n'

const empty: WorkspacePins = { pins: [], limit: 50, can_pin: false, revision: 0 }

export function useWorkspacePins(userId: string | null, workspaceId: string | null, preview: boolean) {
  const [retry, setRetry] = useState(0)
  const scope = `${userId ?? ''}:${workspaceId ?? ''}:${preview}`
  const current = useRef(scope)
  current.current = scope
  const [state, setState] = useState({ scope, data: empty, supported: false, busy: false, error: '' })
  const refreshRef = useRef<() => Promise<void>>(async () => {})
  const writeLock = useRef(false)
  const invalidateRead = useRef(() => {})

  useEffect(() => {
    let active = true
    let supported = false
    let generation = 0
    let timer: ReturnType<typeof setTimeout> | undefined
    const controller = new AbortController()
    setState({ scope, data: empty, supported: false, busy: false, error: '' })
    writeLock.current = false
    const base = `/api/workspaces/${encodeURIComponent(workspaceId ?? '')}/pins`

    async function refresh() {
      if (!active || !supported) return
      const request = ++generation
      try {
        const data = parseWorkspacePins(await api<unknown>(base, { signal: controller.signal, cache: 'no-store' }))
        if (active && current.current === scope && request === generation) {
          setState((s) => ({ ...s, scope, data, supported: true, error: '' }))
        }
      } catch (error) {
        if (!active || current.current !== scope || request !== generation) return
        const denied = error instanceof ApiError && [401, 403].includes(error.status)
        setState((s) => ({
          ...s,
          data: denied ? empty : { ...s.data, can_pin: false },
          error: t('pins.load_failed'),
        }))
      }
    }
    refreshRef.current = refresh
    invalidateRead.current = () => { generation++ }

    function schedule(event?: Event) {
      if (event instanceof CustomEvent && event.detail !== workspaceId) return
      if (document.hidden || timer) return
      // Bounded coalescing: continuous events cannot postpone reconciliation.
      timer = setTimeout(() => { timer = undefined; void refresh() }, 150)
    }
    if (userId && workspaceId && !preview) {
      void api<{ capabilities?: string[] }>('/api/auth/config', { signal: controller.signal, cache: 'no-store' })
        .then(async (config) => {
          if (!active || current.current !== scope) return
          supported = Array.isArray(config.capabilities) && config.capabilities.includes(PINS_CAPABILITY)
          if (supported) await refresh()
        })
        .catch(() => {
          // Capability discovery can fail on an older/unreachable Proxy.
          // Do not show pin-specific errors before support is established.
          supported = false
        })
    }
    window.addEventListener(PINS_EVENT, schedule)
    window.addEventListener('focus', schedule)
    document.addEventListener('visibilitychange', schedule)
    return () => {
      active = false
      controller.abort()
      clearTimeout(timer)
      window.removeEventListener(PINS_EVENT, schedule)
      window.removeEventListener('focus', schedule)
      document.removeEventListener('visibilitychange', schedule)
    }
  }, [scope, userId, workspaceId, preview, retry])

  // Hide the previous workspace's data immediately, before the effect resets it.
  const visible = state.scope === scope ? state : { scope, data: empty, supported: false, busy: false, error: '' }
  async function toggle(target: Pick<WorkspacePin, 'kind' | 'id'>) {
    if (!visible.data.can_pin || visible.busy || writeLock.current || !workspaceId) return
    const requestScope = scope
    invalidateRead.current()
    writeLock.current = true
    setState((s) => ({ ...s, busy: true, error: '' }))
    try {
      const pinned = visible.data.pins.some((pin) => pin.kind === target.kind && pin.id === target.id)
      await api(`/api/workspaces/${encodeURIComponent(workspaceId)}/pins/${target.kind}/${encodeURIComponent(target.id)}`, {
        method: pinned ? 'DELETE' : 'PUT',
      })
      if (current.current === requestScope) await refreshRef.current()
    } catch (error) {
      if (current.current === requestScope) {
        // An older successful GET must not undo a denial from this mutation.
        invalidateRead.current()
        const denied = error instanceof ApiError && [401, 403].includes(error.status)
        setState((s) => ({
          ...s,
          data: denied ? empty : s.data,
          error: t(error instanceof ApiError && error.status === 409 ? 'pins.limit_reached' : 'pins.write_failed'),
        }))
      }
    } finally {
      if (current.current === requestScope) {
        writeLock.current = false
        setState((s) => ({ ...s, busy: false }))
      }
    }
  }
  return {
    ...visible.data,
    supported: visible.supported,
    busy: visible.busy,
    error: visible.error,
    toggle,
    refresh: () => visible.supported ? refreshRef.current() : setRetry((n) => n + 1),
  }
}
export type PinsController = ReturnType<typeof useWorkspacePins>
