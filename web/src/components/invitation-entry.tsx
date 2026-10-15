import { useEffect, useRef, useState } from "react"
import { api, ApiError, proxyUrl, type User } from "@/lib/api"
import { AuthScreen, type AuthMode } from "@/components/auth-screen"
import { Button } from "@/components/ui/button"
import { t } from "@/i18n"

export type InvitationPreview = {
  kind: "organization" | "personal" | null
  organization_id: string | null
  organization_name: string | null
  status: "valid" | "invalid"
  capabilities: string[]
}

export type InvitationIntent = { token: string } | { pending_id: string; preview: InvitationPreview }

// A full OAuth navigation loses component memory. Core holds the short-lived
// invitation reference, bound to the human session and browser cookie.
export async function readPendingInvitation(): Promise<InvitationIntent | null> {
  const response = await fetch(proxyUrl("/api/invitations/pending"), { credentials: "include", cache: "no-store" })
  if ([204, 404, 405, 501].includes(response.status)) return null
  if (!response.ok) throw new Error(t("invite.unavailable"))
  const value = await response.json().catch(() => { throw new Error(t("invite.unavailable")) }) as InvitationPreview & { pending_id: string }
  if (typeof value?.pending_id !== "string" || !value.pending_id || value.pending_id.length > 256) throw new Error(t("invite.unavailable"))
  return { pending_id: value.pending_id, preview: value }
}

function unsupported(reason: unknown) {
  return reason instanceof ApiError && [404, 405, 501].includes(reason.status)
}

// The bearer invite stays in component memory, never browser storage.
// Preview and accept use JSON bodies; OAuth start follows the Core contract.
// Preview cannot accept; every accept is reached only from the Join button.
export function InvitationEntry({ intent, user, onAuthenticated, onJoined, onDismiss }: {
  intent: InvitationIntent
  user: User | null | undefined
  onAuthenticated: (user: User) => void
  onJoined: (organizationId: string) => Promise<void>
  onDismiss: () => void
}) {
  const token = "token" in intent ? intent.token : undefined
  const [preview, setPreview] = useState<InvitationPreview | null>(null)
  const [error, setError] = useState("")
  const [acceptError, setAcceptError] = useState("")
  const [authAllowed, setAuthAllowed] = useState(false)
  const [busy, setBusy] = useState(false)
  const heading = useRef<HTMLHeadingElement>(null)

  useEffect(() => {
    let disposed = false
    if (token !== undefined && (!token || token.length > 256)) { setError(t("invite.invalid")); return }
    const request = "preview" in intent ? Promise.resolve(intent.preview)
      : api<InvitationPreview>("/api/invitations/preview", { method: "POST", body: JSON.stringify({ token }), cache: "no-store" })
    request.then((value) => {
      if (disposed) return
      if (!value.capabilities?.includes("organization.invitation-accept.v1")) {
        setError(t("invite.unsupported"))
      } else if (value.status !== "valid" || !["organization", "personal"].includes(value.kind ?? "") || (value.kind === "organization" && (!value.organization_id || !value.organization_name))) {
        setError(t("invite.invalid"))
      } else setPreview(value)
    }).catch((reason) => {
      if (!disposed) setError(unsupported(reason) ? t("invite.unsupported") : reason instanceof ApiError && [400, 409, 410].includes(reason.status) ? t("invite.invalid") : t("invite.unavailable"))
    })
    return () => { disposed = true }
  }, [intent, token])

  // This is a full-page confirmation, with no background interactive elements.
  // Focus its heading again after authentication returns from the form.
  useEffect(() => { if (!authAllowed) heading.current?.focus() }, [authAllowed, preview, error])

  async function joined(organizationId: string) {
    try { await onJoined(organizationId) }
    catch { setError(t("invite.switch_failed")) }
  }

  function authenticated(value: User, mode?: AuthMode) {
    onAuthenticated(value)
    setAuthAllowed(false)
    // Registration joins atomically in Core, after consent on this preview.
    if (mode === "register" && preview?.organization_id) void joined(preview.organization_id)
    else if (preview?.kind === "personal") onDismiss()
  }

  async function decline() {
    if (busy) return
    if ("pending_id" in intent) {
      setBusy(true)
      setAcceptError("")
      try {
        const response = await fetch(proxyUrl("/api/invitations/pending"), { method: "DELETE", credentials: "include", headers: { "content-type": "application/json" } })
        if (!response.ok) throw new Error("decline_failed")
      }
      catch { setAcceptError(t("invite.decline_failed")); setBusy(false); return }
    }
    onDismiss()
  }

  async function accept() {
    if (!preview || busy) return
    if (!user) { setAuthAllowed(true); return }
    setBusy(true)
    setAcceptError("")
    try {
      const accepted = await api<{ organization_id: string }>("/api/invitations/accept", { method: "POST", body: JSON.stringify("token" in intent ? { token: intent.token } : { pending_id: intent.pending_id }) })
      if (accepted.organization_id !== preview.organization_id) { setError(t("invite.switch_failed")); return }
      await joined(preview.organization_id!)
    } catch (reason) {
      if (unsupported(reason)) setError(t("invite.unsupported"))
      else if (reason instanceof ApiError && [400, 409, 410].includes(reason.status)) setError(t("invite.invalid"))
      else setAcceptError(t("invite.accept_failed"))
    } finally { setBusy(false) }
  }

  if (!error && preview && !user && user !== undefined && (authAllowed || preview.kind === "personal")) {
    return <AuthScreen inviteToken={token} initialMode={preview.kind === "organization" ? "login" : "register"} joinOrganization={preview.kind === "organization" && authAllowed} onAuthenticated={authenticated} />
  }
  const personal = preview?.kind === "personal"
  return <main className="grid min-h-dvh place-items-center bg-sidebar p-4">
    <section role="dialog" aria-modal="true" aria-labelledby="invitation-title" aria-describedby="invitation-description" className="w-full max-w-md space-y-4 rounded-lg border bg-background p-6 shadow-sm" onKeyDown={(event) => {
      if (event.key === "Escape" && !busy) { event.preventDefault(); void decline() }
    }}>
      <h1 ref={heading} tabIndex={-1} id="invitation-title" className="break-words text-lg font-semibold outline-none">
        {error ? t("invite.unavailable_title") : preview ? personal ? t("invite.personal_title") : t("invite.join_title", { organization: preview.organization_name! }) : t("invite.loading")}
      </h1>
      <p id="invitation-description" role={error ? "alert" : undefined} className={`break-words text-sm leading-6 ${error ? "text-destructive" : "text-muted-foreground"}`}>
        {error || (personal ? t("invite.personal_signed_in") : preview ? user ? t("invite.signed_in", { email: user.email }) : t("invite.sign_in_detail") : t("invite.preview_detail"))}
      </p>
      {acceptError && <p role="alert" className="text-sm text-destructive">{acceptError}</p>}
      <div className="flex flex-wrap justify-end gap-2">
        <Button variant="outline" disabled={busy} onClick={() => void decline()}>{t("common.cancel")}</Button>
        {!error && preview?.kind === "organization" && <Button disabled={busy || user === undefined} onClick={() => void accept()}>{busy ? t("auth.wait") : t("invite.join")}</Button>}
      </div>
    </section>
  </main>
}
