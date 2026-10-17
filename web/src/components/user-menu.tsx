import type { Ref } from "react"
import { ListChecks, LogOut, MoreHorizontal, PanelLeftClose, Settings as SettingsIcon } from "lucide-react"
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuLabel, DropdownMenuSeparator, DropdownMenuTrigger } from "@/components/ui/dropdown-menu"
import type { User } from "@/lib/api"
import { useT } from "@/i18n"

// The personal menu at the bottom of the tree sidebar. On phones it is the only way to reach Settings and Log out,
// so touch targets are at least 44px, text is at least 11px, the panel never exceeds the viewport and the trigger
// stays clear of the home indicator. Desktop keeps the compact sizes.
const item = "min-h-11 gap-3 px-3 text-sm sm:min-h-0 sm:gap-2 sm:px-2 sm:text-xs"

export function UserMenu({ user, role, preview, collapseRef, onCollapse, onSettings, onTour, onLogout }: {
  user: User
  role: string
  collapseRef: Ref<HTMLButtonElement>
  onCollapse: () => void
  preview: boolean
  onSettings: () => void
  onTour: () => void
  onLogout: () => void
}) {
  const t = useT()
  return <div className="flex shrink-0 items-center gap-1 border-t p-2 pb-[max(0.5rem,env(safe-area-inset-bottom))] md:[@media(pointer:fine)]:gap-0.5 md:[@media(pointer:fine)]:px-2 md:[@media(pointer:fine)]:py-1">
    <button ref={collapseRef} type="button" aria-label={t("resizer.collapse_tree_sidebar")} title={t("resizer.collapse_tree_sidebar")} aria-expanded="true" aria-controls="tree-sidebar" onClick={onCollapse} className="hidden size-9 shrink-0 place-items-center [@media(pointer:coarse)]:size-11 md:[@media(pointer:fine)]:size-7 rounded-[5px] text-muted-foreground outline-none hover:bg-accent hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring md:grid"><PanelLeftClose aria-hidden="true" className="size-4" /></button>
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <button type="button" aria-label={t("userMenu.trigger")} className="grid h-12 min-w-0 flex-1 grid-cols-[minmax(0,1fr)_24px] items-center gap-2.5 rounded-[5px] px-2 text-left hover:bg-accent sm:h-11 sm:grid-cols-[minmax(0,1fr)_20px] sm:gap-2 md:[@media(pointer:fine)]:h-7">
          <span className="block min-w-0 truncate text-sm font-medium sm:text-xs">{user.preferred_name}</span>
          <MoreHorizontal className="size-5 text-muted-foreground sm:size-4" />
        </button>
      </DropdownMenuTrigger>
      <DropdownMenuContent side="top" align="start" collisionPadding={8} className="max-h-[calc(100dvh-1rem)] w-[min(20rem,calc(100vw-1rem))] overflow-y-auto sm:w-60">
        <DropdownMenuLabel className="px-3 py-2.5 sm:px-2 sm:py-1.5">
          <span className="block truncate text-sm sm:text-xs">{user.preferred_name}</span>
          <span className="mt-0.5 block truncate text-xs font-normal text-muted-foreground sm:text-[10px]">{user.email} · {role}</span>
        </DropdownMenuLabel>
        <DropdownMenuSeparator />
        <DropdownMenuItem className={item} onSelect={onSettings}><SettingsIcon />{t("userMenu.settings")}</DropdownMenuItem>
        <DropdownMenuItem className={item} onSelect={onTour}><ListChecks />{t("userMenu.tour")}</DropdownMenuItem>
        <DropdownMenuSeparator />
        <DropdownMenuItem className={item} onSelect={onLogout} disabled={preview}><LogOut />{t("userMenu.logout")}</DropdownMenuItem>
      </DropdownMenuContent>
    </DropdownMenu>
  </div>
}
