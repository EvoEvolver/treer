import { createPortal } from "react-dom"
import { Button } from "@/components/ui/button"

// Announced to assistive technology as soon as it appears (role=alert is an
// assertive live region), so a failed action is not silent for screen readers.
// It is portalled to <body>: an open modal dialog marks the rest of the app
// aria-hidden and sets pointer-events:none, which would block Dismiss exactly
// when a dialog action failed. aria-live keeps Radix from hiding the region.
export function ErrorToast({ message, onDismiss }: { message: string | null; onDismiss: () => void }) {
  if (!message) return null
  return createPortal(
    <div role="alert" aria-live="assertive" className="pointer-events-auto fixed bottom-4 left-1/2 z-[90] flex max-w-[calc(100vw-2rem)] -translate-x-1/2 items-center gap-3 rounded-md border bg-background px-4 py-3 text-sm shadow-lg"><span className="truncate">{message}</span><Button size="sm" variant="ghost" onClick={onDismiss}>Dismiss</Button></div>,
    document.body,
  )
}
