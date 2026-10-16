import type { AppDeployment } from "@/lib/api"

// The URL an App is routed on by the Proxy ingress (https://svc-….<apps domain>/): workspace-authenticated, so opening
// it directly is safe. It must come from the server's App record and is validated before use; anything doubtful
// returns null and the caller falls back to the Console deep link.
export function appOwnUrl(app: Pick<AppDeployment, "public_url">, blockedOrigins: readonly string[]): string | null {
  if (typeof app.public_url !== "string" || app.public_url.length === 0 || app.public_url.length > 2048) return null
  let url: URL
  try { url = new URL(app.public_url) } catch { return null }
  if (url.protocol !== "https:" || url.username !== "" || url.password !== "") return null
  const host = url.hostname.toLowerCase()
  // A real DNS name only: no IP literals, no localhost, no single-label host.
  if (!/^[a-z0-9]([a-z0-9-]*[a-z0-9])?(\.[a-z0-9]([a-z0-9-]*[a-z0-9])?)+$/.test(host) || /^\d+(\.\d+){3}$/.test(host) || host === "localhost" || host.endsWith(".localhost")) return null
  // It must be the App's own origin, never the Console or the Proxy.
  if (blockedOrigins.includes(url.origin)) return null
  return url.href
}
