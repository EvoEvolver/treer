import { FormEvent, useEffect, useState, type ReactNode } from "react"
import { CreditCard, SlidersHorizontal, UserRound } from "lucide-react"
import { api, type User } from "@/lib/api"
import { persistTheme, readTheme, type Theme } from "@/lib/theme"
import { cn } from "@/lib/utils"
import { Button } from "@/components/ui/button"
import { Dialog, DialogContent, DialogDescription, DialogTitle } from "@/components/ui/dialog"
import { Input } from "@/components/ui/input"
import { Label } from "@/components/ui/label"
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select"

// Autofocus would open the on-screen keyboard over the whole sheet as soon as Settings opens on a phone.
const finePointer = () => typeof matchMedia === "function" && matchMedia("(hover: hover) and (pointer: fine)").matches

type SettingsSection = "billing" | "account" | "general"

const sections: { id: SettingsSection; label: string; icon: typeof UserRound }[] = [
  { id: "billing", label: "Usage & billing", icon: CreditCard },
  { id: "account", label: "Account", icon: UserRound },
  { id: "general", label: "General", icon: SlidersHorizontal },
]

function Field({ label, htmlFor, hint, children }: { label: string; htmlFor?: string; hint?: string; children: ReactNode }) {
  return <div className="space-y-2"><Label htmlFor={htmlFor}>{label}</Label>{children}{hint && <span className="block text-[10px] text-muted-foreground">{hint}</span>}</div>
}

export function SettingsDialog({ open, onOpenChange, user, onUserChange, onError }: { open: boolean; onOpenChange: (open: boolean) => void; user: User; onUserChange: (user: User) => void; onError: (reason: unknown) => void }) {
  const [section, setSection] = useState<SettingsSection>("account")
  const [preferredName, setPreferredName] = useState(user.preferred_name)
  const [email, setEmail] = useState(user.email)
  const [theme, setTheme] = useState<Theme>(() => readTheme())
  const [saving, setSaving] = useState(false)

  useEffect(() => {
    if (!open) return
    setSection("account")
    setPreferredName(user.preferred_name)
    setEmail(user.email)
  }, [open, user])

  async function saveAccount(event: FormEvent) {
    event.preventDefault()
    setSaving(true)
    try {
      const updated = await api<User>("/api/auth/profile", { method: "PATCH", body: JSON.stringify({ email, preferred_name: preferredName }) })
      onUserChange(updated)
    } catch (reason) {
      onError(reason)
    } finally {
      setSaving(false)
    }
  }

  function changeTheme(next: Theme) {
    persistTheme(next)
    setTheme(next)
  }

  return <Dialog open={open} onOpenChange={onOpenChange}>
    <DialogContent className="flex h-[min(36rem,calc(100dvh-2rem))] max-w-[760px] flex-col gap-0 overflow-hidden p-0 max-sm:left-0 max-sm:top-0 max-sm:h-dvh max-sm:max-h-none max-sm:w-full max-sm:max-w-none max-sm:translate-x-0 max-sm:translate-y-0 max-sm:rounded-none max-sm:border-0 sm:rounded-lg">
      <DialogTitle className="sr-only">Settings</DialogTitle>
      <DialogDescription className="sr-only">Account, appearance, and billing preferences.</DialogDescription>
      <div className="flex min-h-0 flex-1 flex-col sm:flex-row">
        <aside className="flex shrink-0 flex-col bg-sidebar px-2.5 py-4 max-sm:pb-2 sm:w-[188px]">
          <div className="px-2 pb-3 text-sm font-semibold max-sm:pb-2 max-sm:pr-12 max-sm:text-base">Settings</div>
          <nav aria-label="Settings" className="flex gap-0.5 max-sm:overflow-x-auto sm:flex-col">
            {sections.map((item) => {
              const Icon = item.icon
              const active = section === item.id
              return <button key={item.id} type="button" aria-current={active ? "page" : undefined} onClick={() => setSection(item.id)} className={cn("flex items-center gap-2 rounded-[5px] px-2 py-1.5 text-left text-xs font-medium max-sm:min-h-11 max-sm:shrink-0 max-sm:px-3 max-sm:text-sm", active ? "bg-background text-foreground shadow-sm" : "text-muted-foreground hover:bg-background/70 hover:text-foreground")}>
                <Icon className="size-3.5 shrink-0 opacity-70" />
                <span className="truncate">{item.label}</span>
              </button>
            })}
          </nav>
        </aside>
        <div className="min-h-0 min-w-0 flex-1 overflow-auto border-t border-border/80 px-4 py-5 max-sm:pb-[max(1.25rem,env(safe-area-inset-bottom))] sm:border-l sm:border-t-0 sm:px-7 sm:py-6">
          {section === "billing" && <section>
            <h2 className="text-base font-semibold">Usage & billing</h2>
            <p className="mt-2 max-w-md text-sm leading-6 text-muted-foreground">Usage and billing are not available yet. This control plane does not meter seats or invoices. When billing ships, plan and usage details will appear here.</p>
          </section>}
          {section === "account" && <section>
            <h2 className="text-base font-semibold">Account</h2>
            <p className="mt-2 max-w-md text-sm leading-6 text-muted-foreground">Your preferred name is visible to other organization members.</p>
            <form onSubmit={saveAccount} className="mt-6 max-w-md space-y-4">
              <Field label="Preferred name" htmlFor="settings-preferred-name"><Input id="settings-preferred-name" className="max-sm:h-11 max-sm:text-base" value={preferredName} onChange={(event) => setPreferredName(event.target.value)} required autoFocus={finePointer()} maxLength={80} autoComplete="name" /></Field>
              <Field label="Email" htmlFor="settings-email"><Input id="settings-email" className="max-sm:h-11 max-sm:text-base" type="email" value={email} onChange={(event) => setEmail(event.target.value)} required maxLength={254} autoComplete="email" /></Field>
              <div className="flex justify-end"><Button type="submit" className="max-sm:h-11 max-sm:w-full max-sm:text-base" disabled={saving}>{saving ? "Saving" : "Save"}</Button></div>
            </form>
          </section>}
          {section === "general" && <section>
            <h2 className="text-base font-semibold">General</h2>
            <div className="mt-6 max-w-md space-y-6">
              <div className="space-y-2">
                <Label>Theme</Label>
                <div role="group" aria-label="Theme" className="inline-flex rounded-[5px] bg-muted p-0.5">
                  <button type="button" aria-pressed={theme === "light"} onClick={() => changeTheme("light")} className={cn("h-8 rounded-[5px] px-3 text-xs font-medium max-sm:h-11 max-sm:px-5 max-sm:text-sm", theme === "light" ? "bg-background text-foreground shadow-sm" : "text-muted-foreground hover:text-foreground")}>Light</button>
                  <button type="button" aria-pressed={theme === "dark"} onClick={() => changeTheme("dark")} className={cn("h-8 rounded-[5px] px-3 text-xs font-medium max-sm:h-11 max-sm:px-5 max-sm:text-sm", theme === "dark" ? "bg-background text-foreground shadow-sm" : "text-muted-foreground hover:text-foreground")}>Dark</button>
                </div>
              </div>
              <Field label="Language" htmlFor="settings-language" hint="Only English is available.">
                <Select value="en" onValueChange={() => undefined}>
                  <SelectTrigger id="settings-language" aria-label="Language" className="max-sm:h-11 max-sm:text-base"><SelectValue /></SelectTrigger>
                  <SelectContent><SelectItem value="en">English</SelectItem></SelectContent>
                </Select>
              </Field>
            </div>
          </section>}
        </div>
      </div>
    </DialogContent>
  </Dialog>
}
