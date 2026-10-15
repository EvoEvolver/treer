import { useCallback, useEffect, useState } from "react"

export const OVERVIEW_TAB = "overview"
export const MAX_OPEN_TABS = 64
export type WorkspaceTab = { id: string; live: boolean; used: number; revision: number; logs: boolean; appId: string | null }
type State = { scope: string; tabs: WorkspaceTab[] }
const emptyTab = (id: string): WorkspaceTab => ({ id, live: false, used: 0, revision: 0, logs: false, appId: null })
export function tabsStorageKey(user: string, workspace: string) { return `treer:tabs:v1:${encodeURIComponent(user)}:${encodeURIComponent(workspace)}` }
function read(scope: string): State {
  try {
    const raw = localStorage.getItem(scope)
    if (!raw || raw.length > 16_384) return { scope, tabs: [] }
    const ids: unknown = JSON.parse(raw)
    if (Array.isArray(ids)) return { scope, tabs: [...new Set(ids.filter((id): id is string => typeof id === "string" && /^[a-zA-Z0-9_-]{1,128}$/.test(id) && id !== OVERVIEW_TAB))].slice(0, MAX_OPEN_TABS).map(emptyTab) }
  } catch { /* Storage is optional; no credentials or content are persisted. */ }
  return { scope, tabs: [] }
}
function save(state: State) { try { if (state.scope) localStorage.setItem(state.scope, JSON.stringify(state.tabs.map((tab) => tab.id))) } catch { /* In-memory tabs still work. */ } }
function bounded(tabs: WorkspaceTab[], limit: number) {
  const live = tabs.filter((tab) => tab.live).sort((a, b) => b.used - a.used)
  const keep = new Set(live.slice(0, limit - 1).map((tab) => tab.id)) // Overview is always live.
  return tabs.map((tab) => tab.live && !keep.has(tab.id) ? { ...tab, live: false } : tab)
}

export function useWorkspaceTabs(userId: string | undefined, workspaceId: string | null, mobile: boolean, ids: string[], ready: boolean, preview: boolean) {
  const scope = userId && workspaceId && !preview ? tabsStorageKey(userId, workspaceId) : ""
  const limit = mobile ? 4 : 6
  const [state, setState] = useState<State>(() => scope ? read(scope) : { scope, tabs: [] })
  const tabs = state.scope === scope ? state.tabs : []
  useEffect(() => { setState((current) => current.scope === scope ? current : scope ? read(scope) : { scope, tabs: [] }) }, [scope])
  const update = useCallback((change: (tabs: WorkspaceTab[]) => WorkspaceTab[]) => {
    setState((current) => {
      const base = current.scope === scope ? current : scope ? read(scope) : { scope, tabs: [] }
      const next = { scope, tabs: change(base.tabs) }
      save(next)
      return next
    })
  }, [scope])
  const validIds = ids.join("|")
  useEffect(() => {
    if (!ready) return
    const valid = new Set(ids)
    update((current) => current.some((tab) => !valid.has(tab.id)) ? current.filter((tab) => valid.has(tab.id)) : current)
    // The signature prevents snapshot identity churn from causing storage writes.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [validIds, ready, update])
  useEffect(() => { update((current) => bounded(current, limit)) }, [limit, update])
  const open = (id: string, logs?: boolean, appId: string | null = null) => update((current) => {
    const previous = current.find((tab) => tab.id === id) ?? emptyTab(id)
    const used = Math.max(0, ...current.map((tab) => tab.used)) + 1
    const next = { ...previous, live: true, used, ...(logs === undefined ? {} : { logs, appId }) }
    const list = current.some((tab) => tab.id === id) ? current.map((tab) => tab.id === id ? next : tab) : [...current.slice(-(MAX_OPEN_TABS - 1)), next]
    return bounded(list, limit)
  })
  const close = (id: string) => update((current) => current.filter((tab) => tab.id !== id))
  const closeMany = (ids: string[]) => update((current) => current.filter((tab) => !ids.includes(tab.id)))
  const setLogs = (id: string, logs: boolean, appId: string | null) => update((current) => current.map((tab) => tab.id === id ? { ...tab, logs, appId } : tab))
  const reload = (id: string) => update((current) => current.map((tab) => tab.id === id ? { ...tab, revision: tab.revision + 1 } : tab))
  return { tabs, open, close, closeMany, setLogs, reload, limit, scope, liveCount: 1 + tabs.filter((tab) => tab.live).length }
}
