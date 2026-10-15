// Core constructs the authorization URL; only the selected supported IdP may
// receive it. Never navigate to an API-supplied arbitrary origin or URL token.
export function oauthAuthorizationUrl(provider: "github" | "google", value: string): string {
  const url = new URL(value)
  const origin = provider === "github" ? "https://github.com" : "https://accounts.google.com"
  if (url.protocol !== "https:" || url.origin !== origin || url.username || url.password || url.hash || ["invite", "invite_token", "pending_id"].some((key) => url.searchParams.has(key))) throw new Error("invalid_authorization_url")
  return url.toString()
}
