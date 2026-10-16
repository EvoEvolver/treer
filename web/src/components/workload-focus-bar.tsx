import { ExternalLink } from "lucide-react"
import { t } from "@/i18n"

// Focus mode (?focus=1) shows only one workload in the whole window. This is the whole chrome: where you are and a
// way back to the full Console. The link is a real URL, so it also works in a new tab.
export function WorkloadFocusBar({ path, status, consoleHref, onOpenConsole }: { path: string; status?: string; consoleHref: string; onOpenConsole: () => void }) {
  return <div className="flex h-10 shrink-0 items-center justify-between gap-3 border-b bg-sidebar px-3 text-xs">
    <span className="flex min-w-0 items-center gap-2"><span title={path} className="truncate font-medium">{path}</span>{status && <span className="shrink-0 text-muted-foreground">{status}</span>}</span>
    <a href={consoleHref} onClick={(event) => { if (event.button !== 0 || event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) return; event.preventDefault(); onOpenConsole() }} className="inline-flex min-h-8 shrink-0 items-center gap-1.5 rounded-md px-2 outline-none hover:bg-accent focus-visible:ring-2 focus-visible:ring-ring"><ExternalLink aria-hidden="true" className="size-3.5" />{t("app.open_full_console")}</a>
  </div>
}
