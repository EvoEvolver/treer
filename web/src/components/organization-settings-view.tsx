import { EmptyState, Field, IconButton } from "@/components/control-plane-ui"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select"
import { type Member, type Organization, type OrganizationGroup, type Workspace } from "@/lib/api"
import {
  Building2,
  FolderKanban,
  Plus,
  ScrollText,
  ShieldCheck,
  Trash2,
  UserRound,
  Users
} from "lucide-react"
import { FormEvent } from "react"

export type OrganizationSettingsViewProps = {
  organization?: Organization
  name: string
  workspaces: Workspace[]
  selectedWorkspaceId: string | null
  members: Member[]
  groups: OrganizationGroup[]
  groupName: string
  canManage: boolean
  currentRole: Organization["role"]
  onNameChange: (value: string) => void
  onRename: (event: FormEvent) => void
  onCreateOrganization: () => void
  onCreateWorkspace: () => void
  onSelectWorkspace: (workspaceId: string) => void
  onInvite: () => void
  onMemberRoleChange: (userId: string, role: Member["role"]) => void
  onRemoveMember: (userId: string) => void
  onGroupNameChange: (value: string) => void
  onCreateGroup: (event: FormEvent) => void
  onDeleteGroup: (groupId: string) => void
  onSetGroupMember: (groupId: string, userId: string, present: boolean) => void
  onOpenAudit: () => void
  onClose: () => void
}

export function OrganizationSettingsView({ organization, name, workspaces, selectedWorkspaceId, members, groups, groupName, canManage, currentRole, onNameChange, onRename, onCreateOrganization, onCreateWorkspace, onSelectWorkspace, onInvite, onMemberRoleChange, onRemoveMember, onGroupNameChange, onCreateGroup, onDeleteGroup, onSetGroupMember, onOpenAudit, onClose }: OrganizationSettingsViewProps) {
  if (!organization) return <div className="grid min-h-0 flex-1 place-items-center p-8 text-sm text-muted-foreground">Organization not found. <Button variant="outline" className="mt-3" onClick={onClose}>Back</Button></div>

  return <div className="min-h-0 overflow-auto"><div className="mx-auto w-full max-w-[1120px] px-5 py-8 sm:px-8 sm:py-12 lg:px-14">
    <div className="mb-10 flex flex-col gap-4 sm:flex-row sm:items-start sm:justify-between">
      <div className="min-w-0">
        <div className="mb-3 flex items-center gap-3"><span className="grid size-9 shrink-0 place-items-center rounded-md bg-[#e8deee] text-[#694a73]"><Building2 className="size-4" /></span><h1 className="truncate text-2xl font-semibold">{organization.name}</h1></div>
        <p className="font-mono text-xs text-muted-foreground">{organization.organization_id}</p>
        <p className="mt-1 text-xs capitalize text-muted-foreground">{currentRole}</p>
      </div>
      <div className="flex w-full shrink-0 items-center gap-2 sm:w-auto"><Button variant="outline" className="min-w-0 flex-1 sm:flex-none" onClick={onCreateOrganization}><Plus />New organization</Button><Button variant="outline" className="shrink-0" onClick={onClose}>Close</Button></div>
    </div>

    <section className="border-y py-6">
      <div className="grid gap-5 md:grid-cols-[220px_minmax(0,1fr)]">
        <div><h2 className="text-sm font-semibold">General</h2><p className="mt-1 text-xs leading-5 text-muted-foreground">The organization name shown to its members.</p></div>
        <form onSubmit={onRename} className="grid gap-2 sm:grid-cols-[minmax(0,1fr)_auto] sm:items-end"><div className="min-w-0"><Field label="Organization name"><Input aria-label="Organization name" value={name} onChange={(event) => onNameChange(event.target.value)} required maxLength={80} disabled={!canManage} /></Field></div>{canManage && <Button type="submit" className="w-full sm:w-auto" disabled={!name.trim() || name.trim() === organization.name}>Save</Button>}</form>
      </div>
    </section>

    <section className="border-b py-6">
      <div className="grid gap-5 xl:grid-cols-[220px_minmax(0,1fr)]">
        <div><div className="flex items-center justify-between gap-3"><h2 className="text-sm font-semibold">Workspaces</h2><IconButton label="Create workspace" onClick={onCreateWorkspace}><Plus /></IconButton></div><p className="mt-1 text-xs leading-5 text-muted-foreground">Workspaces owned by this organization.</p></div>
        <div className="border-y">
          {workspaces.map((item) => <button key={item.workspace_id} type="button" className="grid min-h-14 w-full grid-cols-[minmax(0,1fr)_auto] items-center gap-3 border-b py-3 text-left last:border-b-0 hover:bg-accent/50" aria-label={`Select ${item.name} workspace`} onClick={() => onSelectWorkspace(item.workspace_id)}><span className="min-w-0"><span className="block truncate text-xs font-medium">{item.name}</span><span className="mt-1 block truncate font-mono text-[9px] text-muted-foreground">{item.workspace_id}</span></span>{item.workspace_id === selectedWorkspaceId && <span className="text-[10px] font-medium text-emerald-700">Current</span>}</button>)}
          {!workspaces.length && <EmptyState icon={<FolderKanban />} label="No workspaces yet" />}
        </div>
      </div>
    </section>

    <section className="border-b py-6">
      <div className="grid gap-5 xl:grid-cols-[220px_minmax(0,1fr)]">
        <div><div className="flex items-center justify-between gap-3"><h2 className="text-sm font-semibold">Members</h2>{canManage && <IconButton label="Invite member" onClick={onInvite}><UserRound /></IconButton>}</div><p className="mt-1 text-xs leading-5 text-muted-foreground">People with organization access.</p></div>
        <div className="border-y">
          {members.map((member) => <div key={member.user_id} className="grid min-h-14 grid-cols-[minmax(0,1fr)_110px_auto] items-center gap-3 border-b py-2 last:border-b-0"><span className="min-w-0"><span className="block truncate text-xs font-medium">{member.preferred_name}</span><span className="mt-0.5 block truncate text-[10px] text-muted-foreground">{member.email}</span></span>{currentRole === "owner" && member.role !== "owner" ? <Select value={member.role} onValueChange={(value: Member["role"]) => onMemberRoleChange(member.user_id, value)}><SelectTrigger className="h-8 text-xs"><SelectValue /></SelectTrigger><SelectContent><SelectItem value="member">Member</SelectItem><SelectItem value="admin">Admin</SelectItem></SelectContent></Select> : <span className="text-xs capitalize text-muted-foreground">{member.role}</span>}{canManage && member.role !== "owner" ? <IconButton label={`Remove ${member.preferred_name}`} className="text-destructive hover:text-destructive" onClick={() => onRemoveMember(member.user_id)}><Trash2 /></IconButton> : <span />}</div>)}
          {!members.length && <EmptyState icon={<Users />} label="No members" />}
        </div>
      </div>
    </section>

    <section className="border-b py-6">
      <div className="grid gap-5 xl:grid-cols-[220px_minmax(0,1fr)]">
        <div><h2 className="text-sm font-semibold">Groups</h2><p className="mt-1 text-xs leading-5 text-muted-foreground">Member groups used for workspace access.</p></div>
        <div className="space-y-4">
          {canManage && <form onSubmit={onCreateGroup} className="flex gap-2"><Input aria-label="Group name" value={groupName} onChange={(event) => onGroupNameChange(event.target.value)} placeholder="Group name" required maxLength={80} /><Button type="submit" variant="outline" disabled={!groupName.trim()}><Plus />Create</Button></form>}
          <div className="border-y divide-y">
            {groups.map((group) => <div key={group.group_id} className="py-4"><div className="mb-3 flex items-center justify-between gap-3"><div className="min-w-0"><h3 className="truncate text-xs font-semibold">{group.name}</h3><p className="mt-1 text-[10px] text-muted-foreground">{group.member_ids.length} members</p></div>{canManage && <IconButton label={`Delete ${group.name}`} className="text-destructive hover:text-destructive" onClick={() => onDeleteGroup(group.group_id)}><Trash2 /></IconButton>}</div><div className="grid gap-2 sm:grid-cols-2">{members.map((member) => <label key={member.user_id} className="flex min-w-0 items-center gap-2 text-xs"><input type="checkbox" className="size-4 accent-foreground" checked={group.member_ids.includes(member.user_id)} disabled={!canManage} onChange={(event) => onSetGroupMember(group.group_id, member.user_id, event.target.checked)} /><span className="truncate">{member.preferred_name}</span></label>)}</div></div>)}
            {!groups.length && <EmptyState icon={<ShieldCheck />} label="No groups yet" />}
          </div>
        </div>
      </div>
    </section>

    {canManage && <section className="border-b py-6"><div className="grid gap-5 md:grid-cols-[220px_minmax(0,1fr)]"><div><h2 className="text-sm font-semibold">Activity</h2><p className="mt-1 text-xs leading-5 text-muted-foreground">Management events and workspace traffic usage.</p></div><div><Button variant="outline" onClick={onOpenAudit}><ScrollText />Open audit</Button></div></div></section>}
  </div></div>
}
