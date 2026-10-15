import type { NewAgentFolder } from "@/components/workload-tree"
import type { AgentLaunchProfile,AppDeployment } from "@/lib/api"
import { defaultAgentName,type DeleteTarget,type RenameTarget } from "@/lib/control-plane"
import { useState } from "react"

// State only: request handlers, validation and lifecycle behavior stay in the
// workspace controller. Defaults match the original component declarations.
export function useWorkspaceDialogState() {
  const [settingsOpen, setSettingsOpen] = useState(false)
  const [createOrganizationOpen, setCreateOrganizationOpen] = useState(false)
  const [createWorkspaceOpen, setCreateWorkspaceOpen] = useState(false)
  const [deleteWorkspaceOpen, setDeleteWorkspaceOpen] = useState(false)
  const [createAgentOpen, setCreateAgentOpen] = useState(false)
  const [agentFolder, setAgentFolder] = useState<NewAgentFolder | null>(null)
  const [renameError, setRenameError] = useState("")
  const [createAgentError, setCreateAgentError] = useState("")
  const [creatingAgent, setCreatingAgent] = useState(false)
  const [deletingApp, setDeletingApp] = useState<AppDeployment | null>(null)
  const [profileEditorOpen, setProfileEditorOpen] = useState(false)
  const [editingLaunchProfile, setEditingLaunchProfile] = useState<AgentLaunchProfile | null>(null)
  const [launchingProfile, setLaunchingProfile] = useState<AgentLaunchProfile | null>(null)
  const [deletingProfile, setDeletingProfile] = useState<AgentLaunchProfile | null>(null)
  const [installOpen, setInstallOpen] = useState(false)
  const [inviteOpen, setInviteOpen] = useState(false)
  const [uploadOpen, setUploadOpen] = useState(false)
  const [uploadDirectory, setUploadDirectory] = useState(".")
  const [uploadFile, setUploadFile] = useState<File | null>(null)
  const [overwriteUpload, setOverwriteUpload] = useState(false)
  const [uploading, setUploading] = useState(false)
  const [uploadInputKey, setUploadInputKey] = useState(0)
  const [uploadMessage, setUploadMessage] = useState("")
  const [uploadError, setUploadError] = useState("")
  const [renameTarget, setRenameTarget] = useState<RenameTarget>(null)
  const [deleteTarget, setDeleteTarget] = useState<DeleteTarget>(null)
  const [createOrganizationName, setCreateOrganizationName] = useState("")
  const [createWorkspaceName, setCreateWorkspaceName] = useState("")
  const [agentName, setAgentName] = useState(defaultAgentName("terminal"))
  const [agentNameCustomized, setAgentNameCustomized] = useState(false)
  const [agentProfileId, setAgentProfileId] = useState("terminal")
  const [agentServerId, setAgentServerId] = useState("")
  const [agentCwd, setAgentCwd] = useState(".")
  const [agentCommandLine, setAgentCommandLine] = useState("")
  const [launchProfileName, setLaunchProfileName] = useState("")
  const [launchProfileDescription, setLaunchProfileDescription] = useState("")
  const [launchProfileCwd, setLaunchProfileCwd] = useState(".")
  const [launchProfileCommandLine, setLaunchProfileCommandLine] = useState("")
  const [launchMachineId, setLaunchMachineId] = useState("")
  const [launchAgentName, setLaunchAgentName] = useState("")
  const [renameName, setRenameName] = useState("")
  const [unixInstallCommand, setInstallCommand] = useState("")
  const [unixConnectCommand, setConnectCommand] = useState("")
  const [windowsInstallCommand, setWindowsInstallCommand] = useState("")
  const [windowsConnectCommand, setWindowsConnectCommand] = useState("")
  const [installPlatform, setInstallPlatform] = useState("unix")
  const [inviteUrl, setInviteUrl] = useState("")
  const installCommand = installPlatform === "windows" ? windowsInstallCommand : unixInstallCommand
  const connectCommand = installPlatform === "windows" ? windowsConnectCommand : unixConnectCommand
  return {
    settingsOpen,
    setSettingsOpen,
    createOrganizationOpen,
    setCreateOrganizationOpen,
    createWorkspaceOpen,
    setCreateWorkspaceOpen,
    deleteWorkspaceOpen,
    setDeleteWorkspaceOpen,
    createAgentOpen,
    setCreateAgentOpen,
    agentFolder,
    setAgentFolder,
    renameError,
    setRenameError,
    createAgentError,
    setCreateAgentError,
    creatingAgent,
    setCreatingAgent,
    deletingApp,
    setDeletingApp,
    profileEditorOpen,
    setProfileEditorOpen,
    editingLaunchProfile,
    setEditingLaunchProfile,
    launchingProfile,
    setLaunchingProfile,
    deletingProfile,
    setDeletingProfile,
    installOpen,
    setInstallOpen,
    inviteOpen,
    setInviteOpen,
    uploadOpen,
    setUploadOpen,
    uploadDirectory,
    setUploadDirectory,
    uploadFile,
    setUploadFile,
    overwriteUpload,
    setOverwriteUpload,
    uploading,
    setUploading,
    uploadInputKey,
    setUploadInputKey,
    uploadMessage,
    setUploadMessage,
    uploadError,
    setUploadError,
    renameTarget,
    setRenameTarget,
    deleteTarget,
    setDeleteTarget,
    createOrganizationName,
    setCreateOrganizationName,
    createWorkspaceName,
    setCreateWorkspaceName,
    agentName,
    setAgentName,
    agentNameCustomized,
    setAgentNameCustomized,
    agentProfileId,
    setAgentProfileId,
    agentServerId,
    setAgentServerId,
    agentCwd,
    setAgentCwd,
    agentCommandLine,
    setAgentCommandLine,
    launchProfileName,
    setLaunchProfileName,
    launchProfileDescription,
    setLaunchProfileDescription,
    launchProfileCwd,
    setLaunchProfileCwd,
    launchProfileCommandLine,
    setLaunchProfileCommandLine,
    launchMachineId,
    setLaunchMachineId,
    launchAgentName,
    setLaunchAgentName,
    renameName,
    setRenameName,
    unixInstallCommand,
    setInstallCommand,
    unixConnectCommand,
    setConnectCommand,
    windowsInstallCommand,
    setWindowsInstallCommand,
    windowsConnectCommand,
    setWindowsConnectCommand,
    installPlatform,
    setInstallPlatform,
    inviteUrl,
    setInviteUrl,
    installCommand,
    connectCommand
  }
}
