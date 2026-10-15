import { en, type MessageKey } from "./en";
import { zh } from "./zh";

export type { MessageKey };
export type Locale = "en" | "zh";
const dictionaries: Record<Locale, Partial<Record<MessageKey, string>>> = { en, zh };

function detect(): Locale {
  const asked = new URLSearchParams(location.search).get("lang");
  if (asked === "en" || asked === "zh") return asked;
  return navigator.language.toLowerCase().startsWith("zh") ? "zh" : "en";
}
export const locale: Locale = detect();
document.documentElement.lang = locale === "zh" ? "zh-Hans" : "en";

/** `{{name}}` interpolation; a key missing in the active locale falls back to English. */
export function t(key: MessageKey, values?: Record<string, string | number>): string {
  const template = dictionaries[locale][key] ?? en[key];
  return values ? template.replace(/\{\{(\w+)\}\}/g, (_, name: string) => String(values[name] ?? "")) : template;
}
