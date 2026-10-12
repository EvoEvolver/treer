import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Label } from "@/components/ui/label"
import { api, proxyUrl, type User } from "@/lib/api"
import { authorizationReturnUrl } from "@/lib/control-plane"
import { cn } from "@/lib/utils"
import {
  Github,
  Mail
} from "lucide-react"
import { FormEvent, useEffect, useState } from "react"

export type AuthMode = "login" | "register" | "forgot" | "reset"

export type AuthConfig = { github: boolean; google: boolean; invitation_required: boolean }

export function AuthScreen({ onAuthenticated }: { onAuthenticated: (user: User) => void }) {
  const parameters = new URLSearchParams(window.location.search)
  const invite = parameters.get("invite")
  const resetToken = parameters.get("reset")
  const oauthError = parameters.get("oauth_error")
  const [mode, setMode] = useState<AuthMode>(resetToken ? "reset" : invite ? "register" : "login")
  const [authConfig, setAuthConfig] = useState<AuthConfig>({ github: false, google: false, invitation_required: true })
  const [email, setEmail] = useState("")
  const [preferredName, setPreferredName] = useState("")
  const [password, setPassword] = useState("")
  const [passwordConfirmation, setPasswordConfirmation] = useState("")
  const [error, setError] = useState(oauthError ? "OAuth sign-in failed. Try again." : "")
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
      setError("Passwords do not match")
      return
    }
    setSubmitting(true)
    try {
      if (mode === "forgot") {
        await api<{ ok: boolean }>("/api/auth/request-password-reset", { method: "POST", body: JSON.stringify({ email }) })
        setNotice("If an account exists for this email, a reset link has been sent.")
        return
      }
      if (mode === "reset") {
        await api<{ ok: boolean }>("/api/auth/reset-password", { method: "POST", body: JSON.stringify({ token: resetToken, password }) })
        showSignIn("Password updated. Sign in with your new password.")
        return
      }
      const registering = mode === "register"
      const path = registering ? "/api/auth/register" : "/api/auth/login"
      const body = registering ? { invite, email, preferred_name: preferredName, password } : { email, password }
      const user = await api<User>(path, { method: "POST", body: JSON.stringify(body) })
      const returnTo = authorizationReturnUrl()
      if (returnTo) {
        window.location.assign(returnTo)
        return
      }
      window.history.replaceState(null, "", window.location.pathname)
      onAuthenticated(user)
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : "Authentication failed")
    } finally {
      setSubmitting(false)
    }
  }

  function oauthUrl(provider: "github" | "google") {
    const url = new URL(proxyUrl(`/api/auth/oauth/${provider}/start`))
    if (invite) url.searchParams.set("invite", invite)
    return url.toString()
  }

  const title = mode === "register" ? "Join Treer" : mode === "forgot" ? "Reset your password" : mode === "reset" ? "Choose a new password" : "Sign in to Treer"
  const description = mode === "register" ? invite ? "Create your account from this invitation." : "Create your account and personal organization." : mode === "forgot" ? "Enter the email associated with your account." : mode === "reset" ? "Use at least 8 characters for your new password." : "Open your agent workspace."

  return <main className="grid min-h-dvh place-items-center bg-sidebar p-4">
    <form onSubmit={submit} className="w-full max-w-[390px] rounded-lg border bg-background p-7 shadow-sm">
      <div className="mb-6 grid size-9 place-items-center rounded-md bg-[#37352f] font-serif text-lg font-bold text-white">T</div>
      <h1 className="text-xl font-semibold">{title}</h1>
      <p className="mt-1 text-sm text-muted-foreground">{description}</p>
      {(mode === "login" || mode === "register") && (authConfig.github || authConfig.google) && <div className="mt-6 space-y-2">
        {authConfig.github && <Button type="button" variant="outline" className="w-full" onClick={() => { window.location.href = oauthUrl("github") }}><Github />Continue with GitHub</Button>}
        {authConfig.google && <Button type="button" variant="outline" className="w-full" onClick={() => { window.location.href = oauthUrl("google") }}><Mail />Continue with Google</Button>}
        <div className="flex items-center gap-3 py-2 text-[11px] text-muted-foreground"><span className="h-px flex-1 bg-border" /><span>or use email</span><span className="h-px flex-1 bg-border" /></div>
      </div>}
      {mode !== "reset" && <div className={cn("space-y-2", (mode === "login" || mode === "register") && (authConfig.github || authConfig.google) ? "mt-2" : "mt-6")}><Label htmlFor="email">Email</Label><Input id="email" type="email" autoComplete="email" value={email} maxLength={254} onChange={(event) => setEmail(event.target.value)} required autoFocus /></div>}
      {mode === "register" && <div className="mt-4 space-y-2"><Label htmlFor="preferred-name">Preferred name</Label><Input id="preferred-name" autoComplete="name" value={preferredName} maxLength={80} onChange={(event) => setPreferredName(event.target.value)} required /></div>}
      {(mode === "login" || mode === "register" || mode === "reset") && <div className={cn("space-y-2", mode === "reset" ? "mt-6" : "mt-4")}><Label htmlFor="password">{mode === "reset" ? "New password" : "Password"}</Label><Input id="password" type="password" autoComplete={mode === "login" ? "current-password" : "new-password"} value={password} minLength={mode === "login" ? undefined : 8} maxLength={1024} onChange={(event) => setPassword(event.target.value)} required autoFocus={mode === "reset"} /></div>}
      {mode === "reset" && <div className="mt-4 space-y-2"><Label htmlFor="password-confirmation">Confirm new password</Label><Input id="password-confirmation" type="password" autoComplete="new-password" value={passwordConfirmation} minLength={8} maxLength={1024} onChange={(event) => setPasswordConfirmation(event.target.value)} required /></div>}
      {mode === "login" && <div className="mt-2 text-right"><Button type="button" variant="ghost" size="sm" className="h-auto px-0 py-1 text-xs text-muted-foreground" onClick={() => { setMode("forgot"); setError(""); setNotice("") }}>Forgot password?</Button></div>}
      <div className="mt-3 min-h-5 text-xs text-destructive">{error}</div>
      {notice && <div className="mt-2 text-xs leading-5 text-emerald-700">{notice}</div>}
      <div className="mt-4 flex items-center justify-between gap-3">
        {(mode === "register" || mode === "forgot" || mode === "reset") && <Button type="button" variant="ghost" className="px-0 text-primary" onClick={() => showSignIn()}>Back to sign in</Button>}
        {mode === "login" && !authConfig.invitation_required && <Button type="button" variant="ghost" className="px-0 text-primary" onClick={() => { setMode("register"); setError(""); setNotice("") }}>Create account</Button>}
        <Button type="submit" className="ml-auto" disabled={submitting}>{submitting ? "Please wait" : mode === "register" ? "Create account" : mode === "forgot" ? "Send reset link" : mode === "reset" ? "Update password" : "Sign in"}</Button>
      </div>
    </form>
  </main>
}
