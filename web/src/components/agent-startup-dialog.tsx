import { useEffect, useState } from "react"
import { getAgentStartup, restartAgentWithStartup, type Agent } from "@/lib/api"
import { agentPath } from "@/lib/workload-path"
import { formatCommandLine, parseCommandLine } from "@/lib/command-line"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Textarea } from "@/components/ui/textarea"
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog"
import { t, tnodes } from "@/i18n"

export function AgentStartupDialog({ workspaceId, agent, onClose, onSaved }: { workspaceId: string; agent: Agent; onClose: () => void; onSaved: () => void }) {
  const [command, setCommand] = useState("")
  const [cwd, setCwd] = useState("")
  const [loading, setLoading] = useState(true)
  const [saving, setSaving] = useState(false)
  const [error, setError] = useState("")
  useEffect(() => {
    let active = true
    void getAgentStartup(workspaceId, agent.agent_id).then((spec) => {
      if (active) { setCommand(formatCommandLine(spec.command, spec.args)); setCwd(spec.cwd) }
    }).catch((reason) => { if (active) setError(reason instanceof Error ? reason.message : t("startup.unable_to_edit_startup_command")) }).finally(() => { if (active) setLoading(false) })
    return () => { active = false }
  }, [workspaceId, agent.agent_id])
  async function save() {
    setSaving(true); setError("")
    try {
      const parsed = parseCommandLine(command)
      await restartAgentWithStartup(workspaceId, agent.agent_id, { ...parsed, cwd })
      onSaved(); onClose()
    } catch (reason) { setError(reason instanceof Error ? reason.message : t("startup.unable_to_edit_startup_command")) } finally { setSaving(false) }
  }
  return <Dialog open onOpenChange={(open) => { if (!open && !saving) onClose() }}><DialogContent className="max-h-[calc(100dvh-2rem)] min-w-0 overflow-y-auto p-4 sm:p-6">
    <DialogHeader><DialogTitle>{t("startup.edit_startup_command")}</DialogTitle><DialogDescription>{tnodes("startup.intro", { path: <span className="break-all">{agentPath(agent)}</span> })}</DialogDescription></DialogHeader>
    {loading ? <p role="status">{t("startup.loading_startup_command")}</p> : <>
      <label className="min-w-0 space-y-2 text-sm">{t("apps.command")}<Textarea aria-label={t("startup.startup_command")} value={command} onChange={(event) => setCommand(event.target.value)} disabled={saving} className="min-h-28 max-h-[40dvh] resize-y font-mono" /></label>
      <label className="min-w-0 space-y-2 text-sm">{t("apps.working_directory")}<Input aria-label={t("startup.startup_working_directory")} value={cwd} onChange={(event) => setCwd(event.target.value)} disabled={saving} className="font-mono" /></label>
    </>}
    {error && <p role="alert" className="text-sm text-destructive break-words">{error}</p>}
    <DialogFooter><Button variant="outline" disabled={saving} onClick={onClose}>{t("common.cancel")}</Button><Button disabled={loading || saving || !command.trim()} onClick={() => void save()}>{saving ? t("startup.restarting") : t("startup.save_and_restart")}</Button></DialogFooter>
  </DialogContent></Dialog>
}
