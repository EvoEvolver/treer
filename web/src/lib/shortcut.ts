// The sidebar shortcut is Cmd+B on macOS and Ctrl+B elsewhere; labels and the matcher share one platform check.
export function isMacPlatform() {
  const nav = navigator as Navigator & { userAgentData?: { platform?: string } }
  return /mac|iphone|ipad/i.test(nav.userAgentData?.platform || navigator.platform || navigator.userAgent || "")
}
export const sidebarShortcutLabel = () => (isMacPlatform() ? "⌘B" : "Ctrl+B")
export const sidebarShortcutKeys = () => (isMacPlatform() ? "Meta+B" : "Control+B")

// True when the event is the sidebar shortcut and should be handled by the Console. Terminals (Ctrl+B is the tmux prefix)
// and rich-text editables (bold) keep the key; other keyboards, repeated keys and extra modifiers are ignored.
export function isSidebarShortcut(event: KeyboardEvent) {
  if (event.defaultPrevented || event.repeat || event.isComposing || event.altKey || event.shiftKey) return false
  if (event.key.toLowerCase() !== "b") return false
  const mac = isMacPlatform()
  if (mac ? !event.metaKey || event.ctrlKey : !event.ctrlKey || event.metaKey) return false
  const target = event.target
  if (target instanceof Element && target.closest(".xterm, [contenteditable]:not([contenteditable=false])")) return false
  return true
}
