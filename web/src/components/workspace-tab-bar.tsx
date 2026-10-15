import { useEffect, useRef, useState } from "react"
import { ChevronDown, LayoutDashboard, PanelsTopLeft, RotateCw, TerminalSquare, X } from "lucide-react"
import type { Agent } from "@/lib/api"
import { workloadKind } from "@/lib/workload-path"
import { WorkloadIcon } from "@/components/workload-icon"
import { OVERVIEW_TAB, type WorkspaceTab } from "@/components/use-workspace-tabs"
import { Button } from "@/components/ui/button"
import { Dialog, DialogContent, DialogDescription, DialogTitle } from "@/components/ui/dialog"
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger } from "@/components/ui/dropdown-menu"
import { cn } from "@/lib/utils"
import { t } from "@/i18n"

export type TabLabel = { agent: Agent; path: string; machine: string; ui: boolean }
export function WorkspaceTabBar({ tabs, labels, activeId, limit, liveCount, mobile, onOpen, onClose }: {
  tabs: WorkspaceTab[]; labels: Map<string, TabLabel>; activeId: string | null; limit: number; liveCount: number; mobile: boolean
  onOpen: (id: string) => void; onClose: (id: string) => void
}) {
  const [switcher, setSwitcher] = useState(false)
  const list = useRef<HTMLDivElement>(null)
  const ids = [OVERVIEW_TAB, ...tabs.map((tab) => tab.id)]
  const title = (id: string) => id === OVERVIEW_TAB ? "Overview" : labels.get(id)?.agent.name ?? id
  const path = (id: string) => id === OVERVIEW_TAB ? "Overview" : labels.get(id)?.path ?? id
  const live = (id: string) => id === OVERVIEW_TAB || Boolean(tabs.find((tab) => tab.id === id)?.live)
  const select = (id: string) => { onOpen(id); setSwitcher(false) }
  useEffect(() => { list.current?.querySelector('[aria-selected="true"]')?.scrollIntoView({ block: "nearest", inline: "nearest" }) }, [activeId])
  useEffect(() => {
    const shortcut = (event: KeyboardEvent) => {
      if (event.defaultPrevented || event.altKey || event.isComposing || !(event.ctrlKey || event.metaKey)) return
      if (document.querySelector('[role="dialog"]')) return
      const target = event.target
      if (target instanceof Element && target.closest('input, textarea, select, [contenteditable]:not([contenteditable="false"]), .xterm')) return
      if (/^[1-9]$/.test(event.key) && ids[Number(event.key) - 1]) {
        event.preventDefault(); event.stopPropagation(); onOpen(ids[Number(event.key) - 1])
      } else if (event.key.toLowerCase() === "w" && activeId && activeId !== OVERVIEW_TAB) {
        event.preventDefault(); event.stopPropagation(); onClose(activeId)
      }
    }
    window.addEventListener("keydown", shortcut, true)
    return () => window.removeEventListener("keydown", shortcut, true)
  })
  const icon = (id: string) => id === OVERVIEW_TAB ? <LayoutDashboard aria-hidden="true" className="size-3.5 shrink-0" /> : labels.get(id)?.ui ? <PanelsTopLeft aria-hidden="true" className="size-3.5 shrink-0" /> : <TerminalSquare aria-hidden="true" className="size-3.5 shrink-0" />
  const stateDot = (id: string) => {
    const status = labels.get(id)?.agent.status
    return id !== OVERVIEW_TAB && <span aria-hidden="true" className={cn("size-1.5 shrink-0 rounded-full", status === "blocked" || status === "failed" ? "bg-destructive" : status === "working" || status === "starting" ? "bg-primary" : "bg-muted-foreground/60")} />
  }
  const hint = t("tabs.hint", { live: liveCount, limit })
  return <>
    {mobile ? <div className="flex h-11 shrink-0 items-center gap-3 border-b bg-sidebar px-3"><span title={activeId ? path(activeId) : t("tabs.console")} className="min-w-0 flex-1 truncate text-xs font-medium">{activeId ? title(activeId) : t("tabs.console")}</span><button type="button" aria-label={t("tabs.show_all", { count: ids.length })} onClick={() => setSwitcher(true)} className="grid size-8 shrink-0 place-items-center rounded-md border text-xs font-semibold outline-none hover:bg-accent focus-visible:ring-2 focus-visible:ring-ring">{ids.length}</button></div> : <div className="flex h-10 shrink-0 border-b bg-sidebar">
      <div ref={list} role="tablist" aria-label={t("tabs.open_pages")} className="flex min-w-0 flex-1 overflow-x-auto [scrollbar-width:none]">{ids.map((id) => <div key={id} className={cn("relative flex shrink-0 items-center border-r", id === OVERVIEW_TAB && "sticky left-0 z-10 bg-sidebar", id === activeId && "bg-background before:absolute before:inset-x-0 before:top-0 before:h-0.5 before:bg-primary")}>
        <button type="button" role="tab" id={`workspace-tab-${id}`} aria-controls={`workspace-panel-${id}`} aria-selected={id === activeId} tabIndex={id === (activeId ?? OVERVIEW_TAB) ? 0 : -1} aria-label={t("tabs.tab", { path: path(id) })} title={live(id) ? path(id) : t("tabs.unloaded_suffix", { path: path(id) })} onClick={() => select(id)} onKeyDown={(event) => {
          let target: string | undefined
          const index = ids.indexOf(id)
          if (event.key === "ArrowRight") target = ids[(index + 1) % ids.length]
          if (event.key === "ArrowLeft") target = ids[(index + ids.length - 1) % ids.length]
          if (event.key === "Home") target = ids[0]
          if (event.key === "End") target = ids.at(-1)
          if (target) { event.preventDefault(); onOpen(target); requestAnimationFrame(() => document.getElementById(`workspace-tab-${target}`)?.focus()) }
          if (event.key === "Delete" && id !== OVERVIEW_TAB) { event.preventDefault(); onClose(id); requestAnimationFrame(() => list.current?.querySelector<HTMLButtonElement>('[tabindex="0"]')?.focus()) }
        }} className={cn("flex h-full min-w-0 items-center gap-2 px-3 text-xs outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring", id !== OVERVIEW_TAB && "w-[150px] max-w-[190px]", id === activeId ? "text-foreground" : "text-muted-foreground")}>
          {stateDot(id)}{icon(id)}<span className={cn("truncate", !live(id) && "italic opacity-70")}>{title(id)}</span>{!live(id) && <RotateCw aria-hidden="true" className="size-3 shrink-0" />}
        </button>{id !== OVERVIEW_TAB && <button type="button" tabIndex={-1} aria-label={t("tabs.close", { path: path(id) })} onClick={() => onClose(id)} className="mr-1 grid size-7 shrink-0 place-items-center rounded text-muted-foreground hover:bg-accent hover:text-foreground"><X className="size-3" /></button>}
      </div>)}</div>
      <div className="flex shrink-0 items-center gap-2 px-2"><span title={hint} className="hidden rounded-full border px-2 py-1 text-[10px] text-muted-foreground lg:inline">{t("tabs.live", { live: liveCount, limit })}</span><DropdownMenu><DropdownMenuTrigger asChild><Button variant="ghost" size="icon" className="size-8" aria-label={t("tabs.all_tabs")}><ChevronDown /></Button></DropdownMenuTrigger><DropdownMenuContent align="end" className="max-h-[70dvh] w-72 overflow-y-auto">{ids.map((id) => <DropdownMenuItem key={id} onSelect={() => select(id)} className="flex min-w-0 gap-2">{icon(id)}<span className="min-w-0 flex-1 truncate" title={path(id)}>{path(id)}</span><span className="text-[10px] text-muted-foreground">{live(id) ? "live" : "unloaded"}</span></DropdownMenuItem>)}</DropdownMenuContent></DropdownMenu></div>
    </div>}
    <Dialog open={mobile && switcher} onOpenChange={setSwitcher}><DialogContent className="flex h-[100dvh] w-full max-w-none flex-col gap-0 rounded-none p-0 [&>button]:hidden"><div className="flex shrink-0 items-center gap-3 border-b px-4 py-3 pt-[max(0.75rem,env(safe-area-inset-top))]"><DialogTitle className="flex-1 text-sm">{t("tabs.open_tabs", { count: ids.length })}</DialogTitle><span className="text-[10px] text-muted-foreground">{t("tabs.live", { live: liveCount, limit })}</span><Button variant="ghost" size="sm" onClick={() => setSwitcher(false)}>{t("tabs.done")}</Button></div><DialogDescription className="sr-only">{t("tabs.switch_or_close_a_page")}</DialogDescription><div className="grid min-h-0 grid-cols-2 content-start gap-3 overflow-y-auto p-4">{ids.map((id) => <div key={id} className={cn("relative min-w-0 rounded-lg border bg-card", id === activeId && "border-primary ring-1 ring-primary")}>
      <button type="button" aria-label={t("tabs.open", { path: path(id) })} onClick={() => select(id)} className="flex w-full min-w-0 flex-col gap-3 rounded-lg p-3 text-left outline-none focus-visible:ring-2 focus-visible:ring-ring"><span className="flex w-full min-w-0 items-center gap-2 pr-5">{stateDot(id)}<span className="truncate text-xs font-medium" title={path(id)}>{title(id)}</span></span><span aria-hidden="true" className={cn("flex h-20 w-full items-center justify-center rounded-md bg-muted/40 text-muted-foreground", !live(id) && "opacity-50")}>{id === OVERVIEW_TAB ? <LayoutDashboard className="size-6" /> : <WorkloadIcon kind={labels.get(id)?.agent.kind ?? "command"} className="size-6" />}</span><span className="w-full truncate text-[10px] text-muted-foreground">{id === OVERVIEW_TAB ? t("tabs.pinned_always_live") : `${workloadKind(labels.get(id)?.agent ?? { kind: "command" })} · ${labels.get(id)?.machine ?? ""}`}</span><span className="text-[10px] text-muted-foreground">{live(id) ? labels.get(id)?.agent.status ?? "Overview" : "↻ Unloaded · select to reload"}</span></button>
      {id !== OVERVIEW_TAB && <button type="button" aria-label={t("tabs.close", { path: path(id) })} onClick={() => onClose(id)} className="absolute right-0 top-0 grid size-10 place-items-center rounded-md text-muted-foreground hover:bg-accent"><X className="size-3.5" /></button>}
    </div>)}</div><p className="shrink-0 border-t p-4 pb-[max(1rem,env(safe-area-inset-bottom))] text-[10px] leading-4 text-muted-foreground">{hint}</p></DialogContent></Dialog>
  </>
}
