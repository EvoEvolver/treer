import type { MouseEvent } from "react"

// Same-origin, same-session new page; noopener keeps it from reaching back into this one. The URL carries IDs only.
export function openInNewBrowserTab(href: string) {
  window.open(href, "_blank", "noopener")
}

// Props for a control that opens a workload in place but, like a link, opens it in a new browser tab for
// ctrl/cmd-click or middle-click. Without an href it behaves as a plain click.
export function newTabProps(href: string | undefined, open: () => void) {
  if (!href) return { onClick: open }
  return {
    onClick: (event: MouseEvent) => { if (event.ctrlKey || event.metaKey) { event.preventDefault(); openInNewBrowserTab(href) } else open() },
    onAuxClick: (event: MouseEvent) => { if (event.button === 1) { event.preventDefault(); openInNewBrowserTab(href) } },
    // Stops the browser's middle-click autoscroll so the aux click is delivered.
    onMouseDown: (event: MouseEvent) => { if (event.button === 1) event.preventDefault() },
  }
}
