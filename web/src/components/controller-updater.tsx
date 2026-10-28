import { useCallback, useEffect, useState } from "react"
import { api, ApiError, type Machine } from "@/lib/api"
import { t } from "@/i18n"
import { Button } from "@/components/ui/button"

export const CONTROLLER_UPDATE_CAPABILITY = "machine.controller-update.v2"

export type ControllerUpdateState = "idle" | "downloading" | "installing" | "restarting" | "failed" | "done"
export type ControllerUpdateStatus = { state: ControllerUpdateState; reason?: string; target_version?: string; controller_version?: string }

const ACTIVE: ControllerUpdateState[] = ["downloading", "installing", "restarting"]
const RELEASE_TAG = /^v\d+\.\d+\.\d+(-[0-9A-Za-z.-]+)?$/

// "Update machine M to release V". The Controller downloads and verifies the
// signed release, replaces its binaries and restarts through its Host; the
// Proxy reports done once the machine reconnects with that version.
export function ControllerUpdater({ workspaceId, machine }: { workspaceId: string; machine: Machine }) {
  const supported = machine.capabilities?.includes(CONTROLLER_UPDATE_CAPABILITY) ?? false
  const base = `/api/workspaces/${encodeURIComponent(workspaceId)}/machines/${encodeURIComponent(machine.server_id)}/controller-update`
  const [version, setVersion] = useState("")
  const [status, setStatus] = useState<ControllerUpdateStatus | null>(null)
  const [error, setError] = useState("")
  const [busy, setBusy] = useState(false)
  const active = status ? ACTIVE.includes(status.state) : false

  const load = useCallback(async (signal?: AbortSignal) => {
    try {
      setStatus(await api<ControllerUpdateStatus>(base, { signal, cache: "no-store" }))
    } catch (reason) {
      if (!signal?.aborted) setError(reason instanceof Error ? reason.message : t("controller_update.failed"))
    }
  }, [base])

  useEffect(() => {
    if (!supported) return
    const controller = new AbortController()
    void load(controller.signal)
    return () => controller.abort()
  }, [supported, load])

  useEffect(() => {
    if (!active) return
    const controller = new AbortController()
    const timer = window.setInterval(() => void load(controller.signal), 2000)
    return () => { window.clearInterval(timer); controller.abort() }
  }, [active, load])

  if (!supported) return <p className="max-w-xs text-xs text-muted-foreground">{t("controller_update.manual")}</p>

  const submit = async () => {
    setBusy(true)
    setError("")
    try {
      setStatus(await api<ControllerUpdateStatus>(base, { method: "POST", body: JSON.stringify({ version: version.trim() }) }))
    } catch (reason) {
      setError(reason instanceof ApiError || reason instanceof Error ? reason.message : t("controller_update.failed"))
    } finally {
      setBusy(false)
    }
  }

  const valid = RELEASE_TAG.test(version.trim())
  const state = status?.state ?? "idle"
  return <form className="flex flex-col gap-2" aria-label={t("controller_update.title")} onSubmit={(event) => { event.preventDefault(); if (valid && !active) void submit() }}>
    <div className="flex flex-wrap items-center gap-2">
      <input className="h-9 w-32 rounded-md border bg-background px-2 font-mono text-sm max-md:min-h-11" placeholder="v1.2.3" aria-label={t("controller_update.version")} value={version} onChange={(event) => setVersion(event.target.value)} disabled={busy || active} />
      <Button type="submit" variant="outline" className="max-md:min-h-11" disabled={!valid || busy || active || machine.status !== "online"}>{t("controller_update.title")}</Button>
    </div>
    {state !== "idle" && <p role="status" className={`text-xs ${state === "failed" ? "text-destructive" : "text-muted-foreground"}`}>
      {t(`controller_update.state_${state}`, { version: status?.target_version ?? "" })}{state === "failed" && status?.reason ? ` ${status.reason}` : ""}
    </p>}
    {error && <p role="alert" className="text-xs text-destructive">{error}</p>}
  </form>
}
