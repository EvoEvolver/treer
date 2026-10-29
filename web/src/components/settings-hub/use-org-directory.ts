import { useCallback, useEffect, useRef, useState } from "react"
import { api, type Member, type OrganizationGroup } from "@/lib/api"
import { PREVIEW_USER } from "@/lib/control-plane"

export type OrgDirectory = {
  organizationId: string | null
  members: Member[]
  groups: OrganizationGroup[]
  loaded: boolean
  error: string | null
  reload: () => Promise<void>
  setGroups: (groups: OrganizationGroup[]) => void
}

// Members and groups of one organization, loaded once per organization and on demand.
export function useOrgDirectory(organizationId: string | null, enabled: boolean, preview: boolean): OrgDirectory {
  const [state, setState] = useState<{ organizationId: string | null; members: Member[]; groups: OrganizationGroup[]; loaded: boolean; error: string | null }>({ organizationId: null, members: [], groups: [], loaded: false, error: null })
  const current = useRef(organizationId)
  current.current = organizationId
  const reload = useCallback(async () => {
    const requested = organizationId
    if (preview) { setState({ organizationId: requested, members: [{ ...PREVIEW_USER, role: "owner" }], groups: [], loaded: true, error: null }); return }
    if (!requested) { setState({ organizationId: null, members: [], groups: [], loaded: true, error: null }); return }
    try {
      const [memberData, groupData] = await Promise.all([
        api<{ members: Member[] }>(`/api/organizations/${encodeURIComponent(requested)}/members`),
        api<{ groups: OrganizationGroup[] }>(`/api/organizations/${encodeURIComponent(requested)}/groups`),
      ])
      if (current.current !== requested) return
      setState({ organizationId: requested, members: memberData.members, groups: groupData.groups, loaded: true, error: null })
    } catch (reason) {
      if (current.current !== requested) return
      setState((previous) => ({ ...previous, organizationId: requested, loaded: true, error: reason instanceof Error ? reason.message : String(reason) }))
    }
  }, [organizationId, preview])
  useEffect(() => {
    if (!enabled || state.organizationId === organizationId && state.loaded) return
    void reload()
  }, [enabled, organizationId, reload]) // eslint-disable-line react-hooks/exhaustive-deps
  const same = state.organizationId === organizationId
  return {
    organizationId,
    members: same ? state.members : [],
    groups: same ? state.groups : [],
    loaded: same && state.loaded,
    error: same ? state.error : null,
    reload,
    setGroups: (groups) => setState((previous) => ({ ...previous, groups })),
  }
}
