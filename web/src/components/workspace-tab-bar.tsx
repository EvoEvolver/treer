import { useEffect, useRef, useState, type KeyboardEvent as ReactKeyboardEvent, type PointerEvent, type Ref } from "react"
import { ChevronDown, MoreHorizontal, PanelLeft, PanelLeftOpen, LayoutDashboard, PanelsTopLeft, RotateCw, TerminalSquare, X } from "lucide-react"
import type { Agent } from "@/lib/api"
import { workloadKind } from "@/lib/workload-path"
import { WorkloadIcon } from "@/components/workload-icon"
import { OVERVIEW_TAB, type WorkspaceTab } from "@/components/use-workspace-tabs"
import { Button } from "@/components/ui/button"
import { Dialog, DialogContent, DialogDescription, DialogTitle } from "@/components/ui/dialog"
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger, DropdownMenuLabel, DropdownMenuSeparator } from "@/components/ui/dropdown-menu"
import { cn } from "@/lib/utils"
import { statusLabel, t } from "@/i18n"
import { sidebarShortcutKeys, sidebarShortcutLabel } from "@/lib/shortcut"

export type TabAction = "newtab" | "logs" | "rename" | "upload" | "reload" | "fullscreen" | "back"
export type TabActions = { app: boolean; logs: boolean; online: boolean; active: boolean; ui: boolean }
export type TabLabel = { agent: Agent; path: string; machine: string; ui: boolean }
export function WorkspaceTabBar({ tabs, labels, activeId, limit, liveCount, mobile, onOpen, onClose, actions, onAction, onCloseMany, onBrowseTree, sidebarCollapsed = false, expandRef, onExpandSidebar }: {
  tabs: WorkspaceTab[]; labels: Map<string, TabLabel>; activeId: string | null; limit: number; liveCount: number; mobile: boolean
  onOpen: (id: string) => void; onClose: (id: string) => void; onBrowseTree: () => void
  actions: Map<string, TabActions>; onAction: (id: string, action: TabAction) => void; onCloseMany: (ids: string[]) => void
  sidebarCollapsed?: boolean; expandRef?: Ref<HTMLButtonElement>; onExpandSidebar?: () => void
}) {
  const [switcher, setSwitcher] = useState(false)
  const [menu, setMenu] = useState<{ id: string; x: number; y: number } | null>(null)
  const mobileTitle = useRef<HTMLButtonElement>(null)
  const switcherContent = useRef<HTMLDivElement>(null)
  const returnFocus = useRef<HTMLElement | null>(null)
  const actionFrame = useRef<number | null>(null)
  const hold = useRef<ReturnType<typeof setTimeout> | null>(null)
  const touch = useRef<{ x: number; y: number } | null>(null)
  const suppressClick = useRef(0)
  const clearHold = () => { if (hold.current) clearTimeout(hold.current); hold.current = null; touch.current = null }
  useEffect(() => () => { clearHold(); if (actionFrame.current !== null) cancelAnimationFrame(actionFrame.current) }, [])
  const openMenu = (id: string, origin: HTMLElement, x?: number, y?: number) => {
    clearHold()
    const rect = origin.getBoundingClientRect()
    returnFocus.current = origin
    setMenu({ id, x: x ?? rect.left + 12, y: y ?? rect.bottom })
  }
  const menuEvents = (id: string) => ({
    onContextMenu: (event: React.MouseEvent<HTMLElement>) => { event.preventDefault(); openMenu(id, event.currentTarget, event.clientX, event.clientY) },
    onPointerDown: (event: PointerEvent<HTMLElement>) => {
      if (event.pointerType === "mouse" || event.button !== 0) return
      clearHold()
      const origin = event.currentTarget, x = event.clientX, y = event.clientY
      touch.current = { x, y }
      hold.current = setTimeout(() => { suppressClick.current = Date.now() + 800; openMenu(id, origin, x, y) }, 550)
    },
    onPointerMove: (event: PointerEvent<HTMLElement>) => { if (touch.current && Math.hypot(event.clientX - touch.current.x, event.clientY - touch.current.y) > 10) clearHold() },
    onPointerUp: clearHold, onPointerCancel: clearHold,
    onKeyDown: (event: ReactKeyboardEvent<HTMLElement>) => { if (event.key === "ContextMenu" || (event.shiftKey && event.key === "F10")) { event.preventDefault(); openMenu(id, event.currentTarget) } },
  })
  const clicked = (id: string) => { if (Date.now() >= suppressClick.current) select(id) }
  const list = useRef<HTMLDivElement>(null)
  const ids = [OVERVIEW_TAB, ...tabs.map((tab) => tab.id)]
  const title = (id: string) => id === OVERVIEW_TAB ? t("app.overview") : labels.get(id)?.agent.name ?? id
  const path = (id: string) => id === OVERVIEW_TAB ? t("app.overview") : labels.get(id)?.path ?? id
  const live = (id: string) => id === OVERVIEW_TAB || Boolean(tabs.find((tab) => tab.id === id)?.live)
  const select = (id: string) => { onOpen(id); setSwitcher(false) }
  useEffect(() => { list.current?.querySelector('[aria-selected="true"]')?.scrollIntoView({ block: "nearest", inline: "nearest" }) }, [activeId])
  useEffect(() => {
    const shortcut = (event: KeyboardEvent) => {
      if (event.defaultPrevented || event.altKey || event.isComposing || !(event.ctrlKey || event.metaKey)) return
      if (document.querySelector('[role="dialog"], [role="menu"]')) return
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
  const runAction = (id: string, action: TabAction) => {
    // Let the menu/switcher FocusScope unmount before opening a dialog. Otherwise
    // its close autofocus can steal focus from (and dismiss) the new dialog.
    setMenu(null)
    // window.open needs the user gesture, so it runs now rather than after the animation frame.
    if (action === "newtab") { setSwitcher(false); onAction(id, action); return }
    if (["rename", "upload", "fullscreen", "back"].includes(action)) setSwitcher(false)
    actionFrame.current = requestAnimationFrame(() => { actionFrame.current = null; onAction(id, action) })
  }
  const menuOptions = menu && actions.get(menu.id)
  const contextMenu = <DropdownMenu modal={false} open={Boolean(menu)} onOpenChange={(open) => !open && setMenu(null)}><DropdownMenuTrigger asChild><button tabIndex={-1} aria-hidden="true" className="pointer-events-none fixed size-px opacity-0" style={{ left: menu?.x ?? 0, top: menu?.y ?? 0 }} /></DropdownMenuTrigger><DropdownMenuContent align="start" className={cn("max-h-[80dvh] w-64 overflow-y-auto", mobile && "[&_[role=menuitem]]:min-h-11")} aria-label={t("tabs.actions")} onCloseAutoFocus={(event) => { event.preventDefault(); if (returnFocus.current?.isConnected) returnFocus.current.focus(); else (switcherContent.current?.querySelector<HTMLButtonElement>("button") ?? list.current?.querySelector<HTMLButtonElement>('[tabindex="0"]') ?? mobileTitle.current)?.focus() }}>
    <DropdownMenuLabel className="max-w-64 truncate" title={menu ? path(menu.id) : ""}>{menu ? path(menu.id) : ""}</DropdownMenuLabel>
    {menuOptions && menu && <>
      {menuOptions.app && <DropdownMenuItem onSelect={() => runAction(menu.id, "logs")}>{menuOptions.logs ? t("surface.show_app") : t("surface.show_logs")}</DropdownMenuItem>}
      <DropdownMenuItem onSelect={() => runAction(menu.id, "rename")}>{menuOptions.app ? t("tabs.rename_app") : t("app.rename_agent")}</DropdownMenuItem>
      <DropdownMenuItem disabled={!menuOptions.online} onSelect={() => runAction(menu.id, "upload")}>{t("surface.upload_file")}</DropdownMenuItem>
      <DropdownMenuItem onSelect={() => runAction(menu.id, "newtab")}>{t("app.open_in_new_tab")}</DropdownMenuItem>
      <DropdownMenuItem onSelect={() => runAction(menu.id, "reload")}>{menuOptions.ui ? t("app.reload_interface") : t("app.reconnect_terminal")}</DropdownMenuItem>
      {mobile && <><DropdownMenuItem onSelect={() => runAction(menu.id, "fullscreen")}>{menuOptions.ui ? t("app.open_full_screen_interface") : t("app.open_full_screen_terminal")}</DropdownMenuItem><DropdownMenuItem onSelect={() => runAction(menu.id, "back")}>{t("surface.back_to_workloads")}</DropdownMenuItem></>}
    </>}
    <DropdownMenuSeparator />
    <DropdownMenuItem disabled={menu?.id === OVERVIEW_TAB} onSelect={() => menu && onClose(menu.id)}>{t("tabs.close_page")}</DropdownMenuItem>
    <DropdownMenuItem disabled={!tabs.some((tab) => tab.id !== menu?.id)} onSelect={() => menu && onCloseMany(tabs.filter((tab) => tab.id !== menu.id).map((tab) => tab.id))}>{t("tabs.close_others")}</DropdownMenuItem>
    <DropdownMenuItem disabled={!menu || ids.indexOf(menu.id) === ids.length - 1} onSelect={() => menu && onCloseMany(ids.slice(ids.indexOf(menu.id) + 1))}>{t("tabs.close_right")}</DropdownMenuItem>
  </DropdownMenuContent></DropdownMenu>
  const moreButton = (id: string, phone = false) => <button type="button" aria-label={t("tabs.more_actions", { path: path(id) })} aria-haspopup="menu" aria-expanded={menu?.id === id} onClick={(event) => openMenu(id, event.currentTarget)} className={cn("grid shrink-0 place-items-center rounded text-muted-foreground outline-none hover:bg-accent hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring", phone ? "h-11 w-11" : "size-6")}><MoreHorizontal aria-hidden="true" className="size-3.5" /></button>
  const hint = t("tabs.hint", { live: liveCount, limit })
  return <>
    {mobile ? <div className="box-content flex h-11 shrink-0 items-center gap-2 border-b bg-sidebar px-2"><Button variant="ghost" size="icon" className="size-11 shrink-0" aria-label={t("app.browse_tree")} title={t("app.browse_tree")} aria-controls="tree-sidebar" onClick={onBrowseTree}><PanelLeft aria-hidden="true" className="size-4" /></Button><button ref={mobileTitle} type="button" {...menuEvents(activeId ?? OVERVIEW_TAB)} onClick={() => { if (Date.now() >= suppressClick.current) setSwitcher(true) }} aria-label={t("tabs.active_actions", { path: activeId ? path(activeId) : t("tabs.console") })} title={activeId ? path(activeId) : t("tabs.console")} className="flex min-w-0 flex-1 touch-pan-y select-none items-center gap-2 self-stretch rounded-sm text-left text-xs font-medium outline-none focus-visible:ring-2 focus-visible:ring-ring">{activeId && stateDot(activeId)}<span className="truncate">{activeId ? title(activeId) : t("tabs.console")}</span>{activeId && activeId !== OVERVIEW_TAB && <span className="shrink-0 text-[10px] text-muted-foreground">{statusLabel(labels.get(activeId)?.agent.status ?? "")}</span>}</button>{activeId && moreButton(activeId, true)}<button type="button" aria-label={t("tabs.show_all", { count: ids.length })} onClick={() => setSwitcher(true)} className="grid size-11 shrink-0 place-items-center rounded-md border text-xs font-semibold outline-none hover:bg-accent focus-visible:ring-2 focus-visible:ring-ring">{ids.length}</button></div> : <div className="flex h-10 shrink-0 border-b bg-sidebar">
      {sidebarCollapsed && onExpandSidebar && <button ref={expandRef} type="button" aria-label={t("resizer.expand_tree_sidebar")} title={`${t("resizer.expand_tree_sidebar")} (${sidebarShortcutLabel()})`} aria-keyshortcuts={sidebarShortcutKeys()} aria-expanded="false" aria-controls="tree-sidebar" onClick={onExpandSidebar} className="grid size-10 shrink-0 place-items-center border-r text-muted-foreground outline-none hover:bg-accent hover:text-foreground focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring"><PanelLeftOpen aria-hidden="true" className="size-4" /></button>}
      <div ref={list} role="tablist" aria-label={t("tabs.open_pages")} className="flex min-w-0 flex-1 overflow-x-auto [scrollbar-width:none]">{ids.map((id) => <div key={id} className={cn("relative flex shrink-0 items-center border-r", id === OVERVIEW_TAB && "sticky left-0 z-10 bg-sidebar", id === activeId && "bg-background before:absolute before:inset-x-0 before:top-0 before:h-0.5 before:bg-primary")}>
        <button type="button" role="tab" id={`workspace-tab-${id}`} aria-controls={`workspace-panel-${id}`} aria-selected={id === activeId} tabIndex={id === (activeId ?? OVERVIEW_TAB) ? 0 : -1} aria-label={t("tabs.tab", { path: path(id) })} title={live(id) ? path(id) : t("tabs.unloaded_suffix", { path: path(id) })} {...menuEvents(id)} onClick={() => clicked(id)} onKeyDown={(event) => {
          menuEvents(id).onKeyDown(event)
          if (event.defaultPrevented) return
          let target: string | undefined
          const index = ids.indexOf(id)
          if (event.key === "ArrowRight") target = ids[(index + 1) % ids.length]
          if (event.key === "ArrowLeft") target = ids[(index + ids.length - 1) % ids.length]
          if (event.key === "Home") target = ids[0]
          if (event.key === "End") target = ids.at(-1)
          if (target) { event.preventDefault(); onOpen(target); requestAnimationFrame(() => document.getElementById(`workspace-tab-${target}`)?.focus()) }
          if (event.key === "Delete" && id !== OVERVIEW_TAB) { event.preventDefault(); onClose(id); requestAnimationFrame(() => list.current?.querySelector<HTMLButtonElement>('[tabindex="0"]')?.focus()) }
        }} className={cn("flex h-full min-w-0 items-center gap-2 px-3 text-xs outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring", id !== OVERVIEW_TAB && "w-[176px] max-w-[220px]", id === activeId ? "text-foreground" : "text-muted-foreground")}>
          {stateDot(id)}{icon(id)}<span className={cn("truncate", !live(id) && "italic opacity-70")}>{title(id)}</span>{id !== OVERVIEW_TAB && <span className={cn("text-[10px]", labels.get(id)?.agent.status === "blocked" || labels.get(id)?.agent.status === "failed" ? "text-destructive" : "sr-only")}>{statusLabel(labels.get(id)?.agent.status ?? "")}</span>}{!live(id) && <RotateCw aria-hidden="true" className="size-3 shrink-0" />}
        </button>{id === activeId && moreButton(id)}{id !== OVERVIEW_TAB && <button type="button" tabIndex={-1} aria-label={t("tabs.close", { path: path(id) })} onClick={() => onClose(id)} className="mr-1 grid size-7 shrink-0 place-items-center rounded text-muted-foreground hover:bg-accent hover:text-foreground"><X className="size-3" /></button>}
      </div>)}</div>
      <div className="flex shrink-0 items-center gap-2 px-2"><DropdownMenu><DropdownMenuTrigger asChild><Button variant="ghost" size="icon" className="size-8" aria-label={t("tabs.all_tabs")}><ChevronDown /></Button></DropdownMenuTrigger><DropdownMenuContent align="end" className="max-h-[70dvh] w-72 overflow-y-auto">{ids.map((id) => <DropdownMenuItem key={id} onSelect={() => select(id)} className="flex min-w-0 gap-2">{icon(id)}<span className="min-w-0 flex-1 truncate" title={path(id)}>{path(id)}</span>{!live(id) && <RotateCw aria-label={t("tabs.reconnects")} className="size-3 text-muted-foreground" />}</DropdownMenuItem>)}<DropdownMenuSeparator /><p className="px-2 py-1.5 text-[11px] leading-4 text-muted-foreground">{hint}</p></DropdownMenuContent></DropdownMenu></div>
    </div>}
    {(!mobile || !switcher) && contextMenu}
    <Dialog open={mobile && switcher} onOpenChange={setSwitcher}><DialogContent ref={switcherContent} className="flex h-[100dvh] w-full max-w-none flex-col gap-0 rounded-none p-0 [&>button]:hidden"><div className="flex shrink-0 items-center gap-3 border-b px-4 py-3 pt-[max(0.75rem,env(safe-area-inset-top))]"><DialogTitle className="flex-1 text-sm">{t("tabs.open_tabs", { count: ids.length })}</DialogTitle><Button variant="ghost" size="sm" onClick={() => setSwitcher(false)}>{t("tabs.done")}</Button></div><DialogDescription className="sr-only">{t("tabs.switch_or_close_a_page")}</DialogDescription><div className="grid min-h-0 grid-cols-2 content-start gap-3 overflow-y-auto p-4">{ids.map((id) => <div key={id} className={cn("relative min-w-0 rounded-lg border bg-card", id === activeId && "border-primary ring-1 ring-primary")}>
      <button type="button" aria-label={t("tabs.open", { path: path(id) })} {...menuEvents(id)} onClick={() => clicked(id)} className="flex w-full min-w-0 touch-pan-y select-none flex-col gap-3 rounded-lg p-3 text-left outline-none focus-visible:ring-2 focus-visible:ring-ring"><span className="flex w-full min-w-0 items-center gap-2 pr-5">{stateDot(id)}<span className="truncate text-xs font-medium" title={path(id)}>{title(id)}</span></span><span aria-hidden="true" className={cn("flex h-20 w-full items-center justify-center rounded-md bg-muted/40 text-muted-foreground", !live(id) && "opacity-50")}>{id === OVERVIEW_TAB ? <LayoutDashboard className="size-6" /> : <WorkloadIcon kind={labels.get(id)?.agent.kind ?? "command"} className="size-6" />}</span><span className="w-full truncate text-[10px] text-muted-foreground">{id === OVERVIEW_TAB ? t("tabs.pinned_always_live") : `${workloadKind(labels.get(id)?.agent ?? { kind: "command" })} · ${labels.get(id)?.machine ?? ""}`}</span><span className="text-[10px] text-muted-foreground">{live(id) ? statusLabel(labels.get(id)?.agent.status ?? "") || t("app.overview") : t("tabs.unloaded_select")}</span></button>
      {id !== OVERVIEW_TAB && <button type="button" aria-label={t("tabs.close", { path: path(id) })} onClick={() => onClose(id)} className="absolute right-0 top-0 grid size-10 place-items-center rounded-md text-muted-foreground hover:bg-accent"><X className="size-3.5" /></button>}
    </div>)}</div><p className="shrink-0 border-t p-4 pb-[max(1rem,env(safe-area-inset-bottom))] text-[10px] leading-4 text-muted-foreground">{hint}</p>{mobile && switcher && contextMenu}</DialogContent></Dialog>
  </>
}
