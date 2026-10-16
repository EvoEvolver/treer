import { useEffect, useRef, useState } from "react"
import { api, ApiError } from "@/lib/api"
import type { AgentCreationFailure } from "@/lib/agent-creation-failure"
import { t } from "@/i18n"
import { Button } from "@/components/ui/button"
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog"

export function AgentReservationRecovery({ workspaceId, failure, onCleaned }: { workspaceId: string; failure: AgentCreationFailure; onCleaned: () => void }) {
  const [confirm, setConfirm] = useState(false)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState("")
  const live = useRef(true)
  useEffect(() => { live.current = true; return () => { live.current = false } }, [])

  async function cleanup() {
    if (busy) return
    setBusy(true); setError("")
    try {
      const result = await api<{ agent_id: string; deleted: boolean }>(`/api/workspaces/${encodeURIComponent(workspaceId)}/agent-reservations/${encodeURIComponent(failure.agent_id)}/cleanup`, { method: "POST", body: "{}" })
      if (!live.current) return
      if (result.agent_id !== failure.agent_id || result.deleted !== true) throw new Error(t("provider.cleanup_unconfirmed"))
      setConfirm(false)
      onCleaned()
    } catch (reason) {
      if (!live.current) return
      const key = reason instanceof ApiError && reason.status === 501 ? "provider.cleanup_unavailable"
        : reason instanceof ApiError && reason.status === 409 ? "provider.cleanup_observed"
        : reason instanceof ApiError && reason.status === 404 ? "provider.cleanup_not_found" : null
      setError(key ? t(key) : reason instanceof Error ? reason.message : t("provider.cleanup_unconfirmed"))
    } finally { if (live.current) setBusy(false) }
  }

  return <div className="min-w-0 space-y-2 rounded-md border p-3 text-sm">
    <p>{t("provider.reservation_unknown")}</p>
    <code className="block break-all text-xs">{failure.agent_id}</code>
    <Button type="button" variant="outline" className="max-md:min-h-11" onClick={() => { setError(""); setConfirm(true) }}>{t("provider.cleanup")}</Button>
    <Dialog open={confirm} onOpenChange={(open) => { if (!busy) setConfirm(open) }}>
      <DialogContent>
        <DialogHeader><DialogTitle>{t("provider.cleanup")}</DialogTitle><DialogDescription>{t("provider.cleanup_detail")}</DialogDescription></DialogHeader>
        <code className="break-all text-xs">{failure.agent_id}</code>
        {error && <p role="alert" className="break-words text-sm text-destructive">{error}</p>}
        <DialogFooter>
          <Button type="button" variant="outline" className="max-md:min-h-11" disabled={busy} onClick={() => setConfirm(false)}>{t("common.cancel")}</Button>
          <Button type="button" variant="destructive" className="max-md:min-h-11" disabled={busy} onClick={() => void cleanup()}>{busy ? t("provider.cleaning") : t("provider.confirm_cleanup")}</Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  </div>
}
