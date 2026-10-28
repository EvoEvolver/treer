import type { HTMLAttributes, ReactNode } from "react"
import { ChevronRight, CircleAlert, CircleX, WifiOff } from "lucide-react"
import { cn } from "@/lib/utils"
import { statusLabel } from "@/i18n"

// One row anatomy for every Tree level (folders, Agents, Apps, App-owned nodes):
// [indent][chevron slot][icon] name ......... [status | actions]
// Rows are 28px on fine pointers and 44px on touch / phone layouts; the chevron
// slot is always reserved so icons and names align across folders and leaves.
export const TREE_INDENT = 12
const ROW_START = 4
const CHEVRON = 16
export const treeIndent = (depth: number) => ROW_START + depth * TREE_INDENT
const indent = treeIndent

export const treeRowHeight = "h-7 max-md:h-11 [@media(pointer:coarse)]:h-11"

export function TreeGroup({ depth, children, busy }: { depth: number; children: ReactNode; busy?: boolean }) {
  // The guide sits under the parent's chevron so a branch reads as one column.
  return <div role="group" aria-busy={busy} className="relative">
    <span aria-hidden="true" className="pointer-events-none absolute inset-y-0 z-[1] w-px bg-border" style={{ left: indent(depth - 1) + CHEVRON / 2 }} />
    {children}
  </div>
}

type Primary =
  | { kind: "link"; props: { href: string } & HTMLAttributes<HTMLAnchorElement> }
  | { kind: "button"; onClick: () => void; toggles?: boolean }

export function TreeRow({ depth, expanded, toggle, primary, treeKey, icon, label, detail, title, muted, trailing, actions, selected, highlight, rowProps }: {
  depth: number
  /** undefined for leaves; a folder or expandable node passes its state. */
  expanded?: boolean
  /** A separate disclosure button, for rows whose primary action opens something. */
  toggle?: { label: string; onClick: () => void }
  primary: Primary
  treeKey: string
  icon: ReactNode
  label: string
  /** Secondary muted text after the name, e.g. the containing folder. */
  detail?: string
  title?: string
  muted?: boolean
  trailing?: ReactNode
  actions?: ReactNode
  selected?: boolean
  highlight?: "drop" | "before" | "after" | null
  rowProps?: HTMLAttributes<HTMLDivElement> & Record<`data-${string}`, string | undefined>
}) {
  const chevron = <ChevronRight aria-hidden="true" className={cn("size-3.5 text-muted-foreground transition-transform motion-reduce:transition-none", expanded && "rotate-90")} />
  const content = <>
    <span aria-hidden={toggle ? "true" : undefined} className="grid w-4 shrink-0 place-items-center">{expanded !== undefined && !toggle && chevron}</span>
    <span className={cn("grid size-4 shrink-0 place-items-center [&_svg]:size-4", selected ? "text-primary" : "text-muted-foreground")}>{icon}</span>
    <span className={cn("min-w-0 flex-1 truncate", muted && !selected && "text-muted-foreground")}>{label}{detail && <span className="ml-1.5 text-[11px] font-normal text-muted-foreground">{detail}</span>}</span>
    {trailing && <span className={cn("flex shrink-0 items-center gap-1", actions && "md:[@media(pointer:fine)]:group-hover/row:opacity-0 md:[@media(pointer:fine)]:group-focus-within/row:opacity-0 md:[@media(pointer:fine)]:group-has-[[data-state=open]]/row:opacity-0")}>{trailing}</span>}
  </>
  const className = cn(
    "flex min-w-0 flex-1 items-center gap-1.5 rounded-md pr-2 text-left text-[13px] leading-none outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring",
    // Room for the actions button: always on touch, on hover/focus elsewhere, so it never covers the name.
    actions && "max-md:pr-12 [@media(pointer:coarse)]:pr-12 md:[@media(pointer:fine)]:group-hover/row:pr-8 md:[@media(pointer:fine)]:group-focus-within/row:pr-8",
  )
  const style = { paddingLeft: indent(depth) }
  return <div {...rowProps} data-tree-row="" title={title} className={cn(
    "group/row relative flex items-stretch rounded-md text-foreground transition-colors motion-reduce:transition-none",
    treeRowHeight,
    selected ? "bg-primary/10 font-medium dark:bg-primary/20" : "hover:bg-accent",
    highlight === "drop" && "bg-primary/10 ring-1 ring-inset ring-primary",
    highlight === "before" && "shadow-[inset_0_2px_0_hsl(var(--primary))]",
    highlight === "after" && "shadow-[inset_0_-2px_0_hsl(var(--primary))]",
  )}>
    {primary.kind === "link"
      ? <a {...primary.props} data-tree-primary="" data-tree-key={treeKey} className={className} style={style}>{content}</a>
      : <button type="button" data-tree-primary="" data-tree-key={treeKey} data-tree-toggle={primary.toggles ? "" : undefined} className={className} style={style} onClick={primary.onClick}>{content}</button>}
    {toggle && <button type="button" tabIndex={-1} data-tree-toggle="" aria-label={toggle.label} aria-expanded={expanded} onClick={toggle.onClick}
      className="absolute inset-y-0 z-[2] flex w-5 items-center rounded-sm pl-[3px] hover:bg-foreground/10 max-md:w-7 [@media(pointer:coarse)]:w-7" style={{ left: indent(depth) - 2 }}>{chevron}</button>}
    {actions}
  </div>
}

// Row actions: hidden until the row is hovered or focused, always visible on touch or while its menu is open.
export const treeActionsClass = "absolute right-0.5 top-1/2 z-[2] size-6 -translate-y-1/2 rounded-md text-muted-foreground hover:bg-foreground/10 hover:text-foreground max-md:size-11 [@media(pointer:coarse)]:size-11 pointer-events-none opacity-0 group-hover/row:pointer-events-auto group-hover/row:opacity-100 group-focus-within/row:pointer-events-auto group-focus-within/row:opacity-100 focus-visible:pointer-events-auto focus-visible:opacity-100 data-[state=open]:pointer-events-auto data-[state=open]:opacity-100 data-[state=open]:bg-foreground/10 max-md:pointer-events-auto max-md:opacity-100 [@media(pointer:coarse)]:pointer-events-auto [@media(pointer:coarse)]:opacity-100"

// Status uses shape as well as colour: a dot for working, a hollow ring for stopped,
// an icon for each attention state. Idle and running (an App's normal state) show nothing.
export type TreeStatusKey = "working" | "starting" | "blocked" | "failed" | "offline" | "exited" | "stopped"
export function treeStatus(status: string, online: boolean): TreeStatusKey | null {
  const key = online ? status : "offline"
  return ["working", "starting", "blocked", "failed", "offline", "exited", "stopped"].includes(key) ? key as TreeStatusKey : null
}
export const isAttention = (key: TreeStatusKey | null) => key === "blocked" || key === "failed" || key === "offline"
export const isInactive = (key: TreeStatusKey | null) => key === "exited" || key === "stopped"

export function TreeStatus({ status, marker = true }: { status: TreeStatusKey; marker?: boolean }) {
  const label = statusLabel(status)
  const common = { role: "img", "aria-label": label, title: label, "data-tree-status": marker ? status : undefined }
  if (status === "working" || status === "starting") return <span {...common} className="mx-1 size-2 shrink-0 rounded-full bg-status-working" />
  if (isInactive(status)) return <span {...common} className="mx-1 size-2 shrink-0 rounded-full border-[1.5px] border-muted-foreground" />
  const Icon = status === "failed" ? CircleX : status === "offline" ? WifiOff : CircleAlert
  return <span {...common} className="grid shrink-0 place-items-center text-status-attention"><Icon aria-hidden="true" className="size-3.5" /></span>
}

// A collapsed folder summarises what it hides, so attention never disappears behind a chevron.
export function TreeCount({ tone, count, label }: { tone: "attention" | "working"; count: number; label: string }) {
  return <span aria-hidden="true" title={label} className={cn("flex items-center gap-1 text-[11px] font-medium tabular-nums", tone === "attention" ? "text-status-attention" : "text-status-working")}>
    {tone === "attention" ? <CircleAlert className="size-3" /> : <span className="size-1.5 rounded-full bg-status-working" />}{count}
  </span>
}
