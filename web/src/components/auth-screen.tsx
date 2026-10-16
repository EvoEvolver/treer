import { oauthAuthorizationUrl } from "@/lib/oauth-start"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Label } from "@/components/ui/label"
import { api, type User } from "@/lib/api"
import { authorizationReturnUrl } from "@/lib/control-plane"
import { cn } from "@/lib/utils"
import {
  Github,
  Mail
} from "lucide-react"
import { FormEvent, useEffect, useState } from "react"
import { LOCALES, LOCALE_LABELS, setLocale, useLocale, useT } from "@/i18n"

export type AuthMode = "login" | "register" | "forgot" | "reset"

export type AuthConfig = { github: boolean; google: boolean; invitation_required: boolean }

export function AuthScreen({ onAuthenticated, inviteToken, initialMode, joinOrganization = false }: {
  onAuthenticated: (user: User, mode?: AuthMode) => void
  inviteToken?: string
  initialMode?: AuthMode
  joinOrganization?: boolean
}) {
  const t = useT()
  const locale = useLocale()
  const parameters = new URLSearchParams(window.location.search)
  const invite = inviteToken ?? parameters.get("invite")
  const resetToken = parameters.get("reset")
  const oauthError = parameters.get("oauth_error")
  const [mode, setMode] = useState<AuthMode>(resetToken ? "reset" : initialMode ?? (invite ? "register" : "login"))
  const [authConfig, setAuthConfig] = useState<AuthConfig>({ github: false, google: false, invitation_required: true })
  const [email, setEmail] = useState("")
  const [preferredName, setPreferredName] = useState("")
  const [password, setPassword] = useState("")
  const [passwordConfirmation, setPasswordConfirmation] = useState("")
  const [error, setError] = useState(oauthError ? t("auth.err.oauth") : "")
  const [notice, setNotice] = useState("")
  const [submitting, setSubmitting] = useState(false)

  useEffect(() => {
    api<AuthConfig>("/api/auth/config").then(setAuthConfig).catch(() => undefined)
    if (oauthError) window.history.replaceState(null, "", window.location.pathname)
  }, [oauthError])

  function showSignIn(message = "") {
    window.history.replaceState(null, "", window.location.pathname)
    setMode("login")
    setPassword("")
    setPasswordConfirmation("")
    setError("")
    setNotice(message)
  }

  async function submit(event: FormEvent) {
    event.preventDefault()
    setError("")
    setNotice("")
    if (mode === "reset" && password !== passwordConfirmation) {
      setError(t("auth.err.mismatch"))
      return
    }
    setSubmitting(true)
    try {
      if (mode === "forgot") {
        await api<{ ok: boolean }>("/api/auth/request-password-reset", { method: "POST", body: JSON.stringify({ email }) })
        setNotice(t("auth.notice.resetSent"))
        return
      }
      if (mode === "reset") {
        await api<{ ok: boolean }>("/api/auth/reset-password", { method: "POST", body: JSON.stringify({ token: resetToken, password }) })
        showSignIn(t("auth.notice.passwordUpdated"))
        return
      }
      const registering = mode === "register"
      const path = registering ? "/api/auth/register" : "/api/auth/login"
      const body = registering ? { invite, email, preferred_name: preferredName, password, ...(joinOrganization ? { join_organization: true } : {}) } : { email, password }
      const user = await api<User>(path, { method: "POST", body: JSON.stringify(body) })
      // The invitation entry owns the post-auth confirmation and organization
      // switch. Finish it before following any authorization return URL.
      const returnTo = inviteToken === undefined ? authorizationReturnUrl() : null
      if (returnTo) {
        window.location.assign(returnTo)
        return
      }
      if (inviteToken === undefined) window.history.replaceState(null, "", window.location.pathname)
      onAuthenticated(user, mode)
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : t("auth.err.failed"))
    } finally {
      setSubmitting(false)
    }
  }

  async function startOAuth(provider: "github" | "google") {
    if (submitting) return
    const path = `/api/auth/oauth/${provider}/start`
    setSubmitting(true)
    setError("")
    try {
      const { authorization_url } = await api<{ authorization_url: string }>(path, {
        method: "POST", headers: { "X-Treer-Console": "1" },
        body: JSON.stringify(invite ? { invite_token: invite, join_organization: joinOrganization } : {}),
      })
      window.location.assign(oauthAuthorizationUrl(provider, authorization_url))
    } catch { setError(t("auth.err.oauth")) }
    finally { setSubmitting(false) }
  }

  const title = t(`auth.title.${mode}`)
  const description = mode === "register" ? t(invite ? "auth.desc.registerInvite" : "auth.desc.register") : t(`auth.desc.${mode}`)

  return <main className="flex min-h-dvh flex-col items-center justify-center bg-sidebar p-4">
    <form onSubmit={submit} className="w-full max-w-[390px] rounded-lg border bg-background p-7 shadow-sm">
      <div className="mb-6 grid size-9 place-items-center rounded-md bg-[#37352f] font-serif text-lg font-bold text-white">T</div>
      <h1 className="text-xl font-semibold">{title}</h1>
      <p className="mt-1 text-sm text-muted-foreground">{description}</p>
      {(mode === "login" || mode === "register") && (authConfig.github || authConfig.google) && <div className="mt-6 space-y-2">
        {authConfig.github && <Button type="button" variant="outline" className="w-full" disabled={submitting} onClick={() => void startOAuth("github")}><Github />{t("auth.github")}</Button>}
        {authConfig.google && <Button type="button" variant="outline" className="w-full" disabled={submitting} onClick={() => void startOAuth("google")}><Mail />{t("auth.google")}</Button>}
        <div className="flex items-center gap-3 py-2 text-[11px] text-muted-foreground"><span className="h-px flex-1 bg-border" /><span>{t("auth.orEmail")}</span><span className="h-px flex-1 bg-border" /></div>
      </div>}
      {mode !== "reset" && <div className={cn("space-y-2", (mode === "login" || mode === "register") && (authConfig.github || authConfig.google) ? "mt-2" : "mt-6")}><Label htmlFor="email">{t("auth.email")}</Label><Input id="email" type="email" autoComplete="email" value={email} maxLength={254} onChange={(event) => setEmail(event.target.value)} required autoFocus /></div>}
      {mode === "register" && <div className="mt-4 space-y-2"><Label htmlFor="preferred-name">{t("auth.preferredName")}</Label><Input id="preferred-name" autoComplete="name" value={preferredName} maxLength={80} onChange={(event) => setPreferredName(event.target.value)} required /></div>}
      {(mode === "login" || mode === "register" || mode === "reset") && <div className={cn("space-y-2", mode === "reset" ? "mt-6" : "mt-4")}><Label htmlFor="password">{mode === "reset" ? t("auth.newPassword") : t("auth.password")}</Label><Input id="password" type="password" autoComplete={mode === "login" ? "current-password" : "new-password"} value={password} minLength={mode === "login" ? undefined : 8} maxLength={1024} onChange={(event) => setPassword(event.target.value)} required autoFocus={mode === "reset"} /></div>}
      {mode === "reset" && <div className="mt-4 space-y-2"><Label htmlFor="password-confirmation">{t("auth.confirmPassword")}</Label><Input id="password-confirmation" type="password" autoComplete="new-password" value={passwordConfirmation} minLength={8} maxLength={1024} onChange={(event) => setPasswordConfirmation(event.target.value)} required /></div>}
      {mode === "login" && <div className="mt-2 text-right"><Button type="button" variant="ghost" size="sm" className="h-auto px-0 py-1 text-xs text-muted-foreground" onClick={() => { setMode("forgot"); setError(""); setNotice("") }}>{t("auth.forgotLink")}</Button></div>}
      <div className="mt-3 min-h-5 text-xs text-destructive">{error}</div>
      {notice && <div className="mt-2 text-xs leading-5 text-emerald-700">{notice}</div>}
      <div className="mt-4 flex items-center justify-between gap-3">
        {(mode === "register" || mode === "forgot" || mode === "reset") && <Button type="button" variant="ghost" className="px-0 text-primary" onClick={() => showSignIn()}>{t("auth.backToSignIn")}</Button>}
        {mode === "login" && (!authConfig.invitation_required || Boolean(invite)) && <Button type="button" variant="ghost" className="px-0 text-primary" onClick={() => { setMode("register"); setError(""); setNotice("") }}>{t("auth.createAccount")}</Button>}
        <Button type="submit" className="ml-auto" disabled={submitting}>{submitting ? t("auth.wait") : mode === "register" ? t("auth.createAccount") : mode === "forgot" ? t("auth.sendReset") : mode === "reset" ? t("auth.updatePassword") : t("auth.signIn")}</Button>
      </div>
    </form>
    <div role="group" aria-label={t("common.language")} className="mt-3 flex justify-center gap-3 text-xs text-muted-foreground">
      {LOCALES.map((code) => <button key={code} type="button" aria-pressed={locale === code} onClick={() => setLocale(code)} className={cn("rounded px-1.5 py-1", locale === code ? "font-semibold text-foreground" : "hover:text-foreground")}>{LOCALE_LABELS[code]}</button>)}
    </div>
  </main>
}
