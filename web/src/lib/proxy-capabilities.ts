import { useEffect, useState } from "react"
import { api } from "@/lib/api"

// `/api/health` advertises what this Proxy supports. One request per page
// session; a failed or non-JSON answer counts as "advertises nothing" (an older
// or unreachable Proxy) and is asked again on the next mount.
export const SETTINGS_ADMIN_CAPABILITY = "settings.admin.v1"

let cached: Promise<string[]> | null = null

export function loadProxyCapabilities(): Promise<string[]> {
  cached ??= api<{ capabilities?: unknown }>("/api/health", { cache: "no-store" })
    .then((health) => Array.isArray(health?.capabilities) ? health.capabilities.filter((value): value is string => typeof value === "string") : [])
    .catch(() => { cached = null; return [] })
  return cached
}

// null until the Proxy answered (or failed); then the advertised list.
export function useProxyCapabilities(enabled = true): string[] | null {
  const [value, setValue] = useState<string[] | null>(null)
  useEffect(() => {
    if (!enabled) return
    let active = true
    void loadProxyCapabilities().then((list) => { if (active) setValue(list) })
    return () => { active = false }
  }, [enabled])
  return value
}
