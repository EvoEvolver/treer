import { type Theme } from "@/lib/theme"

export const EMBED_THEME_MESSAGE_TYPE = "treer:embed-theme"

export function interfaceFrameKey(
  agentId: string,
  instanceId: string,
  registeredAt: string,
  revision: number,
) {
  // registered_at is part of the live descriptor. A new timestamp means a new
  // registration and should remount the embedded UI.
  return `${agentId}:${instanceId}:${registeredAt}:${revision}`
}

export function withEmbedTheme(src: string, theme: Theme): string {
  const url = new URL(src, typeof window === "undefined" ? "http://localhost/" : window.location.href)
  url.searchParams.set("theme", theme)
  return url.toString()
}

export function postEmbedTheme(frame: HTMLIFrameElement | null | undefined, theme: Theme) {
  const target = frame?.contentWindow
  if (!target) return
  // Appearance is not privileged data, but never broadcast to an unknown origin.
  try {
    if (!frame?.src) return
    const url = new URL(frame.src, window.location.href)
    if (!['http:', 'https:'].includes(url.protocol)) return
    target.postMessage({ type: EMBED_THEME_MESSAGE_TYPE, theme }, url.origin)
  } catch { return }
}
