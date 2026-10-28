import { forwardRef, useEffect, useImperativeHandle, useMemo, useRef, useState, type ReactNode } from "react"
import { machineName, proxyUrl, type Agent, type AppDeployment, type Machine } from "@/lib/api"
import { activeStatuses, machineOnline, type TerminalState } from "@/lib/control-plane"
import { interfaceFrameKey, postEmbedTheme, readEmbedAppearance, withEmbedTheme } from "@/lib/embed-ui"
import type { Theme } from "@/lib/theme"
import type { WorkspaceTab } from "@/components/use-workspace-tabs"
import { TerminalPane, type TerminalPaneHandle } from "@/components/terminal-pane"
import { MobileTerminalKeys } from "@/components/mobile-terminal-keys"
import { MachineRecovery } from "@/components/control-plane-ui"
import { Button } from "@/components/ui/button"
import { cn } from "@/lib/utils"
import { statusLabel, t } from "@/i18n"
import { AppInterface } from '@/components/app-interface'
import { IsolatedAgentInterface } from '@/components/isolated-agent-interface'
import type { AgentEmbedMode } from '@/components/use-agent-origin-capability'

const connectionKeys = {
  "not attached": "surface.terminal_not_attached", connecting: "surface.terminal_connecting", live: "surface.terminal_live",
  reconnecting: "surface.terminal_reconnecting", closed: "surface.terminal_closed", error: "surface.terminal_error",
} as const satisfies Record<TerminalState, string>

export const WorkloadSurface = forwardRef<TerminalPaneHandle, {
  workspaceId: string; agent: Agent; app?: AppDeployment; machine?: Machine; path: string; tab: WorkspaceTab
  visible: boolean; mobile: boolean; theme: Theme; ctrlArmed: boolean
  embedMode: AgentEmbedMode; embedOrigin?: string
  transformInput: (value: string) => string; onToggleCtrl: () => void
  onCopy: (value: string) => void; onLogs: (logs: boolean) => void
  /** Compact header actions for the terminal view (upload, new tab, reconnect). */
  actions?: ReactNode
}>(function WorkloadSurface({ workspaceId, agent, app, machine, path, tab, visible, mobile, theme, ctrlArmed, embedMode, embedOrigin, transformInput, onToggleCtrl, onCopy, onLogs, actions }, ref) {
  const terminal = useRef<TerminalPaneHandle>(null)
  const frame = useRef<HTMLIFrameElement>(null)
  const loadedFrame = useRef<HTMLIFrameElement | null>(null)
  const [status, setStatus] = useState<TerminalState>("not attached")
  const logs = tab.logs && tab.appId === (app?.app_id ?? null)
  const online = machineOnline(machine)
  const agentInterface = !logs && !app && online && agent.interface?.ui_path ? agent.interface : undefined
  const showApp = Boolean(app && !logs)
  const url = useMemo(() => {
    if (app && showApp && online && app.status === "running" && activeStatuses.has(agent.status)) return withEmbedTheme(proxyUrl(`/api/workspaces/${encodeURIComponent(workspaceId)}/apps/${encodeURIComponent(app.app_id)}/ui/`), theme, readEmbedAppearance())
    if (agentInterface) return withEmbedTheme(proxyUrl(`/api/workspaces/${encodeURIComponent(workspaceId)}/agents/${encodeURIComponent(agent.agent_id)}/interface/ui/`), theme, readEmbedAppearance())
    return null
    // Theme changes use postMessage, never a replacement of the iframe instance.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [workspaceId, agent.agent_id, agent.status, app?.app_id, app?.status, app?.hostname, online, showApp, agentInterface?.instance_id, agentInterface?.registered_at, tab.revision])
  useImperativeHandle(ref, () => ({ fit: () => terminal.current?.fit(), focus: () => terminal.current?.focus(), send: (data) => terminal.current?.send(data) }), [])
  // The initial about:blank document still has the Console origin. Initial
  // appearance travels in src; send messages only to a frame that has loaded.
  useEffect(() => { if (frame.current && loadedFrame.current === frame.current) postEmbedTheme(frame.current, theme, readEmbedAppearance()) }, [theme])
  useEffect(() => { if (visible) { const id = requestAnimationFrame(() => terminal.current?.fit()); return () => cancelAnimationFrame(id) } }, [visible, mobile])
  if (!online) return <div className="grid size-full min-h-0 place-items-center bg-sidebar px-6 text-center"><div className="max-w-sm"><p className="text-sm font-medium">{t("surface.machine_is_offline")}</p><p className="mt-2 text-xs leading-5 text-muted-foreground">{t("surface.offline_detail", { machine: machineName(machine, agent.server_id) })}</p><MachineRecovery workspaceId={workspaceId} onCopy={onCopy} reason={t("surface.recovery_reason")} /></div></div>
  if (showApp && !url) return <div className="grid size-full place-items-center px-6 text-center"><div className="max-w-sm space-y-3"><p className="text-sm font-medium">{t("surface.app_is_not_running")}</p><p className="text-xs text-muted-foreground">{t("surface.start_this_app_from_its")}</p><Button variant="outline" onClick={() => onLogs(true)}>{t("surface.show_logs")}</Button></div></div>
  if (url && agentInterface && embedMode !== 'legacy') return <IsolatedAgentInterface key={`${interfaceFrameKey(agent.agent_id, agentInterface.instance_id, agentInterface.registered_at, tab.revision)}:${embedMode}:${embedOrigin ?? ''}`} workspaceId={workspaceId} agentId={agent.agent_id} origin={embedMode === 'isolated' ? embedOrigin : undefined} pending={embedMode === 'pending'} fallbackUrl={url} title={`${path || 'Agent'} interface`} theme={theme} />
  if (url) return <div className="flex size-full min-h-0 min-w-0 flex-col overflow-hidden">
    {showApp && <p role="note" className="shrink-0 border-b bg-sidebar px-3 py-2 text-xs leading-5 text-foreground">{t("surface.app_shared_origin")}</p>}
    {showApp ? <AppInterface key={`${workspaceId}:${app!.app_id}:${interfaceFrameKey(agent.agent_id, app?.hostname ?? "", String(agent.run_generation ?? ""), tab.revision)}`} workspaceId={workspaceId} appId={app!.app_id} url={url} title={`${path || "App"} interface`} theme={theme} visible={visible} /> : <iframe ref={frame} key={interfaceFrameKey(agent.agent_id, agentInterface?.instance_id ?? app?.hostname ?? "", agentInterface?.registered_at ?? String(agent.run_generation ?? ""), tab.revision)} src={url} title={`${path || "Agent"} interface`} className="block w-full min-h-0 flex-1 border-0 bg-background" style={{ colorScheme: theme }} sandbox="allow-scripts allow-forms allow-same-origin allow-modals allow-downloads allow-popups allow-popups-to-escape-sandbox" onLoad={() => { loadedFrame.current = frame.current; postEmbedTheme(frame.current, theme, readEmbedAppearance()) }} />}
  </div>
  // Full-bleed: the terminal is the screen users stare at, so it fills the pane under one compact header.
  const state = statusLabel(agent.status)
  return <div className={cn("grid size-full min-h-0 min-w-0 grid-rows-[auto_minmax(0,1fr)] overflow-hidden bg-[#0f1215]", mobile && "grid-rows-[auto_minmax(0,1fr)_auto]")}>
    <div className="flex h-9 min-w-0 items-center gap-2 border-b bg-background pl-3 pr-1 text-xs max-md:h-11">
      <span title={path} className="truncate font-medium text-foreground">{path || "Terminal"}</span>
      <span className="hidden shrink-0 text-muted-foreground sm:inline" title={agent.agent_id}>{machineName(machine)}</span>
      <span className="flex shrink-0 items-center gap-1.5 text-muted-foreground max-md:hidden"><span aria-hidden="true" className={cn("size-1.5 rounded-full", ["blocked", "failed"].includes(agent.status) ? "bg-status-attention" : ["working", "starting"].includes(agent.status) ? "bg-status-working" : "bg-muted-foreground/60")} />{state}</span>
      {/* Only the exceptions are worth reading: a live connection says nothing. */}
      <span className="ml-auto shrink-0 text-[10px] uppercase tracking-wide text-muted-foreground" aria-live="polite">{status === "live" ? "" : t(connectionKeys[status])}</span>
      {actions}
    </div>
    <div className="min-h-0 min-w-0 overflow-hidden"><TerminalPane ref={terminal} key={tab.revision} workspaceId={workspaceId} agentId={agent.agent_id} active={activeStatuses.has(agent.status)} visible={visible} onStatusChange={setStatus} transformInput={visible ? transformInput : undefined} /></div>
    {mobile && <MobileTerminalKeys terminalRef={terminal} ctrlArmed={ctrlArmed} onToggleCtrl={onToggleCtrl} />}
  </div>
})
