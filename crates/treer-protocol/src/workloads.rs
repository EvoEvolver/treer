use super::*;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateWorkspaceRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateAgentRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_id: Option<String>,
    pub kind: String,
    pub name: String,
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default = "default_cols")]
    pub cols: u16,
    #[serde(default = "default_rows")]
    pub rows: u16,
    /// Linux network-sandbox ports to publish on the machine loopback.
    /// Each value is both the namespace listen port and the host publish port.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub publish_ports: Vec<u16>,
    /// Legacy recipe automation. Nonempty values are rejected with migration
    /// guidance; installers now run through explicit commands or profiles.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recipe: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetAgentStartupRequest {
    #[serde(default)]
    pub cwd: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub publish_ports: Vec<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentStartupSpec {
    pub agent_id: String,
    pub server_id: String,
    pub kind: String,
    pub name: String,
    pub cwd: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub publish_ports: Vec<u16>,
    pub enabled: bool,
    pub generation: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidateAgentStartupRequest {
    #[serde(default)]
    pub agent_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidateAgentStartupResponse {
    #[serde(default)]
    pub stopped_agent_ids: Vec<String>,
    #[serde(default)]
    pub active_agent_ids: Vec<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppDesiredState {
    #[default]
    Running,
    Stopped,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppDeploymentStatus {
    #[default]
    Pending,
    Running,
    Stopped,
    Exited,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppDeployment {
    pub app_id: String,
    pub workspace_id: String,
    pub name: String,
    pub server_id: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub cwd: String,
    pub port: u16,
    pub hostname: String,
    pub service_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access: Option<ServiceIngressAccess>,
    pub desired_state: AppDesiredState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_agent_id: Option<String>,
    #[serde(default)]
    pub restart_count: u64,
    #[serde(default)]
    pub status: AppDeploymentStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub created_by: String,
    pub updated_at: DateTime<Utc>,
    pub updated_by: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateAppDeploymentRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_id: Option<String>,
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub cwd: String,
    pub port: u16,
    #[serde(default)]
    pub hostname: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub public: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentLaunchProfile {
    pub profile_id: String,
    pub workspace_id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub cwd: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub created_by: String,
    pub updated_at: DateTime<Utc>,
    pub updated_by: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateAgentLaunchProfileRequest {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub cwd: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateAgentLaunchProfileRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchAgentProfileRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default = "default_cols")]
    pub cols: u16,
    #[serde(default = "default_rows")]
    pub rows: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenameRequest {
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MachineExecRequest {
    #[serde(default)]
    pub cwd: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default = "default_machine_exec_timeout_ms")]
    pub timeout_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MachineExecResponse {
    pub server_id: String,
    pub cwd: String,
    pub command: String,
    pub args: Vec<String>,
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub truncated: bool,
    pub timed_out: bool,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UploadMachineFileRequest {
    #[serde(default)]
    pub directory: String,
    pub file_name: String,
    pub content_base64: String,
    #[serde(default)]
    pub overwrite: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UploadMachineFileResponse {
    pub server_id: String,
    pub path: String,
    pub bytes_written: u64,
}

const fn default_machine_exec_timeout_ms() -> u64 {
    30_000
}

const fn default_cols() -> u16 {
    120
}

const fn default_rows() -> u16 {
    36
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptAgentRequest {
    pub text: String,
}

/// Public delivery options. Sender identity is supplied only by the Proxy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubmitAgentPromptRequest {
    pub text: String,
    #[serde(default)]
    pub delivery: PromptDelivery,
    #[serde(default)]
    pub notify: Option<PromptNotify>,
    #[serde(default)]
    pub require_receipt: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputAgentRequest {
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadAgentOutputResponse {
    pub agent_id: String,
    pub revision: u64,
    pub text: String,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentPrompt {
    pub prompt_id: String,
    pub text: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentPromptQueueResponse {
    pub agent_id: String,
    pub prompts: Vec<AgentPrompt>,
    pub remaining: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentServerSnapshot {
    pub server: ServerInfo,
    pub agents: Vec<AgentInfo>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum AgentCommand {
    StartupAdminGet {
        agent_id: String,
    },
    StartupAdminSet {
        agent_id: String,
        request: ReplaceAgentStartupRequest,
    },
    RestartWithStartup {
        agent_id: String,
        request: ReplaceAgentStartupRequest,
    },
    RestoreRuntime {},
    EnsureLifecycle {
        agent_id: String,
        workload_credential: Option<String>,
        request: CreateAgentRequest,
        enabled: bool,
        #[serde(default = "default_start_missing")]
        start_missing: bool,
    },
    Start {
        agent_id: String,
    },
    Restart {
        agent_id: String,
    },
    Create {
        agent_id: String,
        workload_credential: String,
        request: CreateAgentRequest,
    },
    Prompt {
        agent_id: String,
        text: String,
    },
    SubmitPrompt {
        agent_id: String,
        request: SubmitAgentPromptRequest,
        sender: Option<PromptSender>,
    },
    QueueRead {
        agent_id: String,
    },
    InterfaceStatus {
        agent_id: String,
    },
    SubmissionRead {
        agent_id: String,
        submission_id: String,
    },
    QueueCancel {
        agent_id: String,
        submission_id: String,
    },
    Input {
        agent_id: String,
        data: Vec<u8>,
    },
    Read {
        agent_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        lines: Option<usize>,
    },
    Transcript {
        agent_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cursor: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        limit: Option<usize>,
    },
    Stop {
        agent_id: String,
    },
    Abort {
        agent_id: String,
    },
    StartupSet {
        agent_id: String,
        request: SetAgentStartupRequest,
    },
    StartupGet {
        agent_id: String,
    },
    StartupClear {
        agent_id: String,
    },
    ProbeNetwork {
        host: String,
        port: u16,
        timeout_ms: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target_agent_id: Option<String>,
    },
    /// Fetches bounded JSON from a machine-local HTTP service. The Controller
    /// always connects to loopback; the Proxy cannot supply an arbitrary host.
    FetchServiceHttp {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        policy_schema_version: Option<u32>,
        port: u16,
        path: String,
        max_bytes: u32,
        timeout_ms: u64,
    },
    Exec {
        request: MachineExecRequest,
    },
    UploadBegin {
        upload_id: String,
        directory: String,
        file_name: String,
        overwrite: bool,
    },
    UploadChunk {
        upload_id: String,
        content_base64: String,
    },
    UploadCommit {
        upload_id: String,
    },
    UploadAbort {
        upload_id: String,
    },
    ShutdownMachine,
}

impl AgentCommand {
    pub fn required_capability(&self) -> Option<&'static str> {
        match self {
            Self::SubmitPrompt { .. }
            | Self::QueueRead { .. }
            | Self::InterfaceStatus { .. }
            | Self::SubmissionRead { .. }
            | Self::QueueCancel { .. } => Some(CAPABILITY_AGENT_PROMPT_DELIVERY),
            Self::StartupAdminGet { .. }
            | Self::StartupAdminSet { .. }
            | Self::RestartWithStartup { .. } => Some(CAPABILITY_AGENT_STARTUP_ADMIN),
            Self::EnsureLifecycle { .. } | Self::Start { .. } | Self::Restart { .. } => {
                Some(CAPABILITY_AGENT_LIFECYCLE)
            }
            Self::RestoreRuntime {} => Some(CAPABILITY_RUNTIME_RESTORE),
            Self::Abort { .. } => Some(CAPABILITY_AGENT_ABORT),
            Self::StartupSet { .. } | Self::StartupGet { .. } | Self::StartupClear { .. } => {
                Some(CAPABILITY_AGENT_STARTUP)
            }
            Self::Exec { .. } => Some(CAPABILITY_MACHINE_EXEC),
            Self::UploadBegin { .. }
            | Self::UploadChunk { .. }
            | Self::UploadCommit { .. }
            | Self::UploadAbort { .. } => Some(CAPABILITY_MACHINE_UPLOAD),
            Self::FetchServiceHttp {
                policy_schema_version: Some(2),
                ..
            } => Some(CAPABILITY_POLICY_PATH_RULES),
            Self::FetchServiceHttp { .. } => Some(CAPABILITY_SERVICE_HTTP_FETCH),
            Self::Create { .. }
            | Self::Prompt { .. }
            | Self::Input { .. }
            | Self::Read { .. }
            | Self::Transcript { .. }
            | Self::Stop { .. }
            | Self::ProbeNetwork { .. }
            | Self::ShutdownMachine => None,
        }
    }
}

/// Replaces argv, retaining the target's saved cwd when omitted. Identity,
/// credentials, ports, and generation are not client-controlled.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReplaceAgentStartupRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
}

impl ReplaceAgentStartupRequest {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.command.trim().is_empty()
            || self.cwd.as_ref().is_some_and(|cwd| cwd.contains('\0'))
            || std::iter::once(&self.command)
                .chain(self.args.iter())
                .any(|arg| arg.contains('\0'))
        {
            return Err(ProtocolError::new(
                "invalid_agent_startup",
                "startup command must be nonempty and paths/argv must not contain NUL",
            ));
        }
        // Reject Treer workload/operator secrets, including another Agent's credential.
        // Arbitrary third-party secrets cannot be recognized: callers must use
        // private files or inherited environment instead of embedding them.
        if std::iter::once(&self.command)
            .chain(self.args.iter())
            .chain(self.cwd.iter())
            .any(|arg| {
                ["wlc_", "opc_"].iter().any(|prefix| {
                    arg.match_indices(*prefix).any(|(offset, _)| {
                        arg.as_bytes()
                            .get(offset + 4..offset + 68)
                            .is_some_and(|secret| secret.iter().all(u8::is_ascii_hexdigit))
                    })
                })
            })
        {
            return Err(ProtocolError::new(
                "invalid_agent_startup",
                "startup spec must not contain workload or operator credentials",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommandEnvelope {
    pub command_id: String,
    pub workspace_id: String,
    pub command: AgentCommand,
}
