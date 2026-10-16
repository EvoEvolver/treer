import type { KeyboardEvent, MouseEvent } from "react"

// Props for a real link (<a href>) that opens a workload in place on a plain left click, while ctrl/cmd/shift-click,
// middle-click and the browser's own "Open in new tab" and "Copy link address" use the href natively.
// Space activates it like a button, so rows keep the keyboard behaviour they had as buttons.
export function workloadLinkProps(href: string, open: () => void) {
  return {
    href,
    draggable: false, // a draggable row must be dragged by its link, not drag the URL
    onClick: (event: MouseEvent<HTMLElement>) => {
      if (event.defaultPrevented || event.button !== 0 || event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) return
      event.preventDefault()
      open()
    },
    onKeyDown: (event: KeyboardEvent<HTMLElement>) => {
      if (event.key !== " " || event.metaKey || event.ctrlKey || event.altKey) return
      event.preventDefault()
      open()
    },
  }
}
