import { useState } from "react"
import { api, ApiError, type PolicyProviderStatus, type PolicyPublishAuthority, type WorkspacePolicyProvider } from "@/lib/api"
import { Button } from "@/components/ui/button"
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog"

const capability = "policy.publish-authority.v1"
const guide = "https://github.com/EvoEvolver/treer/blob/main/docs/operator-guide.md#workspace-network"
const label = (value: PolicyPublishAuthority) => value === "owner_session" ? "Owner session" : "Provider operator"
const valid = (value: unknown): value is PolicyPublishAuthority => value === "owner_session" || value === "provider_operator"

export function PolicyPublishAuthoritySettings({ workspaceId, status, isOwner, disabled, onSaved }: {
  workspaceId: string; status?: PolicyProviderStatus; isOwner: boolean; disabled: boolean
  onSaved: (provider: WorkspacePolicyProvider) => void
}) {
  const [confirm, setConfirm] = useState(false)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState("")
  const [notice, setNotice] = useState("")
  const [unsupported, setUnsupported] = useState(false)
  const provider = status?.provider
  const supported = !unsupported && Array.isArray(status?.capabilities) && status.capabilities.includes(capability) && provider?.workspace_id === workspaceId && valid(provider.publish_authority)
  async function change(authority: PolicyPublishAuthority) {
    if (!isOwner || disabled || busy || !supported || !provider) return
    setBusy(true); setError(""); setNotice("")
    try {
      // The shared browser API sends the Core session cookie. Never accept or attach App/service tokens.
      const result = await api<{ provider: WorkspacePolicyProvider; publish_authority: PolicyPublishAuthority; capabilities: string[] }>(
        `/api/workspaces/${encodeURIComponent(workspaceId)}/policy-provider/publish-authority`, {
          method: "PUT", body: JSON.stringify({ expected_app_id: provider.app_id, publish_authority: authority }),
        })
      if (!Array.isArray(result?.capabilities) || !result.capabilities.includes(capability) || result.publish_authority !== authority ||
          result.provider?.publish_authority !== authority || result.provider?.app_id !== provider.app_id || result.provider?.workspace_id !== workspaceId) {
        setUnsupported(true)
        throw new Error("Core did not confirm publication authority. Upgrade all Proxy replicas and reload; no fallback was attempted.")
      }
      onSaved(result.provider)
      setNotice(`Publication authority changed to ${label(authority)} (${authority}). Core recorded policy_provider.publish_authority.set in the organization audit.`)
      setConfirm(false)
    } catch (reason) {
      if (reason instanceof ApiError && [404, 405, 501].includes(reason.status)) {
        setUnsupported(true)
        setError("Publication authority is unavailable on this Proxy. Upgrade all replicas and reload; no fallback was attempted.")
      } else setError(reason instanceof Error ? reason.message : "Unable to change publication authority")
    } finally { setBusy(false) }
  }
  return <div className="space-y-3 border-t pt-4" aria-label="Policy publication authority">
    <h3 className="text-xs font-semibold">Publication authority</h3>
    {!status ? <p className="text-xs text-muted-foreground">Publication authority unavailable until the Core binding loads.</p> : !provider ? <p className="text-xs text-muted-foreground">Configure a Policy Provider to set publication authority.</p> : !supported ? <p role="status" className="text-xs text-muted-foreground">Publication authority unavailable: this Proxy has not confirmed support. Upgrade all Proxy replicas. No fallback is available.</p> : <>
      <p className="text-xs">Current: <strong>{label(provider.publish_authority!)}</strong> <code className="break-all text-muted-foreground">({provider.publish_authority})</code></p>
      <p className="text-xs leading-5 text-muted-foreground">{provider.publish_authority === "owner_session" ? "Publishing requires a current Treer owner session and the Policy App administration token." : "Policy App administration token holders can publish v2 rules without per-request owner approval."}</p>
      {isOwner ? <Button size="sm" variant="outline" disabled={disabled || busy} onClick={() => {
        if (provider.publish_authority === "owner_session") { setError(""); setNotice(""); setConfirm(true) }
        else void change("owner_session")
      }}>{provider.publish_authority === "owner_session" ? "Enable provider operator…" : "Require owner session"}</Button> : <p className="text-xs text-muted-foreground">Read only. Only a workspace owner using a trusted Core session can change this setting.</p>}
    </>}
    {notice && <p role="status" className="break-words text-xs leading-5">{notice}</p>}
    {error && <p role="alert" className="break-words text-xs text-destructive">{error}</p>}
    <Dialog open={confirm} onOpenChange={(open) => { if (!busy) setConfirm(open) }}>
      <DialogContent className="max-h-[calc(100dvh-2rem)] overflow-y-auto">
        <DialogHeader><DialogTitle>Allow provider operator publication?</DialogTitle><DialogDescription>This grants full Policy administration authority to holders of the Policy App administration token.</DialogDescription></DialogHeader>
        <p className="text-sm leading-6">Token holders can publish v2 rules and switch to enforce without an owner approving each publication. A compromised token or administration Agent can change workspace authorization.</p>
        <p className="text-sm leading-6">Recommended: use Policy to isolate the Agent that administers Policy. Restrict its Tree path, tree moves, control of that Agent, machine exec and file access. Agents sharing an OS user are not isolated by 0600 files or loopback ports.</p>
        <a className="text-sm underline underline-offset-4" href={guide} target="_blank" rel="noreferrer">Operator guide: isolate the Policy administration Agent</a>
        {error && <p role="alert" className="break-words text-sm text-destructive">{error}</p>}
        <DialogFooter><Button variant="outline" disabled={busy} onClick={() => setConfirm(false)}>Cancel</Button><Button disabled={disabled || busy || !isOwner || !supported} onClick={() => void change("provider_operator")}>{busy ? "Saving…" : "Enable provider operator"}</Button></DialogFooter>
      </DialogContent>
    </Dialog>
  </div>
}
