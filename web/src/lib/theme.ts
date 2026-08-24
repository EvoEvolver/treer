export const THEME_STORAGE_KEY = "treer-theme"
export const THEME_COLOR = { light: "#f7f7f5", dark: "#1c1b18" } as const
export type Theme = "light" | "dark"

export function isTheme(value: string | null): value is Theme {
  return value === "light" || value === "dark"
}

export function readTheme(): Theme {
  try {
    const value = localStorage.getItem(THEME_STORAGE_KEY)
    return isTheme(value) ? value : "light"
  } catch {
    return "light"
  }
}

export function applyTheme(theme: Theme) {
  const root = document.documentElement
  root.classList.toggle("dark", theme === "dark")
  root.style.colorScheme = theme
  const meta = document.querySelector('meta[name="theme-color"]')
  if (meta) meta.setAttribute("content", THEME_COLOR[theme])
}

export function persistTheme(theme: Theme) {
  try {
    localStorage.setItem(THEME_STORAGE_KEY, theme)
  } catch {
    // Private mode or quota failures should not block applying the class.
  }
  applyTheme(theme)
}

export function applyStoredTheme() {
  applyTheme(readTheme())
}
