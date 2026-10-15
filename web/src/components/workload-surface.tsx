import { forwardRef, useEffect, useImperativeHandle, useMemo, useRef, useState } from "react"
import { Upload, X } from "lucide-react"
import { machineName, proxyUrl, type Agent, type AppDeployment, type Machine } from "@/lib/api"
import { activeStatuses, machineOnline, type TerminalState } from "@/lib/control-plane"
import { interfaceFrameKey, postEmbedTheme, readEmbedAppearance, withEmbedTheme } from "@/lib/embed-ui"
import type { Theme } from "@/lib/theme"
import type { WorkspaceTab } from "@/components/use-workspace-tabs"
import { TerminalPane, type TerminalPaneHandle } from "@/components/terminal-pane"
import { MobileTerminalKeys } from "@/components/mobile-terminal-keys"
import { IconButton, MachineRecovery } from "@/components/control-plane-ui"
import { Button } from "@/components/ui/button"
import { cn } from "@/lib/utils"

export const WorkloadSurface = forwardRef<TerminalPaneHandle, {
  workspaceId: string; agent: Agent; app?: AppDeployment; machine?: Machine; path: string; tab: WorkspaceTab
  visible: boolean; mobile: boolean; theme: Theme; ctrlArmed: boolean
  transformInput: (value: string) => string; onToggleCtrl: () => void; onClose: () => void; onUpload: () => void
  onCopy: (value: string) => void; onLogs: (logs: boolean) => void
}>(function WorkloadSurface({ workspaceId, agent, app, machine, path, tab, visible, mobile, theme, ctrlArmed, transformInput, onToggleCtrl, onClose, onUpload, onCopy, onLogs }, ref) {
  const terminal = useRef<TerminalPaneHandle>(null)
  const frame = useRef<HTMLIFrameElement>(null)
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
  useEffect(() => { postEmbedTheme(frame.current, theme, readEmbedAppearance()) }, [theme])
  useEffect(() => { if (visible) { const id = requestAnimationFrame(() => terminal.current?.fit()); return () => cancelAnimationFrame(id) } }, [visible, mobile])
  const closeButton = <button type="button" className="grid size-8 place-items-center rounded-md text-muted-foreground hover:bg-accent" aria-label={url ? "Close full-screen interface" : "Close full-screen terminal"} onClick={onClose}><X className="size-4" /></button>
  const appToggle = app && <Button size="sm" variant="ghost" onClick={() => onLogs(!logs)}>{logs ? "Show App" : "Show logs"}</Button>
  if (!online) return <div className="grid size-full min-h-0 place-items-center bg-sidebar px-6 text-center"><div className="max-w-sm">{mobile && <Button variant="ghost" onClick={onClose}>Back to workloads</Button>}<p className="text-sm font-medium">Machine is offline</p><p className="mt-2 text-xs leading-5 text-muted-foreground">{machineName(machine, agent.server_id)} is not connected to the control plane. It may be stopped, waking from sleep, or fenced as a duplicate.</p><MachineRecovery workspaceId={workspaceId} onCopy={onCopy} reason="Copy a recovery command and run it on that machine. restart-controller keeps Agents; start launches a stopped Host." /></div></div>
  if (showApp && !url) return <div className="grid size-full place-items-center px-6 text-center"><div className="max-w-sm space-y-3"><p className="text-sm font-medium">App is not running</p><p className="text-xs text-muted-foreground">Start this App from its workload menu to open its interface.</p><Button variant="outline" onClick={() => onLogs(true)}>Show logs</Button>{mobile && <Button variant="ghost" onClick={onClose}>Back to workloads</Button>}</div></div>
  if (url) return <div className={cn("size-full min-h-0 min-w-0 overflow-hidden", mobile && "grid grid-rows-[44px_minmax(0,1fr)]")}>
    {mobile && <div className="flex min-w-0 items-center justify-between gap-3 border-b px-3.5"><span title={path} className="truncate text-xs font-semibold">{path}</span><div className="flex shrink-0 items-center gap-1">{appToggle}<IconButton label="Upload file" onClick={onUpload}><Upload /></IconButton>{closeButton}</div></div>}
    <iframe ref={frame} key={interfaceFrameKey(agent.agent_id, agentInterface?.instance_id ?? app?.hostname ?? "", agentInterface?.registered_at ?? String(agent.run_generation ?? ""), tab.revision)} src={url} title={`${path || "Agent"} interface`} className="block size-full min-h-0 border-0 bg-background" style={{ colorScheme: theme }} sandbox="allow-scripts allow-forms allow-same-origin allow-modals allow-downloads" onLoad={() => postEmbedTheme(frame.current, theme, readEmbedAppearance())} />
  </div>
  return <div className={cn("flex size-full min-h-0 justify-center overflow-hidden px-3 pb-4 pt-4 sm:px-8 sm:pb-7 sm:pt-6 lg:px-16", mobile && "!p-0")}>
    <div className={cn("grid h-full min-h-0 w-full max-w-[1120px] grid-rows-[42px_minmax(0,1fr)] overflow-hidden rounded-md border border-zinc-800 bg-[#0f1215] shadow-[0_8px_28px_rgba(15,18,21,.14)]", mobile && "max-w-none grid-rows-[44px_minmax(0,1fr)_auto] rounded-none border-0 shadow-none")}>
      <div className="flex min-w-0 items-center justify-between gap-3 border-b border-zinc-800 bg-[#191d20] px-3.5"><div className="flex min-w-0 items-baseline gap-2"><span title={path} className="truncate text-xs font-semibold text-zinc-200">{path || "Terminal"}</span><span className="hidden truncate font-mono text-[9px] text-zinc-500 sm:block">{agent.agent_id} · {machineName(machine)}</span></div><div className="flex shrink-0 items-center gap-2"><span className="inline-flex items-center gap-1.5 text-[9px] uppercase text-zinc-500">{status}</span>{mobile && <>{appToggle}<IconButton label="Upload file" className="text-zinc-400 hover:bg-white/10 hover:text-zinc-100" onClick={onUpload}><Upload /></IconButton>{closeButton}</>}</div></div>
      <div className="min-h-0 min-w-0 overflow-hidden"><TerminalPane ref={terminal} key={tab.revision} workspaceId={workspaceId} agentId={agent.agent_id} active={activeStatuses.has(agent.status)} visible={visible} onStatusChange={setStatus} transformInput={visible ? transformInput : undefined} /></div>
      {mobile && <MobileTerminalKeys terminalRef={terminal} ctrlArmed={ctrlArmed} onToggleCtrl={onToggleCtrl} />}
    </div>
  </div>
})
