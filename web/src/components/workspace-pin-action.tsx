import { Pin, PinOff } from 'lucide-react'
import { DropdownMenuItem } from '@/components/ui/dropdown-menu'
import type { WorkspacePin } from '@/lib/workspace-pins'
import type { PinsController } from './use-workspace-pins'
import { t } from '@/i18n'

export function WorkspacePinAction({ pins, target }: {
  pins: PinsController
  target: Pick<WorkspacePin, 'kind' | 'id'> | null
}) {
  if (!pins.supported || !pins.can_pin || !target) return null
  const pinned = pins.pins.some((pin) => pin.kind === target.kind && pin.id === target.id)
  const Icon = pinned ? PinOff : Pin
  return <DropdownMenuItem className="[@media(pointer:coarse)]:min-h-11" disabled={pins.busy} onSelect={() => void pins.toggle(target)}>
    <Icon aria-hidden="true" />{t(pinned ? 'pins.unpin' : 'pins.pin')}
  </DropdownMenuItem>
}

export function WorkspacePinsNotice({ pins }: { pins: PinsController }) {
  return pins.error ? <div role="alert" className="mx-3 mb-3 rounded-md border border-destructive/50 bg-destructive/10 p-3 text-xs text-foreground">
    <p>{pins.error}</p>
    {pins.retryable && <button type="button" className="mt-2 min-h-11 rounded px-2 underline focus-visible:ring-2 focus-visible:ring-ring" onClick={() => void pins.refresh()}>
      {t('pins.retry')}
    </button>}
  </div> : null
}
