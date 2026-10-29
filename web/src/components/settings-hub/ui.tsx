import { useEffect, useId, useMemo, useRef, useState, type ReactNode } from "react"
import { Code2, Lock, Search, TriangleAlert } from "lucide-react"
import { Button } from "@/components/ui/button"
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog"
import { Input } from "@/components/ui/input"
import { Label } from "@/components/ui/label"
import { cn } from "@/lib/utils"
import { intlLocale, t } from "@/i18n"

export function PageHeader({ title, description, actions }: { title: string; description?: ReactNode; actions?: ReactNode }) {
  return <div className="mb-6 flex flex-col gap-3 sm:flex-row sm:items-start sm:justify-between">
    <div className="min-w-0">
      <h1 className="truncate text-xl font-semibold">{title}</h1>
      {description && <p className="mt-1 max-w-prose text-sm text-muted-foreground">{description}</p>}
    </div>
    {actions && <div className="flex shrink-0 flex-wrap items-center gap-2">{actions}</div>}
  </div>
}

export function Section({ title, description, children, tone, id }: { title: string; description?: ReactNode; children: ReactNode; tone?: "danger"; id?: string }) {
  return <section id={id} aria-label={title} className={cn("mb-6 rounded-lg border p-4 sm:p-5", tone === "danger" && "border-destructive/40")}>
    <h2 className={cn("text-sm font-semibold", tone === "danger" && "text-destructive")}>{title}</h2>
    {description && <p className="mt-1 text-xs leading-5 text-muted-foreground">{description}</p>}
    <div className="mt-4">{children}</div>
  </section>
}

export type ApiCall = { method: "GET" | "POST" | "PUT" | "PATCH" | "DELETE"; path: string; note?: string }

// Agents operate the same API as this page. Shown on every settings page.
export function ApiEquivalent({ calls }: { calls: ApiCall[] }) {
  return <details className="group mt-8 rounded-md border bg-muted/30 text-xs" data-api-equivalent="">
    <summary className="flex cursor-pointer list-none items-center gap-2 px-3 py-2 font-medium text-muted-foreground outline-none hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring [&::-webkit-details-marker]:hidden max-md:min-h-11"><Code2 aria-hidden="true" className="size-3.5" />{t("hub.api_equivalent")}</summary>
    <div className="border-t px-3 py-2">
      <p className="mb-2 text-muted-foreground">{t("hub.api_equivalent_hint")}</p>
      <ul className="space-y-1 font-mono">{calls.map((call) => <li key={`${call.method} ${call.path}`} className="break-all"><span className="mr-2 inline-block w-14 font-semibold text-foreground">{call.method}</span>{call.path}{call.note && <span className="ml-2 font-sans text-muted-foreground">— {call.note}</span>}</li>)}</ul>
    </div>
  </details>
}

// A control the viewer cannot use says why instead of failing after a click.
export function PermissionNote({ children }: { children: ReactNode }) {
  return <p role="note" className="mb-4 flex items-start gap-2 rounded-md border border-dashed px-3 py-2 text-xs text-muted-foreground"><Lock aria-hidden="true" className="mt-0.5 size-3.5 shrink-0" /><span>{children}</span></p>
}

export function Unavailable({ children }: { children: ReactNode }) {
  return <p role="status" className="flex items-start gap-2 rounded-md bg-muted/50 px-3 py-3 text-xs text-muted-foreground"><TriangleAlert aria-hidden="true" className="mt-0.5 size-3.5 shrink-0" /><span>{children}</span></p>
}

export function ErrorNote({ message }: { message: string | null | undefined }) {
  if (!message) return null
  return <p role="alert" className="mt-2 break-words text-xs text-destructive">{message}</p>
}

export function Badge({ children, tone }: { children: ReactNode; tone?: "warning" | "muted" | "primary" }) {
  return <span className={cn("inline-flex max-w-full items-center truncate rounded-full px-2 py-0.5 text-[11px] font-medium", tone === "warning" ? "bg-amber-500/15 text-amber-800 dark:text-amber-300" : tone === "primary" ? "bg-primary/10 text-primary" : "bg-muted text-muted-foreground")}>{children}</span>
}

// Role and access edits are staged; nothing changes until Save.
export function UnsavedBar({ count, saving, onSave, onDiscard, warning }: { count: number; saving: boolean; onSave: () => void; onDiscard: () => void; warning?: string | null }) {
  if (!count) return null
  return <div role="region" aria-label={t("hub.unsaved_changes")} className="sticky bottom-0 z-10 -mx-1 mt-4 flex flex-wrap items-center justify-between gap-3 rounded-md border bg-background px-3 py-2 shadow-md">
    <div className="min-w-0 text-xs"><span className="font-medium">{t("hub.unsaved_count", { count })}</span>{warning && <span className="mt-1 block text-destructive">{warning}</span>}</div>
    <div className="flex gap-2"><Button variant="outline" size="sm" className="max-md:h-11" onClick={onDiscard} disabled={saving}>{t("hub.discard")}</Button><Button size="sm" className="max-md:h-11" onClick={onSave} disabled={saving || Boolean(warning)}>{saving ? t("common.saving") : t("common.save")}</Button></div>
  </div>
}

// Destructive actions name what is lost and require typing the exact name.
export function ConfirmDialog({ open, onOpenChange, title, description, children, confirmLabel, confirmName, busy, error, disabled, onConfirm }: {
  open: boolean; onOpenChange: (open: boolean) => void; title: string; description: ReactNode; children?: ReactNode
  confirmLabel: string; confirmName?: string; busy?: boolean; error?: string | null; disabled?: boolean; onConfirm: () => void
}) {
  const [typed, setTyped] = useState("")
  const id = useId()
  useEffect(() => { if (open) setTyped("") }, [open])
  const blocked = Boolean(disabled) || (confirmName !== undefined && typed !== confirmName)
  return <Dialog open={open} onOpenChange={onOpenChange}><DialogContent className="max-sm:max-h-[100dvh] max-sm:overflow-y-auto">
    <form onSubmit={(event) => { event.preventDefault(); if (!blocked && !busy) onConfirm() }} className="space-y-4">
      <DialogHeader><DialogTitle>{title}</DialogTitle><DialogDescription>{description}</DialogDescription></DialogHeader>
      {children}
      {confirmName !== undefined && <div className="space-y-2"><Label htmlFor={id}>{t("hub.type_to_confirm", { name: confirmName })}</Label><Input id={id} value={typed} onChange={(event) => setTyped(event.target.value)} autoComplete="off" spellCheck={false} className="max-sm:h-11 max-sm:text-base" /></div>}
      <ErrorNote message={error} />
      <DialogFooter><Button type="button" variant="outline" className="max-sm:h-11" onClick={() => onOpenChange(false)}>{t("common.cancel")}</Button><Button type="submit" variant="destructive" className="max-sm:h-11" disabled={blocked || busy}>{busy ? t("hub.working") : confirmLabel}</Button></DialogFooter>
    </form>
  </DialogContent></Dialog>
}

export type PickerOption = { id: string; label: string; detail?: string; kind?: string }

// One searchable add-picker (people and groups share it).
export function Picker({ label, placeholder, options, onPick, disabled }: { label: string; placeholder: string; options: PickerOption[]; onPick: (option: PickerOption) => void; disabled?: boolean }) {
  const [query, setQuery] = useState("")
  const [open, setOpen] = useState(false)
  const [active, setActive] = useState(0)
  const listId = useId()
  const box = useRef<HTMLDivElement>(null)
  const shown = useMemo(() => {
    const q = query.trim().toLocaleLowerCase()
    return options.filter((option) => !q || option.label.toLocaleLowerCase().includes(q) || option.detail?.toLocaleLowerCase().includes(q)).slice(0, 50)
  }, [options, query])
  useEffect(() => { setActive(0) }, [query, open])
  useEffect(() => {
    if (!open) return
    const close = (event: PointerEvent) => { if (!box.current?.contains(event.target as Node)) setOpen(false) }
    document.addEventListener("pointerdown", close)
    return () => document.removeEventListener("pointerdown", close)
  }, [open])
  function pick(option: PickerOption | undefined) {
    if (!option) return
    onPick(option); setQuery(""); setOpen(false)
  }
  return <div ref={box} className="relative">
    <Search aria-hidden="true" className="pointer-events-none absolute left-2.5 top-1/2 size-3.5 -translate-y-1/2 text-muted-foreground" />
    <Input role="combobox" aria-label={label} aria-expanded={open} aria-controls={listId} aria-autocomplete="list" placeholder={placeholder} value={query} disabled={disabled}
      onFocus={() => setOpen(true)} onChange={(event) => { setQuery(event.target.value); setOpen(true) }}
      onKeyDown={(event) => {
        if (event.key === "ArrowDown") { event.preventDefault(); setOpen(true); setActive((value) => Math.min(value + 1, shown.length - 1)) }
        if (event.key === "ArrowUp") { event.preventDefault(); setActive((value) => Math.max(value - 1, 0)) }
        if (event.key === "Enter" && open) { event.preventDefault(); pick(shown[active]) }
        if (event.key === "Escape") setOpen(false)
      }} className="pl-8 max-md:h-11 max-md:text-base" />
    {open && <ul id={listId} role="listbox" aria-label={label} className="absolute z-30 mt-1 max-h-64 w-full overflow-y-auto rounded-md border bg-popover p-1 text-popover-foreground shadow-md">
      {shown.map((option, index) => <li key={`${option.kind ?? ""}:${option.id}`} role="option" aria-selected={index === active} onPointerDown={(event) => event.preventDefault()} onClick={() => pick(option)} onMouseEnter={() => setActive(index)}
        className={cn("flex cursor-pointer items-center justify-between gap-3 rounded-sm px-2 py-1.5 text-xs max-md:min-h-11", index === active && "bg-accent")}>
        <span className="min-w-0"><span className="block truncate font-medium">{option.label}</span>{option.detail && <span className="block truncate text-muted-foreground">{option.detail}</span>}</span>
        {option.kind && <Badge>{option.kind}</Badge>}
      </li>)}
      {!shown.length && <li className="px-2 py-2 text-xs text-muted-foreground">{t("hub.no_matches")}</li>}
    </ul>}
  </div>
}

export function formatDate(value: string | undefined | null) {
  if (!value) return "—"
  const date = new Date(value)
  return Number.isFinite(date.getTime()) ? date.toLocaleString(intlLocale()) : value
}
