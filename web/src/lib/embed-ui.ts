import { getLocale } from "@/i18n"
import { type Theme } from "@/lib/theme"

export const EMBED_THEME_MESSAGE_TYPE = "treer:embed-theme"

export const EMBED_LOOKS = ["a", "b", "c"] as const
export const EMBED_LANGS = ["en", "zh"] as const
export type EmbedLook = (typeof EMBED_LOOKS)[number]
export type EmbedLang = (typeof EMBED_LANGS)[number]
export const EMBED_LOOK_STORAGE_KEY = "treer-look"

export type EmbedAppearance = { look?: EmbedLook; lang?: EmbedLang }

const asLook = (value: unknown): EmbedLook | undefined =>
  EMBED_LOOKS.find((look) => look === value)

// Console look: optional localStorage "treer-look" (a|b|c). Unset means the
// embedded UI keeps its own default or stored look. Language is the console language (stored choice, else the browser).
export function readEmbedAppearance(): EmbedAppearance {
  let look: EmbedLook | undefined
  try { look = asLook(window.localStorage.getItem(EMBED_LOOK_STORAGE_KEY)) } catch { look = undefined }
  return { look, lang: getLocale() }
}

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

export function withEmbedTheme(src: string, theme: Theme, appearance: EmbedAppearance = {}): string {
  const url = new URL(src, typeof window === "undefined" ? "http://localhost/" : window.location.href)
  url.searchParams.set("theme", theme)
  const look = asLook(appearance.look)
  if (look) url.searchParams.set("look", look)
  const lang = EMBED_LANGS.find((item) => item === appearance.lang)
  if (lang) url.searchParams.set("lang", lang)
  return url.toString()
}

export function embedThemeMessage(theme: Theme, appearance: EmbedAppearance = {}) {
  const look = asLook(appearance.look)
  const lang = EMBED_LANGS.find((item) => item === appearance.lang)
  return { type: EMBED_THEME_MESSAGE_TYPE, theme, ...(look ? { look } : {}), ...(lang ? { lang } : {}) }
}

export function postEmbedTheme(frame: HTMLIFrameElement | null | undefined, theme: Theme, appearance: EmbedAppearance = {}) {
  const target = frame?.contentWindow
  if (!target) return
  // Appearance is not privileged data, but never broadcast to an unknown origin.
  try {
    if (!frame?.src) return
    const url = new URL(frame.src, window.location.href)
    if (!['http:', 'https:'].includes(url.protocol)) return
    target.postMessage(embedThemeMessage(theme, appearance), url.origin)
  } catch { return }
}
