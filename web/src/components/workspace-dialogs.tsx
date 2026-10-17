import { ProviderCliInstaller } from "@/components/provider-cli-installer"
import { AgentReservationRecovery } from "@/components/agent-reservation-recovery"
import type { AgentCreationFailure } from "@/lib/agent-creation-failure"
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
import { t } from "@/i18n"

interface WorkspaceDialogsProps {
  reservationCleaned: boolean
  creationFailure: AgentCreationFailure | null
  creationCommandMissing: boolean
  onReservationCleaned: () => void
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

export function WorkspaceDialogs({ creationCommandMissing, reservationCleaned, creationFailure, onReservationCleaned, state, user, setUser, showError, createOrganization, organization, createWorkspace, saveLaunchProfile, launchFromProfile, onlineMachines, deleteLaunchProfile, createAgent, selectAgentProfile, launchProfiles, selectedCreateProfile, changeAgentCommandLine, deleteAppDeployment, copy, submitRename, snapshot, workloadTree, uploadToAgentMachine, selectedAgentMachine, selectedAgent, selectedAgentMachineOnline, confirmDelete, workspaceMachineCount, workspace, confirmDeleteWorkspace }: WorkspaceDialogsProps) {
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

    <SimpleNameDialog open={createOrganizationOpen} onOpenChange={setCreateOrganizationOpen} title={t("dialogs.create_organization")} description={t("dialogs.organizations_contain_members_and_workspaces")} label={t("org.organization_name")} value={createOrganizationName} onValueChange={setCreateOrganizationName} onSubmit={createOrganization} />
    <SimpleNameDialog open={createWorkspaceOpen} onOpenChange={setCreateWorkspaceOpen} title={t("org.create_workspace")} description={t("dialogs.add_workspace_to", { org: organization?.name ?? t("dialogs.this_organization") })} label={t("wsettings.workspace_name")} value={createWorkspaceName} onValueChange={setCreateWorkspaceName} onSubmit={createWorkspace} dataTour="create-workspace-dialog" />

    <Dialog open={profileEditorOpen} onOpenChange={(open) => { setProfileEditorOpen(open); if (!open) setEditingLaunchProfile(null) }}><DialogContent className="max-w-xl"><form onSubmit={saveLaunchProfile} className="grid gap-4 sm:grid-cols-2"><DialogHeader className="sm:col-span-2"><DialogTitle>{editingLaunchProfile ? t("dialogs.edit_launch_profile") : t("dialogs.new_launch_profile")}</DialogTitle><DialogDescription>{t("dialogs.reusable_agent_process_settings_for")}</DialogDescription></DialogHeader><Field label={t("dialogs.profile_name")}><Input value={launchProfileName} onChange={(event) => setLaunchProfileName(event.target.value)} required autoFocus maxLength={80} /></Field><Field label={t("apps.working_directory")}><Input className="font-mono" value={launchProfileCwd} onChange={(event) => setLaunchProfileCwd(event.target.value)} required /></Field><div className="sm:col-span-2"><Field label={t("dialogs.description")}><Input value={launchProfileDescription} onChange={(event) => setLaunchProfileDescription(event.target.value)} maxLength={1000} /></Field></div><div className="sm:col-span-2"><Field label={t("apps.command")}><Input className="font-mono" value={launchProfileCommandLine} onChange={(event) => setLaunchProfileCommandLine(event.target.value)} placeholder={t("dialogs.codex_review_base_main")} required /></Field></div><DialogFooter className="sm:col-span-2"><Button type="button" variant="outline" onClick={() => setProfileEditorOpen(false)}>{t("common.cancel")}</Button><Button type="submit"><Rocket />{editingLaunchProfile ? t("dialogs.save_profile") : t("dialogs.create_profile")}</Button></DialogFooter></form></DialogContent></Dialog>

    <Dialog open={Boolean(launchingProfile)} onOpenChange={(open) => !open && setLaunchingProfile(null)}><DialogContent><form onSubmit={launchFromProfile} className="space-y-4"><DialogHeader><DialogTitle>Run {launchingProfile?.name}</DialogTitle><DialogDescription>{t("dialogs.choose_where_to_start_this")}</DialogDescription></DialogHeader><Field label={t("traffic.class_machine")}><Select value={launchMachineId} onValueChange={setLaunchMachineId} required><SelectTrigger><SelectValue placeholder={t("dialogs.select_an_online_machine")} /></SelectTrigger><SelectContent>{onlineMachines.map((machine) => <SelectItem key={machine.server_id} value={machine.server_id}>{machineName(machine)}</SelectItem>)}</SelectContent></Select></Field><Field label={t("dialogs.agent_name")}><Input value={launchAgentName} onChange={(event) => setLaunchAgentName(event.target.value)} required maxLength={80} /></Field><DialogFooter><Button type="button" variant="outline" onClick={() => setLaunchingProfile(null)}>{t("common.cancel")}</Button><Button type="submit" disabled={!launchMachineId}><Play />{t("dialogs.run_agent")}</Button></DialogFooter></form></DialogContent></Dialog>

    <Dialog open={Boolean(deletingProfile)} onOpenChange={(open) => !open && setDeletingProfile(null)}><DialogContent><DialogHeader><DialogTitle>{t("dialogs.delete_launch_profile")}</DialogTitle><DialogDescription>{t("dialogs.delete_profile_confirm", { name: deletingProfile?.name ?? "" })}</DialogDescription></DialogHeader><DialogFooter><Button variant="outline" onClick={() => setDeletingProfile(null)}>{t("common.cancel")}</Button><Button variant="destructive" onClick={deleteLaunchProfile}>{t("dialogs.delete_profile")}</Button></DialogFooter></DialogContent></Dialog>

    <Dialog open={createAgentOpen} onOpenChange={setCreateAgentOpen}>
      <DialogContent className="max-h-[calc(100dvh-2rem)] overflow-y-auto" data-tour="create-agent-dialog">
        <form onSubmit={createAgent} className="min-w-0 space-y-4">
          <DialogHeader>
            <DialogTitle>{t("dialogs.create_agent")}</DialogTitle>
            <DialogDescription>{t("dialogs.start_a_terminal_or_agent")}</DialogDescription>
          </DialogHeader>
          {agentFolder && <p className="break-all text-xs text-muted-foreground">{t("dialogs.tree_folder", { path: agentFolder.path })}</p>}
          <Field label={t("traffic.class_machine")}>
            <Select value={agentServerId} onValueChange={setAgentServerId} required>
              <SelectTrigger aria-label={t("traffic.class_machine")}><SelectValue placeholder={t("dialogs.select_a_machine")} /></SelectTrigger>
              <SelectContent>{onlineMachines.map((machine) => <SelectItem key={machine.server_id} value={machine.server_id}>{machineName(machine)}</SelectItem>)}</SelectContent>
            </Select>
          </Field>
          <Field label={t("dialogs.launch")}>
            <Select value={agentProfileId} onValueChange={selectAgentProfile}>
              <SelectTrigger aria-label={t("dialogs.launch")} data-tour="agent-launch"><SelectValue /></SelectTrigger>
              <SelectContent>
                <SelectItem value="terminal">{t("machine.terminal")}</SelectItem>
                <SelectItem value="manual">{t("dialogs.custom_command")}</SelectItem>
                {launchProfiles.map((profile) => <SelectItem key={profile.profile_id} value={profile.profile_id}>{profile.name}</SelectItem>)}
              </SelectContent>
            </Select>
          </Field>
          {agentProfileId === "terminal" || agentProfileId === "manual"
            ? <Field label={t("apps.working_directory")}><Input value={agentCwd} onChange={(event) => setAgentCwd(event.target.value)} /></Field>
            : selectedCreateProfile
              ? <div className="min-w-0 max-w-full overflow-hidden rounded-md border bg-muted/30 px-3 py-2"><code className="block max-w-full truncate text-xs" title={formatCommandLine(selectedCreateProfile.command, selectedCreateProfile.args)}>{formatCommandLine(selectedCreateProfile.command, selectedCreateProfile.args)}</code><span className="mt-1 block max-w-full truncate text-[10px] text-muted-foreground">{selectedCreateProfile.cwd || "."}</span></div>
              : null}
          {agentProfileId === "manual" && <Field label={t("apps.command")}><Input aria-label={t("apps.command")} className="font-mono" value={agentCommandLine} onChange={(event) => changeAgentCommandLine(event.target.value)} placeholder={t("dialogs.path_to_your_launcher_option")} required /></Field>}
          {selectedCreateProfile && <span className="block text-[10px] text-muted-foreground">{t("dialogs.runs_the_saved_command_on")}</span>}
          <Field label={t("dialogs.name")}><Input aria-label={t("dialogs.name")} value={agentName} onChange={(event) => { setAgentName(event.target.value); setAgentNameCustomized(true) }} required /></Field>
          {reservationCleaned && <p role="status" className="text-sm">{t("provider.cleaned")}</p>}
          {createAgentError && <p role="alert" className="break-words text-sm text-destructive">{createAgentError}</p>}
          {creationFailure?.creation_state === "identity_removed" && <p role="status" className="text-sm">{t("provider.identity_removed")}</p>}
          {creationFailure && creationFailure.creation_state !== "identity_removed" && workspace && <AgentReservationRecovery key={`${workspace.workspace_id}/${creationFailure.agent_id}`} workspaceId={workspace.workspace_id} failure={creationFailure} onCleaned={onReservationCleaned} />}
          {createAgentOpen && creationCommandMissing && creationFailure?.creation_state === "identity_removed" && creationFailure.command && creationFailure.machine_id === agentServerId && workspace && snapshot?.servers.find(item => item.server_id === agentServerId) && <ProviderCliInstaller key={`${workspace.workspace_id}/${agentServerId}/${creationFailure.agent_id}`} workspaceId={workspace.workspace_id} machine={snapshot.servers.find(item => item.server_id === agentServerId)!} command={creationFailure.command} />}
          <DialogFooter>
            <Button type="button" variant="outline" onClick={() => setCreateAgentOpen(false)}>{t("common.cancel")}</Button>
            <Button type="submit" disabled={creatingAgent || Boolean(creationFailure && creationFailure.creation_state !== "identity_removed") || !agentServerId || (agentProfileId === "manual" && !agentCommandLine.trim())}>{creatingAgent ? t("dialogs.starting") : agentProfileId === "terminal" ? t("dialogs.create_terminal") : t("dialogs.create_agent")}</Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
    <Dialog open={Boolean(deletingApp)} onOpenChange={(open) => !open && setDeletingApp(null)}><DialogContent><DialogHeader><DialogTitle>{t("apps.delete_app")}</DialogTitle><DialogDescription>{t("dialogs.delete_app_confirm", { name: deletingApp?.name ?? "" })}</DialogDescription></DialogHeader><DialogFooter><Button variant="outline" onClick={() => setDeletingApp(null)}>{t("common.cancel")}</Button><Button variant="destructive" onClick={deleteAppDeployment}>{t("apps.delete_app")}</Button></DialogFooter></DialogContent></Dialog>


    <Dialog open={installOpen} onOpenChange={setInstallOpen}>
      <DialogContent className="max-w-xl" data-tour="add-machine-dialog">
        <DialogHeader><DialogTitle>{t("dialogs.add_machine")}</DialogTitle><DialogDescription>{t("dialogs.install_treer_then_connect_this")}</DialogDescription></DialogHeader>
        <div className="space-y-4">
          <Field label={t("dialogs.operating_system")}><Select value={installPlatform} onValueChange={setInstallPlatform}><SelectTrigger aria-label={t("dialogs.installation_operating_system")}><SelectValue /></SelectTrigger><SelectContent><SelectItem value="unix">{t("dialogs.linux_macos")}</SelectItem><SelectItem value="windows">{t("dialogs.windows_powershell")}</SelectItem></SelectContent></Select></Field>
          <Field label={t("dialogs.1_install_treer")}><div className="space-y-2"><Textarea readOnly value={installCommand} className="min-h-20 font-mono text-xs" /><Button size="sm" variant="outline" onClick={() => copy(installCommand)}><Copy />{t("dialogs.copy_install_command")}</Button></div></Field>
          <Field label={t("dialogs.2_connect_workspace")}><div className="space-y-2"><Textarea readOnly value={connectCommand} className="min-h-24 font-mono text-xs" /><Button size="sm" onClick={() => copy(connectCommand)}><Copy />{t("dialogs.copy_connection_command")}</Button></div></Field>
        </div>
        <DialogFooter><Button variant="outline" onClick={() => setInstallOpen(false)}>{t("org.close")}</Button></DialogFooter>
      </DialogContent>
    </Dialog>

    <Dialog open={Boolean(renameTarget)} onOpenChange={(open) => !open && setRenameTarget(null)}><DialogContent><form onSubmit={submitRename}><DialogHeader><DialogTitle>{t("dialogs.rename_title", { kind: renameTarget ? t(`kind.${renameTarget.kind}`) : "" })}</DialogTitle><DialogDescription>{t("dialogs.rename_body", { target: renameTarget?.kind === "agent" ? agentPath(snapshot?.agents.find((agent) => agent.agent_id === renameTarget.id) ?? { agent_id: renameTarget.id, name: renameTarget.name, server_id: "", kind: "", status: "" }, workloadTree) : renameTarget?.name })}</DialogDescription></DialogHeader><div className="my-5"><Field label={t("dialogs.name")}><Input value={renameName} onChange={(event) => setRenameName(event.target.value)} required autoFocus /></Field></div>{renameError && <p role="alert" className="mb-4 break-words text-sm text-destructive">{renameError}</p>}<DialogFooter><Button type="button" variant="outline" onClick={() => setRenameTarget(null)}>{t("common.cancel")}</Button><Button type="submit">{t("machine.rename")}</Button></DialogFooter></form></DialogContent></Dialog>

    <Dialog open={uploadOpen} onOpenChange={(open) => !uploading && setUploadOpen(open)}><DialogContent><form onSubmit={uploadToAgentMachine} className="space-y-4"><DialogHeader><DialogTitle>{t("surface.upload_file")}</DialogTitle><DialogDescription>{t("dialogs.upload_to", { machine: machineName(selectedAgentMachine, selectedAgent?.server_id) })}</DialogDescription></DialogHeader><Field label={t("dialogs.destination_directory")}><Input aria-label={t("dialogs.destination_directory")} className="font-mono" value={uploadDirectory} onChange={(event) => setUploadDirectory(event.target.value)} placeholder="." required disabled={uploading} /></Field><Field label={t("dialogs.file")}><Input key={uploadInputKey} aria-label={t("dialogs.file")} type="file" onChange={(event) => setUploadFile(event.target.files?.[0] ?? null)} disabled={uploading} /></Field><label className="flex items-center gap-2 text-xs text-muted-foreground"><input type="checkbox" className="size-4 accent-foreground" checked={overwriteUpload} onChange={(event) => setOverwriteUpload(event.target.checked)} disabled={uploading} />{t("dialogs.replace_an_existing_file_with")}</label><p className="text-[10px] text-muted-foreground">{t("dialogs.maximum_16_mib")}</p>{uploadMessage && <p className="text-xs text-emerald-700">{uploadMessage}</p>}{uploadError && <p className="text-xs text-red-600">{uploadError}</p>}<DialogFooter><Button type="button" variant="outline" onClick={() => setUploadOpen(false)} disabled={uploading}>{t("org.close")}</Button><Button type="submit" disabled={!uploadFile || uploading || !selectedAgentMachineOnline}><Upload />{uploading ? t("dialogs.uploading") : t("dialogs.upload")}</Button></DialogFooter></form></DialogContent></Dialog>

    <Dialog open={Boolean(deleteTarget)} onOpenChange={(open) => !open && setDeleteTarget(null)}><DialogContent><DialogHeader><DialogTitle>{t("dialogs.delete_title", { kind: deleteTarget ? t(`kind.${deleteTarget.kind}`) : "" })}</DialogTitle><DialogDescription>{deleteTarget?.kind === "machine" ? t("dialogs.remove_machine_confirm", { name: deleteTarget.name }) : t("dialogs.delete_agent_confirm", { path: deleteTarget ? agentPath(snapshot?.agents.find((agent) => agent.agent_id === deleteTarget.id) ?? { agent_id: deleteTarget.id, name: deleteTarget.name, server_id: "", kind: "", status: "" }, workloadTree) : t("dialogs.agent_fallback") })}</DialogDescription></DialogHeader><DialogFooter><Button variant="outline" onClick={() => setDeleteTarget(null)}>{t("common.cancel")}</Button><Button variant="destructive" onClick={confirmDelete}>{t("machine.delete")}</Button></DialogFooter></DialogContent></Dialog>
    <Dialog open={deleteWorkspaceOpen} onOpenChange={(open) => !open && setDeleteWorkspaceOpen(false)}><DialogContent><DialogHeader><DialogTitle>{t("wsettings.delete_workspace")}</DialogTitle><DialogDescription>{workspaceMachineCount === undefined ? t("dialogs.checking_inventory") : workspaceMachineCount > 0 ? t(workspaceMachineCount === 1 ? "dialogs.delete_machines_first_one" : "dialogs.delete_machines_first_many", { count: workspaceMachineCount, name: workspace?.name ?? t("dialogs.this_workspace") }) : t("dialogs.delete_workspace_confirm", { name: workspace?.name ?? "" })}</DialogDescription></DialogHeader><DialogFooter><Button variant="outline" onClick={() => setDeleteWorkspaceOpen(false)}>{t("common.cancel")}</Button><Button variant="destructive" disabled={workspaceMachineCount !== 0} onClick={() => void confirmDeleteWorkspace()}>{t("wsettings.delete_workspace")}</Button></DialogFooter></DialogContent></Dialog>

    <Dialog open={inviteOpen} onOpenChange={setInviteOpen}><DialogContent><DialogHeader><DialogTitle>{t("org.invite_member")}</DialogTitle><DialogDescription>{t("dialogs.this_registration_link_can_be")}</DialogDescription></DialogHeader><Textarea readOnly value={inviteUrl} className="min-h-24 font-mono text-xs" /><DialogFooter><Button variant="outline" onClick={() => setInviteOpen(false)}>{t("org.close")}</Button><Button onClick={() => copy(inviteUrl)}><Copy />{t("dialogs.copy_link")}</Button></DialogFooter></DialogContent></Dialog>
  </>
}
