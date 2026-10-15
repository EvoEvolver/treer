import { ChevronRight } from "lucide-react"
import { t } from "@/i18n"

export function AgentBreadcrumbs({ path }: { path: string }) {
  const parts = path.split("/").filter(Boolean)
  return <nav aria-label={t("crumbs.agent_path")} title={path} className="min-w-0 overflow-x-auto whitespace-nowrap">
    <ol className="flex items-center gap-1">
      {parts.map((part, index) => <li className="flex items-center gap-1" key={index}>
        {index > 0 && <ChevronRight aria-hidden="true" className="size-3 shrink-0" />}
        <span aria-current={index === parts.length - 1 ? "page" : undefined}>{part}</span>
      </li>)}
    </ol>
  </nav>
}
