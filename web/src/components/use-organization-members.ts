import { api,type Member,type OrganizationGroup } from "@/lib/api"
import { PREVIEW_USER,type MainView } from "@/lib/control-plane"
import { useCallback,useEffect,useState,type FormEvent,type RefObject } from "react"

interface OrganizationMembersOptions {
  preview: boolean
  organizationId: string | null
  organizationIdRef: RefObject<string | null>
  mainView: MainView
  showError: (reason: unknown) => void
}

export function useOrganizationMembers({ preview, organizationId, organizationIdRef, mainView, showError }: OrganizationMembersOptions) {
  const [members, setMembers] = useState<Member[]>([])
  const [groups, setGroups] = useState<OrganizationGroup[]>([])
  const [groupName, setGroupName] = useState("")
  const loadOrganizationSettings = useCallback(async () => {
    if (preview) {
      setMembers([{ ...PREVIEW_USER, role: "owner" }])
      setGroups([])
      return
    }
    const requestedOrganizationId = organizationId
    if (!requestedOrganizationId) {
      setMembers([])
      setGroups([])
      return
    }
    const [memberData, groupData] = await Promise.all([
      api<{ members: Member[] }>(`/api/organizations/${encodeURIComponent(requestedOrganizationId)}/members`),
      api<{ groups: OrganizationGroup[] }>(`/api/organizations/${encodeURIComponent(requestedOrganizationId)}/groups`),
    ])
    if (organizationIdRef.current !== requestedOrganizationId) return
    setMembers(memberData.members)
    setGroups(groupData.groups)
  }, [preview, organizationId])

  useEffect(() => {
    if (mainView === "organization") loadOrganizationSettings().catch(showError)
  }, [mainView, loadOrganizationSettings, showError])

  async function createGroup(event: FormEvent) {
    event.preventDefault()
    if (!organizationId) return
    try {
      await api(`/api/organizations/${encodeURIComponent(organizationId)}/groups`, { method: "POST", body: JSON.stringify({ name: groupName }) })
      setGroupName("")
      await loadOrganizationSettings()
    } catch (reason) { showError(reason) }
  }

  async function deleteGroup(groupId: string) {
    if (!organizationId) return
    try {
      await api(`/api/organizations/${encodeURIComponent(organizationId)}/groups/${encodeURIComponent(groupId)}`, { method: "DELETE" })
      await loadOrganizationSettings()
    } catch (reason) { showError(reason) }
  }

  async function setGroupMember(groupId: string, userId: string, present: boolean) {
    if (!organizationId) return
    try {
      const data = await api<{ groups: OrganizationGroup[] }>(`/api/organizations/${encodeURIComponent(organizationId)}/groups/${encodeURIComponent(groupId)}/members/${encodeURIComponent(userId)}`, { method: present ? "PUT" : "DELETE" })
      setGroups(data.groups)
    } catch (reason) { showError(reason) }
  }

  async function updateMemberRole(userId: string, role: Member["role"]) {
    if (!organizationId) return
    try {
      await api(`/api/organizations/${encodeURIComponent(organizationId)}/members/${encodeURIComponent(userId)}`, { method: "PATCH", body: JSON.stringify({ role }) })
      await loadOrganizationSettings()
    } catch (reason) { showError(reason) }
  }

  async function removeMember(userId: string) {
    if (!organizationId) return
    try {
      await api(`/api/organizations/${encodeURIComponent(organizationId)}/members/${encodeURIComponent(userId)}`, { method: "DELETE" })
      await loadOrganizationSettings()
    } catch (reason) { showError(reason) }
  }

  return { members, groups, groupName, setGroupName, createGroup, deleteGroup, setGroupMember, updateMemberRole, removeMember }
}
