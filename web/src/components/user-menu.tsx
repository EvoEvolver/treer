import { ListChecks, LogOut, MoreHorizontal, Settings as SettingsIcon } from "lucide-react"
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuLabel, DropdownMenuSeparator, DropdownMenuTrigger } from "@/components/ui/dropdown-menu"
import type { User } from "@/lib/api"

// The personal menu at the bottom of the tree sidebar. On phones it is the only way to reach Settings and Log out,
// so touch targets are at least 44px, text is at least 11px, the panel never exceeds the viewport and the trigger
// stays clear of the home indicator. Desktop keeps the compact sizes.
const item = "min-h-11 gap-3 px-3 text-sm sm:min-h-0 sm:gap-2 sm:px-2 sm:text-xs"

export function UserMenu({ user, badge, role, preview, onSettings, onTour, onLogout }: {
  user: User
  badge: string
  role: string
  preview: boolean
  onSettings: () => void
  onTour: () => void
  onLogout: () => void
}) {
  return <div className="shrink-0 border-t p-2 pb-[max(0.5rem,env(safe-area-inset-bottom))]">
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <button type="button" aria-label="User menu" className="grid h-12 w-full grid-cols-[32px_minmax(0,1fr)_24px] items-center gap-2.5 rounded-[5px] px-2 text-left hover:bg-accent sm:h-11 sm:grid-cols-[28px_minmax(0,1fr)_20px] sm:gap-2">
          <span className="grid size-8 place-items-center rounded bg-[#e8deee] text-xs font-bold text-[#694a73] sm:size-7 sm:text-[10px]">{badge}</span>
          <span className="min-w-0">
            <span className="block truncate text-sm font-medium sm:text-xs">{user.preferred_name}</span>
            <span className="block truncate text-[11px] text-muted-foreground sm:text-[9px]">{user.email}</span>
          </span>
          <MoreHorizontal className="size-5 text-muted-foreground sm:size-4" />
        </button>
      </DropdownMenuTrigger>
      <DropdownMenuContent side="top" align="start" collisionPadding={8} className="max-h-[calc(100dvh-1rem)] w-[min(20rem,calc(100vw-1rem))] overflow-y-auto sm:w-60">
        <DropdownMenuLabel className="px-3 py-2.5 sm:px-2 sm:py-1.5">
          <span className="block truncate text-sm sm:text-xs">{user.preferred_name}</span>
          <span className="mt-0.5 block truncate text-xs font-normal text-muted-foreground sm:text-[10px]">{user.email} · {role}</span>
        </DropdownMenuLabel>
        <DropdownMenuSeparator />
        <DropdownMenuItem className={item} onSelect={onSettings}><SettingsIcon />Settings</DropdownMenuItem>
        <DropdownMenuItem className={item} onSelect={onTour}><ListChecks />Replay product tour</DropdownMenuItem>
        <DropdownMenuSeparator />
        <DropdownMenuItem className={item} onSelect={onLogout} disabled={preview}><LogOut />Log out</DropdownMenuItem>
      </DropdownMenuContent>
    </DropdownMenu>
  </div>
}
