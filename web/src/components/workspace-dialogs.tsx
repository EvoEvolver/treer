import { Field,SimpleNameDialog } from "@/components/control-plane-ui"
import { SettingsDialog } from "@/components/settings"
import { Button } from "@/components/ui/button"
import { Dialog,DialogContent,DialogDescription,DialogFooter,DialogHeader,DialogTitle } from "@/components/ui/dialog"
import { Input } from "@/components/ui/input"
import { Select,SelectContent,SelectItem,SelectTrigger,SelectValue } from "@/components/ui/select"
import { Textarea } from "@/components/ui/textarea"
import type { useWorkspaceDialogState } from "@/components/workspace-dialog-state"
import type { Agent,AgentLaunchProfile,Machine,Organization,Snapshot,User,WorkloadTreeState,Workspace } from "@/lib/api"
import { machineName } from "@/lib/api"
import { formatCommandLine } from "@/lib/command-line"
import { agentPath } from "@/lib/workload-path"
import { Copy,Play,Rocket,Upload } from "lucide-react"
import type { Dispatch,FormEvent,SetStateAction } from "react"

interface WorkspaceDialogsProps {
  state: ReturnType<typeof useWorkspaceDialogState>
  user: User
  setUser: Dispatch<SetStateAction<User | null | undefined>>
  showError: (reason: unknown) => void
  createOrganization: (event: FormEvent) => Promise<void>
  organization: Organization | undefined
  createWorkspace: (event: FormEvent) => Promise<void>
  saveLaunchProfile: (event: FormEvent) => Promise<void>
  launchFromProfile: (event: FormEvent) => Promise<void>
  onlineMachines: Machine[]
  deleteLaunchProfile: () => Promise<void>
  createAgent: (event: FormEvent) => Promise<void>
  selectAgentProfile: (profileId: string) => void
  launchProfiles: AgentLaunchProfile[]
  selectedCreateProfile: AgentLaunchProfile | undefined
  changeAgentCommandLine: (commandLine: string) => void
  deleteAppDeployment: () => Promise<void>
  copy: (value: string) => Promise<void>
  submitRename: (event: FormEvent) => Promise<void>
  snapshot: Snapshot | null
  workloadTree: WorkloadTreeState
  uploadToAgentMachine: (event: FormEvent) => Promise<void>
  selectedAgentMachine: Machine | undefined
  selectedAgent: Agent | undefined
  selectedAgentMachineOnline: boolean
  confirmDelete: () => Promise<void>
  workspaceMachineCount: number | undefined
  workspace: Workspace | undefined
  confirmDeleteWorkspace: () => Promise<void>
}

export function WorkspaceDialogs({ state, user, setUser, showError, createOrganization, organization, createWorkspace, saveLaunchProfile, launchFromProfile, onlineMachines, deleteLaunchProfile, createAgent, selectAgentProfile, launchProfiles, selectedCreateProfile, changeAgentCommandLine, deleteAppDeployment, copy, submitRename, snapshot, workloadTree, uploadToAgentMachine, selectedAgentMachine, selectedAgent, selectedAgentMachineOnline, confirmDelete, workspaceMachineCount, workspace, confirmDeleteWorkspace }: WorkspaceDialogsProps) {
  const {
    settingsOpen, setSettingsOpen, createOrganizationOpen, setCreateOrganizationOpen,
    createWorkspaceOpen, setCreateWorkspaceOpen, deleteWorkspaceOpen, setDeleteWorkspaceOpen,
    createAgentOpen, setCreateAgentOpen, agentFolder, renameError, createAgentError, creatingAgent,
    deletingApp, setDeletingApp, profileEditorOpen, setProfileEditorOpen, editingLaunchProfile,
    setEditingLaunchProfile, launchingProfile, setLaunchingProfile, deletingProfile, setDeletingProfile,
    installOpen, setInstallOpen, inviteOpen, setInviteOpen, uploadOpen, setUploadOpen, uploadDirectory,
    setUploadDirectory, uploadFile, setUploadFile, overwriteUpload, setOverwriteUpload, uploading,
    uploadInputKey, uploadMessage, uploadError, renameTarget, setRenameTarget, deleteTarget,
    setDeleteTarget, createOrganizationName, setCreateOrganizationName, createWorkspaceName,
    setCreateWorkspaceName, agentName, setAgentName, setAgentNameCustomized, agentProfileId,
    agentServerId, setAgentServerId, agentCwd, setAgentCwd, agentCommandLine, launchProfileName,
    setLaunchProfileName, launchProfileDescription, setLaunchProfileDescription, launchProfileCwd,
    setLaunchProfileCwd, launchProfileCommandLine, setLaunchProfileCommandLine, launchMachineId,
    setLaunchMachineId, launchAgentName, setLaunchAgentName, renameName, setRenameName, installPlatform,
    setInstallPlatform, inviteUrl, installCommand, connectCommand,
  } = state
  return <>
    <SettingsDialog open={settingsOpen} onOpenChange={setSettingsOpen} user={user} onUserChange={setUser} onError={showError} />

    <SimpleNameDialog open={createOrganizationOpen} onOpenChange={setCreateOrganizationOpen} title="Create organization" description="Organizations contain members and workspaces." label="Organization name" value={createOrganizationName} onValueChange={setCreateOrganizationName} onSubmit={createOrganization} />
    <SimpleNameDialog open={createWorkspaceOpen} onOpenChange={setCreateWorkspaceOpen} title="Create workspace" description={`Add a workspace to ${organization?.name ?? "this organization"}.`} label="Workspace name" value={createWorkspaceName} onValueChange={setCreateWorkspaceName} onSubmit={createWorkspace} dataTour="create-workspace-dialog" />

    <Dialog open={profileEditorOpen} onOpenChange={(open) => { setProfileEditorOpen(open); if (!open) setEditingLaunchProfile(null) }}><DialogContent className="max-w-xl"><form onSubmit={saveLaunchProfile} className="grid gap-4 sm:grid-cols-2"><DialogHeader className="sm:col-span-2"><DialogTitle>{editingLaunchProfile ? "Edit launch profile" : "New launch profile"}</DialogTitle><DialogDescription>Reusable Agent process settings for this workspace.</DialogDescription></DialogHeader><Field label="Profile name"><Input value={launchProfileName} onChange={(event) => setLaunchProfileName(event.target.value)} required autoFocus maxLength={80} /></Field><Field label="Working directory"><Input className="font-mono" value={launchProfileCwd} onChange={(event) => setLaunchProfileCwd(event.target.value)} required /></Field><div className="sm:col-span-2"><Field label="Description"><Input value={launchProfileDescription} onChange={(event) => setLaunchProfileDescription(event.target.value)} maxLength={1000} /></Field></div><div className="sm:col-span-2"><Field label="Command"><Input className="font-mono" value={launchProfileCommandLine} onChange={(event) => setLaunchProfileCommandLine(event.target.value)} placeholder="codex review --base main" required /></Field></div><DialogFooter className="sm:col-span-2"><Button type="button" variant="outline" onClick={() => setProfileEditorOpen(false)}>Cancel</Button><Button type="submit"><Rocket />{editingLaunchProfile ? "Save profile" : "Create profile"}</Button></DialogFooter></form></DialogContent></Dialog>

    <Dialog open={Boolean(launchingProfile)} onOpenChange={(open) => !open && setLaunchingProfile(null)}><DialogContent><form onSubmit={launchFromProfile} className="space-y-4"><DialogHeader><DialogTitle>Run {launchingProfile?.name}</DialogTitle><DialogDescription>Choose where to start this Agent.</DialogDescription></DialogHeader><Field label="Machine"><Select value={launchMachineId} onValueChange={setLaunchMachineId} required><SelectTrigger><SelectValue placeholder="Select an online machine" /></SelectTrigger><SelectContent>{onlineMachines.map((machine) => <SelectItem key={machine.server_id} value={machine.server_id}>{machineName(machine)}</SelectItem>)}</SelectContent></Select></Field><Field label="Agent name"><Input value={launchAgentName} onChange={(event) => setLaunchAgentName(event.target.value)} required maxLength={80} /></Field><DialogFooter><Button type="button" variant="outline" onClick={() => setLaunchingProfile(null)}>Cancel</Button><Button type="submit" disabled={!launchMachineId}><Play />Run Agent</Button></DialogFooter></form></DialogContent></Dialog>

    <Dialog open={Boolean(deletingProfile)} onOpenChange={(open) => !open && setDeletingProfile(null)}><DialogContent><DialogHeader><DialogTitle>Delete launch profile</DialogTitle><DialogDescription>Delete {deletingProfile?.name}? Existing Agents are not affected.</DialogDescription></DialogHeader><DialogFooter><Button variant="outline" onClick={() => setDeletingProfile(null)}>Cancel</Button><Button variant="destructive" onClick={deleteLaunchProfile}>Delete profile</Button></DialogFooter></DialogContent></Dialog>

    <Dialog open={createAgentOpen} onOpenChange={setCreateAgentOpen}>
      <DialogContent data-tour="create-agent-dialog">
        <form onSubmit={createAgent} className="min-w-0 space-y-4">
          <DialogHeader>
            <DialogTitle>Create agent</DialogTitle>
            <DialogDescription>Start a terminal or agent on an online machine in this workspace.</DialogDescription>
          </DialogHeader>
          {agentFolder && <p className="break-all text-xs text-muted-foreground">Tree folder: {agentFolder.path}</p>}
          <Field label="Machine">
            <Select value={agentServerId} onValueChange={setAgentServerId} required>
              <SelectTrigger aria-label="Machine"><SelectValue placeholder="Select a machine" /></SelectTrigger>
              <SelectContent>{onlineMachines.map((machine) => <SelectItem key={machine.server_id} value={machine.server_id}>{machineName(machine)}</SelectItem>)}</SelectContent>
            </Select>
          </Field>
          <Field label="Launch">
            <Select value={agentProfileId} onValueChange={selectAgentProfile}>
              <SelectTrigger aria-label="Launch" data-tour="agent-launch"><SelectValue /></SelectTrigger>
              <SelectContent>
                <SelectItem value="terminal">Terminal</SelectItem>
                <SelectItem value="manual">Custom command</SelectItem>
                {launchProfiles.map((profile) => <SelectItem key={profile.profile_id} value={profile.profile_id}>{profile.name}</SelectItem>)}
              </SelectContent>
            </Select>
          </Field>
          {agentProfileId === "terminal" || agentProfileId === "manual"
            ? <Field label="Working directory"><Input value={agentCwd} onChange={(event) => setAgentCwd(event.target.value)} /></Field>
            : selectedCreateProfile
              ? <div className="min-w-0 max-w-full overflow-hidden rounded-md border bg-muted/30 px-3 py-2"><code className="block max-w-full truncate text-xs" title={formatCommandLine(selectedCreateProfile.command, selectedCreateProfile.args)}>{formatCommandLine(selectedCreateProfile.command, selectedCreateProfile.args)}</code><span className="mt-1 block max-w-full truncate text-[10px] text-muted-foreground">{selectedCreateProfile.cwd || "."}</span></div>
              : null}
          {agentProfileId === "manual" && <Field label="Command"><Input aria-label="Command" className="font-mono" value={agentCommandLine} onChange={(event) => changeAgentCommandLine(event.target.value)} placeholder="/path/to/your-launcher --option" required /></Field>}
          {selectedCreateProfile && <span className="block text-[10px] text-muted-foreground">Runs the saved command on this machine. Install its runtime and dependencies before launching.</span>}
          <Field label="Name"><Input aria-label="Name" value={agentName} onChange={(event) => { setAgentName(event.target.value); setAgentNameCustomized(true) }} required /></Field>
          {createAgentError && <p role="alert" className="break-words text-sm text-destructive">{createAgentError}</p>}
          <DialogFooter>
            <Button type="button" variant="outline" onClick={() => setCreateAgentOpen(false)}>Cancel</Button>
            <Button type="submit" disabled={creatingAgent || !agentServerId || (agentProfileId === "manual" && !agentCommandLine.trim())}>{creatingAgent ? "Starting…" : agentProfileId === "terminal" ? "Create terminal" : "Create agent"}</Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
    <Dialog open={Boolean(deletingApp)} onOpenChange={(open) => !open && setDeletingApp(null)}><DialogContent><DialogHeader><DialogTitle>Delete App</DialogTitle><DialogDescription>Delete {deletingApp?.name}, stop its process, and remove its service and virtual hostname?</DialogDescription></DialogHeader><DialogFooter><Button variant="outline" onClick={() => setDeletingApp(null)}>Cancel</Button><Button variant="destructive" onClick={deleteAppDeployment}>Delete App</Button></DialogFooter></DialogContent></Dialog>


    <Dialog open={installOpen} onOpenChange={setInstallOpen}>
      <DialogContent className="max-w-xl" data-tour="add-machine-dialog">
        <DialogHeader><DialogTitle>Add machine</DialogTitle><DialogDescription>Install Treer, then connect this workspace.</DialogDescription></DialogHeader>
        <div className="space-y-4">
          <Field label="Operating system"><Select value={installPlatform} onValueChange={setInstallPlatform}><SelectTrigger aria-label="Installation operating system"><SelectValue /></SelectTrigger><SelectContent><SelectItem value="unix">Linux / macOS</SelectItem><SelectItem value="windows">Windows · PowerShell</SelectItem></SelectContent></Select></Field>
          <Field label="1. Install Treer"><div className="space-y-2"><Textarea readOnly value={installCommand} className="min-h-20 font-mono text-xs" /><Button size="sm" variant="outline" onClick={() => copy(installCommand)}><Copy />Copy install command</Button></div></Field>
          <Field label="2. Connect workspace"><div className="space-y-2"><Textarea readOnly value={connectCommand} className="min-h-24 font-mono text-xs" /><Button size="sm" onClick={() => copy(connectCommand)}><Copy />Copy connection command</Button></div></Field>
        </div>
        <DialogFooter><Button variant="outline" onClick={() => setInstallOpen(false)}>Close</Button></DialogFooter>
      </DialogContent>
    </Dialog>

    <Dialog open={Boolean(renameTarget)} onOpenChange={(open) => !open && setRenameTarget(null)}><DialogContent><form onSubmit={submitRename}><DialogHeader><DialogTitle>Rename {renameTarget?.kind}</DialogTitle><DialogDescription>Rename {renameTarget?.kind === "agent" ? agentPath(snapshot?.agents.find((agent) => agent.agent_id === renameTarget.id) ?? { agent_id: renameTarget.id, name: renameTarget.name, server_id: "", kind: "", status: "" }, workloadTree) : renameTarget?.name}. Names must be unique within the same folder.</DialogDescription></DialogHeader><div className="my-5"><Field label="Name"><Input value={renameName} onChange={(event) => setRenameName(event.target.value)} required autoFocus /></Field></div>{renameError && <p role="alert" className="mb-4 break-words text-sm text-destructive">{renameError}</p>}<DialogFooter><Button type="button" variant="outline" onClick={() => setRenameTarget(null)}>Cancel</Button><Button type="submit">Rename</Button></DialogFooter></form></DialogContent></Dialog>

    <Dialog open={uploadOpen} onOpenChange={(open) => !uploading && setUploadOpen(open)}><DialogContent><form onSubmit={uploadToAgentMachine} className="space-y-4"><DialogHeader><DialogTitle>Upload file</DialogTitle><DialogDescription>Upload to {machineName(selectedAgentMachine, selectedAgent?.server_id)}. The destination directory is relative to the Machine root.</DialogDescription></DialogHeader><Field label="Destination directory"><Input aria-label="Destination directory" className="font-mono" value={uploadDirectory} onChange={(event) => setUploadDirectory(event.target.value)} placeholder="." required disabled={uploading} /></Field><Field label="File"><Input key={uploadInputKey} aria-label="File" type="file" onChange={(event) => setUploadFile(event.target.files?.[0] ?? null)} disabled={uploading} /></Field><label className="flex items-center gap-2 text-xs text-muted-foreground"><input type="checkbox" className="size-4 accent-foreground" checked={overwriteUpload} onChange={(event) => setOverwriteUpload(event.target.checked)} disabled={uploading} />Replace an existing file with the same name</label><p className="text-[10px] text-muted-foreground">Maximum 16 MiB</p>{uploadMessage && <p className="text-xs text-emerald-700">{uploadMessage}</p>}{uploadError && <p className="text-xs text-red-600">{uploadError}</p>}<DialogFooter><Button type="button" variant="outline" onClick={() => setUploadOpen(false)} disabled={uploading}>Close</Button><Button type="submit" disabled={!uploadFile || uploading || !selectedAgentMachineOnline}><Upload />{uploading ? "Uploading" : "Upload"}</Button></DialogFooter></form></DialogContent></Dialog>

    <Dialog open={Boolean(deleteTarget)} onOpenChange={(open) => !open && setDeleteTarget(null)}><DialogContent><DialogHeader><DialogTitle>Delete {deleteTarget?.kind}</DialogTitle><DialogDescription>{deleteTarget?.kind === "machine" ? `Remove ${deleteTarget.name} and all of its agents? Its credential will be revoked, but its local service will not be uninstalled.` : `Delete ${deleteTarget ? agentPath(snapshot?.agents.find((agent) => agent.agent_id === deleteTarget.id) ?? { agent_id: deleteTarget.id, name: deleteTarget.name, server_id: "", kind: "", status: "" }, workloadTree) : "Agent"} and stop its process? This agent will not return after reconnecting.`}</DialogDescription></DialogHeader><DialogFooter><Button variant="outline" onClick={() => setDeleteTarget(null)}>Cancel</Button><Button variant="destructive" onClick={confirmDelete}>Delete</Button></DialogFooter></DialogContent></Dialog>
    <Dialog open={deleteWorkspaceOpen} onOpenChange={(open) => !open && setDeleteWorkspaceOpen(false)}><DialogContent><DialogHeader><DialogTitle>Delete workspace</DialogTitle><DialogDescription>{workspaceMachineCount === undefined ? "Checking the workspace machine inventory..." : workspaceMachineCount > 0 ? `Delete all ${workspaceMachineCount} ${workspaceMachineCount === 1 ? "machine" : "machines"} from ${workspace?.name ?? "this workspace"} first.` : `Delete ${workspace?.name}? It will disappear from active views while historical traffic and messages are retained. This cannot be undone.`}</DialogDescription></DialogHeader><DialogFooter><Button variant="outline" onClick={() => setDeleteWorkspaceOpen(false)}>Cancel</Button><Button variant="destructive" disabled={workspaceMachineCount !== 0} onClick={() => void confirmDeleteWorkspace()}>Delete workspace</Button></DialogFooter></DialogContent></Dialog>

    <Dialog open={inviteOpen} onOpenChange={setInviteOpen}><DialogContent><DialogHeader><DialogTitle>Invite member</DialogTitle><DialogDescription>This registration link can be used once.</DialogDescription></DialogHeader><Textarea readOnly value={inviteUrl} className="min-h-24 font-mono text-xs" /><DialogFooter><Button variant="outline" onClick={() => setInviteOpen(false)}>Close</Button><Button onClick={() => copy(inviteUrl)}><Copy />Copy link</Button></DialogFooter></DialogContent></Dialog>
  </>
}
