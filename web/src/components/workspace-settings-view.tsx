import { PolicyPublishAuthoritySettings } from "@/components/policy-publish-authority"
import { EmptyState, Field, IconButton, Status } from "@/components/control-plane-ui"
import { LaunchProfilesList } from "@/components/launch-profiles-list"
import { MachineItem } from "@/components/machine-view"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select"
import { api, machineName, type AgentLaunchProfile, type AppDeployment, type Machine, type Member, type Organization, type OrganizationGroup, type PolicyProviderStatus, type Workspace, type WorkspaceAccess } from "@/lib/api"
import {
  Download,
  ExternalLink,
  Play,
  Plus,
  RotateCw,
  Server,
  Settings as SettingsIcon,
  ShieldCheck,
  Square,
  Trash2
} from "lucide-react"
import { FormEvent, useEffect, useState } from "react"

export type WorkspaceSettingsViewProps = {
  workspace?: Workspace
  organization?: Organization
  name: string
  machines: Machine[]
  machineCount?: number
  profiles: AgentLaunchProfile[]
  profilesLoading: boolean
  apps: AppDeployment[]
  onPolicyAppInstalled: (app: AppDeployment) => void
  canDelete: boolean
  preview: boolean
  onNameChange: (value: string) => void
  onRename: (event: FormEvent) => void
  onAddMachine: () => void
  onOpenMachine: (machine: Machine) => void
  onRenameMachine: (machine: Machine) => void
  onDeleteMachine: (machine: Machine) => void
  onCopy: (value: string) => void
  onRefreshProfiles: () => void
  onNewProfile: () => void
  onEditProfile: (profile: AgentLaunchProfile) => void
  onLaunchProfile: (profile: AgentLaunchProfile) => void
  onDeleteProfile: (profile: AgentLaunchProfile) => void
  onDelete: () => void
  onClose: () => void
}

export function WorkspaceSettingsView({ workspace, organization, name, machines, machineCount, profiles, profilesLoading, apps, onPolicyAppInstalled, preview, onNameChange, onRename, onAddMachine, onOpenMachine, onRenameMachine, onDeleteMachine, onCopy, onRefreshProfiles, onNewProfile, onEditProfile, onLaunchProfile, onDeleteProfile, onDelete, onClose }: WorkspaceSettingsViewProps) {
  const [access, setAccess] = useState<WorkspaceAccess>()
  const [organizationMembers, setOrganizationMembers] = useState<Member[]>([])
  const [organizationGroups, setOrganizationGroups] = useState<OrganizationGroup[]>([])
  const [selectedUserId, setSelectedUserId] = useState("")
  const [selectedGroupId, setSelectedGroupId] = useState("")
  const [accessError, setAccessError] = useState<string | null>(null)
  const [policyProvider, setPolicyProvider] = useState<PolicyProviderStatus>()
  const [policyAppId, setPolicyAppId] = useState("")
  const [policyFailureMode, setPolicyFailureMode] = useState<"fail_closed" | "fail_open">("fail_closed")
  const [policyStaleSeconds, setPolicyStaleSeconds] = useState("300")
  const [policyInstallMachineId, setPolicyInstallMachineId] = useState("")
  const [policyBusy, setPolicyBusy] = useState(false)
  const [policyError, setPolicyError] = useState<string | null>(null)
  useEffect(() => {
    if (preview || !workspace || !organization) return
    Promise.all([
      api<{ access: WorkspaceAccess }>(`/api/workspaces/${encodeURIComponent(workspace.workspace_id)}/access`),
      api<{ members: Member[] }>(`/api/organizations/${encodeURIComponent(organization.organization_id)}/members`),
      api<{ groups: OrganizationGroup[] }>(`/api/organizations/${encodeURIComponent(organization.organization_id)}/groups`),
    ]).then(([accessData, memberData, groupData]) => {
      setAccess(accessData.access)
      setOrganizationMembers(memberData.members)
      setOrganizationGroups(groupData.groups)
    }).catch((reason) => setAccessError(reason instanceof Error ? reason.message : "Unable to load workspace access"))
  }, [preview, workspace?.workspace_id, organization?.organization_id])
  useEffect(() => {
    if (preview || !workspace) return
    api<PolicyProviderStatus>(`/api/workspaces/${encodeURIComponent(workspace.workspace_id)}/policy-provider`)
      .then((status) => {
        setPolicyProvider(status)
        setPolicyAppId(status.provider?.app_id ?? "")
        setPolicyFailureMode(status.provider?.failure_mode ?? "fail_closed")
        setPolicyStaleSeconds(String(status.provider?.max_stale_seconds ?? 300))
        setPolicyError(null)
      })
      .catch((reason) => setPolicyError(reason instanceof Error ? reason.message : "Unable to load Policy Provider"))
  }, [preview, workspace?.workspace_id])
  useEffect(() => {
    if (machines.some((machine) => machine.server_id === policyInstallMachineId && machine.status === "online")) return
    setPolicyInstallMachineId(machines.find((machine) => machine.status === "online")?.server_id ?? "")
  }, [machines, policyInstallMachineId])
  useEffect(() => {
    if (window.location.hash === "#policy-provider") document.getElementById("policy-provider")?.scrollIntoView({ block: "start" })
  }, [workspace?.workspace_id, policyProvider])
  async function savePolicyProvider() {
    if (!workspace || !policyAppId) return
    const maxStaleSeconds = Number(policyStaleSeconds)
    if (!Number.isInteger(maxStaleSeconds) || maxStaleSeconds < 0 || maxStaleSeconds > 86400) {
      setPolicyError("Stale cache window must be between 0 and 86400 seconds")
      return
    }
    setPolicyBusy(true); setPolicyError(null)
    try {
      const status = await api<PolicyProviderStatus>(`/api/workspaces/${encodeURIComponent(workspace.workspace_id)}/policy-provider`, { method: "PUT", body: JSON.stringify({ app_id: policyAppId, failure_mode: policyFailureMode, max_stale_seconds: maxStaleSeconds }) })
      setPolicyProvider(status)
    } catch (reason) { setPolicyError(reason instanceof Error ? reason.message : "Unable to configure Policy Provider") }
    finally { setPolicyBusy(false) }
  }
  async function removePolicyProvider() {
    if (!workspace) return
    setPolicyBusy(true); setPolicyError(null)
    try {
      await api(`/api/workspaces/${encodeURIComponent(workspace.workspace_id)}/policy-provider`, { method: "DELETE" })
      const status = await api<PolicyProviderStatus>(`/api/workspaces/${encodeURIComponent(workspace.workspace_id)}/policy-provider`)
      setPolicyProvider(status); setPolicyAppId("")
    } catch (reason) { setPolicyError(reason instanceof Error ? reason.message : "Unable to remove Policy Provider") }
    finally { setPolicyBusy(false) }
  }
  async function installDefaultPolicyApp() {
    if (!workspace || !policyInstallMachineId) return
    setPolicyBusy(true); setPolicyError(null)
    try {
      const status = await api<PolicyProviderStatus>(`/api/workspaces/${encodeURIComponent(workspace.workspace_id)}/policy-provider/default-app`, { method: "POST", body: JSON.stringify({ server_id: policyInstallMachineId }) })
      setPolicyProvider(status)
      setPolicyAppId(status.provider?.app_id ?? "")
      if (status.app) onPolicyAppInstalled(status.app)
    } catch (reason) { setPolicyError(reason instanceof Error ? reason.message : "Unable to install the default Policy App") }
    finally { setPolicyBusy(false) }
  }
  async function policyLifecycle(action: "start" | "stop" | "restart") {
    if (!workspace || !policyProvider?.app) return
    setPolicyBusy(true); setPolicyError(null)
    try {
      const data = await api<{ app: AppDeployment }>(`/api/workspaces/${encodeURIComponent(workspace.workspace_id)}/apps/${encodeURIComponent(policyProvider.app.app_id)}/${action}`, { method: "POST", body: "{}" })
      setPolicyProvider((current) => current ? { ...current, app: data.app } : current)
    } catch (reason) { setPolicyError(reason instanceof Error ? reason.message : `Unable to ${action} Policy App`) }
    finally { setPolicyBusy(false) }
  }
  async function onAccessMode(mode: WorkspaceAccess["access_mode"]) {
    if (!workspace) return
    try {
      const data = await api<{ access: WorkspaceAccess }>(`/api/workspaces/${encodeURIComponent(workspace.workspace_id)}/access`, { method: "PATCH", body: JSON.stringify({ access_mode: mode }) })
      setAccess(data.access)
    } catch (reason) { setAccessError(reason instanceof Error ? reason.message : "Unable to update access") }
  }
  async function onUserGrant(userId: string, role: WorkspaceAccess["current_role"] | null) {
    if (!workspace) return
    try {
      const data = await api<{ access: WorkspaceAccess }>(`/api/workspaces/${encodeURIComponent(workspace.workspace_id)}/access/users/${encodeURIComponent(userId)}`, role ? { method: "PUT", body: JSON.stringify({ role }) } : { method: "DELETE" })
      setAccess(data.access); setSelectedUserId("")
    } catch (reason) { setAccessError(reason instanceof Error ? reason.message : "Unable to update member") }
  }
  async function onGroupGrant(groupId: string, role: WorkspaceAccess["current_role"] | null) {
    if (!workspace) return
    try {
      const data = await api<{ access: WorkspaceAccess }>(`/api/workspaces/${encodeURIComponent(workspace.workspace_id)}/access/groups/${encodeURIComponent(groupId)}`, role ? { method: "PUT", body: JSON.stringify({ role }) } : { method: "DELETE" })
      setAccess(data.access); setSelectedGroupId("")
    } catch (reason) { setAccessError(reason instanceof Error ? reason.message : "Unable to update group") }
  }
  if (!workspace) return <div className="grid min-h-0 flex-1 place-items-center p-8 text-sm text-muted-foreground">Workspace not found. <Button variant="outline" className="mt-3" onClick={onClose}>Back</Button></div>
  const deleteBlocked = machineCount === undefined || machineCount > 0
  const isOwner = access?.current_role === "owner"
  const availableMembers = organizationMembers.filter((member) => !access?.members.some((grant) => grant.user_id === member.user_id))
  const availableGroups = organizationGroups.filter((group) => !access?.groups.some((grant) => grant.group_id === group.group_id))

  return <div className="min-h-0 overflow-auto"><div className="mx-auto w-full max-w-[1120px] px-5 py-8 sm:px-8 sm:py-12 lg:px-14">
    <div className="mb-10 flex items-start justify-between gap-4">
      <div className="min-w-0">
        <div className="mb-3 flex items-center gap-3"><span className="grid size-9 shrink-0 place-items-center rounded-md bg-[#e8deee] text-[#694a73]"><SettingsIcon className="size-4" /></span><h1 className="truncate text-2xl font-semibold">{workspace.name}</h1></div>
        <p className="font-mono text-xs text-muted-foreground">{workspace.workspace_id}</p>
        {organization && <p className="mt-1 text-xs text-muted-foreground">{organization.name}</p>}
      </div>
      <Button variant="outline" className="shrink-0" onClick={onClose}>Close</Button>
    </div>

    {!preview && machines.length === 0 && machineCount === 0 && <div role="status" className="mb-6 flex flex-col items-start gap-3 rounded-md border bg-muted/50 p-4 text-sm sm:flex-row sm:items-center sm:justify-between">
      <div><p className="font-medium">Add a machine to create agents</p><p className="mt-1 text-xs text-muted-foreground">Agents run on machines. Connect your first machine to this workspace, then add an agent from the Tree menu.</p></div>
      <Button variant="outline" className="shrink-0" onClick={onAddMachine}>Set up a machine</Button>
    </div>}

    <section className="border-y py-6">
      <div className="grid gap-5 md:grid-cols-[220px_minmax(0,1fr)]">
        <div><h2 className="text-sm font-semibold">General</h2><p className="mt-1 text-xs leading-5 text-muted-foreground">The display name shown to organization members.</p></div>
        <form onSubmit={onRename} className="grid gap-2 sm:grid-cols-[minmax(0,1fr)_auto] sm:items-end"><div className="min-w-0"><Field label="Workspace name"><Input aria-label="Workspace name" value={name} onChange={(event) => onNameChange(event.target.value)} required maxLength={80} /></Field></div><Button type="submit" className="w-full sm:w-auto" disabled={!name.trim() || name.trim() === workspace.name}>Save</Button></form>
      </div>
    </section>

    <section className="border-b py-6" data-tour="workspace-machines">
      <div className="grid gap-5 xl:grid-cols-[220px_minmax(0,1fr)]">
        <div><div className="flex items-center justify-between gap-3"><h2 className="text-sm font-semibold">Machines</h2><span data-tour="add-machine"><Button variant="outline" size="sm" className="h-8" onClick={onAddMachine}><Plus />Add</Button></span></div><p className="mt-1 text-xs leading-5 text-muted-foreground">Hosts enrolled in this workspace. Open a machine to inspect its agents, services, and network activity.</p></div>
        <div className="border-y py-1">
          {machines.map((machine) => <MachineItem key={machine.server_id} machine={machine} machines={machines} workspaceId={workspace.workspace_id} onClick={() => onOpenMachine(machine)} onRename={() => onRenameMachine(machine)} onDelete={() => onDeleteMachine(machine)} onCopy={onCopy} />)}
          {!machines.length && <EmptyState icon={machineCount === undefined ? <RotateCw className="animate-spin" /> : <Server />} label={machineCount === undefined ? "Loading machines" : "No machines connected"} />}
        </div>
      </div>
    </section>

    <section className="border-b py-6">
      <div className="grid gap-5 xl:grid-cols-[220px_minmax(0,1fr)]">
        <div><div className="flex items-center justify-between gap-2"><h2 className="text-sm font-semibold">Launch profiles</h2><div className="flex items-center gap-1"><IconButton label="Refresh profiles" onClick={onRefreshProfiles} disabled={profilesLoading}><RotateCw /></IconButton><IconButton label="New profile" onClick={onNewProfile}><Plus /></IconButton></div></div><p className="mt-1 text-xs leading-5 text-muted-foreground">Reusable Agent commands and their default working directories.</p></div>
        <LaunchProfilesList profiles={profiles} loading={profilesLoading} onEdit={onEditProfile} onLaunch={onLaunchProfile} onDelete={onDeleteProfile} />
      </div>
    </section>

    <section className="border-b py-6">
      <div className="grid gap-5 xl:grid-cols-[220px_minmax(0,1fr)]">
        <div><h2 className="text-sm font-semibold">Access</h2><p className="mt-1 text-xs leading-5 text-muted-foreground">Choose whether everyone in the organization or only selected people and groups can open this workspace.</p></div>
        <div className="space-y-5">
          <Field label="Who can access"><Select value={access?.access_mode ?? "organization"} onValueChange={(mode: WorkspaceAccess["access_mode"]) => void onAccessMode(mode)} disabled={!isOwner || preview}><SelectTrigger><SelectValue /></SelectTrigger><SelectContent><SelectItem value="organization">Everyone in organization</SelectItem><SelectItem value="restricted">Selected people and groups</SelectItem></SelectContent></Select></Field>
          {accessError && <p className="text-xs text-destructive">{accessError}</p>}
          {access && <>
            {access.access_mode === "organization" && <p className="text-xs leading-5 text-muted-foreground">Everyone already has member access. Add owners here to share access management and workspace deletion.</p>}
            <div><h3 className="mb-2 text-xs font-semibold">People</h3><div className="border-y">{access.members.map((grant) => <div key={grant.user_id} className="grid min-h-12 grid-cols-[minmax(0,1fr)_110px_auto] items-center gap-2 border-b last:border-b-0"><span className="min-w-0"><span className="block truncate text-xs font-medium">{grant.preferred_name}</span><span className="block truncate text-[10px] text-muted-foreground">{grant.email}</span></span>{isOwner ? <Select value={grant.role} onValueChange={(role: WorkspaceAccess["current_role"]) => void onUserGrant(grant.user_id, role)}><SelectTrigger className="h-8 text-xs"><SelectValue /></SelectTrigger><SelectContent><SelectItem value="member">Member</SelectItem><SelectItem value="owner">Owner</SelectItem></SelectContent></Select> : <span className="text-xs capitalize text-muted-foreground">{grant.role}</span>}{isOwner ? <IconButton label={`Remove ${grant.preferred_name}`} onClick={() => void onUserGrant(grant.user_id, null)}><Trash2 /></IconButton> : <span />}</div>)}{!access.members.length && <div className="py-3 text-xs text-muted-foreground">No direct members.</div>}</div>{isOwner && availableMembers.length > 0 && <div className="mt-2 flex gap-2"><Select value={selectedUserId} onValueChange={setSelectedUserId}><SelectTrigger className="min-w-0 flex-1"><SelectValue placeholder="Select organization member" /></SelectTrigger><SelectContent>{availableMembers.map((member) => <SelectItem key={member.user_id} value={member.user_id}>{member.preferred_name}</SelectItem>)}</SelectContent></Select><Button variant="outline" disabled={!selectedUserId} onClick={() => void onUserGrant(selectedUserId, "member")}><Plus />Add</Button></div>}</div>
            <div><h3 className="mb-2 text-xs font-semibold">Groups</h3><div className="border-y">{access.groups.map((grant) => <div key={grant.group_id} className="grid min-h-12 grid-cols-[minmax(0,1fr)_110px_auto] items-center gap-2 border-b last:border-b-0"><span className="min-w-0"><span className="block truncate text-xs font-medium">{grant.name}</span><span className="block text-[10px] text-muted-foreground">{grant.member_count} members</span></span>{isOwner ? <Select value={grant.role} onValueChange={(role: WorkspaceAccess["current_role"]) => void onGroupGrant(grant.group_id, role)}><SelectTrigger className="h-8 text-xs"><SelectValue /></SelectTrigger><SelectContent><SelectItem value="member">Member</SelectItem><SelectItem value="owner">Owner</SelectItem></SelectContent></Select> : <span className="text-xs capitalize text-muted-foreground">{grant.role}</span>}{isOwner ? <IconButton label={`Remove ${grant.name}`} onClick={() => void onGroupGrant(grant.group_id, null)}><Trash2 /></IconButton> : <span />}</div>)}{!access.groups.length && <div className="py-3 text-xs text-muted-foreground">No groups.</div>}</div>{isOwner && availableGroups.length > 0 && <div className="mt-2 flex gap-2"><Select value={selectedGroupId} onValueChange={setSelectedGroupId}><SelectTrigger className="min-w-0 flex-1"><SelectValue placeholder="Select organization group" /></SelectTrigger><SelectContent>{availableGroups.map((group) => <SelectItem key={group.group_id} value={group.group_id}>{group.name}</SelectItem>)}</SelectContent></Select><Button variant="outline" disabled={!selectedGroupId} onClick={() => void onGroupGrant(selectedGroupId, "member")}><Plus />Add</Button></div>}</div>
          </>}
        </div>
      </div>
    </section>

    <section id="policy-provider" className="scroll-mt-4 border-b py-6">
      <div className="grid gap-5 md:grid-cols-[220px_minmax(0,1fr)]">
        <div><div className="flex items-center gap-2"><h2 className="text-sm font-semibold">Policy Provider</h2>{policyProvider?.provider && <Status value={policyProvider.app?.status ?? "unavailable"} />}</div><p className="mt-1 text-xs leading-5 text-muted-foreground">Workspace authorization supplied by a Managed App.</p></div>
        <div className="space-y-4">
          {!policyProvider?.provider && policyProvider?.fallback && <div className="flex flex-wrap items-center justify-between gap-3 border-y py-3"><div className="flex min-w-0 items-center gap-3"><span className="grid size-8 shrink-0 place-items-center rounded-md bg-[#e3eee8] text-[#176b52]"><ShieldCheck className="size-4" /></span><div className="min-w-0"><p className="truncate text-xs font-semibold text-foreground">{policyProvider.fallback.name}</p><p className="mt-0.5 text-[11px] text-muted-foreground">Active · {policyProvider.fallback.mode}{policyProvider.fallback.effect ? ` · ${policyProvider.fallback.effect}` : ` · revision ${policyProvider.fallback.revision}`}</p></div></div><Status value="running" /></div>}
          <div className="grid gap-3 lg:grid-cols-[minmax(0,1fr)_180px_140px]">
            <Field label="Policy App"><Select value={policyAppId || undefined} onValueChange={setPolicyAppId} disabled={!isOwner || policyBusy}><SelectTrigger aria-label="Policy App"><SelectValue placeholder="Not configured" /></SelectTrigger><SelectContent>{apps.filter((app) => app.status === "running" || app.app_id === policyProvider?.provider?.app_id).map((app) => <SelectItem key={app.app_id} value={app.app_id}>{app.name}</SelectItem>)}</SelectContent></Select></Field>
            <Field label="Provider failure"><Select value={policyFailureMode} onValueChange={(value: "fail_closed" | "fail_open") => setPolicyFailureMode(value)} disabled={!isOwner || policyBusy}><SelectTrigger aria-label="Policy Provider failure mode"><SelectValue /></SelectTrigger><SelectContent><SelectItem value="fail_closed">Deny after stale window</SelectItem><SelectItem value="fail_open">Allow after stale window</SelectItem></SelectContent></Select></Field>
            <Field label="Stale seconds"><Input aria-label="Policy Provider stale seconds" type="number" min={0} max={86400} value={policyStaleSeconds} onChange={(event) => setPolicyStaleSeconds(event.target.value)} disabled={!isOwner || policyBusy} /></Field>
          </div>
          {!policyProvider?.provider && isOwner && <div className="grid gap-2 sm:grid-cols-[minmax(0,1fr)_auto] sm:items-end"><Field label="Install default App on"><Select value={policyInstallMachineId || undefined} onValueChange={setPolicyInstallMachineId} disabled={policyBusy}><SelectTrigger aria-label="Default Policy App machine"><SelectValue placeholder="No online machine" /></SelectTrigger><SelectContent>{machines.filter((machine) => machine.status === "online").map((machine) => <SelectItem key={machine.server_id} value={machine.server_id}>{machineName(machine)}</SelectItem>)}</SelectContent></Select></Field><Button variant="outline" onClick={() => void installDefaultPolicyApp()} disabled={!policyInstallMachineId || policyBusy}>{policyBusy ? <RotateCw className="animate-spin" /> : <Download />}Install Default Policy App</Button></div>}
          <div className="flex flex-wrap items-center gap-2"><Button size="sm" onClick={() => void savePolicyProvider()} disabled={!isOwner || !policyAppId || policyBusy}>{policyBusy && <RotateCw className="animate-spin" />}Apply</Button>{policyProvider?.app && policyProvider.app.status !== "running" && <Button size="sm" variant="outline" onClick={() => void policyLifecycle("start")} disabled={!isOwner || policyBusy}><Play />Start</Button>}{policyProvider?.app?.status === "running" && <><Button size="sm" variant="outline" onClick={() => void policyLifecycle("restart")} disabled={!isOwner || policyBusy}><RotateCw />Restart</Button><Button size="sm" variant="outline" onClick={() => void policyLifecycle("stop")} disabled={!isOwner || policyBusy}><Square />Stop</Button></>}{policyProvider?.provider && <Button size="sm" variant="outline" onClick={() => void removePolicyProvider()} disabled={!isOwner || policyBusy}>Remove</Button>}{policyProvider?.app?.public_url && <Button size="sm" variant="outline" asChild><a href={policyProvider.app.public_url} target="_blank" rel="noreferrer"><ExternalLink />Open Policy App</a></Button>}</div>
          {policyProvider?.provider && <div className="grid gap-2 border-y py-3 text-[11px] text-muted-foreground sm:grid-cols-3"><span>Announced <strong className="text-foreground">rev {policyProvider.provider.revision_hint}</strong></span><span>Cached <strong className="text-foreground">{policyProvider.cache.revision === undefined ? "none" : `rev ${policyProvider.cache.revision}`}</strong></span><span>Age <strong className="text-foreground">{policyProvider.cache.age_seconds === undefined ? "-" : `${policyProvider.cache.age_seconds}s`}</strong></span></div>}
          <PolicyPublishAuthoritySettings key={`${workspace.workspace_id}/${policyProvider?.provider?.app_id ?? "none"}`} workspaceId={workspace.workspace_id} status={policyProvider} isOwner={isOwner} disabled={preview || policyBusy} onSaved={(provider) => setPolicyProvider((current) => current ? { ...current, provider } : current)} />
          {(policyError || policyProvider?.cache.last_error) && <p className="text-xs text-destructive">{policyError ?? policyProvider?.cache.last_error}</p>}
          {!apps.length && !machines.some((machine) => machine.status === "online") && <p className="text-xs text-muted-foreground">Connect a machine to install the default Policy App.</p>}
        </div>
      </div>
    </section>

    {isOwner && <section className="border-b py-6">
      <div className="grid gap-5 md:grid-cols-[220px_minmax(0,1fr)]">
        <div><h2 className="text-sm font-semibold text-destructive">Danger zone</h2><p className="mt-1 text-xs leading-5 text-muted-foreground">Deleted workspaces disappear from active views while historical traffic and messages remain.</p></div>
        <div className="grid gap-3 border border-destructive/30 p-4 sm:grid-cols-[minmax(0,1fr)_auto] sm:items-center"><p className="text-xs leading-5 text-muted-foreground">{machineCount === undefined ? "Checking machine inventory..." : machineCount > 0 ? `Delete all ${machineCount} ${machineCount === 1 ? "machine" : "machines"} before deleting this workspace.` : "This action cannot be undone."}</p><Button variant="destructive" className="w-full sm:w-auto" disabled={preview || deleteBlocked} onClick={onDelete}><Trash2 />Delete workspace</Button></div>
      </div>
    </section>}
  </div></div>
}
