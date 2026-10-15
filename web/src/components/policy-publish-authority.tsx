import { useState } from "react"
import { api, ApiError, type PolicyProviderStatus, type PolicyPublishAuthority, type WorkspacePolicyProvider } from "@/lib/api"
import { Button } from "@/components/ui/button"
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog"
import { t } from "@/i18n"

const capability = "policy.publish-authority.v1"
const guide = "https://github.com/EvoEvolver/treer/blob/main/docs/operator-guide.md#workspace-network"
const label = (value: PolicyPublishAuthority) => value === "owner_session" ? t("policy.owner_session") : t("policy.provider_operator")
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
        throw new Error(t("policy.core_did_not_confirm_publication"))
      }
      onSaved(result.provider)
      setNotice(t("policy.changed", { label: label(authority), authority }))
      setConfirm(false)
    } catch (reason) {
      if (reason instanceof ApiError && [404, 405, 501].includes(reason.status)) {
        setUnsupported(true)
        setError(t("policy.publication_authority_is_unavailable_on"))
      } else setError(reason instanceof Error ? reason.message : t("policy.unable_to_change_publication_authority"))
    } finally { setBusy(false) }
  }
  return <div className="space-y-3 border-t pt-4" aria-label={t("policy.policy_publication_authority")}>
    <h3 className="text-xs font-semibold">{t("policy.publication_authority")}</h3>
    {!status ? <p className="text-xs text-muted-foreground">{t("policy.publication_authority_unavailable_until_the")}</p> : !provider ? <p className="text-xs text-muted-foreground">{t("policy.configure_a_policy_provider_to")}</p> : !supported ? <p role="status" className="text-xs text-muted-foreground">{t("policy.publication_authority_unavailable_this_proxy")}</p> : <>
      <p className="text-xs">{t("policy.current")} <strong>{label(provider.publish_authority!)}</strong> <code className="break-all text-muted-foreground">({provider.publish_authority})</code></p>
      <p className="text-xs leading-5 text-muted-foreground">{provider.publish_authority === "owner_session" ? t("policy.publishing_requires_a_current_treer") : t("policy.policy_app_administration_token_holders")}</p>
      {isOwner ? <Button size="sm" variant="outline" disabled={disabled || busy} onClick={() => {
        if (provider.publish_authority === "owner_session") { setError(""); setNotice(""); setConfirm(true) }
        else void change("owner_session")
      }}>{provider.publish_authority === "owner_session" ? t("policy.enable_provider_operator") : t("policy.require_owner_session")}</Button> : <p className="text-xs text-muted-foreground">{t("policy.read_only_only_a_workspace")}</p>}
    </>}
    {notice && <p role="status" className="break-words text-xs leading-5">{notice}</p>}
    {error && <p role="alert" className="break-words text-xs text-destructive">{error}</p>}
    <Dialog open={confirm} onOpenChange={(open) => { if (!busy) setConfirm(open) }}>
      <DialogContent className="max-h-[calc(100dvh-2rem)] overflow-y-auto">
        <DialogHeader><DialogTitle className="pr-6 leading-6">{t("policy.allow_provider_operator_publication")}</DialogTitle><DialogDescription>{t("policy.this_grants_full_policy_administration")}</DialogDescription></DialogHeader>
        <p className="text-sm leading-6">{t("policy.token_holders_can_publish_v2")}</p>
        <p className="text-sm leading-6">{t("policy.recommended_use_policy_to_isolate")}</p>
        <a className="text-sm underline underline-offset-4" href={guide} target="_blank" rel="noreferrer">{t("policy.operator_guide_isolate_the_policy")}</a>
        {error && <p role="alert" className="break-words text-sm text-destructive">{error}</p>}
        <DialogFooter><Button variant="outline" disabled={busy} onClick={() => setConfirm(false)}>{t("common.cancel")}</Button><Button disabled={disabled || busy || !isOwner || !supported} onClick={() => void change("provider_operator")}>{busy ? t("policy.saving") : t("policy.enable_provider_operator_2")}</Button></DialogFooter>
      </DialogContent>
    </Dialog>
  </div>
}
