import { api,websocketUrl,type Agent,type AppDeployment,type Snapshot,type Workspace } from "@/lib/api"
import { replaceWorkspace,visibleWorkspaceSnapshot } from "@/lib/control-plane"
import { useCallback,useEffect,useRef,useState,type Dispatch,type RefObject,type SetStateAction } from "react"

interface WorkspaceLiveFeedOptions {
  preview: boolean
  workspaceId: string | null
  workspaceIdRef: RefObject<string | null>
  snapshot: Snapshot | null
  setSnapshot: Dispatch<SetStateAction<Snapshot | null>>
  setWorkspaces: Dispatch<SetStateAction<Workspace[]>>
  setSelectedAgentId: Dispatch<SetStateAction<string | null>>
  setSelectedMachineId: Dispatch<SetStateAction<string | null>>
  syncWorkspaces: () => Promise<void>
  showError: (reason: unknown) => void
}

// Core snapshots and App metadata share one refresh/lifecycle boundary. Keep
// the existing mutation fencing, debounce windows and event ordering intact.
export function useWorkspaceLiveFeed({ preview, workspaceId, workspaceIdRef, snapshot, setSnapshot, setWorkspaces, setSelectedAgentId, setSelectedMachineId, syncWorkspaces, showError }: WorkspaceLiveFeedOptions) {
  const [apps, setAppsState] = useState<AppDeployment[]>([])
  const appsRef = useRef(apps)
  appsRef.current = apps
  // Local rename/delete must win over requests that started before the event.
  const appsMutation = useRef(0)
  const appsRequest = useRef(0)
  const appsRefreshTimer = useRef<number | undefined>(undefined)
  const appsRefreshMaxTimer = useRef<number | undefined>(undefined)
  const setApps = useCallback((value: SetStateAction<AppDeployment[]>) => {
    appsMutation.current += 1
    setAppsState(value)
  }, [])
  const [appsLoading, setAppsLoading] = useState(false)

  const loadApps = useCallback(async () => {
    const request = ++appsRequest.current
    const mutation = appsMutation.current
    if (preview || !workspaceId) {
      setApps([])
      return
    }
    setAppsLoading(true)
    const requestedWorkspaceId = workspaceId
    try {
      const data = await api<{ apps: AppDeployment[] }>(`/api/workspaces/${encodeURIComponent(requestedWorkspaceId)}/apps`)
      if (workspaceIdRef.current !== requestedWorkspaceId || request !== appsRequest.current || mutation !== appsMutation.current) return
      setAppsState(data.apps)
    } catch (reason) {
      if (workspaceIdRef.current === requestedWorkspaceId && request === appsRequest.current) throw reason
    } finally {
      if (workspaceIdRef.current === requestedWorkspaceId && request === appsRequest.current) setAppsLoading(false)
    }
  }, [preview, workspaceId, setApps])

  // App mutations are audit events, not dedicated workspace broadcasts. Coalesce
  // runtime/snapshot hints and foreground checks; only Core lists establish mapping.
  const scheduleAppsRefresh = useCallback(() => {
    if (preview || !workspaceId) return
    const refresh = () => {
      window.clearTimeout(appsRefreshTimer.current)
      window.clearTimeout(appsRefreshMaxTimer.current)
      appsRefreshTimer.current = undefined
      appsRefreshMaxTimer.current = undefined
      if (workspaceIdRef.current === workspaceId) void loadApps().catch(showError)
    }
    window.clearTimeout(appsRefreshTimer.current)
    appsRefreshTimer.current = window.setTimeout(refresh, 200)
    // A continuous stream must not keep an App undiscovered indefinitely.
    if (appsRefreshMaxTimer.current === undefined) appsRefreshMaxTimer.current = window.setTimeout(refresh, 2000)
  }, [preview, workspaceId, loadApps, showError])

  useEffect(() => {
    const refresh = () => { if (document.visibilityState === "visible") scheduleAppsRefresh() }
    window.addEventListener("focus", refresh)
    document.addEventListener("visibilitychange", refresh)
    return () => {
      window.clearTimeout(appsRefreshTimer.current)
      window.clearTimeout(appsRefreshMaxTimer.current)
      appsRefreshTimer.current = undefined
      appsRefreshMaxTimer.current = undefined
      window.removeEventListener("focus", refresh)
      document.removeEventListener("visibilitychange", refresh)
    }
  }, [scheduleAppsRefresh])

  const refreshSnapshot = useCallback(async () => {
    if (preview || !workspaceId) return
    const requestedWorkspaceId = workspaceId
    const data = visibleWorkspaceSnapshot(await api<Snapshot>(`/api/workspaces/${encodeURIComponent(requestedWorkspaceId)}/snapshot`))
    if (workspaceIdRef.current !== requestedWorkspaceId || data.workspace.workspace_id !== requestedWorkspaceId) return
    setSnapshot(data)
    setWorkspaces((items) => replaceWorkspace(items, data.workspace))
  }, [preview, workspaceId])

  useEffect(() => {
    if (preview) return
    if (!workspaceId) {
      setSnapshot(null)
      setSelectedAgentId(null)
      return
    }
    let disposed = false
    let socket: WebSocket | null = null
    let timer: number | undefined
    setSnapshot((current) => current?.workspace.workspace_id === workspaceId ? current : null)
    refreshSnapshot().catch(showError)
    const connect = () => {
      if (disposed || workspaceIdRef.current !== workspaceId) return
      socket = new WebSocket(websocketUrl(`/api/workspaces/${encodeURIComponent(workspaceId)}/events`))
      socket.onmessage = (event) => {
        if (disposed || workspaceIdRef.current !== workspaceId) return
        const message = JSON.parse(event.data) as { event: string; revision?: number; data?: Snapshot | Workspace | Agent }
        if (message.event === "workspace.snapshot" && message.data) {
          const next = visibleWorkspaceSnapshot(message.data as Snapshot)
          if (next.workspace.workspace_id !== workspaceId) return
          setSnapshot(next)
          setWorkspaces((items) => replaceWorkspace(items, next.workspace))
          scheduleAppsRefresh()
        } else if (message.event === "workspace.renamed" && message.data) {
          const updated = message.data as Workspace
          setWorkspaces((items) => replaceWorkspace(items, updated))
          setSnapshot((current) => current?.workspace.workspace_id === updated.workspace_id ? { ...current, workspace: updated } : current)
        } else if ((message.event === "agent.updated" || message.event === "agent.renamed") && message.data) {
          const updated = message.data as Agent
          const knownApp = appsRef.current.find((app) => (app.runtime_agent_id ?? app.app_id) === updated.agent_id)
          if (updated.kind === "app" || updated.name.startsWith("app:") || knownApp) scheduleAppsRefresh()
          if (knownApp && updated.name !== knownApp.name && updated.name !== `app:${knownApp.name}`) setApps((current) => current.map((app) => app.runtime_agent_id === updated.agent_id || app.app_id === updated.agent_id ? { ...app, name: updated.name === `app:${app.name}` ? app.name : updated.name } : app))
          setSnapshot((current) => {
            if (!current || current.workspace.workspace_id !== workspaceId) return current
            const agents = current.agents.some((agent) => agent.agent_id === updated.agent_id)
              ? current.agents.map((agent) => agent.agent_id === updated.agent_id ? updated : agent)
              : [...current.agents, updated]
            return { ...current, revision: Math.max(current.revision, message.revision ?? current.revision), agents }
          })
        } else if (message.event === "agent.deleted" && message.data) {
          const removed = message.data as Agent
          if (appsRef.current.some((app) => (app.runtime_agent_id ?? app.app_id) === removed.agent_id)) {
            scheduleAppsRefresh()
            setApps((current) => current.filter((app) => app.runtime_agent_id !== removed.agent_id && app.app_id !== removed.agent_id))
          }
          setSnapshot((current) => {
            if (!current || current.workspace.workspace_id !== workspaceId) return current
            return { ...current, revision: Math.max(current.revision, message.revision ?? current.revision), agents: current.agents.filter((agent) => agent.agent_id !== removed.agent_id) }
          })
        } else if (message.event === "workspace.deleted" && message.data) {
          const removed = message.data as Workspace
          setWorkspaces((items) => items.filter((item) => item.workspace_id !== removed.workspace_id))
          if (removed.workspace_id === workspaceId) {
            workspaceIdRef.current = null
            setSnapshot(null)
            setSelectedAgentId(null)
            setSelectedMachineId(null)
            void syncWorkspaces().catch(showError)
          }
        }
        else {
          if (message.event.startsWith("server.")) scheduleAppsRefresh()
          refreshSnapshot().catch(showError)
        }
      }
      socket.onclose = () => {
        if (disposed || workspaceIdRef.current !== workspaceId) return
        timer = window.setTimeout(() => connect(), 1200)
      }
    }
    connect()
    return () => { disposed = true; window.clearTimeout(timer); socket?.close() }
  }, [preview, workspaceId, refreshSnapshot, syncWorkspaces, showError, scheduleAppsRefresh, setApps])

  const appRuntimeSignature = useRef("")
  useEffect(() => {
    const runtimes = snapshot?.agents.filter((agent) => agent.kind === "app" || agent.name.startsWith("app:") || appsRef.current.some((app) => (app.runtime_agent_id ?? app.app_id) === agent.agent_id)) ?? []
    const signature = JSON.stringify([workspaceId, runtimes.map(({ agent_id, name, status }) => [agent_id, name, status])])
    if (signature === appRuntimeSignature.current) return
    appRuntimeSignature.current = signature
    if (runtimes.length) scheduleAppsRefresh()
  }, [snapshot, workspaceId, scheduleAppsRefresh])

  return { apps, setApps, appsLoading, loadApps, refreshSnapshot }
}
