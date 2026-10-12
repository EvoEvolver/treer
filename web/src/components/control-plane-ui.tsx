import { Button } from "@/components/ui/button"
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog"
import { Input } from "@/components/ui/input"
import { Label } from "@/components/ui/label"
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip"
import { machineRecoveryCommands } from "@/lib/control-plane"
import { cn } from "@/lib/utils"
import {
  Copy
} from "lucide-react"
import type * as React from "react"
import { FormEvent } from "react"

export function MachineRecovery({ workspaceId, onCopy, reason }: { workspaceId: string; onCopy: (value: string) => void; reason?: string }) {
  const commands = machineRecoveryCommands(workspaceId)
  return <div className="mt-3 space-y-2 text-left">
    {reason && <p className="text-xs leading-5 text-muted-foreground">{reason}</p>}
    <p className="text-[10px] uppercase text-muted-foreground">On that machine</p>
    <div className="space-y-2">
      <div className="rounded-md border bg-muted/30 p-2">
        <code className="block break-all font-mono text-[10px] leading-5">{commands.restartController}</code>
        <Button size="sm" variant="outline" className="mt-2 h-7" onClick={() => onCopy(commands.restartController)}><Copy />Copy restart-controller</Button>
      </div>
      <div className="rounded-md border bg-muted/30 p-2">
        <code className="block break-all font-mono text-[10px] leading-5">{commands.start}</code>
        <Button size="sm" variant="outline" className="mt-2 h-7" onClick={() => onCopy(commands.start)}><Copy />Copy restore</Button>
      </div>
    </div>
  </div>
}

export function Status({ value }: { value: string }) {
  return <span className={cn("inline-flex shrink-0 items-center gap-1.5 text-[10px] font-medium capitalize text-zinc-500", value === "idle" && "text-emerald-700", ["working", "starting"].includes(value) && "text-sky-700", value === "blocked" && "text-amber-700", ["failed", "exited", "offline"].includes(value) && "text-red-600")}><span className="size-1.5 rounded-full bg-current opacity-75" />{value}</span>
}

export function IconButton({ label, children, ...props }: React.ComponentProps<typeof Button> & { label: string }) {
  return <Tooltip><TooltipTrigger asChild><Button size="icon" variant="ghost" aria-label={label} {...props}>{children}</Button></TooltipTrigger><TooltipContent>{label}</TooltipContent></Tooltip>
}

export function MobileTerminalKey({ label, active = false, children, onClick }: { label: string; active?: boolean; children: React.ReactNode; onClick: () => void }) {
  return <button type="button" aria-label={label} aria-pressed={active || undefined} onClick={onClick} className={cn("grid h-10 min-w-0 touch-manipulation select-none place-items-center rounded-[5px] border border-zinc-700 bg-[#24292d] px-1 text-[11px] font-medium text-zinc-200 active:bg-[#3a4248]", active && "border-sky-500 bg-sky-500/20 text-sky-200")}>{children}</button>
}

export function Field({ label, children }: { label: string; children: React.ReactNode }) {
  return <div className="space-y-2"><Label>{label}</Label>{children}</div>
}

export function SimpleNameDialog({ open, onOpenChange, title, description, label, value, onValueChange, onSubmit, dataTour }: { open: boolean; onOpenChange: (open: boolean) => void; title: string; description: string; label: string; value: string; onValueChange: (value: string) => void; onSubmit: (event: FormEvent) => void; dataTour?: string }) {
  return <Dialog open={open} onOpenChange={onOpenChange}><DialogContent data-tour={dataTour}><form onSubmit={onSubmit}><DialogHeader><DialogTitle>{title}</DialogTitle><DialogDescription>{description}</DialogDescription></DialogHeader><div className="my-5"><Field label={label}><Input value={value} onChange={(event) => onValueChange(event.target.value)} required autoFocus maxLength={80} /></Field></div><DialogFooter><Button type="button" variant="outline" onClick={() => onOpenChange(false)}>Cancel</Button><Button type="submit">Create</Button></DialogFooter></form></DialogContent></Dialog>
}

export function EmptyState({ icon, label }: { icon: React.ReactNode; label: string }) {
  return <div className="flex flex-col items-center gap-2 px-4 py-8 text-center text-[11px] text-muted-foreground"><span className="[&_svg]:size-4 [&_svg]:opacity-50">{icon}</span>{label}</div>
}
