import { useEffect, useRef, useState } from "react"
import { api, ApiError, machineName, type Machine } from "@/lib/api"
import { matchingProviderRecipe, PROVIDER_CLI_CAPABILITY, providerCliBase, providerCliOperation, providerCliPrepared, providerCliRecipes, providerCliRef, providerCliTerminal,
  type ProviderCliOperation, type ProviderCliPrepared, type ProviderCliRecipe, type ProviderCliRecipes } from "@/lib/provider-cli"
import { t } from "@/i18n"
import { Button } from "@/components/ui/button"
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog"

// Scope this component by workspace/machine at its call sites. Tokens never
// leave memory. A close stops observation, not the server-side operation.
export function ProviderCliInstaller({ workspaceId, machine, command }: { workspaceId: string; machine: Machine; command?: string }) {
  const [catalog, setCatalog] = useState<ProviderCliRecipes | null>(null)
  const [available, setAvailable] = useState(false)
  const [open, setOpen] = useState(false)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState("")
  const [prepared, setPrepared] = useState<Omit<ProviderCliPrepared, "confirmation_token"> | null>(null)
  const token = useRef<string | null>(null)
  const confirmationHeading = useRef<HTMLHeadingElement>(null)
  const trigger = useRef<HTMLButtonElement>(null)
  const [operation, setOperation] = useState<ProviderCliOperation | null>(null)
  const [tracked, setTracked] = useState<{ operation_id: string; recipe: ProviderCliRecipe } | null>(null)
  const [unconfirmed, setUnconfirmed] = useState(false)
  const [probe, setProbe] = useState<{ version: string; executable: string } | null>(null)
  const [now, setNow] = useState(performance.now())
  const expires = useRef(0)
  const live = useRef(true)
  const generation = useRef(0)
  const pending = useRef(false)
  const base = providerCliBase(workspaceId, machine.server_id)
  useEffect(() => { live.current = true; return () => { live.current = false; generation.current++; token.current = null } }, [])

  useEffect(() => {
    const controller = new AbortController()
    let current = true
    void (async () => {
      try {
        const config = await api<{ capabilities?: string[] }>("/api/auth/config", { signal: controller.signal, cache: "no-store" })
        if (!config.capabilities?.includes(PROVIDER_CLI_CAPABILITY)) return
        const result = providerCliRecipes(await api<unknown>(`${base}/recipes`, { signal: controller.signal, cache: "no-store" }), machine.server_id)
        if (!result) throw new Error("invalid_response")
        if (current) { setCatalog(result); setAvailable(true) }
      } catch (reason) {
        if (current && !(reason instanceof ApiError && [404, 501].includes(reason.status))) setError(t("cli.unavailable"))
      }
    })()
    return () => { current = false; controller.abort() }
  }, [base, machine.server_id])

  useEffect(() => {
    if (!prepared) return
    confirmationHeading.current?.focus()
    const timer = window.setInterval(() => setNow(performance.now()), 1000)
    return () => window.clearInterval(timer)
  }, [prepared])

  // Poll read-only once per second while the result is being viewed. Never
  // repeat the install POST, including after transport failure or reconnect.
  useEffect(() => {
    if (!open || !tracked || unconfirmed || (operation && providerCliTerminal(operation.status))) return
    const controller = new AbortController()
    let active = true
    let timer: number | undefined
    async function poll() {
      try {
        const result = providerCliOperation(await api<unknown>(`${base}/operations/${encodeURIComponent(tracked!.operation_id)}`, { signal: controller.signal, cache: "no-store" }),
          { ...providerCliRef(tracked!.recipe), operation_id: tracked!.operation_id, machine_id: machine.server_id })
        if (!result) throw new Error("invalid_response")
        if (!active) return
        setOperation(result)
        if (!providerCliTerminal(result.status)) timer = window.setTimeout(() => void poll(), 1000)
      } catch {
        if (active) { setError(t("cli.result_unavailable")); setUnconfirmed(true) }
      }
    }
    void poll()
    return () => { active = false; controller.abort(); if (timer) window.clearTimeout(timer) }
  }, [base, machine.server_id, open, tracked, unconfirmed])

  async function reloadRecipes() {
    if (pending.current || tracked || unconfirmed) return
    pending.current = true; setBusy(true); setError("")
    try {
      const result = providerCliRecipes(await api<unknown>(`${base}/recipes`, { cache: "no-store" }), machine.server_id)
      if (!live.current) return
      if (!result) throw new Error("invalid_response")
      setCatalog(result)
    } catch (reason) { if (live.current) setError(failureText(reason)) }
    finally { pending.current = false; if (live.current) setBusy(false) }
  }

  async function prepare(recipe: ProviderCliRecipe) {
    if (!catalog || pending.current || tracked || unconfirmed) return
    pending.current = true; setBusy(true); setError(""); setProbe(null)
    const epoch = ++generation.current
    try {
      const result = providerCliPrepared(await api<unknown>(`${base}/prepare`, { method: "POST", body: JSON.stringify(providerCliRef(recipe)) }), catalog, recipe)
      if (!live.current || generation.current !== epoch) return
      if (!result) throw new Error("invalid_response")
      const { confirmation_token, ...display } = result
      token.current = confirmation_token
      // The server's DB clock owns token expiry. A local wall-clock offset
      // must not reject a fresh confirmation; this is only a relative UX cap.
      expires.current = performance.now() + 120_000
      setPrepared(display); setNow(performance.now())
    } catch (reason) { if (live.current && generation.current === epoch) setError(failureText(reason)) }
    finally { pending.current = false; if (live.current) setBusy(false) }
  }

  async function install() {
    if (pending.current || !prepared || !token.current || performance.now() >= expires.current) return
    const confirmation = token.current
    token.current = null // discard before dispatch; even an ambiguous failure cannot retry it
    const recipe = prepared.recipe
    setPrepared(null); setError(""); pending.current = true; setBusy(true)
    try {
      const result = await api<{ operation_id: string; machine_id: string; status: string }>(`${base}/install`, { method: "POST", body: JSON.stringify({ confirmation_token: confirmation }) })
      if (!live.current) return
      if (result.machine_id !== machine.server_id || !/^[A-Za-z0-9_-]{1,128}$/.test(result.operation_id) ||
        !["queued", "running", "succeeded", "failed", "timed_out", "unknown"].includes(result.status)) throw new Error("invalid_response")
      setTracked({ operation_id: result.operation_id, recipe })
      // An unknown acceptance is displayed without automatic polling/re-dispatch.
      if (result.status === "unknown") { setUnconfirmed(true); setError(t("cli.unknown")) }
    } catch (reason) {
      if (live.current) {
        setError(failureText(reason))
        // HTTP 4xx/501 means explicit refusal. Transport/5xx may have dispatched.
        if (!(reason instanceof ApiError && (reason.status >= 400 && reason.status < 500 || reason.status === 501))) setUnconfirmed(true)
      }
    } finally { pending.current = false; if (live.current) setBusy(false) }
  }

  async function refreshResult() {
    if (!tracked || pending.current) return
    pending.current = true; setBusy(true); setError("")
    try {
      const result = providerCliOperation(await api<unknown>(`${base}/operations/${encodeURIComponent(tracked.operation_id)}`, { cache: "no-store" }),
        { ...providerCliRef(tracked.recipe), operation_id: tracked.operation_id, machine_id: machine.server_id })
      if (!live.current) return
      if (!result) throw new Error("invalid_response")
      setOperation(result); setUnconfirmed(false)
      if (!providerCliTerminal(result.status)) setTracked(current => current ? { ...current } : null)
    } catch (reason) { if (live.current) setError(failureText(reason)) }
    finally { pending.current = false; if (live.current) setBusy(false) }
  }

  async function check() {
    if (!tracked || pending.current) return
    pending.current = true; setBusy(true); setError("")
    try {
      const result = await api<{ installed: boolean; found_in_prefix: boolean; version?: string; executable?: string; exit_code: number | null; timed_out: boolean }>(`${base}/check`, { method: "POST", body: JSON.stringify(providerCliRef(tracked.recipe)) })
      if (!live.current) return
      if (result.installed !== true || result.found_in_prefix !== true || result.exit_code !== 0 || result.timed_out ||
        typeof result.executable !== "string" || !result.executable.startsWith("/") || typeof result.version !== "string" || result.version.length > 256) throw new Error("check_failed")
      setProbe({ version: result.version, executable: result.executable })
    } catch (reason) { if (live.current) setError(failureText(reason)) }
    finally { pending.current = false; if (live.current) setBusy(false) }
  }

  function changeOpen(next: boolean) {
    setOpen(next)
    if (!next) { generation.current++; token.current = null; setPrepared(null) }
  }
  function cancel() { changeOpen(false) }
  const matched = command && catalog ? matchingProviderRecipe(catalog.recipes, command, "identity_removed", machine.server_id, machine.server_id) : null
  if (!available || !catalog || (command && !matched)) return null
  const recipes = command && matched ? [matched] : catalog.recipes
  return <>
    <Button ref={trigger} type="button" variant="outline" className="max-md:min-h-11" onClick={() => { changeOpen(true); if (command && matched && !tracked && !unconfirmed) void prepare(matched) }} disabled={machine.status !== "online"}>{command ? t("cli.install_provider", { provider: matched!.provider_id }) : t("cli.title")}</Button>
    <Dialog open={open} onOpenChange={changeOpen}>
      <DialogContent onCloseAutoFocus={event => { if (trigger.current) { event.preventDefault(); trigger.current.focus() } }} className="max-h-[calc(100dvh-2rem)] max-w-2xl overflow-y-auto">
        <DialogHeader><DialogTitle>{t("cli.title")}</DialogTitle><DialogDescription className="break-all">{t("cli.machine", { name: machineName(machine), id: machine.server_id, platform: catalog.platform })}</DialogDescription></DialogHeader>
        <div className="min-w-0 space-y-4 text-sm">
          {error && <p role="alert" className="break-words text-destructive">{error}</p>}
          {error && !tracked && !unconfirmed && !prepared && <Button type="button" variant="outline" className="max-md:min-h-11" disabled={busy} onClick={() => void reloadRecipes()}>{t("cli.reload_recipes")}</Button>}
          {prepared ? <>
            <h3 ref={confirmationHeading} tabIndex={-1} className="font-semibold outline-none">{t("cli.confirm", { provider: prepared.recipe.provider_id })}</h3>
            <p>{t("cli.warning", { registry: prepared.recipe.package!.registry })}</p>
            <p>{t("cli.npm_config")}</p>
            <p>{t("cli.transitive")}</p>
            {prepared.recipe.lifecycle_scripts!.enabled && <p className="font-semibold">{t("cli.scripts")}</p>}
            <ol className="space-y-2" aria-label={t("cli.plan")}>{prepared.recipe.plan!.map(item => <li key={item.step} className="min-w-0 rounded-md border bg-muted/30 p-3">
              <span className="font-medium">{t("cli.step", { step: item.step })} · {purposeLabel(item.purpose)}</span>
              {item.kind === "command" ? <div className="mt-1 space-y-1 [overflow-wrap:anywhere] font-mono text-xs"><p>{breakable(item.executable)}</p>{item.args.map((arg, i) => <p key={i}>{breakable(JSON.stringify(arg))}</p>)}</div>
                : item.kind === "verify" ? <p className="mt-1 break-all font-mono text-xs">{item.algorithm}: {item.expected}</p>
                : <p className="mt-1 break-all font-mono text-xs">{item.rule}</p>}
            </li>)}</ol>
            <p className="text-xs text-muted-foreground">{t("cli.expires", { time: new Date(prepared.expires_at).toLocaleTimeString() })}</p>
            <div className="flex flex-wrap justify-end gap-2"><Button type="button" variant="outline" className="max-md:min-h-11" onClick={cancel}>{t("common.cancel")}</Button><Button type="button" className="max-md:min-h-11" disabled={busy || now >= expires.current} onClick={() => void install()}>{t("cli.confirm_install")}</Button></div>
          </> : tracked || unconfirmed ? <>
            <div role="status" className="rounded-md border bg-muted/30 p-3 font-semibold">{unconfirmed ? t("cli.unknown") : probe ? t("cli.checked", { version: probe.version }) : t(`cli.state_${operation?.status ?? "running"}`)}</div>
            {probe && <code className="block break-all">{probe.executable}</code>}
            {(probe || operation?.status === "succeeded") && <><p>{t("cli.path_warning")}</p><p className="whitespace-pre-wrap break-words">{tracked?.recipe.login_hint}</p></>}
            {tracked && <details className="min-w-0 rounded-md border p-3 text-muted-foreground"><summary className="cursor-pointer py-1 text-sm">{t("cli.details")}</summary><code className="mt-3 block break-all text-xs">{tracked.operation_id}</code>
            {operation && providerCliTerminal(operation.status) && <div className="mt-3 space-y-3">
              {operation.error_code && <code className="block break-all text-xs">{operation.error_code}</code>}
              <p>{t("cli.npm_config")}</p>
              {(operation.steps ?? [{ step: operation.step ?? 1, stdout: operation.stdout, stderr: operation.stderr, truncated: operation.truncated }]).map(item => <div key={item.step} className="min-w-0 space-y-1"><h4>{t("cli.step", { step: item.step })}</h4><pre className="max-h-48 overflow-auto whitespace-pre-wrap break-all rounded-md border bg-muted/30 p-3 text-xs">{visibleOutput(item.stdout)}{item.stderr ? `\n${visibleOutput(item.stderr)}` : ""}</pre>{item.truncated && <p>{t("cli.truncated")}</p>}</div>)}
            </div>}</details>}
            {tracked && (unconfirmed || operation?.status === "unknown") && <Button type="button" variant="outline" className="max-md:min-h-11" disabled={busy} onClick={() => void refreshResult()}>{t("cli.refresh_result")}</Button>}
            {tracked && (unconfirmed || operation && providerCliTerminal(operation.status)) && <Button type="button" variant="outline" className="max-md:min-h-11" disabled={busy} onClick={() => void check()}>{t("cli.check")}</Button>}
            <p className="text-xs text-muted-foreground">{t("cli.close_hint")}</p>
          </> : <div className="space-y-3">{recipes.map(recipe => <div key={recipe.recipe_id} className="flex flex-wrap items-center justify-between gap-3 rounded-md border p-3"><div className="min-w-0"><h3 className="font-medium">{recipe.provider_id}</h3><p className="break-words text-xs text-muted-foreground">{recipe.supported ? `${recipe.package!.version} · Node ≥ ${recipe.node_min}` : unsupportedText(recipe.unsupported_reason)}</p></div>{recipe.supported && <Button type="button" variant="outline" className="max-md:min-h-11" disabled={busy} onClick={() => void prepare(recipe)}>{t("cli.install_provider", { provider: recipe.provider_id })}</Button>}</div>)}</div>}
        </div>
        {!prepared && <DialogFooter><Button type="button" variant="outline" className="max-md:min-h-11" onClick={() => changeOpen(false)}>{t("org.close")}</Button></DialogFooter>}
      </DialogContent>
    </Dialog>
  </>
}
function breakable(value: string) {
  return value.split(/(?<=[/=-])/).map((part, i) => <span key={i}>{part}<wbr /></span>)
}
function visibleOutput(value: string) {
  // Keep output literal and readable, without interpreting terminal controls.
  return value.replace(/[\x00-\x08\x0b-\x1f\x7f-\x9f\u200b-\u200f\u202a-\u202e\u2066-\u2069\ufeff]/g, char => `\\u${char.charCodeAt(0).toString(16).padStart(4, "0")}`)
}
function failureText(reason: unknown) {
  if (reason instanceof ApiError) {
    if (reason.status === 403) return t("cli.denied")
    if (reason.status === 409) return t("cli.changed")
    if (reason.status === 429) return t("cli.limited")
    if ([404, 501].includes(reason.status)) return t("cli.unavailable")
    if (reason.status === 410) return t("cli.expired")
  }
  return t("cli.failed")
}
function unsupportedText(reason?: string) {
  if (reason?.startsWith("windows")) return t("cli.windows")
  if (reason?.includes("lifecycle")) return t("cli.lifecycle_pending")
  return t("cli.unsupported")
}

function purposeLabel(purpose: string) {
  switch (purpose) {
    case "pack": return t("cli.pack")
    case "integrity": return t("cli.integrity")
    case "install": return t("cli.install")
    case "prefix": return t("cli.prefix")
    case "check": return t("cli.probe")
    default: return purpose
  }
}
