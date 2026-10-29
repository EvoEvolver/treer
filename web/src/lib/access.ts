import type { Member, OrganizationGroup, WorkspaceAccess } from "@/lib/api"

export type AccessVia = { kind: "org_role" | "group" | "direct"; detail: string }
export type EffectiveMember = { user: { id: string; name: string; email: string }; role: "owner" | "member"; via: AccessVia[] }
export type EffectiveAccess = { members: EffectiveMember[]; mode: WorkspaceAccess["access_mode"] }

// Mirrors Core's workspace role derivation (organizations.rs workspace_member_role):
// organization owners and admins are always workspace owners; an owner grant
// (direct or through a group) makes an owner; open workspaces and any grant make
// a member. Used to preview unsaved changes and when the Proxy predates
// GET /api/workspaces/{id}/access/effective.
export function computeEffectiveAccess(access: Pick<WorkspaceAccess, "access_mode" | "members" | "groups">, members: Member[], groups: OrganizationGroup[]): EffectiveAccess {
  const result: EffectiveMember[] = []
  for (const member of members) {
    const via: AccessVia[] = []
    let owner = false, any = false
    if (member.role === "owner" || member.role === "admin") { via.push({ kind: "org_role", detail: member.role }); owner = true; any = true }
    const direct = access.members.find((grant) => grant.user_id === member.user_id)
    if (direct) { via.push({ kind: "direct", detail: direct.role }); any = true; owner ||= direct.role === "owner" }
    for (const grant of access.groups) {
      const group = groups.find((item) => item.group_id === grant.group_id)
      if (!group?.member_ids.includes(member.user_id)) continue
      via.push({ kind: "group", detail: `${grant.name} · ${grant.role}` }); any = true; owner ||= grant.role === "owner"
    }
    if (access.access_mode === "organization") {
      if (!via.some((item) => item.kind === "org_role")) via.unshift({ kind: "org_role", detail: "organization" })
      any = true
    }
    if (any) result.push({ user: { id: member.user_id, name: member.preferred_name, email: member.email }, role: owner ? "owner" : "member", via })
  }
  result.sort((left, right) => Number(right.role === "owner") - Number(left.role === "owner") || left.user.name.localeCompare(right.user.name))
  return { members: result, mode: access.access_mode }
}

// Accepts only the documented shape; anything else is treated as unavailable.
export function decodeEffectiveAccess(value: unknown): EffectiveAccess | null {
  const body = value as EffectiveAccess | null
  if (!body || !Array.isArray(body.members) || (body.mode !== "organization" && body.mode !== "restricted")) return null
  const kinds = new Set(["org_role", "group", "direct"])
  const members = body.members.filter((item) => item && item.user && typeof item.user.id === "string" && typeof item.user.name === "string" && (item.role === "owner" || item.role === "member") && Array.isArray(item.via))
    .map((item) => ({ user: { id: item.user.id, name: item.user.name, email: typeof item.user.email === "string" ? item.user.email : "" }, role: item.role, via: item.via.filter((via) => via && kinds.has(via.kind) && typeof via.detail === "string").slice(0, 16) }))
  return { members, mode: body.mode }
}

// True when the signed-in user would stop being a workspace owner after the change.
export function losesOwnership(userId: string, next: Pick<WorkspaceAccess, "access_mode" | "members" | "groups">, members: Member[], groups: OrganizationGroup[]) {
  const me = computeEffectiveAccess(next, members, groups).members.find((item) => item.user.id === userId)
  return me?.role !== "owner"
}
