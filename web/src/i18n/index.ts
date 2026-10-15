import { createElement, Fragment, useSyncExternalStore, type ReactNode } from "react"
import { en, type MessageKey } from "./en"
import { zh } from "./zh"

export const LOCALES = ["en", "zh"] as const
export type Locale = (typeof LOCALES)[number]
export const LOCALE_STORAGE_KEY = "treer-lang"
export const LOCALE_LABELS: Record<Locale, string> = { en: "English", zh: "简体中文" }
export type { MessageKey }

const dictionaries: Record<Locale, Record<MessageKey, string>> = { en, zh }
const asLocale = (value: unknown): Locale | null => LOCALES.find((locale) => locale === value) ?? null

// Order: stored choice, then the browser language (zh* -> zh), then English.
function detect(): Locale {
  try {
    const stored = asLocale(window.localStorage.getItem(LOCALE_STORAGE_KEY))
    if (stored) return stored
  } catch { /* storage unavailable: fall through */ }
  const language = typeof navigator === "undefined" ? "" : navigator.language.toLowerCase()
  return language.startsWith("zh") ? "zh" : "en"
}

let current: Locale = typeof window === "undefined" ? "en" : detect()
const listeners = new Set<() => void>()

function syncDocument() {
  if (typeof document !== "undefined") document.documentElement.lang = current === "zh" ? "zh-Hans" : "en"
}
syncDocument()

export const getLocale = (): Locale => current
export function subscribeLocale(listener: () => void) {
  listeners.add(listener)
  return () => { listeners.delete(listener) }
}

export function setLocale(next: Locale, persist = true) {
  const locale = asLocale(next)
  if (!locale || locale === current) return
  current = locale
  if (persist) { try { window.localStorage.setItem(LOCALE_STORAGE_KEY, locale) } catch { /* in-memory only */ } }
  syncDocument()
  for (const listener of [...listeners]) listener()
}

// Plain-text interpolation only: values are never parsed as markup.
export function t(key: MessageKey, vars?: Record<string, string | number | undefined>): string {
  const template = dictionaries[current][key] ?? en[key]
  return vars ? template.replace(/\{(\w+)\}/g, (match, name: string) => (name in vars ? String(vars[name] ?? "") : match)) : template
}

// Subscribes the component to language changes and returns t.
export function useT() {
  useSyncExternalStore(subscribeLocale, getLocale, () => "en" as Locale)
  return t
}
export function useLocale(): Locale {
  return useSyncExternalStore(subscribeLocale, getLocale, () => "en" as Locale)
}

// Interpolates React nodes (links, code, bold) into a message: "Restart {path} now" -> ["Restart ", <path/>, " now"].
export function tnodes(key: MessageKey, nodes: Record<string, ReactNode>): ReactNode {
  const template = dictionaries[current][key] ?? en[key]
  return template.split(/(\{\w+\})/).map((part, index) => {
    const name = /^\{(\w+)\}$/.exec(part)?.[1]
    return createElement(Fragment, { key: index }, name && name in nodes ? nodes[name] : part)
  })
}

// Server-provided enums (agent status, member role) are shown through the dictionary when known, raw otherwise.
export function statusLabel(value: string): string {
  const key = `status.${value}`
  return key in en ? t(key as MessageKey) : value
}
export function roleLabel(value: string): string {
  const key = `role.${value}`
  return key in en ? t(key as MessageKey) : value
}

// Locale tag for Intl/Date formatting: Chinese follows the console choice, English keeps the browser's own formatting.
export const intlLocale = (): string | undefined => (current === "zh" ? "zh-CN" : undefined)
