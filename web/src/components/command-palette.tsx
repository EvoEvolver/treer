import { useEffect, useMemo, useRef, useState, type ReactNode } from "react"
import { Search } from "lucide-react"
import { Dialog, DialogContent, DialogDescription, DialogTitle } from "@/components/ui/dialog"
import { cn } from "@/lib/utils"
import { t } from "@/i18n"

export type PaletteGroup = "workloads" | "machines" | "people" | "workspaces" | "settings"
export type PaletteItem = { id: string; group: PaletteGroup; label: string; detail?: string; keywords?: string; icon?: ReactNode; run: () => void }

const ORDER: PaletteGroup[] = ["workloads", "machines", "people", "workspaces", "settings"]
const TITLES: Record<PaletteGroup, "palette.workloads" | "palette.machines" | "palette.people" | "palette.workspaces" | "palette.settings"> = {
  workloads: "palette.workloads", machines: "palette.machines", people: "palette.people", workspaces: "palette.workspaces", settings: "palette.settings",
}
const LIMIT = 8

// One search for everything the Console can open: workloads, machines, people,
// workspaces and settings pages. Opened with Cmd/Ctrl+K or the top bar.
export function CommandPalette({ open, onOpenChange, items }: { open: boolean; onOpenChange: (open: boolean) => void; items: PaletteItem[] }) {
  const [query, setQuery] = useState("")
  const [active, setActive] = useState(0)
  const list = useRef<HTMLUListElement>(null)
  useEffect(() => { if (open) { setQuery(""); setActive(0) } }, [open])
  const results = useMemo(() => {
    const q = query.trim().toLocaleLowerCase()
    const matched = items.filter((item) => !q || `${item.label} ${item.detail ?? ""} ${item.keywords ?? ""}`.toLocaleLowerCase().includes(q))
    return ORDER.flatMap((group) => matched.filter((item) => item.group === group).slice(0, q ? LIMIT : group === "settings" ? 4 : LIMIT))
  }, [items, query])
  useEffect(() => { setActive(0) }, [query])
  useEffect(() => { list.current?.querySelector<HTMLElement>(`[data-index="${active}"]`)?.scrollIntoView({ block: "nearest" }) }, [active])
  function run(item: PaletteItem | undefined) {
    if (!item) return
    onOpenChange(false)
    item.run()
  }
  let index = -1
  return <Dialog open={open} onOpenChange={onOpenChange}>
    <DialogContent className="top-[12vh] max-w-xl translate-y-0 gap-0 overflow-hidden p-0 max-sm:top-0 max-sm:h-dvh max-sm:max-w-none max-sm:rounded-none [&>button]:hidden">
      <DialogTitle className="sr-only">{t("palette.title")}</DialogTitle>
      <DialogDescription className="sr-only">{t("palette.description")}</DialogDescription>
      <div className="flex items-center gap-2 border-b px-3">
        <Search aria-hidden="true" className="size-4 shrink-0 text-muted-foreground" />
        <input autoFocus role="combobox" aria-expanded="true" aria-controls="command-palette-results" aria-activedescendant={results[active] ? `palette-item-${active}` : undefined} aria-label={t("palette.title")} placeholder={t("palette.placeholder")} value={query} onChange={(event) => setQuery(event.target.value)}
          onKeyDown={(event) => {
            if (event.key === "ArrowDown") { event.preventDefault(); setActive((value) => Math.min(value + 1, results.length - 1)) }
            if (event.key === "ArrowUp") { event.preventDefault(); setActive((value) => Math.max(value - 1, 0)) }
            if (event.key === "Enter") { event.preventDefault(); run(results[active]) }
          }}
          className="h-12 min-w-0 flex-1 bg-transparent text-sm outline-none placeholder:text-muted-foreground max-sm:text-base" />
        <button type="button" onClick={() => onOpenChange(false)} className="rounded border px-1.5 py-0.5 text-[10px] text-muted-foreground max-sm:min-h-11 max-sm:px-3 max-sm:text-xs">{t("palette.close")}</button>
      </div>
      <ul ref={list} id="command-palette-results" role="listbox" aria-label={t("palette.results")} className="max-h-[60vh] overflow-y-auto p-2 max-sm:max-h-none max-sm:flex-1">
        {ORDER.map((group) => {
          const entries = results.filter((item) => item.group === group)
          if (!entries.length) return null
          return <li key={group} role="presentation">
            <p className="px-2 pb-1 pt-2 text-[11px] font-semibold uppercase tracking-wider text-muted-foreground">{t(TITLES[group])}</p>
            <ul role="group" aria-label={t(TITLES[group])}>{entries.map((item) => {
              index += 1
              const position = index
              return <li key={`${group}:${item.id}`} id={`palette-item-${position}`} data-index={position} role="option" aria-selected={position === active} onMouseMove={() => setActive(position)} onClick={() => run(item)}
                className={cn("flex cursor-pointer items-center gap-3 rounded-md px-2 py-2 text-sm max-sm:min-h-11", position === active && "bg-accent")}>
                {item.icon && <span aria-hidden="true" className="grid size-4 shrink-0 place-items-center text-muted-foreground [&_svg]:size-4">{item.icon}</span>}
                <span className="min-w-0 flex-1 truncate">{item.label}</span>
                {item.detail && <span className="max-w-[45%] shrink-0 truncate text-xs text-muted-foreground">{item.detail}</span>}
              </li>
            })}</ul>
          </li>
        })}
        {!results.length && <li className="px-2 py-8 text-center text-sm text-muted-foreground">{t("palette.empty", { query })}</li>}
      </ul>
    </DialogContent>
  </Dialog>
}

// Terminals and editors keep Ctrl+K (readline kill-line).
export function usePaletteShortcut(onOpen: () => void, enabled: boolean) {
  const handler = useRef(onOpen)
  handler.current = onOpen
  useEffect(() => {
    if (!enabled) return
    function onKey(event: KeyboardEvent) {
      if (!(event.metaKey || event.ctrlKey) || event.altKey || event.shiftKey || event.key.toLowerCase() !== "k") return
      const target = event.target
      if (target instanceof Element && target.closest(".xterm, textarea, [contenteditable]:not([contenteditable=\"false\"])")) return
      event.preventDefault()
      handler.current()
    }
    window.addEventListener("keydown", onKey)
    return () => window.removeEventListener("keydown", onKey)
  }, [enabled])
}
