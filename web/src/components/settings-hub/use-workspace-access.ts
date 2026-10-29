import { useEffect, useState } from "react"
import { api, type Member, type OrganizationGroup, type Workspace, type WorkspaceAccess } from "@/lib/api"
import { computeEffectiveAccess, decodeEffectiveAccess, type EffectiveAccess } from "@/lib/access"

export type WorkspaceAccessEntry = { access?: WorkspaceAccess; effective?: EffectiveAccess | null; error?: boolean }

// Who can open each workspace and with which role, used by the org pages
// (role badges, per-member workspace counts, what a removal takes away).
// Prefers Core's /access/effective; falls back to computing it from /access.
export function useWorkspaceAccessIndex(workspaces: Workspace[], enabled: boolean, preview: boolean, revision = 0) {
  const [index, setIndex] = useState<Record<string, WorkspaceAccessEntry>>({})
  const key = workspaces.map((workspace) => workspace.workspace_id).join("\n")
  useEffect(() => {
    if (!enabled || preview || !key) return
    let active = true
    for (const id of key.split("\n").slice(0, 50)) {
      const path = `/api/workspaces/${encodeURIComponent(id)}`
      Promise.all([
        api<{ access: WorkspaceAccess }>(`${path}/access`).then((data) => data.access, () => undefined),
        api<unknown>(`${path}/access/effective`).then(decodeEffectiveAccess, () => null),
      ]).then(([access, effective]) => { if (active) setIndex((current) => ({ ...current, [id]: { access, effective, error: !access && !effective } })) })
    }
    return () => { active = false }
  }, [enabled, preview, key, revision])
  return index
}

export function effectiveFor(entry: WorkspaceAccessEntry | undefined, members: Member[], groups: OrganizationGroup[]): EffectiveAccess | null {
  if (!entry) return null
  if (entry.effective) return entry.effective
  return entry.access ? computeEffectiveAccess(entry.access, members, groups) : null
}

export function workspacesOf(userId: string, workspaces: Workspace[], index: Record<string, WorkspaceAccessEntry>, members: Member[], groups: OrganizationGroup[]) {
  return workspaces.filter((workspace) => effectiveFor(index[workspace.workspace_id], members, groups)?.members.some((member) => member.user.id === userId))
}
