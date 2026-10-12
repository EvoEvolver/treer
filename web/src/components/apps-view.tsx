import { EmptyState, Field, IconButton, Status } from "@/components/control-plane-ui"
import { Button } from "@/components/ui/button"
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select"
import { machineName, type AppDeployment, type Machine } from "@/lib/api"
import { formatCommandLine } from "@/lib/command-line"
import {
  Copy,
  ExternalLink,
  PanelsTopLeft,
  Play,
  RotateCw,
  Settings as SettingsIcon,
  Square,
  Trash2
} from "lucide-react"

export function AppsView({ apps, machines, loading, onOpen, onSettings }: { apps: AppDeployment[]; machines: Machine[]; loading: boolean; onOpen: (app: AppDeployment) => void; onSettings: (app: AppDeployment) => void }) {
  return <div className="min-h-0 overflow-auto"><div className="mx-auto w-full max-w-[1120px] px-5 py-8 sm:px-8 sm:py-12 lg:px-14">
    <div className="mb-8 flex items-end justify-between gap-4"><div><div className="mb-2 grid size-9 place-items-center rounded-md bg-emerald-100 text-emerald-800"><PanelsTopLeft className="size-4" /></div><h1 className="text-2xl font-semibold">Apps</h1></div><span className="text-xs text-muted-foreground">{apps.length} deployments</span></div>
    <div className="border-y">
      <div className="hidden h-9 grid-cols-[minmax(150px,1fr)_minmax(220px,1.3fr)_minmax(150px,1fr)_auto] items-center gap-4 border-b text-[10px] font-medium uppercase text-muted-foreground sm:grid"><span>App</span><span>URL</span><span>Machine</span><span className="w-20" /></div>
      {apps.map((app) => { const machine = machines.find((item) => item.server_id === app.server_id); const interfaceBase = app.public_url?.replace(/\/$/, "") ?? app.hostname; return <div key={app.app_id} className="grid min-h-[72px] grid-cols-[minmax(0,1fr)_auto] items-center gap-x-3 gap-y-1 border-b py-3 last:border-b-0 sm:grid-cols-[minmax(150px,1fr)_minmax(220px,1.3fr)_minmax(150px,1fr)_auto] sm:gap-4">
        <button type="button" aria-label={`Open ${app.name} settings`} onClick={() => onSettings(app)} className="col-start-1 row-start-1 min-w-0 text-left sm:col-start-auto sm:row-start-auto"><span className="flex items-center gap-2"><span className="truncate text-xs font-medium hover:underline">{app.name}</span><Status value={app.status} /></span><span className="mt-1 block truncate font-mono text-[9px] text-muted-foreground" title={formatCommandLine(app.command, app.args)}>{formatCommandLine(app.command, app.args)}</span>{app.last_error && <span className="mt-1 block truncate text-[9px] text-red-600" title={app.last_error}>{app.last_error}</span>}</button>
        <button type="button" className="col-start-1 row-start-2 min-w-0 truncate text-left font-mono text-[10px] hover:underline sm:col-start-auto sm:row-start-auto" aria-label={`Open ${app.name} interface`} onClick={() => onOpen(app)}>{interfaceBase}/</button>
        <span className="col-start-1 row-start-3 min-w-0 truncate text-[10px] text-muted-foreground sm:col-start-auto sm:row-start-auto">{machineName(machine, app.server_id)} · restarts {app.restart_count}</span>
        <span className="col-start-2 row-span-3 row-start-1 flex items-center justify-end gap-1 sm:col-start-auto sm:row-span-1 sm:row-start-auto"><IconButton label={`Open ${app.name}`} onClick={() => onOpen(app)} disabled={app.status !== "running"}><ExternalLink /></IconButton><IconButton label={`Open ${app.name} settings`} onClick={() => onSettings(app)}><SettingsIcon /></IconButton></span>
      </div>})}
      {!apps.length && <EmptyState icon={<PanelsTopLeft />} label={loading ? "Loading Apps" : "No Apps yet"} />}
    </div>
  </div></div>
}

export function AppSettingsView({ app, machine, onOpen, onAccess, onAction, onDelete, onClose, onCopy }: { app?: AppDeployment; machine?: Machine; onOpen: (app: AppDeployment) => void; onAccess: (app: AppDeployment, access: "public" | "workspace") => void; onAction: (app: AppDeployment, action: "start" | "stop" | "restart") => void; onDelete: (app: AppDeployment) => void; onClose: () => void; onCopy: (value: string) => void }) {
  if (!app) return <div className="grid min-h-0 place-items-center p-8 text-sm text-muted-foreground">App not found.</div>
  const machineIsOnline = machine?.status === "online"
  const commandLine = formatCommandLine(app.command, app.args)
  return <div className="min-h-0 overflow-auto"><div className="mx-auto w-full max-w-[1120px] px-5 py-8 sm:px-8 sm:py-12 lg:px-14">
    <div className="mb-10 flex items-start justify-between gap-4">
      <div className="min-w-0"><div className="mb-3 flex items-center gap-3"><span className="grid size-9 shrink-0 place-items-center rounded-md bg-emerald-100 text-emerald-800"><SettingsIcon className="size-4" /></span><h1 className="truncate text-2xl font-semibold">{app.name}</h1><Status value={app.status} /></div><p className="font-mono text-xs text-muted-foreground">{app.app_id}</p></div>
      <Button variant="outline" className="hidden shrink-0 sm:inline-flex" onClick={onClose}>Close</Button>
    </div>

    <section className="border-y py-6"><div className="grid gap-5 md:grid-cols-[220px_minmax(0,1fr)]"><div><h2 className="text-sm font-semibold">Access</h2><p className="mt-1 text-xs leading-5 text-muted-foreground">Treer authentication for the App's dedicated public URL.</p></div><div className="space-y-3"><Field label="Who can access"><Select value={app.access ?? "workspace"} onValueChange={(access: "public" | "workspace") => onAccess(app, access)}><SelectTrigger aria-label="App access"><SelectValue /></SelectTrigger><SelectContent><SelectItem value="workspace">Workspace session required</SelectItem><SelectItem value="public">Public · no authentication</SelectItem></SelectContent></Select></Field>{app.public_url ? <div className="flex min-w-0 items-center gap-1"><button type="button" className="min-w-0 truncate font-mono text-xs hover:underline" onClick={() => onOpen(app)}>{app.public_url}</button><IconButton label="Copy App URL" onClick={() => onCopy(app.public_url!)}><Copy /></IconButton><IconButton label={`Open ${app.name}`} onClick={() => onOpen(app)} disabled={app.status !== "running"}><ExternalLink /></IconButton></div> : <p className="text-xs text-muted-foreground">No wildcard App origin is configured.</p>}{app.access === "public" && <p className="text-xs text-destructive">Anyone with this URL can access the App without a Treer session.</p>}</div></div></section>

    <section className="border-b py-6"><div className="grid gap-5 md:grid-cols-[220px_minmax(0,1fr)]"><div><h2 className="text-sm font-semibold">Runtime</h2><p className="mt-1 text-xs leading-5 text-muted-foreground">Managed process and endpoint details.</p></div><div className="space-y-4"><dl className="grid gap-x-4 gap-y-3 text-xs sm:grid-cols-[130px_minmax(0,1fr)]"><dt className="text-muted-foreground">Machine</dt><dd>{machineName(machine, app.server_id)}</dd><dt className="text-muted-foreground">Command</dt><dd className="break-all font-mono">{commandLine}</dd><dt className="text-muted-foreground">Working directory</dt><dd className="break-all font-mono">{app.cwd || "."}</dd><dt className="text-muted-foreground">Service</dt><dd className="break-all font-mono">{app.service_id} · port {app.port}</dd><dt className="text-muted-foreground">Restarts</dt><dd>{app.restart_count}</dd></dl>{app.last_error && <p className="text-xs text-destructive">{app.last_error}</p>}<div className="flex flex-wrap gap-2">{app.desired_state === "stopped" ? <Button size="sm" onClick={() => onAction(app, "start")} disabled={!machineIsOnline}><Play />Start</Button> : <Button size="sm" variant="outline" onClick={() => onAction(app, "stop")} disabled={!machineIsOnline}><Square />Stop</Button>}<Button size="sm" variant="outline" onClick={() => onAction(app, "restart")} disabled={!machineIsOnline}><RotateCw />Restart</Button></div></div></div></section>

    <section className="border-b py-6"><div className="grid gap-5 md:grid-cols-[220px_minmax(0,1fr)]"><div><h2 className="text-sm font-semibold text-destructive">Danger zone</h2></div><div className="flex items-center justify-between gap-4 border border-destructive/30 p-4"><p className="text-xs text-muted-foreground">Stops the process and removes its service, hostname, and public origin.</p><Button variant="destructive" className="shrink-0" onClick={() => onDelete(app)}><Trash2 />Delete App</Button></div></div></section>
  </div></div>
}
