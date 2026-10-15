import { AppWindow, Bot } from "lucide-react"

// One visual vocabulary for the Tree and its map; runtime adapters stay invisible.
export function WorkloadIcon({ kind, className }: { kind: string; className?: string }) {
  const Icon = kind === "app" ? AppWindow : Bot
  return <Icon aria-hidden="true" data-workload-icon={kind === "app" ? "app" : "agent"} className={className} />
}
