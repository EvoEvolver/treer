use std::collections::HashSet;
use std::ffi::OsString;
use std::io::IsTerminal;
use std::io::Read as StdRead;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use std::{env, fs};

use anyhow::{bail, Context};
use chrono::{DateTime, Utc};
use clap::{Parser, Subcommand, ValueEnum};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, size as terminal_size};
use futures_util::{SinkExt, StreamExt};
use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
use reqwest::Method;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
#[cfg(unix)]
use tokio::net::{UnixListener, UnixStream};
use tokio::process::Command as TokioCommand;
use tokio::sync::Semaphore;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::Message;
use treer_protocol::{
    AcknowledgeMessagesRequest, AgentInfo, AgentStatus, ApiError, CreateAgentLaunchProfileRequest,
    CreateAgentRequest, CreateMachineServiceRequest, CreateServiceIngressRequest,
    CreateVirtualNetworkHostRequest, GetMessageResponse, ImportMessagesRequest, InputAgentRequest,
    LaunchAgentProfileRequest, LegacyMailMessage, MachineServiceProtocol, MessageExternalSource,
    PluginManifest, PluginOAuthExchangeRequest, PluginOAuthStartRequest, ReceiveMessagesRequest,
    RenameRequest, RevokePluginSessionRequest, RevokePluginSessionsRequest, SendMessageRequest,
    ServerInfo, ServiceIngressAccess, TerminalClientMessage, TerminalServerMessage,
    UpdateAgentLaunchProfileRequest, UpdateMachineServiceRequest, UpdateServiceIngressRequest,
    WorkloadIdentityTokenRequest, WorkloadIdentityTokenResponse, WorkspaceSnapshot,
    AGENT_ID_HEADER, OPERATOR_CREDENTIAL_HEADER, PLUGIN_ID_HEADER, PLUGIN_SESSION_HEADER,
    WORKLOAD_CREDENTIAL_HEADER,
};
use url::Url;

const WAIT_POLL_INTERVAL: Duration = Duration::from_millis(150);
const ATTACH_DETACH_BYTE: u8 = 0x1d;
const PLUGIN_BROKER_SOCKET_ENV: &str = "TREER_PLUGIN_BROKER_SOCKET";
const PLUGIN_BROKER_TOKEN_ENV: &str = "TREER_PLUGIN_BROKER_TOKEN";
const PLUGIN_HUMAN_SESSION_ENV: &str = "TREER_PLUGIN_HUMAN_SESSION";
const INTERNAL_PLUGIN_ID_ENV: &str = "TREER_INTERNAL_PLUGIN_ID";
const INTERNAL_PLUGIN_SESSION_ENV: &str = "TREER_INTERNAL_PLUGIN_SESSION";
const PLUGIN_BROKER_MAX_REQUEST_BYTES: usize = 2 * 1024 * 1024;
const PLUGIN_BROKER_MAX_OUTPUT_BYTES: usize = 2 * 1024 * 1024;
const PLUGIN_PACKAGE_MAX_BYTES: u64 = 32 * 1024 * 1024;
const PLUGIN_PACKAGE_MAX_FILES: usize = 4_096;
const IGNORED_PLUGIN_DIRECTORIES: &[&str] = &[".git", "__pycache__", "node_modules", "target"];
const SKILL: &str = include_str!("../../../skills/treer/SKILL.md");

#[derive(Debug, Parser)]
#[command(
    name = "treer",
    about = "Discover and coordinate Treer agents",
    version = treer_build_info::DISPLAY,
    arg_required_else_help = true
)]
struct Args {
    #[arg(
        long,
        visible_alias = "skills",
        help = "Print the bundled agent skill and exit"
    )]
    skill: bool,
    #[arg(long, env = "TREER_AGENT_SERVER_URL")]
    url: Option<Url>,
    #[arg(long, env = "TREER_WORKSPACE_ID", default_value = "default")]
    workspace: String,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    #[command(about = "Control and communicate with workspace agents")]
    Agent {
        #[command(subcommand)]
        command: AgentCommand,
    },
    #[command(about = "Manage reusable Agent launch profiles")]
    Profile {
        #[command(subcommand)]
        command: ProfileCommand,
    },
    #[command(about = "Discover human members of the workspace organization")]
    Human {
        #[command(subcommand)]
        command: HumanCommand,
    },
    #[command(about = "Manage workspace machines")]
    Machine {
        #[command(subcommand)]
        command: MachineCommand,
    },
    #[command(about = "Manage workspace virtual hosts", visible_alias = "vhost")]
    VirtualHost {
        #[command(subcommand)]
        command: VirtualHostCommand,
    },
    #[command(about = "Register and maintain long-running machine services")]
    Service {
        #[command(subcommand)]
        command: ServiceCommand,
    },
    #[command(about = "Publish machine services through wildcard HTTPS ingress")]
    Publish {
        #[command(subcommand)]
        command: PublishCommand,
    },
    #[command(about = "Obtain a short-lived identity token for a workspace service")]
    Identity {
        #[command(subcommand)]
        command: IdentityCommand,
    },
    #[command(about = "Exchange durable contextual Messages")]
    Message {
        #[command(subcommand)]
        command: MessageCommand,
    },
    #[command(about = "Validate, install, inspect, and run CLI-only script plugins")]
    Plugin {
        #[command(subcommand)]
        command: PluginCommand,
    },
    #[command(about = "Show the current managed agent identity")]
    Whoami,
    #[command(about = "Show this workspace, its machines, and its agents")]
    Discover,
    #[command(about = "List agents (compatibility alias for `agent list`)")]
    List,
    #[command(about = "Create an agent")]
    Create {
        #[arg(long)]
        server: Option<String>,
        #[arg(long)]
        kind: String,
        #[arg(long)]
        name: String,
        #[arg(long, default_value = ".")]
        cwd: String,
        #[arg(last = true)]
        args: Vec<String>,
    },
    #[command(about = "Submit a prompt (compatibility alias for `agent prompt`)")]
    Prompt {
        target: String,
        text: String,
        #[command(flatten)]
        wait: PromptWaitArgs,
    },
    #[command(about = "Read output (compatibility alias for `agent read`)")]
    Read {
        target: String,
        #[arg(long, default_value_t = 100)]
        lines: usize,
    },
    #[command(about = "Rename an agent (compatibility alias for `agent rename`)")]
    Rename { target: String, name: String },
    #[command(about = "Delete an agent (compatibility alias for `agent delete`)")]
    Delete { target: String },
    #[command(about = "Attach an interactive terminal (compatibility alias for `agent attach`)")]
    Attach { target: String },
    #[command(about = "Stop an agent (compatibility alias for `agent stop`)")]
    Stop { target: String },
}

#[derive(Debug, Subcommand)]
enum HumanCommand {
    #[command(about = "List organization members addressable from this workspace")]
    List,
}

#[derive(Debug, Subcommand)]
enum MessageCommand {
    #[command(about = "Send a durable Message")]
    Send {
        #[arg(long = "to", required = true)]
        recipients: Vec<String>,
        #[arg(long = "context")]
        context_ids: Vec<String>,
        #[arg(long, conflicts_with = "body_file")]
        body: Option<String>,
        #[arg(long, value_name = "PATH", conflicts_with = "body")]
        body_file: Option<PathBuf>,
        #[arg(long)]
        idempotency_key: Option<String>,
        #[arg(long, value_name = "RFC3339")]
        expires_at: Option<String>,
        #[arg(long)]
        correlation_id: Option<String>,
        #[arg(long)]
        trace_id: Option<String>,
        #[arg(long, value_name = "PATH")]
        external_source_file: Option<PathBuf>,
    },
    #[command(about = "Reply with a context edge to an existing Message")]
    Reply {
        message_id: String,
        #[arg(long = "to")]
        recipients: Vec<String>,
        #[arg(long = "context")]
        additional_context_ids: Vec<String>,
        #[arg(long, conflicts_with = "body_file")]
        body: Option<String>,
        #[arg(long, value_name = "PATH", conflicts_with = "body")]
        body_file: Option<PathBuf>,
        #[arg(long)]
        idempotency_key: Option<String>,
        #[arg(long, value_name = "PATH")]
        external_source_file: Option<PathBuf>,
    },
    #[command(about = "Get one visible Message by stable ID")]
    Get { message_id: String },
    #[command(about = "List visible Message history without acknowledging it")]
    List {
        #[arg(long)]
        before: Option<String>,
        #[arg(long, default_value_t = 50)]
        limit: u16,
    },
    #[command(about = "Receive unacknowledged deliveries")]
    Receive {
        #[arg(long, default_value_t = 0, value_name = "MS")]
        wait: u64,
        #[arg(long, default_value_t = 50)]
        limit: u16,
    },
    #[command(about = "Explicitly acknowledge one or more deliveries")]
    Ack {
        #[arg(required = true)]
        delivery_ids: Vec<String>,
        #[arg(long)]
        operation_id: Option<String>,
    },
    #[command(about = "Import a bounded operator-authorized legacy Message batch")]
    Import {
        #[arg(long, default_value = "legacy-mail-v1")]
        format: String,
        #[arg(long, value_name = "PATH")]
        body_file: PathBuf,
        #[arg(long)]
        operation_id: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
enum PluginCommand {
    #[command(about = "Validate a plugin package without executing it")]
    Validate { package: PathBuf },
    #[command(about = "Install an immutable plugin package without executing it")]
    Install { package: PathBuf },
    #[command(about = "List installed plugin versions")]
    List,
    #[command(about = "Inspect the selected installed version of a plugin")]
    Inspect { id: String },
    #[command(about = "Run a plugin in the foreground through a command-limited broker")]
    Run {
        id: String,
        #[arg(long)]
        config: PathBuf,
    },
    #[command(about = "Manage a broker-bound human session for this plugin")]
    Auth {
        #[command(subcommand)]
        command: PluginAuthCommand,
    },
}

#[derive(Debug, Subcommand)]
enum PluginAuthCommand {
    #[command(about = "Create a browser authorization URL")]
    Start {
        #[arg(long)]
        service: String,
        #[arg(long)]
        redirect_uri: String,
    },
    #[command(about = "Exchange an OAuth callback for a plugin-bound human session")]
    Exchange {
        #[arg(long)]
        service: String,
        #[arg(long)]
        code: String,
        #[arg(long)]
        state: String,
    },
    #[command(about = "Revoke one plugin-bound human session")]
    Revoke { session_capability: String },
    #[command(about = "Revoke every human session for this plugin instance")]
    RevokeAll,
}

#[derive(Debug, Subcommand)]
enum AgentCommand {
    #[command(about = "List agents in the current workspace")]
    List,
    #[command(about = "Show one agent by unique name or id")]
    Get { target: String },
    #[command(about = "Rename an agent")]
    Rename { target: String, name: String },
    #[command(about = "Stop and permanently remove an agent")]
    Delete { target: String },
    #[command(about = "Attach the current terminal; press Ctrl-] to detach")]
    Attach { target: String },
    #[command(about = "Submit a prompt to another agent")]
    Prompt {
        target: String,
        text: String,
        #[command(flatten)]
        wait: PromptWaitArgs,
    },
    #[command(about = "Read recent terminal output")]
    Read {
        target: String,
        #[arg(long, default_value_t = 100)]
        lines: usize,
    },
    #[command(about = "Send raw terminal key presses")]
    SendKeys {
        target: String,
        #[arg(required = true, num_args = 1..)]
        keys: Vec<String>,
    },
    #[command(about = "Wait for an agent status")]
    Wait {
        target: String,
        #[arg(long, value_enum)]
        until: Vec<WaitStatus>,
        #[arg(long, value_name = "MS")]
        timeout: Option<u64>,
    },
    #[command(about = "Stop an agent")]
    Stop { target: String },
}

#[derive(Debug, Subcommand)]
enum ProfileCommand {
    #[command(about = "List launch profiles in the current workspace")]
    List,
    #[command(about = "Show a launch profile by unique name or id")]
    Get { target: String },
    #[command(about = "Create a reusable command-based launch profile")]
    Create {
        name: String,
        #[arg(long, default_value = "")]
        description: String,
        #[arg(long, default_value = ".")]
        cwd: String,
        #[arg(value_name = "COMMAND")]
        executable: String,
        #[arg(last = true)]
        args: Vec<String>,
    },
    #[command(about = "Update a launch profile")]
    Update {
        target: String,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        description: Option<String>,
        #[arg(long)]
        cwd: Option<String>,
        #[arg(long = "command")]
        executable: Option<String>,
        #[arg(long = "arg", allow_hyphen_values = true)]
        args: Vec<String>,
        #[arg(long, conflicts_with = "args")]
        clear_args: bool,
    },
    #[command(about = "Delete a launch profile")]
    Delete { target: String },
    #[command(about = "Create an Agent from a launch profile")]
    Launch {
        target: String,
        #[arg(long)]
        machine: Option<String>,
        #[arg(long)]
        name: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
enum MachineCommand {
    #[command(about = "Rename a machine")]
    Rename { target: String, name: String },
    #[command(about = "Remove a machine and revoke its credential")]
    Delete { target: String },
}

#[derive(Debug, Subcommand)]
enum VirtualHostCommand {
    #[command(about = "List workspace virtual hosts")]
    List,
    #[command(about = "Map a virtual hostname to a registered service")]
    Add { hostname: String, service: String },
    #[command(about = "Delete a workspace virtual host")]
    Delete { hostname: String },
}

#[derive(Debug, Subcommand)]
enum ServiceCommand {
    #[command(about = "List registered machine services")]
    List,
    #[command(about = "Register a long-running service")]
    Register {
        name: String,
        #[arg(long)]
        machine: Option<String>,
        #[arg(long, default_value = "127.0.0.1")]
        target_host: String,
        #[arg(long)]
        port: u16,
        #[arg(long, value_enum, default_value_t = CliServiceProtocol::Tcp)]
        protocol: CliServiceProtocol,
    },
    #[command(about = "Update a registered service")]
    Update {
        target: String,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        machine: Option<String>,
        #[arg(long)]
        target_host: Option<String>,
        #[arg(long)]
        port: Option<u16>,
        #[arg(long, value_enum)]
        protocol: Option<CliServiceProtocol>,
    },
    #[command(about = "Probe a service from its machine")]
    Probe { target: String },
    #[command(about = "Delete a service and its virtual hosts")]
    Delete { target: String },
}

#[derive(Debug, Subcommand)]
enum IdentityCommand {
    #[command(about = "Print an audience-bound Bearer token")]
    Token {
        #[arg(value_name = "SERVICE")]
        audience: String,
        #[arg(long, help = "Print the complete JSON token response")]
        json: bool,
    },
}

#[derive(Debug, Subcommand)]
enum PublishCommand {
    #[command(about = "List published service endpoints")]
    List,
    #[command(about = "Publish an HTTP service")]
    Create {
        service: String,
        #[arg(long)]
        slug: Option<String>,
        #[arg(long, value_enum, default_value_t = CliIngressAccess::Public)]
        access: CliIngressAccess,
    },
    #[command(about = "Change a published endpoint's access mode")]
    Access {
        target: String,
        access: CliIngressAccess,
    },
    #[command(about = "Enable a published endpoint")]
    Enable { target: String },
    #[command(about = "Disable a published endpoint without deleting it")]
    Disable { target: String },
    #[command(about = "Delete a published endpoint")]
    Delete { target: String },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum CliIngressAccess {
    Public,
    Workspace,
}

impl From<CliIngressAccess> for ServiceIngressAccess {
    fn from(value: CliIngressAccess) -> Self {
        match value {
            CliIngressAccess::Public => Self::Public,
            CliIngressAccess::Workspace => Self::Workspace,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum CliServiceProtocol {
    Tcp,
    Http,
}

impl From<CliServiceProtocol> for MachineServiceProtocol {
    fn from(value: CliServiceProtocol) -> Self {
        match value {
            CliServiceProtocol::Tcp => Self::Tcp,
            CliServiceProtocol::Http => Self::Http,
        }
    }
}

#[derive(Debug, clap::Args)]
struct PromptWaitArgs {
    #[arg(
        long,
        help = "Wait for a matching status after observed agent activity"
    )]
    wait: bool,
    #[arg(long, value_enum, requires = "wait")]
    until: Vec<WaitStatus>,
    #[arg(long, value_name = "MS", requires = "wait")]
    timeout: Option<u64>,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum WaitStatus {
    Starting,
    Working,
    Idle,
    Blocked,
    Exited,
    Failed,
    Unknown,
}

impl WaitStatus {
    fn matches(self, status: AgentStatus) -> bool {
        matches!(
            (self, status),
            (Self::Starting, AgentStatus::Starting)
                | (Self::Working, AgentStatus::Working)
                | (Self::Idle, AgentStatus::Idle)
                | (Self::Blocked, AgentStatus::Blocked)
                | (Self::Exited, AgentStatus::Exited)
                | (Self::Failed, AgentStatus::Failed)
                | (Self::Unknown, AgentStatus::Unknown)
        )
    }
}

struct ApiClient {
    http: reqwest::Client,
    base: Url,
    source_agent_id: Option<String>,
    workload_credential: Option<String>,
    operator_credential: Option<String>,
    plugin_id: Option<String>,
    plugin_session: Option<String>,
}

#[derive(Debug)]
struct CliFailure {
    code: String,
    message: String,
}

impl std::fmt::Display for CliFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for CliFailure {}

impl ApiClient {
    fn new(base: Url, workspace: &str) -> Self {
        let source_agent_id = env::var("TREER_AGENT_ID").ok();
        let workload_credential = env::var("TREER_WORKLOAD_CREDENTIAL").ok();
        let operator_credential = if source_agent_id.is_none() && workload_credential.is_none() {
            env::var("TREER_OPERATOR_CREDENTIAL")
                .ok()
                .or_else(|| load_operator_credential(workspace))
        } else {
            None
        };
        let plugin_id = env::var(INTERNAL_PLUGIN_ID_ENV).ok();
        let plugin_session = env::var(INTERNAL_PLUGIN_SESSION_ENV).ok();
        Self {
            http: reqwest::Client::new(),
            base,
            source_agent_id,
            workload_credential,
            operator_credential,
            plugin_id,
            plugin_session,
        }
    }

    async fn request<T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> anyhow::Result<T> {
        let url = self
            .base
            .join(path)
            .context("failed to build request URL")?;
        let mut request = self.http.request(method, url);
        if let Some(agent_id) = &self.source_agent_id {
            request = request.header(AGENT_ID_HEADER, agent_id);
        }
        if let Some(credential) = &self.workload_credential {
            request = request.header(WORKLOAD_CREDENTIAL_HEADER, credential);
        }
        if let Some(credential) = &self.operator_credential {
            request = request.header(OPERATOR_CREDENTIAL_HEADER, credential);
        }
        if let Some(plugin_id) = &self.plugin_id {
            request = request.header(PLUGIN_ID_HEADER, plugin_id);
        }
        if let Some(plugin_session) = &self.plugin_session {
            request = request.header(PLUGIN_SESSION_HEADER, plugin_session);
        }
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().await.context("request failed")?;
        let status = response.status();
        let value: Value = response.json().await.context("invalid JSON response")?;
        if !status.is_success() {
            let failure = serde_json::from_value::<ApiError>(value)
                .map(|error| CliFailure {
                    code: error.error.code,
                    message: error.error.message,
                })
                .unwrap_or_else(|_| CliFailure {
                    code: "invalid_api_error".to_string(),
                    message: format!("Treer API returned {status}"),
                });
            return Err(failure.into());
        }
        serde_json::from_value(value).context("unexpected API response")
    }

    async fn value(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> anyhow::Result<Value> {
        self.request(method, path, body).await
    }

    async fn get_agent(&self, target: &str) -> anyhow::Result<AgentInfo> {
        let target = normalize_target(target)?;
        self.request(
            Method::GET,
            &format!("api/agents/{}", path_segment(&target)),
            None,
        )
        .await
    }

    async fn prompt(&self, target: &str, text: String) -> anyhow::Result<AgentInfo> {
        let target = normalize_target(target)?;
        self.request(
            Method::POST,
            &format!("api/agents/{}/prompt", path_segment(&target)),
            Some(json!({ "text": text })),
        )
        .await
    }

    async fn wait_for(
        &self,
        target: &str,
        until: &[WaitStatus],
        timeout_ms: Option<u64>,
        baseline: Option<(AgentStatus, u64)>,
    ) -> anyhow::Result<AgentInfo> {
        let deadline = timeout_ms.map(|timeout| Instant::now() + Duration::from_millis(timeout));
        loop {
            let agent = self.get_agent(target).await?;
            let changed = baseline.is_none_or(|(status, revision)| {
                agent.status != status || agent.output_revision > revision
            });
            if changed && until.iter().any(|expected| expected.matches(agent.status)) {
                return Ok(agent);
            }
            if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                bail!(
                    "timed out waiting for {} (status {}, output revision {})",
                    target,
                    status_name(agent.status),
                    agent.output_revision
                );
            }
            tokio::time::sleep(WAIT_POLL_INTERVAL).await;
        }
    }
}

fn load_operator_credential(workspace: &str) -> Option<String> {
    let bytes = fs::read(local_controller_config(workspace)?).ok()?;
    serde_json::from_slice::<LocalControllerConfig>(&bytes)
        .ok()
        .map(|config| config.operator_credential)
        .filter(|credential| !credential.is_empty())
}

#[tokio::main]
async fn main() {
    if env::var_os(PLUGIN_BROKER_SOCKET_ENV).is_some()
        || env::var_os(PLUGIN_BROKER_TOKEN_ENV).is_some()
    {
        let code = match run_plugin_broker_client().await {
            Ok(code) => code,
            Err(error) => {
                eprintln!(
                    "{}",
                    serde_json::to_string(&json!({
                        "error": {"code": "plugin_broker_failed", "message": error.to_string()}
                    }))
                    .unwrap_or_default()
                );
                1
            }
        };
        std::process::exit(code);
    }
    if let Err(error) = run_cli().await {
        let (code, message) = error.downcast_ref::<CliFailure>().map_or_else(
            || ("cli_failed", error.to_string()),
            |failure| (failure.code.as_str(), failure.message.clone()),
        );
        eprintln!(
            "{}",
            serde_json::to_string(&json!({
                "error": {"code": code, "message": message}
            }))
            .unwrap_or_else(|_| {
                "{\"error\":{\"code\":\"cli_failed\",\"message\":\"Treer command failed\"}}"
                    .to_string()
            })
        );
        std::process::exit(1);
    }
}

async fn run_cli() -> anyhow::Result<()> {
    let args = Args::parse();
    if args.skill {
        print!("{SKILL}");
        return Ok(());
    }
    let command = args
        .command
        .context("a command is required; run `treer --help` for usage")?;
    let command = match command {
        Command::Plugin {
            command: PluginCommand::Auth { command },
        } => {
            let client = ApiClient::new(
                resolve_server_url(args.url.clone(), &args.workspace)?,
                &args.workspace,
            );
            let value = run_plugin_auth_command(&client, command).await?;
            println!("{}", serde_json::to_string_pretty(&value)?);
            return Ok(());
        }
        Command::Plugin { command } => {
            let value = run_plugin_command(command, &args.workspace, args.url.clone()).await?;
            println!("{}", serde_json::to_string_pretty(&value)?);
            return Ok(());
        }
        command => command,
    };
    let client = ApiClient::new(
        resolve_server_url(args.url, &args.workspace)?,
        &args.workspace,
    );
    let value = match command {
        Command::Agent { command } => run_agent_command(&client, command).await?,
        Command::Profile { command } => run_profile_command(&client, command).await?,
        Command::Human { command } => match command {
            HumanCommand::List => client.value(Method::GET, "api/humans", None).await?,
        },
        Command::Machine { command } => run_machine_command(&client, command).await?,
        Command::VirtualHost { command } => run_virtual_host_command(&client, command).await?,
        Command::Service { command } => run_service_command(&client, command).await?,
        Command::Publish { command } => run_publish_command(&client, command).await?,
        Command::Identity { command } => {
            let IdentityCommand::Token { audience, json } = command;
            let response: WorkloadIdentityTokenResponse = client
                .request(
                    Method::POST,
                    "api/identity/token",
                    Some(serde_json::to_value(WorkloadIdentityTokenRequest {
                        audience,
                    })?),
                )
                .await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&response)?);
            } else {
                println!("{}", response.access_token);
            }
            return Ok(());
        }
        Command::Message { command } => run_message_command(&client, command).await?,
        Command::Plugin { .. } => unreachable!("plugin commands return before API client setup"),
        Command::Whoami => whoami(&client).await?,
        Command::Discover => discover(&client).await?,
        Command::List => client.value(Method::GET, "api/agents", None).await?,
        Command::Create {
            server,
            kind,
            name,
            cwd,
            args,
        } => {
            client
                .value(
                    Method::POST,
                    "api/agents",
                    Some(serde_json::to_value(CreateAgentRequest {
                        server_id: server,
                        kind,
                        name,
                        cwd,
                        args,
                        cols: 120,
                        rows: 36,
                    })?),
                )
                .await?
        }
        Command::Prompt { target, text, wait } => {
            prompt_and_maybe_wait(&client, &target, text, wait).await?
        }
        Command::Read { target, lines } => read_agent(&client, &target, lines).await?,
        Command::Rename { target, name } => rename_agent(&client, &target, name).await?,
        Command::Delete { target } => delete_agent(&client, &target).await?,
        Command::Attach { target } => attach_agent(&client, &target).await?,
        Command::Stop { target } => stop_agent(&client, &target).await?,
    };
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}

async fn whoami(client: &ApiClient) -> anyhow::Result<Value> {
    let agent_id = std::env::var("TREER_AGENT_ID")
        .context("TREER_AGENT_ID is not set; `treer whoami` must run inside a managed agent")?;
    let server_id = std::env::var("TREER_SERVER_ID")
        .context("TREER_SERVER_ID is not set; managed agent identity is incomplete")?;
    let snapshot: WorkspaceSnapshot = client.request(Method::GET, "api/discovery", None).await?;
    let (machine, agent) = resolve_self(&snapshot, &agent_id, &server_id)?;
    Ok(json!({
        "workspace": snapshot.workspace,
        "machine": machine,
        "agent": agent,
    }))
}

async fn discover(client: &ApiClient) -> anyhow::Result<Value> {
    let snapshot: WorkspaceSnapshot = client.request(Method::GET, "api/discovery", None).await?;
    let agent_id = std::env::var("TREER_AGENT_ID").ok();
    let server_id = std::env::var("TREER_SERVER_ID").ok();
    discovery_value(snapshot, agent_id.as_deref(), server_id.as_deref())
}

fn discovery_value(
    snapshot: WorkspaceSnapshot,
    agent_id: Option<&str>,
    server_id: Option<&str>,
) -> anyhow::Result<Value> {
    let self_value = match (agent_id, server_id) {
        (Some(agent_id), Some(server_id)) => {
            let (machine, agent) = resolve_self(&snapshot, agent_id, server_id)?;
            json!({ "machine": machine, "agent": agent })
        }
        (None, None) => Value::Null,
        _ => bail!("managed agent identity is incomplete; TREER_AGENT_ID and TREER_SERVER_ID must both be set"),
    };
    let mut value = serde_json::to_value(snapshot)?;
    value
        .as_object_mut()
        .context("workspace discovery response is not an object")?
        .insert("self".to_string(), self_value);
    Ok(value)
}

fn resolve_self(
    snapshot: &WorkspaceSnapshot,
    agent_id: &str,
    server_id: &str,
) -> anyhow::Result<(ServerInfo, AgentInfo)> {
    let agent = snapshot
        .agents
        .iter()
        .find(|agent| agent.agent_id == agent_id)
        .with_context(|| format!("current agent {agent_id} is missing from workspace discovery"))?;
    if agent.server_id != server_id {
        bail!(
            "current agent {agent_id} belongs to machine {}, not injected machine {server_id}",
            agent.server_id
        );
    }
    let machine = snapshot
        .servers
        .iter()
        .find(|machine| machine.server_id == server_id)
        .with_context(|| {
            format!("current machine {server_id} is missing from workspace discovery")
        })?;
    Ok((machine.clone(), agent.clone()))
}

async fn run_agent_command(client: &ApiClient, command: AgentCommand) -> anyhow::Result<Value> {
    match command {
        AgentCommand::List => client.value(Method::GET, "api/agents", None).await,
        AgentCommand::Get { target } => Ok(serde_json::to_value(client.get_agent(&target).await?)?),
        AgentCommand::Rename { target, name } => rename_agent(client, &target, name).await,
        AgentCommand::Delete { target } => delete_agent(client, &target).await,
        AgentCommand::Attach { target } => attach_agent(client, &target).await,
        AgentCommand::Prompt { target, text, wait } => {
            prompt_and_maybe_wait(client, &target, text, wait).await
        }
        AgentCommand::Read { target, lines } => read_agent(client, &target, lines).await,
        AgentCommand::SendKeys { target, keys } => {
            let target = normalize_target(&target)?;
            let data = encode_keys(&keys)?;
            client
                .value(
                    Method::POST,
                    &format!("api/agents/{}/input", path_segment(&target)),
                    Some(serde_json::to_value(InputAgentRequest { data })?),
                )
                .await
        }
        AgentCommand::Wait {
            target,
            until,
            timeout,
        } => {
            let until = default_wait_statuses(until);
            Ok(serde_json::to_value(
                client.wait_for(&target, &until, timeout, None).await?,
            )?)
        }
        AgentCommand::Stop { target } => stop_agent(client, &target).await,
    }
}

async fn run_message_command(client: &ApiClient, command: MessageCommand) -> anyhow::Result<Value> {
    match command {
        MessageCommand::Send {
            recipients,
            context_ids,
            body,
            body_file,
            idempotency_key,
            expires_at,
            correlation_id,
            trace_id,
            external_source_file,
        } => {
            let request = SendMessageRequest {
                recipients,
                context_ids,
                body: read_message_body(body, body_file)?,
                expires_at: expires_at
                    .map(|value| parse_rfc3339(&value, "message expiry"))
                    .transpose()?,
                idempotency_key,
                correlation_id,
                trace_id,
                external_source: read_external_source(external_source_file)?,
            };
            client
                .value(
                    Method::POST,
                    "api/messages",
                    Some(serde_json::to_value(request)?),
                )
                .await
        }
        MessageCommand::Reply {
            message_id,
            mut recipients,
            additional_context_ids,
            body,
            body_file,
            idempotency_key,
            external_source_file,
        } => {
            let message_id = normalize_message_id(&message_id)?;
            let parent: GetMessageResponse = client
                .request(
                    Method::GET,
                    &format!("api/messages/{}", path_segment(&message_id)),
                    None,
                )
                .await?;
            if recipients.is_empty() {
                recipients.push(parent.message.sender.id);
            } else {
                recipients = recipients
                    .into_iter()
                    .map(|recipient| {
                        if recipient == "sender" {
                            parent.message.sender.id.clone()
                        } else {
                            recipient
                        }
                    })
                    .collect();
            }
            let mut context_ids = vec![message_id];
            context_ids.extend(additional_context_ids);
            client
                .value(
                    Method::POST,
                    "api/messages",
                    Some(serde_json::to_value(SendMessageRequest {
                        recipients,
                        context_ids,
                        body: read_message_body(body, body_file)?,
                        expires_at: None,
                        idempotency_key,
                        correlation_id: parent.message.correlation_id,
                        trace_id: parent.message.trace_id,
                        external_source: read_external_source(external_source_file)?,
                    })?),
                )
                .await
        }
        MessageCommand::Get { message_id } => {
            let message_id = normalize_message_id(&message_id)?;
            client
                .value(
                    Method::GET,
                    &format!("api/messages/{}", path_segment(&message_id)),
                    None,
                )
                .await
        }
        MessageCommand::List { before, limit } => {
            let suffix = message_list_suffix(before.as_deref(), limit);
            client.value(Method::GET, &suffix, None).await
        }
        MessageCommand::Receive { wait, limit } => {
            client
                .value(
                    Method::POST,
                    "api/messages/receive",
                    Some(serde_json::to_value(ReceiveMessagesRequest {
                        limit,
                        wait_milliseconds: wait,
                    })?),
                )
                .await
        }
        MessageCommand::Ack {
            delivery_ids,
            operation_id,
        } => {
            client
                .value(
                    Method::POST,
                    "api/messages/ack",
                    Some(serde_json::to_value(AcknowledgeMessagesRequest {
                        delivery_ids,
                        operation_id: operation_id.unwrap_or_else(new_operation_id),
                    })?),
                )
                .await
        }
        MessageCommand::Import {
            format,
            body_file,
            operation_id,
        } => {
            let messages = read_legacy_import(&body_file)?;
            client
                .value(
                    Method::POST,
                    "api/messages/import",
                    Some(serde_json::to_value(ImportMessagesRequest {
                        format,
                        operation_id: operation_id.unwrap_or_else(new_operation_id),
                        messages,
                    })?),
                )
                .await
        }
    }
}

fn read_message_body(body: Option<String>, body_file: Option<PathBuf>) -> anyhow::Result<String> {
    match (body, body_file) {
        (Some(body), None) => Ok(body),
        (None, Some(path)) => read_text_file_or_stdin(&path),
        (None, None) if !std::io::stdin().is_terminal() => read_stdin_text(),
        (None, None) => bail!("message body is required through --body, --body-file, or stdin"),
        (Some(_), Some(_)) => bail!("--body and --body-file cannot be used together"),
    }
}

fn read_external_source(path: Option<PathBuf>) -> anyhow::Result<Option<MessageExternalSource>> {
    path.map(|path| {
        let contents = read_text_file_or_stdin(&path)?;
        serde_json::from_str(&contents).context("external source file is not valid JSON")
    })
    .transpose()
}

fn read_legacy_import(path: &PathBuf) -> anyhow::Result<Vec<LegacyMailMessage>> {
    let contents = read_text_file_or_stdin(path)?;
    if let Ok(messages) = serde_json::from_str::<Vec<LegacyMailMessage>>(&contents) {
        return Ok(messages);
    }
    if let Ok(request) = serde_json::from_str::<ImportMessagesRequest>(&contents) {
        return Ok(request.messages);
    }
    contents
        .lines()
        .filter(|line| !line.trim().is_empty())
        .enumerate()
        .map(|(index, line)| {
            serde_json::from_str::<LegacyMailMessage>(line)
                .with_context(|| format!("legacy-mail-v1 JSONL record {} is invalid", index + 1))
        })
        .collect::<anyhow::Result<Vec<_>>>()
        .and_then(|messages| {
            if messages.is_empty() {
                bail!("legacy-mail-v1 import contains no messages")
            } else {
                Ok(messages)
            }
        })
}

fn read_text_file_or_stdin(path: &PathBuf) -> anyhow::Result<String> {
    if path.as_os_str() == "-" {
        read_stdin_text()
    } else {
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))
    }
}

fn read_stdin_text() -> anyhow::Result<String> {
    let mut contents = String::new();
    std::io::stdin()
        .read_to_string(&mut contents)
        .context("failed to read stdin")?;
    Ok(contents)
}

fn parse_rfc3339(value: &str, field: &str) -> anyhow::Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .map(|value| value.with_timezone(&Utc))
        .with_context(|| format!("{field} must be an RFC3339 timestamp"))
}

fn message_list_suffix(before: Option<&str>, limit: u16) -> String {
    let mut query = url::form_urlencoded::Serializer::new(String::new());
    query.append_pair("limit", &limit.to_string());
    if let Some(before) = before {
        query.append_pair("before", before);
    }
    format!("api/messages?{}", query.finish())
}

fn normalize_message_id(message_id: &str) -> anyhow::Result<String> {
    let message_id = message_id.trim();
    if message_id.is_empty() || message_id.len() > 256 {
        bail!("message ID must be non-empty and at most 256 bytes");
    }
    Ok(message_id.to_string())
}

fn new_operation_id() -> String {
    format!("op_{}", uuid::Uuid::new_v4().simple())
}

#[derive(Debug, Serialize)]
struct ValidatedPluginPackage {
    manifest: PluginManifest,
    package_path: PathBuf,
    entrypoint_script: PathBuf,
    file_count: usize,
    total_bytes: u64,
    package_sha256: String,
}

#[derive(Debug, Serialize)]
struct InstalledPluginSummary {
    id: String,
    display_name: String,
    version: String,
    package_path: PathBuf,
    capabilities: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct PluginBrokerRequest {
    token: String,
    argv: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    stdin: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    human_session: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct PluginBrokerResponse {
    exit_code: i32,
    stdout: String,
    stderr: String,
}

#[cfg(unix)]
#[derive(Clone)]
struct PluginBrokerContext {
    plugin_id: String,
    token: Arc<str>,
    capabilities: Arc<HashSet<String>>,
    workspace: Arc<str>,
    server_url: Arc<str>,
    executable: Arc<PathBuf>,
    concurrency: Arc<Semaphore>,
}

async fn run_plugin_command(
    command: PluginCommand,
    workspace: &str,
    configured_url: Option<Url>,
) -> anyhow::Result<Value> {
    match command {
        PluginCommand::Validate { package } => {
            Ok(serde_json::to_value(validate_plugin_package(&package)?)?)
        }
        PluginCommand::Install { package } => {
            let installed = install_plugin_package(&package)?;
            Ok(json!({"plugin": installed, "installed": true}))
        }
        PluginCommand::List => Ok(json!({"plugins": list_installed_plugins()?})),
        PluginCommand::Inspect { id } => {
            let plugin = load_installed_plugin(&id)?;
            Ok(json!({"plugin": plugin}))
        }
        PluginCommand::Run { id, config } => {
            let package = load_installed_plugin(&id)?;
            let server_url = resolve_server_url(configured_url, workspace)?;
            let status = run_plugin_foreground(&package, &config, workspace, &server_url).await?;
            Ok(json!({
                "plugin_id": package.manifest.id,
                "version": package.manifest.version,
                "exit_code": status
            }))
        }
        PluginCommand::Auth { .. } => {
            unreachable!("plugin auth commands use the brokered API client")
        }
    }
}

async fn run_plugin_auth_command(
    client: &ApiClient,
    command: PluginAuthCommand,
) -> anyhow::Result<Value> {
    let plugin_id = client
        .plugin_id
        .as_ref()
        .context("plugin auth commands are available only through a running plugin broker")?;
    match command {
        PluginAuthCommand::Start {
            service,
            redirect_uri,
        } => {
            client
                .value(
                    Method::POST,
                    "api/plugins/oauth/start",
                    Some(serde_json::to_value(PluginOAuthStartRequest {
                        plugin_id: plugin_id.clone(),
                        service_id: service,
                        redirect_uri,
                    })?),
                )
                .await
        }
        PluginAuthCommand::Exchange {
            service,
            code,
            state,
        } => {
            client
                .value(
                    Method::POST,
                    "api/plugins/oauth/exchange",
                    Some(serde_json::to_value(PluginOAuthExchangeRequest {
                        plugin_id: plugin_id.clone(),
                        service_id: service,
                        code,
                        state,
                    })?),
                )
                .await
        }
        PluginAuthCommand::Revoke { session_capability } => {
            client
                .value(
                    Method::POST,
                    "api/plugins/sessions/revoke",
                    Some(serde_json::to_value(RevokePluginSessionRequest {
                        plugin_id: plugin_id.clone(),
                        session_capability,
                    })?),
                )
                .await
        }
        PluginAuthCommand::RevokeAll => {
            client
                .value(
                    Method::POST,
                    "api/plugins/sessions/revoke-all",
                    Some(serde_json::to_value(RevokePluginSessionsRequest {
                        plugin_id: plugin_id.clone(),
                    })?),
                )
                .await
        }
    }
}

fn validate_plugin_package(path: &Path) -> anyhow::Result<ValidatedPluginPackage> {
    let package_path =
        if path.is_file() && path.file_name().is_some_and(|name| name == "plugin.json") {
            path.parent()
                .context("plugin.json has no parent directory")?
                .to_path_buf()
        } else {
            path.to_path_buf()
        };
    let package_path = package_path
        .canonicalize()
        .with_context(|| format!("plugin package {} does not exist", package_path.display()))?;
    if !package_path.is_dir() {
        bail!("plugin package must be a directory or its plugin.json file");
    }
    let manifest_path = package_path.join("plugin.json");
    let metadata = fs::metadata(&manifest_path)
        .with_context(|| format!("{} is missing", manifest_path.display()))?;
    if !metadata.is_file() || metadata.len() > treer_protocol::MAX_PLUGIN_MANIFEST_BYTES as u64 {
        bail!(
            "plugin.json must be a regular file no larger than {} bytes",
            treer_protocol::MAX_PLUGIN_MANIFEST_BYTES
        );
    }
    let manifest: PluginManifest =
        serde_json::from_slice(&fs::read(&manifest_path).context("failed to read plugin.json")?)
            .context("plugin.json does not match the strict v1 manifest shape")?;
    validate_plugin_manifest(&manifest, &package_path)?;

    let files = collect_plugin_files(&package_path)?;
    let total_bytes = files.iter().try_fold(0_u64, |total, path| {
        fs::metadata(path)
            .map(|metadata| total.saturating_add(metadata.len()))
            .context("failed to inspect plugin file")
    })?;
    if files.len() > PLUGIN_PACKAGE_MAX_FILES || total_bytes > PLUGIN_PACKAGE_MAX_BYTES {
        bail!(
            "plugin package exceeds the {} file or {} byte limit",
            PLUGIN_PACKAGE_MAX_FILES,
            PLUGIN_PACKAGE_MAX_BYTES
        );
    }
    for file in &files {
        let relative = file
            .strip_prefix(&package_path)
            .context("plugin file escaped package root")?;
        let name = relative.file_name().and_then(|value| value.to_str());
        if name == Some("Cargo.toml")
            || file.extension().and_then(|value| value.to_str()) == Some("rs")
        {
            bail!(
                "script plugins may not contain Rust packages or source files: {}",
                relative.display()
            );
        }
        if let Some(expected) = manifest.checksums.get(&relative_path_text(relative)?) {
            let actual = format!("sha256:{}", file_sha256(file)?);
            if &actual != expected {
                bail!("plugin checksum mismatch for {}", relative.display());
            }
        }
    }
    for checked in manifest.checksums.keys() {
        let checked_path = Path::new(checked);
        validate_relative_plugin_path(checked_path)?;
        if ignored_plugin_path(checked_path) {
            bail!("plugin checksum references ignored generated file {checked}");
        }
        if !package_path.join(checked).is_file() {
            bail!("plugin checksum references missing file {checked}");
        }
    }
    let entrypoint_script = resolve_entrypoint_script(&package_path, &manifest)?;
    Ok(ValidatedPluginPackage {
        package_sha256: package_sha256(&package_path, &files)?,
        manifest,
        package_path,
        entrypoint_script,
        file_count: files.len(),
        total_bytes,
    })
}

fn validate_plugin_manifest(manifest: &PluginManifest, root: &Path) -> anyhow::Result<()> {
    if manifest.schema_version != treer_protocol::PLUGIN_MANIFEST_SCHEMA_VERSION {
        bail!(
            "unsupported plugin manifest schema version {}; expected {}",
            manifest.schema_version,
            treer_protocol::PLUGIN_MANIFEST_SCHEMA_VERSION
        );
    }
    validate_plugin_id(&manifest.id)?;
    if manifest.display_name.trim().is_empty() || manifest.display_name.len() > 128 {
        bail!("plugin display_name must contain 1-128 bytes");
    }
    parse_semver(&manifest.version).context("plugin version must be semantic version x.y.z")?;
    let minimum = parse_semver(&manifest.minimum_treer_version)
        .context("minimum_treer_version must be semantic version x.y.z")?;
    let current =
        parse_semver(treer_build_info::VERSION).context("running Treer version is not semantic")?;
    if minimum > current {
        bail!(
            "plugin requires Treer {}, but this CLI is {}",
            manifest.minimum_treer_version,
            treer_build_info::VERSION
        );
    }
    if manifest.entrypoint.argv.is_empty() || manifest.entrypoint.argv.len() > 32 {
        bail!("plugin entrypoint argv must contain 1-32 values");
    }
    if manifest
        .entrypoint
        .argv
        .iter()
        .any(|value| value.is_empty() || value.len() > 4_096 || value.as_bytes().contains(&0))
    {
        bail!("plugin entrypoint values must contain 1-4096 bytes without NUL characters");
    }
    let supported_operating_systems = ["linux", "macos", "windows"];
    if manifest.entrypoint.operating_systems.len() > supported_operating_systems.len()
        || manifest
            .entrypoint
            .operating_systems
            .iter()
            .collect::<HashSet<_>>()
            .len()
            != manifest.entrypoint.operating_systems.len()
        || manifest
            .entrypoint
            .operating_systems
            .iter()
            .any(|value| !supported_operating_systems.contains(&value.as_str()))
    {
        bail!("plugin operating_systems must contain unique supported platform names");
    }
    if manifest.state_version == 0 {
        bail!("plugin state_version must be greater than zero");
    }
    validate_unique_names("capabilities", &manifest.capabilities, 64)?;
    validate_unique_names("configuration", &manifest.configuration, 64)?;
    validate_unique_names("secrets", &manifest.secrets, 32)?;
    for capability in &manifest.capabilities {
        if !KNOWN_PLUGIN_CAPABILITIES.contains(&capability.as_str()) {
            bail!("plugin declares unsupported capability {capability}");
        }
        if capability == "message.import" {
            bail!("ordinary plugin manifests may not declare message.import");
        }
    }
    let plugin_environment_prefix = format!(
        "TREER_{}_",
        manifest.id.to_ascii_uppercase().replace('-', "_")
    );
    for name in manifest.configuration.iter().chain(manifest.secrets.iter()) {
        if !valid_environment_name(name) {
            bail!("plugin environment declaration {name} is invalid");
        }
        if RESERVED_PLUGIN_ENVIRONMENT.contains(&name.as_str()) {
            bail!("plugin may not declare reserved environment variable {name}");
        }
        if name.starts_with("TREER_") && !name.starts_with(&plugin_environment_prefix) {
            bail!(
                "plugin Treer environment variables must use the {plugin_environment_prefix} namespace"
            );
        }
    }
    if manifest
        .configuration
        .iter()
        .any(|name| manifest.secrets.contains(name))
    {
        bail!("plugin configuration and secret names must not overlap");
    }
    if let Some(http) = &manifest.http_service {
        if !http.health_path.starts_with('/')
            || http.health_path.starts_with("//")
            || http.health_path.len() > 512
            || http.health_path.chars().any(char::is_control)
        {
            bail!("plugin HTTP health path is invalid");
        }
        if let Some(name) = &http.listen_environment {
            if !manifest.configuration.contains(name) || !valid_environment_name(name) {
                bail!("HTTP listen environment must be a declared configuration variable");
            }
        }
    }
    if root.join("config.schema.json").exists() {
        let schema: Value = serde_json::from_slice(&fs::read(root.join("config.schema.json"))?)
            .context("config.schema.json is not valid JSON")?;
        if !schema.is_object() {
            bail!("config.schema.json must contain a JSON object");
        }
    }
    Ok(())
}

const KNOWN_PLUGIN_CAPABILITIES: &[&str] = &[
    "message.send",
    "message.read",
    "message.receive",
    "message.ack",
    "agent.prompt",
    "agent.discover",
    "agent.metadata.read",
    "human.list",
    "identity.self.read",
    "plugin.oauth",
];

const RESERVED_PLUGIN_ENVIRONMENT: &[&str] = &[
    "HOME",
    "PATH",
    "LANG",
    "LC_ALL",
    "TZ",
    "TMPDIR",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    "PYTHONUNBUFFERED",
    "TREER_AGENT_ID",
    "TREER_SERVER_ID",
    "TREER_WORKSPACE_ID",
    "TREER_AGENT_SERVER_URL",
    "TREER_WORKLOAD_CREDENTIAL",
    "TREER_OPERATOR_CREDENTIAL",
    "TREER_PROXY_PUBLIC_URL",
    "TREER_PROXY_URL",
    "TREER_MACHINE_TOKEN",
    "TREER_ENROLLMENT_KEY",
    "TREER_HOST_SOCKET",
    "TREER_CLI",
    PLUGIN_BROKER_SOCKET_ENV,
    PLUGIN_BROKER_TOKEN_ENV,
    "TREER_PLUGIN_ID",
    "TREER_PLUGIN_VERSION",
    "TREER_PLUGIN_STATE_DIR",
    "TREER_PLUGIN_CONFIG",
    PLUGIN_HUMAN_SESSION_ENV,
    INTERNAL_PLUGIN_ID_ENV,
    INTERNAL_PLUGIN_SESSION_ENV,
];

fn validate_unique_names(label: &str, values: &[String], maximum: usize) -> anyhow::Result<()> {
    if values.len() > maximum || values.iter().collect::<HashSet<_>>().len() != values.len() {
        bail!("plugin {label} must be unique and contain at most {maximum} values");
    }
    Ok(())
}

fn validate_plugin_id(id: &str) -> anyhow::Result<()> {
    let valid = !id.is_empty()
        && id.len() <= 63
        && id.as_bytes()[0].is_ascii_lowercase()
        && id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-');
    if !valid {
        bail!("plugin id must match [a-z][a-z0-9-]{{0,62}}");
    }
    Ok(())
}

fn valid_environment_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name.as_bytes()[0].is_ascii_uppercase()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
}

fn parse_semver(value: &str) -> anyhow::Result<(u64, u64, u64)> {
    let core = value.split(['-', '+']).next().context("version is empty")?;
    let parts = core
        .split('.')
        .map(str::parse::<u64>)
        .collect::<Result<Vec<_>, _>>()?;
    match parts.as_slice() {
        [major, minor, patch] => Ok((*major, *minor, *patch)),
        _ => bail!("version must contain major, minor, and patch components"),
    }
}

fn collect_plugin_files(root: &Path) -> anyhow::Result<Vec<PathBuf>> {
    fn visit(root: &Path, directory: &Path, files: &mut Vec<PathBuf>) -> anyhow::Result<()> {
        let mut entries = fs::read_dir(directory)
            .with_context(|| format!("failed to read {}", directory.display()))?
            .collect::<Result<Vec<_>, _>>()?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path)?;
            if metadata.file_type().is_symlink() {
                bail!(
                    "plugin packages may not contain symlinks: {}",
                    path.display()
                );
            }
            let relative = path
                .strip_prefix(root)
                .context("plugin file escaped package root")?;
            if ignored_plugin_path(relative) {
                continue;
            }
            if metadata.is_dir() {
                visit(root, &path, files)?;
            } else if metadata.is_file() {
                files.push(path);
            } else {
                bail!(
                    "plugin package contains a non-regular file: {}",
                    path.display()
                );
            }
            if files.len() > PLUGIN_PACKAGE_MAX_FILES {
                bail!("plugin package contains too many files");
            }
        }
        Ok(())
    }

    let mut files = Vec::new();
    visit(root, root, &mut files)?;
    Ok(files)
}

fn ignored_plugin_path(path: &Path) -> bool {
    if path.components().any(|component| {
        let Component::Normal(value) = component else {
            return false;
        };
        IGNORED_PLUGIN_DIRECTORIES
            .iter()
            .any(|ignored| value == std::ffi::OsStr::new(ignored))
    }) {
        return true;
    }
    matches!(
        path.extension().and_then(|value| value.to_str()),
        Some("pyc" | "pyo" | "tsbuildinfo")
    ) || path.file_name().is_some_and(|value| value == ".DS_Store")
}

fn validate_relative_plugin_path(path: &Path) -> anyhow::Result<()> {
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path.components().any(|component| {
            !matches!(component, Component::Normal(_))
                || component.as_os_str().to_string_lossy().contains('\0')
        })
    {
        bail!(
            "plugin path must be a safe relative path: {}",
            path.display()
        );
    }
    Ok(())
}

fn relative_path_text(path: &Path) -> anyhow::Result<String> {
    validate_relative_plugin_path(path)?;
    path.to_str()
        .map(|value| value.replace('\\', "/"))
        .context("plugin paths must be UTF-8")
}

fn resolve_entrypoint_script(root: &Path, manifest: &PluginManifest) -> anyhow::Result<PathBuf> {
    let mut candidates = manifest.entrypoint.argv.iter();
    let first = candidates.next().context("plugin entrypoint is empty")?;
    let first_path = Path::new(first);
    let candidate = if first_path.components().count() > 1
        || first_path.extension().is_some()
        || first.starts_with('.')
    {
        first_path
    } else {
        candidates
            .find_map(|value| {
                let path = Path::new(value);
                (path.extension().is_some() && !value.starts_with('-')).then_some(path)
            })
            .context("plugin entrypoint must identify a script file")?
    };
    validate_relative_plugin_path(candidate)?;
    if ignored_plugin_path(candidate) {
        bail!("plugin entrypoint may not use an ignored generated path");
    }
    let script = root.join(candidate);
    if !script.is_file() {
        bail!(
            "plugin entrypoint script {} does not exist",
            candidate.display()
        );
    }
    let supported = matches!(
        script.extension().and_then(|value| value.to_str()),
        Some("py" | "sh" | "js" | "rb" | "pl" | "ps1")
    );
    if !supported {
        bail!("plugin entrypoint must be a recognized script file");
    }
    Ok(script)
}

fn file_sha256(path: &Path) -> anyhow::Result<String> {
    let mut digest = Sha256::new();
    let mut file = fs::File::open(path)?;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn package_sha256(root: &Path, files: &[PathBuf]) -> anyhow::Result<String> {
    let mut digest = Sha256::new();
    for file in files {
        let relative = relative_path_text(file.strip_prefix(root)?)?;
        digest.update(relative.as_bytes());
        digest.update([0]);
        digest.update(file_sha256(file)?.as_bytes());
        digest.update([0]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn plugin_data_root() -> anyhow::Result<PathBuf> {
    if let Some(path) = env::var_os("TREER_PLUGIN_HOME") {
        return Ok(PathBuf::from(path));
    }
    if let Some(path) = env::var_os("XDG_DATA_HOME") {
        return Ok(PathBuf::from(path).join("treer/plugins"));
    }
    let user_home = env::var_os("HOME").context("HOME is required to locate installed plugins")?;
    Ok(PathBuf::from(user_home).join(".local/share/treer/plugins"))
}

fn plugin_state_root() -> anyhow::Result<PathBuf> {
    if let Some(path) = env::var_os("TREER_PLUGIN_STATE_HOME") {
        return Ok(PathBuf::from(path));
    }
    if let Some(path) = env::var_os("XDG_STATE_HOME") {
        return Ok(PathBuf::from(path).join("treer/plugins"));
    }
    let user_home = env::var_os("HOME").context("HOME is required to locate plugin state")?;
    Ok(PathBuf::from(user_home).join(".local/state/treer/plugins"))
}

fn install_plugin_package(path: &Path) -> anyhow::Result<InstalledPluginSummary> {
    let root = plugin_data_root()?;
    install_plugin_package_into(path, &root)
}

fn install_plugin_package_into(path: &Path, root: &Path) -> anyhow::Result<InstalledPluginSummary> {
    let package = validate_plugin_package(path)?;
    let id_root = root.join(&package.manifest.id);
    let destination = id_root.join(&package.manifest.version);
    if destination.exists() {
        bail!(
            "plugin {} version {} is already installed",
            package.manifest.id,
            package.manifest.version
        );
    }
    fs::create_dir_all(&id_root)
        .with_context(|| format!("failed to create {}", id_root.display()))?;
    let staging = id_root.join(format!(
        ".install-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    let copy_result = (|| -> anyhow::Result<()> {
        fs::create_dir(&staging)?;
        copy_plugin_tree(&package.package_path, &staging)?;
        let staged = validate_plugin_package(&staging)?;
        if staged.package_sha256 != package.package_sha256 {
            bail!("installed plugin copy did not match validated package");
        }
        fs::rename(&staging, &destination)?;
        make_plugin_tree_read_only(&destination)?;
        Ok(())
    })();
    if copy_result.is_err() && staging.exists() {
        let _ = fs::remove_dir_all(&staging);
    }
    copy_result?;
    Ok(installed_summary(&ValidatedPluginPackage {
        package_path: destination,
        ..package
    }))
}

fn copy_plugin_tree(source: &Path, destination: &Path) -> anyhow::Result<()> {
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        let metadata = fs::symlink_metadata(&source_path)?;
        if metadata.file_type().is_symlink() {
            bail!("plugin package changed during installation");
        }
        if ignored_plugin_path(Path::new(&entry.file_name())) {
            continue;
        }
        if metadata.is_dir() {
            fs::create_dir(&destination_path)?;
            copy_plugin_tree(&source_path, &destination_path)?;
        } else if metadata.is_file() {
            fs::copy(&source_path, &destination_path)?;
        } else {
            bail!("plugin package changed during installation");
        }
    }
    Ok(())
}

fn make_plugin_tree_read_only(root: &Path) -> anyhow::Result<()> {
    let mut paths = collect_plugin_files(root)?;
    paths.sort_by_key(|path| std::cmp::Reverse(path.components().count()));
    for path in paths {
        let mut permissions = fs::metadata(&path)?.permissions();
        permissions.set_readonly(true);
        fs::set_permissions(path, permissions)?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut directories = vec![root.to_path_buf()];
        let mut index = 0;
        while index < directories.len() {
            for entry in fs::read_dir(&directories[index])? {
                let path = entry?.path();
                if path.is_dir() {
                    directories.push(path);
                }
            }
            index += 1;
        }
        directories.sort_by_key(|path| std::cmp::Reverse(path.components().count()));
        for directory in directories {
            fs::set_permissions(directory, fs::Permissions::from_mode(0o555))?;
        }
    }
    Ok(())
}

fn list_installed_plugins() -> anyhow::Result<Vec<InstalledPluginSummary>> {
    let root = plugin_data_root()?;
    if !root.exists() {
        return Ok(Vec::new());
    }
    let mut plugins = Vec::new();
    for id_entry in fs::read_dir(&root)? {
        let id_entry = id_entry?;
        if !id_entry.file_type()?.is_dir() {
            continue;
        }
        for version_entry in fs::read_dir(id_entry.path())? {
            let version_entry = version_entry?;
            if !version_entry.file_type()?.is_dir()
                || version_entry.file_name().to_string_lossy().starts_with('.')
            {
                continue;
            }
            let package = validate_plugin_package(&version_entry.path())?;
            plugins.push(installed_summary(&package));
        }
    }
    plugins.sort_by(|left, right| {
        left.id.cmp(&right.id).then_with(|| {
            parse_semver(&left.version)
                .unwrap_or_default()
                .cmp(&parse_semver(&right.version).unwrap_or_default())
        })
    });
    Ok(plugins)
}

fn load_installed_plugin(id: &str) -> anyhow::Result<ValidatedPluginPackage> {
    validate_plugin_id(id)?;
    let id_root = plugin_data_root()?.join(id);
    let mut versions = if id_root.is_dir() {
        fs::read_dir(&id_root)?
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
            .filter_map(|entry| {
                let version = entry.file_name().to_string_lossy().to_string();
                parse_semver(&version)
                    .ok()
                    .map(|parsed| (parsed, entry.path()))
            })
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    versions.sort_by_key(|(version, _)| *version);
    let path = versions
        .pop()
        .map(|(_, path)| path)
        .with_context(|| format!("plugin {id} is not installed"))?;
    let package = validate_plugin_package(&path)?;
    if package.manifest.id != id {
        bail!("installed plugin ID does not match its directory");
    }
    Ok(package)
}

fn installed_summary(package: &ValidatedPluginPackage) -> InstalledPluginSummary {
    InstalledPluginSummary {
        id: package.manifest.id.clone(),
        display_name: package.manifest.display_name.clone(),
        version: package.manifest.version.clone(),
        package_path: package.package_path.clone(),
        capabilities: package.manifest.capabilities.clone(),
    }
}

#[cfg(unix)]
async fn run_plugin_foreground(
    package: &ValidatedPluginPackage,
    config: &Path,
    workspace: &str,
    server_url: &Url,
) -> anyhow::Result<i32> {
    use std::os::unix::fs::PermissionsExt;

    let current_operating_system = std::env::consts::OS;
    if !package.manifest.entrypoint.operating_systems.is_empty()
        && !package
            .manifest
            .entrypoint
            .operating_systems
            .iter()
            .any(|value| value == current_operating_system)
    {
        bail!(
            "plugin {} does not support operating system {current_operating_system}",
            package.manifest.id
        );
    }

    let config = config
        .canonicalize()
        .with_context(|| format!("plugin config {} does not exist", config.display()))?;
    if !config.is_file() {
        bail!("plugin config must be a regular file");
    }
    serde_json::from_slice::<Value>(&fs::read(&config)?)
        .context("plugin config must contain valid JSON")?;
    let state_dir = plugin_state_root()?
        .join(workspace_key(workspace))
        .join(&package.manifest.id)
        .join(&package.manifest.version);
    fs::create_dir_all(&state_dir)?;
    fs::set_permissions(&state_dir, fs::Permissions::from_mode(0o700))?;
    let broker_dir = create_plugin_broker_directory()?;
    let socket_path = broker_dir.join("broker.sock");
    let listener =
        UnixListener::bind(&socket_path).context("failed to create plugin CLI broker")?;
    let token = format!(
        "pbr_{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    let executable = env::current_exe().context("failed to locate the running treer binary")?;
    let context = PluginBrokerContext {
        plugin_id: package.manifest.id.clone(),
        token: Arc::from(token.as_str()),
        capabilities: Arc::new(package.manifest.capabilities.iter().cloned().collect()),
        workspace: Arc::from(workspace),
        server_url: Arc::from(server_url.as_str()),
        executable: Arc::new(executable.clone()),
        concurrency: Arc::new(Semaphore::new(8)),
    };

    let argv = &package.manifest.entrypoint.argv;
    let first = &argv[0];
    let first_path = Path::new(first);
    let program = if first_path.components().count() > 1
        || first_path.extension().is_some()
        || first.starts_with('.')
    {
        package.package_path.join(first_path)
    } else {
        PathBuf::from(first)
    };
    let mut child = TokioCommand::new(program);
    child
        .args(&argv[1..])
        .current_dir(&package.package_path)
        .env_clear()
        .env(PLUGIN_BROKER_SOCKET_ENV, &socket_path)
        .env(PLUGIN_BROKER_TOKEN_ENV, &token)
        .env("TREER_PLUGIN_ID", &package.manifest.id)
        .env("TREER_PLUGIN_VERSION", &package.manifest.version)
        .env("TREER_PLUGIN_STATE_DIR", &state_dir)
        .env("TREER_PLUGIN_CONFIG", &config)
        .env("TREER_CLI", &executable)
        .env("PYTHONUNBUFFERED", "1")
        .kill_on_drop(true);
    for name in [
        "PATH",
        "LANG",
        "LC_ALL",
        "TZ",
        "TMPDIR",
        "SSL_CERT_FILE",
        "SSL_CERT_DIR",
    ] {
        if let Some(value) = env::var_os(name) {
            child.env(name, value);
        }
    }
    if let Some(parent) = executable.parent() {
        let mut paths = vec![parent.to_path_buf()];
        if let Some(existing) = env::var_os("PATH") {
            paths.extend(env::split_paths(&existing));
        }
        child.env("PATH", env::join_paths(paths)?);
    }
    for name in &package.manifest.configuration {
        if let Some(value) = env::var_os(name) {
            child.env(name, value);
        }
    }
    for name in &package.manifest.secrets {
        let value = env::var_os(name)
            .with_context(|| format!("required plugin secret {name} is not set"))?;
        child.env(name, value);
    }
    let mut child = child
        .spawn()
        .with_context(|| format!("failed to start plugin {} entrypoint", package.manifest.id))?;

    let status = loop {
        tokio::select! {
            status = child.wait() => break status.context("plugin process wait failed")?,
            accepted = listener.accept() => {
                let (stream, _) = accepted.context("plugin broker accept failed")?;
                let context = context.clone();
                tokio::spawn(async move {
                    if let Err(error) = handle_plugin_broker_connection(stream, context).await {
                        tracing::warn!("plugin broker request failed: {error}");
                    }
                });
            }
        }
    };
    drop(listener);
    let _ = fs::remove_file(&socket_path);
    let _ = fs::remove_dir(&broker_dir);
    Ok(status.code().unwrap_or(1))
}

#[cfg(unix)]
fn create_plugin_broker_directory() -> anyhow::Result<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::PermissionsExt;

    let name = format!(
        "treer-pbr-{}-{}",
        std::process::id(),
        &uuid::Uuid::new_v4().simple().to_string()[..12]
    );
    let configured = env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from);
    let mut roots = configured.into_iter().collect::<Vec<_>>();
    roots.push(std::env::temp_dir());
    roots.push(PathBuf::from("/tmp"));
    roots.dedup();
    for root in roots {
        let directory = root.join(&name);
        let socket = directory.join("broker.sock");
        if socket.as_os_str().as_bytes().len() >= 100 {
            continue;
        }
        match fs::create_dir(&directory) {
            Ok(()) => {
                fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))?;
                return Ok(directory);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                tracing::debug!(path = %directory.display(), %error, "plugin broker runtime path unavailable");
            }
        }
    }
    bail!("failed to create a short private plugin broker runtime directory")
}

#[cfg(not(unix))]
async fn run_plugin_foreground(
    _package: &ValidatedPluginPackage,
    _config: &Path,
    _workspace: &str,
    _server_url: &Url,
) -> anyhow::Result<i32> {
    bail!("the v1 private plugin broker currently requires Unix domain sockets")
}

#[cfg(unix)]
async fn handle_plugin_broker_connection(
    mut stream: UnixStream,
    context: PluginBrokerContext,
) -> anyhow::Result<()> {
    let permit = context
        .concurrency
        .clone()
        .try_acquire_owned()
        .map_err(|_| anyhow::anyhow!("plugin broker concurrency limit reached"))?;
    let mut request_bytes = Vec::new();
    (&mut stream)
        .take((PLUGIN_BROKER_MAX_REQUEST_BYTES + 1) as u64)
        .read_to_end(&mut request_bytes)
        .await?;
    if request_bytes.len() > PLUGIN_BROKER_MAX_REQUEST_BYTES {
        bail!("plugin broker request exceeds its size limit");
    }
    let request: PluginBrokerRequest =
        serde_json::from_slice(&request_bytes).context("invalid plugin broker request")?;
    if request.token.len() != context.token.len()
        || request
            .token
            .as_bytes()
            .ct_eq(context.token.as_bytes())
            .unwrap_u8()
            != 1
    {
        bail!("plugin broker session is invalid");
    }
    let required = match plugin_command_capabilities(&request.argv) {
        Ok(required) => required,
        Err(error) => {
            write_plugin_broker_denial(&mut stream, "plugin_command_denied", &error.to_string())
                .await?;
            drop(permit);
            return Ok(());
        }
    };
    if required
        .iter()
        .any(|capability| !context.capabilities.contains(*capability))
    {
        write_plugin_broker_denial(
            &mut stream,
            "plugin_command_denied",
            "plugin manifest does not grant this Treer command",
        )
        .await?;
        drop(permit);
        return Ok(());
    }
    if request.human_session.is_some() && !plugin_command_accepts_human_session(&request.argv)? {
        write_plugin_broker_denial(
            &mut stream,
            "plugin_session_command_denied",
            "this Treer command cannot use a delegated human session",
        )
        .await?;
        drop(permit);
        return Ok(());
    }
    tracing::debug!(
        plugin_id = %context.plugin_id,
        capabilities = %required.join(","),
        "plugin broker executing declared Treer command"
    );
    let response = execute_brokered_cli(&context, request).await?;
    stream.write_all(&serde_json::to_vec(&response)?).await?;
    stream.shutdown().await?;
    drop(permit);
    Ok(())
}

#[cfg(unix)]
async fn write_plugin_broker_denial(
    stream: &mut UnixStream,
    code: &str,
    message: &str,
) -> anyhow::Result<()> {
    let response = PluginBrokerResponse {
        exit_code: 1,
        stdout: String::new(),
        stderr: serde_json::to_string(&json!({
            "error": {"code": code, "message": message}
        }))?,
    };
    stream.write_all(&serde_json::to_vec(&response)?).await?;
    stream.shutdown().await?;
    Ok(())
}

#[cfg(unix)]
async fn execute_brokered_cli(
    context: &PluginBrokerContext,
    request: PluginBrokerRequest,
) -> anyhow::Result<PluginBrokerResponse> {
    let mut command = TokioCommand::new(context.executable.as_ref());
    command
        .arg("--url")
        .arg(context.server_url.as_ref())
        .arg("--workspace")
        .arg(context.workspace.as_ref())
        .args(&request.argv)
        .env_remove(PLUGIN_BROKER_SOCKET_ENV)
        .env_remove(PLUGIN_BROKER_TOKEN_ENV)
        .env_remove(PLUGIN_HUMAN_SESSION_ENV)
        .env_remove(INTERNAL_PLUGIN_ID_ENV)
        .env_remove(INTERNAL_PLUGIN_SESSION_ENV)
        .env(INTERNAL_PLUGIN_ID_ENV, &context.plugin_id)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    if let Some(human_session) = request.human_session.as_deref() {
        if human_session.len() > 512
            || !human_session.starts_with("phs_")
            || !human_session.contains('.')
            || human_session.chars().any(char::is_control)
        {
            bail!("plugin human session capability is invalid");
        }
        command.env(INTERNAL_PLUGIN_SESSION_ENV, human_session);
    }
    let mut child = command
        .spawn()
        .context("failed to execute brokered treer command")?;
    if let Some(mut stdin) = child.stdin.take() {
        if let Some(input) = request.stdin {
            stdin.write_all(input.as_bytes()).await?;
        }
        stdin.shutdown().await?;
    }
    let output = tokio::time::timeout(Duration::from_secs(120), child.wait_with_output())
        .await
        .map_err(|_| anyhow::anyhow!("brokered treer command exceeded its runtime limit"))??;
    if output.stdout.len() > PLUGIN_BROKER_MAX_OUTPUT_BYTES
        || output.stderr.len() > PLUGIN_BROKER_MAX_OUTPUT_BYTES
    {
        bail!("brokered treer command exceeded its output limit");
    }
    Ok(PluginBrokerResponse {
        exit_code: output.status.code().unwrap_or(1),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

fn plugin_command_capabilities(argv: &[String]) -> anyhow::Result<Vec<&'static str>> {
    if argv.iter().any(|argument| {
        matches!(
            argument.as_str(),
            "--url" | "--workspace" | "--skill" | "--skills"
        ) || argument.starts_with("--url=")
            || argument.starts_with("--workspace=")
    }) {
        bail!("plugin commands may not override broker connection or identity context");
    }
    let mut command = Vec::<OsString>::with_capacity(argv.len() + 1);
    command.push(OsString::from("treer"));
    command.extend(argv.iter().map(OsString::from));
    let parsed =
        Args::try_parse_from(command).context("plugin submitted an invalid Treer command")?;
    let required = match parsed.command.context("plugin command is required")? {
        Command::Message { command } => match command {
            MessageCommand::Send { .. } => vec!["message.send"],
            MessageCommand::Reply { .. } => vec!["message.read", "message.send"],
            MessageCommand::Get { .. } | MessageCommand::List { .. } => vec!["message.read"],
            MessageCommand::Receive { .. } => vec!["message.receive"],
            MessageCommand::Ack { .. } => vec!["message.ack"],
            MessageCommand::Import { .. } => {
                bail!("message import is unavailable through the plugin broker")
            }
        },
        Command::Agent { command } => match command {
            AgentCommand::List => vec!["agent.discover"],
            AgentCommand::Get { .. } => vec!["agent.metadata.read"],
            AgentCommand::Prompt { .. } => vec!["agent.prompt"],
            _ => bail!("this Agent command is unavailable through the plugin broker"),
        },
        Command::Human {
            command: HumanCommand::List,
        } => vec!["human.list"],
        Command::Whoami => vec!["identity.self.read"],
        Command::Discover | Command::List => vec!["agent.discover"],
        Command::Prompt { .. } => vec!["agent.prompt"],
        Command::Plugin {
            command: PluginCommand::Auth { .. },
        } => vec!["plugin.oauth"],
        Command::Plugin { .. }
        | Command::Identity { .. }
        | Command::Profile { .. }
        | Command::Machine { .. }
        | Command::VirtualHost { .. }
        | Command::Service { .. }
        | Command::Publish { .. }
        | Command::Create { .. }
        | Command::Read { .. }
        | Command::Rename { .. }
        | Command::Delete { .. }
        | Command::Attach { .. }
        | Command::Stop { .. } => {
            bail!("this Treer command is unavailable through the plugin broker")
        }
    };
    Ok(required)
}

fn plugin_command_accepts_human_session(argv: &[String]) -> anyhow::Result<bool> {
    let mut command = Vec::<OsString>::with_capacity(argv.len() + 1);
    command.push(OsString::from("treer"));
    command.extend(argv.iter().map(OsString::from));
    let parsed =
        Args::try_parse_from(command).context("plugin submitted an invalid Treer command")?;
    Ok(matches!(
        parsed.command.context("plugin command is required")?,
        Command::Message {
            command: MessageCommand::Send { .. }
                | MessageCommand::Reply { .. }
                | MessageCommand::Get { .. }
                | MessageCommand::List { .. }
                | MessageCommand::Receive { .. }
                | MessageCommand::Ack { .. },
        } | Command::Human {
            command: HumanCommand::List,
        } | Command::Agent {
            command: AgentCommand::List | AgentCommand::Get { .. },
        }
    ))
}

async fn run_plugin_broker_client() -> anyhow::Result<i32> {
    #[cfg(not(unix))]
    {
        bail!("plugin broker context is unsupported on this platform")
    }
    #[cfg(unix)]
    {
        let socket =
            env::var_os(PLUGIN_BROKER_SOCKET_ENV).context("plugin broker socket is missing")?;
        let token =
            env::var(PLUGIN_BROKER_TOKEN_ENV).context("plugin broker session token is missing")?;
        let argv = env::args_os()
            .skip(1)
            .map(|value| {
                value
                    .into_string()
                    .map_err(|_| anyhow::anyhow!("plugin CLI arguments must be UTF-8"))
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        let stdin = if broker_command_uses_stdin(&argv) {
            Some(read_stdin_text()?)
        } else {
            None
        };
        let human_session = env::var(PLUGIN_HUMAN_SESSION_ENV).ok();
        let request = PluginBrokerRequest {
            token,
            argv,
            stdin,
            human_session,
        };
        let encoded = serde_json::to_vec(&request)?;
        if encoded.len() > PLUGIN_BROKER_MAX_REQUEST_BYTES {
            bail!("plugin broker request exceeds its size limit");
        }
        let mut stream = UnixStream::connect(PathBuf::from(socket))
            .await
            .context("failed to connect to plugin CLI broker")?;
        stream.write_all(&encoded).await?;
        stream.shutdown().await?;
        let mut response = Vec::new();
        (&mut stream)
            .take((PLUGIN_BROKER_MAX_OUTPUT_BYTES * 2 + 1) as u64)
            .read_to_end(&mut response)
            .await?;
        if response.len() > PLUGIN_BROKER_MAX_OUTPUT_BYTES * 2 {
            bail!("plugin broker response exceeds its size limit");
        }
        let response: PluginBrokerResponse =
            serde_json::from_slice(&response).context("invalid plugin broker response")?;
        print!("{}", response.stdout);
        eprint!("{}", response.stderr);
        Ok(response.exit_code.clamp(0, 255))
    }
}

fn broker_command_uses_stdin(argv: &[String]) -> bool {
    argv.windows(2)
        .any(|pair| pair[0] == "--body-file" && pair[1] == "-")
        || argv.iter().any(|argument| argument == "--body-file=-")
}

async fn run_profile_command(client: &ApiClient, command: ProfileCommand) -> anyhow::Result<Value> {
    match command {
        ProfileCommand::List => client.value(Method::GET, "api/launch-profiles", None).await,
        ProfileCommand::Get { target } => {
            let target = normalize_target(&target)?;
            client
                .value(
                    Method::GET,
                    &format!("api/launch-profiles/{}", path_segment(&target)),
                    None,
                )
                .await
        }
        ProfileCommand::Create {
            name,
            description,
            cwd,
            executable,
            args,
        } => {
            let request = CreateAgentLaunchProfileRequest {
                name,
                description,
                cwd,
                command: executable,
                args,
            };
            client
                .value(
                    Method::POST,
                    "api/launch-profiles",
                    Some(serde_json::to_value(request)?),
                )
                .await
        }
        ProfileCommand::Update {
            target,
            name,
            description,
            cwd,
            executable,
            args,
            clear_args,
        } => {
            let args = if clear_args {
                Some(Vec::new())
            } else if args.is_empty() {
                None
            } else {
                Some(args)
            };
            if name.is_none()
                && description.is_none()
                && cwd.is_none()
                && executable.is_none()
                && args.is_none()
            {
                bail!("profile update requires at least one changed field");
            }
            let target = normalize_target(&target)?;
            client
                .value(
                    Method::PATCH,
                    &format!("api/launch-profiles/{}", path_segment(&target)),
                    Some(serde_json::to_value(UpdateAgentLaunchProfileRequest {
                        name,
                        description,
                        cwd,
                        command: executable,
                        args,
                    })?),
                )
                .await
        }
        ProfileCommand::Delete { target } => {
            let target = normalize_target(&target)?;
            client
                .value(
                    Method::DELETE,
                    &format!("api/launch-profiles/{}", path_segment(&target)),
                    None,
                )
                .await
        }
        ProfileCommand::Launch {
            target,
            machine,
            name,
        } => {
            let target = normalize_target(&target)?;
            client
                .value(
                    Method::POST,
                    &format!("api/launch-profiles/{}/launch", path_segment(&target)),
                    Some(serde_json::to_value(LaunchAgentProfileRequest {
                        server_id: machine,
                        agent_name: name,
                        cols: 120,
                        rows: 36,
                    })?),
                )
                .await
        }
    }
}

async fn run_machine_command(client: &ApiClient, command: MachineCommand) -> anyhow::Result<Value> {
    match command {
        MachineCommand::Rename { target, name } => rename_machine(client, &target, name).await,
        MachineCommand::Delete { target } => delete_machine(client, &target).await,
    }
}

async fn run_virtual_host_command(
    client: &ApiClient,
    command: VirtualHostCommand,
) -> anyhow::Result<Value> {
    match command {
        VirtualHostCommand::List => client.value(Method::GET, "api/virtual-hosts", None).await,
        VirtualHostCommand::Add { hostname, service } => {
            client
                .value(
                    Method::POST,
                    "api/virtual-hosts",
                    Some(serde_json::to_value(CreateVirtualNetworkHostRequest {
                        hostname,
                        service_id: service,
                    })?),
                )
                .await
        }
        VirtualHostCommand::Delete { hostname } => {
            client
                .value(
                    Method::DELETE,
                    &format!("api/virtual-hosts/{}", path_segment(&hostname)),
                    None,
                )
                .await
        }
    }
}

async fn run_service_command(client: &ApiClient, command: ServiceCommand) -> anyhow::Result<Value> {
    match command {
        ServiceCommand::List => client.value(Method::GET, "api/services", None).await,
        ServiceCommand::Register {
            name,
            machine,
            target_host,
            port,
            protocol,
        } => {
            let server_id = resolve_service_machine(client, machine.as_deref()).await?;
            client
                .value(
                    Method::POST,
                    "api/services",
                    Some(serde_json::to_value(CreateMachineServiceRequest {
                        name,
                        server_id,
                        target_host,
                        target_port: port,
                        protocol: protocol.into(),
                    })?),
                )
                .await
        }
        ServiceCommand::Update {
            target,
            name,
            machine,
            target_host,
            port,
            protocol,
        } => {
            let server_id = match machine.as_deref() {
                Some(machine) => Some(resolve_service_machine(client, Some(machine)).await?),
                None => None,
            };
            client
                .value(
                    Method::PATCH,
                    &format!("api/services/{}", path_segment(&target)),
                    Some(serde_json::to_value(UpdateMachineServiceRequest {
                        name,
                        server_id,
                        target_host,
                        target_port: port,
                        protocol: protocol.map(Into::into),
                    })?),
                )
                .await
        }
        ServiceCommand::Probe { target } => {
            client
                .value(
                    Method::POST,
                    &format!("api/services/{}/probe", path_segment(&target)),
                    Some(json!({})),
                )
                .await
        }
        ServiceCommand::Delete { target } => {
            client
                .value(
                    Method::DELETE,
                    &format!("api/services/{}", path_segment(&target)),
                    None,
                )
                .await
        }
    }
}

async fn run_publish_command(client: &ApiClient, command: PublishCommand) -> anyhow::Result<Value> {
    match command {
        PublishCommand::List => client.value(Method::GET, "api/publish", None).await,
        PublishCommand::Create {
            service,
            slug,
            access,
        } => {
            client
                .value(
                    Method::POST,
                    "api/publish",
                    Some(serde_json::to_value(CreateServiceIngressRequest {
                        service_id: service,
                        slug,
                        access: access.into(),
                    })?),
                )
                .await
        }
        PublishCommand::Access { target, access } => {
            update_published_endpoint(client, &target, Some(access.into()), None).await
        }
        PublishCommand::Enable { target } => {
            update_published_endpoint(client, &target, None, Some(true)).await
        }
        PublishCommand::Disable { target } => {
            update_published_endpoint(client, &target, None, Some(false)).await
        }
        PublishCommand::Delete { target } => {
            client
                .value(
                    Method::DELETE,
                    &format!("api/publish/{}", path_segment(&target)),
                    None,
                )
                .await
        }
    }
}

async fn update_published_endpoint(
    client: &ApiClient,
    target: &str,
    access: Option<ServiceIngressAccess>,
    enabled: Option<bool>,
) -> anyhow::Result<Value> {
    client
        .value(
            Method::PATCH,
            &format!("api/publish/{}", path_segment(target)),
            Some(serde_json::to_value(UpdateServiceIngressRequest {
                access,
                enabled,
            })?),
        )
        .await
}

async fn resolve_service_machine(
    client: &ApiClient,
    requested: Option<&str>,
) -> anyhow::Result<String> {
    match requested {
        Some(target) => Ok(resolve_machine(client, target).await?.server_id),
        None => std::env::var("TREER_SERVER_ID").context(
            "--machine is required outside a managed agent; managed agents default to their own machine",
        ),
    }
}

async fn prompt_and_maybe_wait(
    client: &ApiClient,
    target: &str,
    text: String,
    wait: PromptWaitArgs,
) -> anyhow::Result<Value> {
    let submitted = client.prompt(target, text).await?;
    if !wait.wait {
        return Ok(serde_json::to_value(submitted)?);
    }
    let until = default_wait_statuses(wait.until);
    let baseline = Some((submitted.status, submitted.output_revision));
    Ok(serde_json::to_value(
        client
            .wait_for(target, &until, wait.timeout, baseline)
            .await?,
    )?)
}

async fn read_agent(client: &ApiClient, target: &str, lines: usize) -> anyhow::Result<Value> {
    let target = normalize_target(target)?;
    client
        .value(
            Method::GET,
            &format!("api/agents/{}/output?lines={lines}", path_segment(&target)),
            None,
        )
        .await
}

async fn stop_agent(client: &ApiClient, target: &str) -> anyhow::Result<Value> {
    let target = normalize_target(target)?;
    client
        .value(
            Method::POST,
            &format!("api/agents/{}/stop", path_segment(&target)),
            Some(json!({})),
        )
        .await
}

async fn rename_agent(client: &ApiClient, target: &str, name: String) -> anyhow::Result<Value> {
    let target = normalize_target(target)?;
    client
        .value(
            Method::PATCH,
            &format!("api/agents/{}", path_segment(&target)),
            Some(serde_json::to_value(RenameRequest { name })?),
        )
        .await
}

async fn delete_agent(client: &ApiClient, target: &str) -> anyhow::Result<Value> {
    let target = normalize_target(target)?;
    client
        .value(
            Method::DELETE,
            &format!("api/agents/{}", path_segment(&target)),
            None,
        )
        .await
}

async fn attach_agent(client: &ApiClient, target: &str) -> anyhow::Result<Value> {
    let target = normalize_target(target)?;
    let url = client
        .base
        .join(&format!("api/agents/{}/terminal", path_segment(&target)))
        .context("failed to build terminal URL")?;
    let outcome = relay_terminal(client, url, &target, true).await?;
    Ok(json!({ "agent": target, "status": "detached", "reason": outcome.reason }))
}

async fn resolve_machine(client: &ApiClient, target: &str) -> anyhow::Result<ServerInfo> {
    let target = normalize_machine_target(target)?;
    let snapshot: WorkspaceSnapshot = client.request(Method::GET, "api/discovery", None).await?;
    if let Some(server) = snapshot
        .servers
        .iter()
        .find(|server| server.server_id == target)
    {
        return Ok(server.clone());
    }
    let mut matches = snapshot
        .servers
        .into_iter()
        .filter(|server| server.name == target);
    let Some(server) = matches.next() else {
        bail!("machine {target} was not found in this workspace");
    };
    if matches.next().is_some() {
        bail!("more than one machine is named {target}; use a server id");
    }
    Ok(server)
}

struct TerminalOutcome {
    reason: String,
}

async fn relay_terminal(
    client: &ApiClient,
    mut url: Url,
    target: &str,
    interactive_required: bool,
) -> anyhow::Result<TerminalOutcome> {
    let interactive = std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
    if interactive_required && !interactive {
        bail!("terminal attach requires an interactive terminal");
    }
    let (cols, rows) = if interactive {
        terminal_size().context("failed to read terminal size")?
    } else {
        (120, 36)
    };
    let websocket_scheme = match url.scheme() {
        "http" => "ws",
        "https" => "wss",
        scheme => bail!("unsupported agent server URL scheme {scheme}"),
    };
    url.set_scheme(websocket_scheme)
        .map_err(|_| anyhow::anyhow!("invalid agent server URL scheme"))?;
    url.query_pairs_mut()
        .append_pair("cols", &cols.max(1).to_string())
        .append_pair("rows", &rows.max(1).to_string());

    let mut request = url
        .as_str()
        .into_client_request()
        .context("failed to build terminal WebSocket request")?;
    if let Some(agent_id) = &client.source_agent_id {
        request.headers_mut().insert(
            AGENT_ID_HEADER,
            HeaderValue::from_str(agent_id).context("invalid managed Agent identity")?,
        );
    }
    if let Some(credential) = &client.workload_credential {
        request.headers_mut().insert(
            WORKLOAD_CREDENTIAL_HEADER,
            HeaderValue::from_str(credential).context("invalid Agent workload credential")?,
        );
    }
    if let Some(credential) = &client.operator_credential {
        request.headers_mut().insert(
            OPERATOR_CREDENTIAL_HEADER,
            HeaderValue::from_str(credential).context("invalid local operator credential")?,
        );
    }
    let (socket, _) = tokio_tungstenite::connect_async(request)
        .await
        .with_context(|| format!("failed to connect to {target}"))?;
    if interactive {
        eprintln!("[treer] connected to {target}; press Ctrl-] to disconnect");
    }
    let raw_mode = interactive.then(RawModeGuard::enable).transpose()?;
    let (mut outgoing, mut incoming) = socket.split();
    let mut stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();
    let mut input = [0_u8; 4096];
    let mut stdin_open = true;
    #[cfg(unix)]
    let mut resize_events =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::window_change())
            .context("failed to listen for terminal resize")?;
    #[cfg(not(unix))]
    let mut resize_events = tokio::time::interval(Duration::from_millis(250));

    let (reason, exit_code, terminal_error, detached) = loop {
        tokio::select! {
            read = stdin.read(&mut input), if stdin_open => {
                let read = read.context("failed to read terminal input")?;
                if read == 0 {
                    if interactive {
                        break ("terminal input closed".to_string(), None, None, false);
                    }
                    stdin_open = false;
                    continue;
                }
                if interactive {
                    if let Some(detach_at) = input[..read].iter().position(|byte| *byte == ATTACH_DETACH_BYTE) {
                        if detach_at > 0 {
                            outgoing
                                .send(Message::Binary(input[..detach_at].to_vec().into()))
                                .await
                                .context("failed to send terminal input")?;
                        }
                        break ("detached".to_string(), None, None, true);
                    }
                }
                outgoing
                    .send(Message::Binary(input[..read].to_vec().into()))
                    .await
                    .context("failed to send terminal input")?;
            }
            message = incoming.next() => {
                let Some(message) = message else {
                    break ("terminal connection closed".to_string(), None, None, false);
                };
                match message.context("failed to read terminal output")? {
                    Message::Binary(data) => {
                        stdout.write_all(&data).await.context("failed to write terminal output")?;
                        stdout.flush().await.context("failed to flush terminal output")?;
                    }
                    Message::Text(text) => match serde_json::from_str::<TerminalServerMessage>(&text)
                        .context("invalid terminal server message")? {
                        TerminalServerMessage::Ready { .. } => {}
                        TerminalServerMessage::Closed { reason: closed_reason, exit_code: closed_exit_code } => {
                            break (
                                closed_reason.unwrap_or_else(|| "remote terminal closed".to_string()),
                                closed_exit_code,
                                None,
                                false,
                            );
                        }
                        TerminalServerMessage::Error { error } => {
                            break (
                                format!("{}: {}", error.code, error.message),
                                None,
                                Some(error),
                                false,
                            );
                        }
                    },
                    Message::Close(frame) => {
                        let reason = frame
                            .map(|frame| frame.reason.to_string())
                            .filter(|reason| !reason.is_empty())
                            .unwrap_or_else(|| "terminal connection closed".to_string());
                        break (reason, None, None, false);
                    }
                    Message::Ping(data) => {
                        outgoing.send(Message::Pong(data)).await.context("failed to reply to terminal ping")?;
                    }
                    Message::Pong(_) | Message::Frame(_) => {}
                }
            }
            _ = wait_for_resize(&mut resize_events), if interactive => {
                let (cols, rows) = terminal_size().context("failed to read terminal size")?;
                let resize = TerminalClientMessage::Resize {
                    cols: cols.max(1),
                    rows: rows.max(1),
                };
                outgoing
                    .send(Message::Text(serde_json::to_string(&resize)?.into()))
                    .await
                    .context("failed to resize remote terminal")?;
            }
        }
    };
    let _ = outgoing.send(Message::Close(None)).await;
    drop(raw_mode);
    if interactive {
        eprintln!("\r\n[treer] {reason}");
    }
    if let Some(error) = terminal_error {
        bail!("{}: {}", error.code, error.message);
    }
    if !interactive && exit_code.is_none() && !detached {
        bail!("remote command ended without an exit status: {reason}");
    }
    Ok(TerminalOutcome { reason })
}

struct RawModeGuard;

impl RawModeGuard {
    fn enable() -> anyhow::Result<Self> {
        enable_raw_mode().context("failed to enable terminal raw mode")?;
        Ok(Self)
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
    }
}

#[cfg(unix)]
async fn wait_for_resize(events: &mut tokio::signal::unix::Signal) {
    let _ = events.recv().await;
}

#[cfg(not(unix))]
async fn wait_for_resize(events: &mut tokio::time::Interval) {
    events.tick().await;
}

async fn rename_machine(client: &ApiClient, target: &str, name: String) -> anyhow::Result<Value> {
    let target = normalize_machine_target(target)?;
    client
        .value(
            Method::PATCH,
            &format!("api/machines/{}", path_segment(&target)),
            Some(serde_json::to_value(RenameRequest { name })?),
        )
        .await
}

async fn delete_machine(client: &ApiClient, target: &str) -> anyhow::Result<Value> {
    let target = normalize_machine_target(target)?;
    client
        .value(
            Method::DELETE,
            &format!("api/machines/{}", path_segment(&target)),
            None,
        )
        .await
}

fn normalize_machine_target(target: &str) -> anyhow::Result<String> {
    if matches!(target, "self" | ".") {
        std::env::var("TREER_SERVER_ID")
            .context("self target requires TREER_SERVER_ID inside a managed agent")
    } else {
        Ok(target.to_string())
    }
}

fn normalize_target(target: &str) -> anyhow::Result<String> {
    if matches!(target, "self" | ".") {
        return std::env::var("TREER_AGENT_ID")
            .context("self target requires TREER_AGENT_ID inside a managed agent");
    }
    Ok(target.to_string())
}

fn path_segment(value: &str) -> String {
    utf8_percent_encode(value, NON_ALPHANUMERIC).to_string()
}

fn resolve_server_url(configured: Option<Url>, workspace: &str) -> anyhow::Result<Url> {
    if let Some(configured) = configured {
        return Ok(configured);
    }

    if let Some(config_path) = local_controller_config(workspace) {
        if config_path.is_file() {
            let bytes = std::fs::read(&config_path)
                .with_context(|| format!("failed to read {}", config_path.display()))?;
            let config: LocalControllerConfig = serde_json::from_slice(&bytes)
                .with_context(|| format!("invalid controller config {}", config_path.display()))?;
            return Url::parse(&format!("http://{}", config.listen))
                .context("invalid local agent server address");
        }
    }

    Url::parse("http://127.0.0.1:8790").context("invalid default agent server URL")
}

#[derive(serde::Deserialize)]
struct LocalControllerConfig {
    listen: String,
    #[serde(default)]
    operator_credential: String,
}

fn local_controller_config(workspace: &str) -> Option<PathBuf> {
    let config_home = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;
    Some(
        config_home
            .join("treer/agent-servers")
            .join(format!("{}-controller.json", workspace_key(workspace))),
    )
}

fn workspace_key(workspace: &str) -> String {
    workspace
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-') {
                character
            } else {
                '_'
            }
        })
        .collect()
}

fn default_wait_statuses(statuses: Vec<WaitStatus>) -> Vec<WaitStatus> {
    if statuses.is_empty() {
        vec![
            WaitStatus::Idle,
            WaitStatus::Blocked,
            WaitStatus::Exited,
            WaitStatus::Failed,
        ]
    } else {
        statuses
    }
}

fn status_name(status: AgentStatus) -> &'static str {
    match status {
        AgentStatus::Starting => "starting",
        AgentStatus::Working => "working",
        AgentStatus::Idle => "idle",
        AgentStatus::Blocked => "blocked",
        AgentStatus::Exited => "exited",
        AgentStatus::Failed => "failed",
        AgentStatus::Unknown => "unknown",
    }
}

fn encode_keys(keys: &[String]) -> anyhow::Result<Vec<u8>> {
    let mut encoded = Vec::new();
    for key in keys {
        let lower = key.to_ascii_lowercase();
        match lower.as_str() {
            "enter" | "return" => encoded.push(b'\r'),
            "tab" => encoded.push(b'\t'),
            "backspace" => encoded.push(0x7f),
            "esc" | "escape" => encoded.push(0x1b),
            "space" => encoded.push(b' '),
            "up" => encoded.extend_from_slice(b"\x1b[A"),
            "down" => encoded.extend_from_slice(b"\x1b[B"),
            "right" => encoded.extend_from_slice(b"\x1b[C"),
            "left" => encoded.extend_from_slice(b"\x1b[D"),
            "home" => encoded.extend_from_slice(b"\x1b[H"),
            "end" => encoded.extend_from_slice(b"\x1b[F"),
            "pageup" | "page-up" => encoded.extend_from_slice(b"\x1b[5~"),
            "pagedown" | "page-down" => encoded.extend_from_slice(b"\x1b[6~"),
            "delete" => encoded.extend_from_slice(b"\x1b[3~"),
            "shift-tab" => encoded.extend_from_slice(b"\x1b[Z"),
            _ if (lower.starts_with("ctrl-") || lower.starts_with("ctrl+")) && lower.len() == 6 => {
                let letter = lower.as_bytes()[5];
                if !letter.is_ascii_lowercase() {
                    bail!("invalid control key {key}");
                }
                encoded.push(letter & 0x1f);
            }
            _ if key.chars().count() == 1 => encoded.extend_from_slice(key.as_bytes()),
            _ => bail!("unknown key {key}"),
        }
    }
    Ok(encoded)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::OnceLock;
    use tokio::sync::Mutex;

    struct EnvironmentGuard {
        values: Vec<(&'static str, Option<OsString>)>,
    }

    impl EnvironmentGuard {
        fn set(values: &[(&'static str, &str)]) -> Self {
            let previous = values
                .iter()
                .map(|(name, _)| (*name, env::var_os(name)))
                .collect();
            for (name, value) in values {
                env::set_var(name, value);
            }
            Self { values: previous }
        }
    }

    impl Drop for EnvironmentGuard {
        fn drop(&mut self) {
            for (name, value) in self.values.drain(..) {
                if let Some(value) = value {
                    env::set_var(name, value);
                } else {
                    env::remove_var(name);
                }
            }
        }
    }

    fn test_plugin_manifest() -> PluginManifest {
        PluginManifest {
            schema_version: treer_protocol::PLUGIN_MANIFEST_SCHEMA_VERSION,
            id: "telegram".to_string(),
            display_name: "Telegram".to_string(),
            version: "0.1.0".to_string(),
            minimum_treer_version: "0.1.0".to_string(),
            entrypoint: treer_protocol::PluginEntrypoint {
                argv: vec!["python3".to_string(), "telegram.py".to_string()],
                operating_systems: vec!["linux".to_string(), "macos".to_string()],
            },
            capabilities: vec!["message.send".to_string()],
            configuration: vec!["TREER_TELEGRAM_CONFIG".to_string()],
            secrets: vec!["TELEGRAM_BOT_TOKEN".to_string()],
            http_service: None,
            state_version: 1,
            checksums: Default::default(),
        }
    }

    #[test]
    fn key_names_encode_to_terminal_sequences() {
        assert_eq!(
            encode_keys(&["ctrl-c".into(), "enter".into(), "up".into()]).expect("encode"),
            b"\x03\r\x1b[A"
        );
    }

    #[test]
    fn path_targets_are_percent_encoded() {
        assert_eq!(path_segment("review agent"), "review%20agent");
    }

    #[test]
    fn discovery_marks_the_current_agent_and_machine() {
        let snapshot: WorkspaceSnapshot = serde_json::from_value(json!({
            "revision": 1,
            "workspace": {
                "workspace_id": "workspace-a",
                "name": "Workspace A",
                "created_at": "2026-08-17T00:00:00Z"
            },
            "servers": [{
                "server_id": "server-a",
                "workspace_id": "workspace-a",
                "name": "builder",
                "hostname": "build-host",
                "root": "/workspace",
                "controller_build": {
                    "version": "0.1.2",
                    "git_commit": "controller-test"
                },
                "host_build": {
                    "version": "0.1.2",
                    "git_commit": "host-test"
                },
                "labels": {},
                "status": "online",
                "connected_at": "2026-08-17T00:00:00Z",
                "last_seen_at": "2026-08-17T00:00:00Z"
            }],
            "agents": [{
                "agent_id": "agent-a",
                "workspace_id": "workspace-a",
                "server_id": "server-a",
                "kind": "codex",
                "name": "reviewer",
                "cwd": ".",
                "status": "idle",
                "started_at": "2026-08-17T00:00:00Z",
                "updated_at": "2026-08-17T00:00:00Z",
                "output_revision": 0
            }]
        }))
        .expect("valid workspace snapshot");

        let value = discovery_value(snapshot.clone(), Some("agent-a"), Some("server-a"))
            .expect("resolve current identity");
        assert_eq!(value["self"]["agent"]["name"], "reviewer");
        assert_eq!(value["self"]["machine"]["name"], "builder");
        assert_eq!(
            discovery_value(snapshot.clone(), None, None).expect("unmanaged discovery")["self"],
            Value::Null
        );
        assert!(resolve_self(&snapshot, "agent-a", "server-other").is_err());
    }

    #[test]
    fn workspace_names_map_to_service_config_names() {
        assert_eq!(workspace_key("team one/alpha"), "team_one_alpha");
    }

    #[test]
    fn skill_flags_work_without_a_subcommand() {
        for flag in ["--skill", "--skills"] {
            let args = Args::try_parse_from(["treer", flag]).expect("skill flag should parse");
            assert!(args.skill);
            assert!(args.command.is_none());
        }
        assert!(SKILL.starts_with("---\nname: treer\n"));
        assert!(!SKILL.contains("TODO"));
    }

    #[test]
    fn attach_commands_parse() {
        let top_level = Args::try_parse_from(["treer", "attach", "reviewer"])
            .expect("top-level attach should parse");
        assert!(matches!(
            top_level.command,
            Some(Command::Attach { target }) if target == "reviewer"
        ));

        let nested = Args::try_parse_from(["treer", "agent", "attach", "reviewer"])
            .expect("nested attach should parse");
        assert!(matches!(
            nested.command,
            Some(Command::Agent {
                command: AgentCommand::Attach { target }
            }) if target == "reviewer"
        ));
    }

    #[test]
    fn machine_delete_command_parses() {
        let args = Args::try_parse_from(["treer", "machine", "delete", "srv_test"])
            .expect("machine delete should parse");
        assert!(matches!(
            args.command,
            Some(Command::Machine {
                command: MachineCommand::Delete { target }
            }) if target == "srv_test"
        ));
    }

    #[test]
    fn launch_profile_commands_parse_structured_arguments() {
        let create = Args::try_parse_from([
            "treer",
            "profile",
            "create",
            "reviewer",
            "--description",
            "Review changes",
            "--cwd",
            "/workspace",
            "codex",
            "--",
            "review",
            "--base",
            "main",
        ])
        .expect("profile create should parse");
        assert!(matches!(
            create.command,
            Some(Command::Profile {
                command: ProfileCommand::Create {
                    name,
                    executable,
                    args,
                    ..
                }
            }) if name == "reviewer" && executable == "codex" && args == ["review", "--base", "main"]
        ));

        let update = Args::try_parse_from([
            "treer",
            "profile",
            "update",
            "reviewer",
            "--arg",
            "--quiet",
            "--arg=check",
        ])
        .expect("profile update should parse flags as argument values");
        assert!(matches!(
            update.command,
            Some(Command::Profile {
                command: ProfileCommand::Update { args, .. }
            }) if args == ["--quiet", "check"]
        ));

        let launch = Args::try_parse_from([
            "treer",
            "profile",
            "launch",
            "reviewer",
            "--machine",
            "builder",
            "--name",
            "review-42",
        ])
        .expect("profile launch should parse");
        assert!(matches!(
            launch.command,
            Some(Command::Profile {
                command: ProfileCommand::Launch { target, machine, name }
            }) if target == "reviewer" && machine.as_deref() == Some("builder") && name.as_deref() == Some("review-42")
        ));
    }

    #[test]
    fn virtual_host_commands_parse() {
        let add = Args::try_parse_from([
            "treer",
            "virtual-host",
            "add",
            "api.internal",
            "api-service",
        ])
        .expect("virtual host add should parse");
        assert!(matches!(
            add.command,
            Some(Command::VirtualHost {
                command: VirtualHostCommand::Add {
                    hostname,
                    service,
                }
            }) if hostname == "api.internal"
                && service == "api-service"
        ));

        let delete = Args::try_parse_from(["treer", "vhost", "delete", "api.internal"])
            .expect("virtual host alias should parse");
        assert!(matches!(
            delete.command,
            Some(Command::VirtualHost {
                command: VirtualHostCommand::Delete { hostname }
            }) if hostname == "api.internal"
        ));
    }

    #[test]
    fn publish_commands_parse() {
        let args = Args::try_parse_from([
            "treer",
            "publish",
            "create",
            "api",
            "--slug",
            "issue-tracker",
            "--access",
            "workspace",
        ])
        .expect("publish create should parse");
        assert!(matches!(
            args.command,
            Some(Command::Publish {
                command: PublishCommand::Create {
                    service,
                    slug: Some(slug),
                    access: CliIngressAccess::Workspace,
                }
            }) if service == "api" && slug == "issue-tracker"
        ));

        let args = Args::try_parse_from(["treer", "publish", "disable", "demo.apps.test"])
            .expect("publish disable should parse");
        assert!(matches!(
            args.command,
            Some(Command::Publish {
                command: PublishCommand::Disable { target }
            }) if target == "demo.apps.test"
        ));
    }

    #[test]
    fn service_commands_parse() {
        let register = Args::try_parse_from([
            "treer",
            "service",
            "register",
            "api",
            "--machine",
            "builder",
            "--port",
            "8080",
            "--protocol",
            "http",
        ])
        .expect("service register should parse");
        assert!(matches!(
            register.command,
            Some(Command::Service {
                command: ServiceCommand::Register {
                    name,
                    machine: Some(machine),
                    port: 8080,
                    protocol: CliServiceProtocol::Http,
                    ..
                }
            }) if name == "api" && machine == "builder"
        ));
    }

    #[test]
    fn identity_token_command_parses() {
        let args = Args::try_parse_from(["treer", "identity", "token", "api"])
            .expect("identity token should parse");
        assert!(matches!(
            args.command,
            Some(Command::Identity {
                command: IdentityCommand::Token {
                    audience,
                    json: false,
                }
            }) if audience == "api"
        ));
    }

    #[test]
    fn human_directory_command_parses() {
        let humans =
            Args::try_parse_from(["treer", "human", "list"]).expect("human list should parse");
        assert!(matches!(
            humans.command,
            Some(Command::Human {
                command: HumanCommand::List
            })
        ));
    }

    #[test]
    fn message_commands_parse_machine_readable_inputs() {
        let send = Args::try_parse_from([
            "treer",
            "message",
            "send",
            "--to",
            "agent-a",
            "--to",
            "user-b",
            "--context",
            "msg-parent",
            "--idempotency-key",
            "telegram:update:42",
            "--body-file",
            "-",
        ])
        .expect("message send should parse");
        assert!(matches!(
            send.command,
            Some(Command::Message {
                command: MessageCommand::Send {
                    recipients,
                    context_ids,
                    idempotency_key: Some(key),
                    body_file: Some(path),
                    ..
                }
            }) if recipients == ["agent-a", "user-b"]
                && context_ids == ["msg-parent"]
                && key == "telegram:update:42"
                && path == Path::new("-")
        ));

        let ack = Args::try_parse_from([
            "treer",
            "message",
            "ack",
            "dlv-a",
            "dlv-b",
            "--operation-id",
            "ack-42",
        ])
        .expect("message ack should parse");
        assert!(matches!(
            ack.command,
            Some(Command::Message {
                command: MessageCommand::Ack {
                    delivery_ids,
                    operation_id: Some(operation_id),
                }
            }) if delivery_ids == ["dlv-a", "dlv-b"] && operation_id == "ack-42"
        ));
    }

    #[test]
    fn plugin_broker_maps_commands_to_declared_capabilities() {
        assert_eq!(
            plugin_command_capabilities(&[
                "message".into(),
                "reply".into(),
                "msg-a".into(),
                "--body".into(),
                "hello".into(),
            ])
            .expect("reply capability"),
            ["message.read", "message.send"]
        );
        assert_eq!(
            plugin_command_capabilities(&[
                "plugin".into(),
                "auth".into(),
                "start".into(),
                "--service".into(),
                "svc-mail".into(),
                "--redirect-uri".into(),
                "https://mail.example/api/auth/callback".into(),
            ])
            .expect("plugin OAuth capability"),
            ["plugin.oauth"]
        );
        assert!(plugin_command_capabilities(&[
            "message".into(),
            "import".into(),
            "--body-file".into(),
            "legacy.json".into(),
        ])
        .is_err());
        assert!(plugin_command_capabilities(&[
            "--url".into(),
            "http://127.0.0.1:1".into(),
            "message".into(),
            "list".into(),
        ])
        .is_err());
    }

    #[test]
    fn delegated_human_sessions_are_limited_to_human_safe_commands() {
        assert!(
            plugin_command_accepts_human_session(&["message".into(), "receive".into(),])
                .expect("parse receive")
        );
        assert!(plugin_command_accepts_human_session(&[
            "agent".into(),
            "get".into(),
            "reviewer".into(),
        ])
        .expect("parse metadata read"));
        assert!(!plugin_command_accepts_human_session(&[
            "agent".into(),
            "prompt".into(),
            "reviewer".into(),
            "wake".into(),
        ])
        .expect("parse prompt"));
        assert!(!plugin_command_accepts_human_session(&[
            "plugin".into(),
            "auth".into(),
            "revoke-all".into(),
        ])
        .expect("parse plugin auth"));
    }

    #[test]
    fn plugin_manifests_cannot_reinject_runtime_credentials_or_paths() {
        let root = std::env::temp_dir().join(format!(
            "treer-plugin-manifest-test-{}",
            uuid::Uuid::new_v4().simple()
        ));
        fs::create_dir(&root).expect("create manifest test directory");
        let manifest = test_plugin_manifest();
        validate_plugin_manifest(&manifest, &root).expect("valid namespaced manifest");

        for forbidden in ["PATH", "TREER_CLI", "TREER_MACHINE_TOKEN"] {
            let mut invalid = manifest.clone();
            invalid.configuration = vec![forbidden.to_string()];
            assert!(validate_plugin_manifest(&invalid, &root).is_err());
        }
        let mut foreign_namespace = manifest.clone();
        foreign_namespace.configuration = vec!["TREER_MAIL_CONFIG".to_string()];
        assert!(validate_plugin_manifest(&foreign_namespace, &root).is_err());
        let mut invalid_os = manifest;
        invalid_os.entrypoint.operating_systems = vec!["freebsd".to_string()];
        assert!(validate_plugin_manifest(&invalid_os, &root).is_err());
        fs::remove_dir(&root).expect("remove manifest test directory");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn plugin_broker_returns_structured_denials_before_execution() {
        let root = std::env::temp_dir().join(format!(
            "treer-plugin-broker-test-{}",
            uuid::Uuid::new_v4().simple()
        ));
        fs::create_dir(&root).expect("create broker test directory");
        let socket_path = root.join("broker.sock");
        let listener = UnixListener::bind(&socket_path).expect("bind broker test socket");
        let context = PluginBrokerContext {
            plugin_id: "telegram".to_string(),
            token: Arc::from("test-token"),
            capabilities: Arc::new(HashSet::new()),
            workspace: Arc::from("workspace-a"),
            server_url: Arc::from("http://127.0.0.1:1/"),
            executable: Arc::new(std::env::current_exe().expect("test executable")),
            concurrency: Arc::new(Semaphore::new(1)),
        };
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept broker request");
            handle_plugin_broker_connection(stream, context)
                .await
                .expect("handle broker denial");
        });
        let mut client = UnixStream::connect(&socket_path)
            .await
            .expect("connect to broker");
        client
            .write_all(
                &serde_json::to_vec(&PluginBrokerRequest {
                    token: "test-token".to_string(),
                    argv: vec!["message".to_string(), "list".to_string()],
                    stdin: None,
                    human_session: None,
                })
                .expect("encode broker request"),
            )
            .await
            .expect("write broker request");
        client.shutdown().await.expect("finish broker request");
        let mut encoded = Vec::new();
        client
            .read_to_end(&mut encoded)
            .await
            .expect("read broker response");
        server.await.expect("broker task");
        let response: PluginBrokerResponse =
            serde_json::from_slice(&encoded).expect("decode broker response");
        assert_eq!(response.exit_code, 1);
        assert!(response.stderr.contains("plugin_command_denied"));
        fs::remove_file(&socket_path).ok();
        fs::remove_dir(&root).expect("remove broker test directory");
    }

    #[test]
    fn plugin_install_is_data_only_and_immutable() {
        let root = std::env::temp_dir().join(format!(
            "treer-plugin-install-test-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let package = root.join("source");
        let installed = root.join("installed");
        fs::create_dir_all(&package).expect("create plugin source");
        let marker = root.join("install-hook-ran");
        let mut manifest = test_plugin_manifest();
        manifest.id = "fixture".to_string();
        manifest.display_name = "Fixture".to_string();
        manifest.entrypoint.argv = vec!["sh".to_string(), "fixture.sh".to_string()];
        manifest.entrypoint.operating_systems = vec![std::env::consts::OS.to_string()];
        manifest.configuration.clear();
        manifest.secrets.clear();
        fs::write(
            package.join("plugin.json"),
            serde_json::to_vec_pretty(&manifest).expect("encode manifest"),
        )
        .expect("write manifest");
        fs::write(
            package.join("fixture.sh"),
            format!("#!/bin/sh\ntouch '{}'\n", marker.display()),
        )
        .expect("write entrypoint");
        fs::create_dir_all(package.join("node_modules/dependency"))
            .expect("create generated dependency tree");
        fs::write(
            package.join("node_modules/dependency/index.js"),
            "must not be installed",
        )
        .expect("write generated dependency");
        fs::create_dir_all(package.join("__pycache__")).expect("create bytecode directory");
        fs::write(package.join("__pycache__/fixture.pyc"), "bytecode")
            .expect("write generated bytecode");

        let result = install_plugin_package_into(&package, &installed).expect("install plugin");
        assert_eq!(result.id, "fixture");
        assert!(
            !marker.exists(),
            "installation must not execute the entrypoint"
        );
        let installed_script = installed.join("fixture/0.1.0/fixture.sh");
        assert!(installed_script.is_file());
        assert!(!installed.join("fixture/0.1.0/node_modules").exists());
        assert!(!installed.join("fixture/0.1.0/__pycache__").exists());
        assert!(fs::metadata(&installed_script)
            .expect("installed script metadata")
            .permissions()
            .readonly());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for directory in [
                installed.join("fixture/0.1.0"),
                installed.join("fixture"),
                installed.clone(),
            ] {
                if directory.exists() {
                    fs::set_permissions(&directory, fs::Permissions::from_mode(0o755))
                        .expect("make test directory removable");
                }
            }
        }
        fs::remove_dir_all(&root).expect("remove install test directory");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn plugin_process_environment_withholds_treer_credentials() {
        static ENVIRONMENT_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        let _lock = ENVIRONMENT_LOCK.get_or_init(|| Mutex::new(())).lock().await;
        let root = std::env::temp_dir().join(format!(
            "treer-plugin-environment-test-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let package = root.join("package");
        let state = root.join("state");
        let config = root.join("config.json");
        fs::create_dir_all(&package).expect("create environment test package");
        fs::write(&config, b"{}").expect("write plugin config");
        let mut manifest = test_plugin_manifest();
        manifest.id = "fixture".to_string();
        manifest.display_name = "Fixture".to_string();
        manifest.entrypoint.argv = vec!["python3".to_string(), "fixture.py".to_string()];
        manifest.entrypoint.operating_systems = vec![std::env::consts::OS.to_string()];
        manifest.capabilities.clear();
        manifest.configuration.clear();
        manifest.secrets.clear();
        fs::write(
            package.join("plugin.json"),
            serde_json::to_vec_pretty(&manifest).expect("encode environment test manifest"),
        )
        .expect("write environment test manifest");
        fs::write(
            package.join("fixture.py"),
            r#"import json
import os
from pathlib import Path

Path("observed.json").write_text(json.dumps(dict(os.environ), sort_keys=True), encoding="utf-8")
"#,
        )
        .expect("write environment test entrypoint");
        let package = validate_plugin_package(&package).expect("validate environment test plugin");
        let _environment = EnvironmentGuard::set(&[
            (
                "TREER_PLUGIN_STATE_HOME",
                state.to_str().expect("state path"),
            ),
            ("TREER_WORKLOAD_CREDENTIAL", "workload-secret-must-not-leak"),
            ("TREER_OPERATOR_CREDENTIAL", "operator-secret-must-not-leak"),
            ("TREER_MACHINE_TOKEN", "machine-secret-must-not-leak"),
            ("UNRELATED_SECRET", "unrelated-secret-must-not-leak"),
        ]);
        let status = run_plugin_foreground(
            &package,
            &config,
            "workspace-a",
            &Url::parse("http://127.0.0.1:1/").expect("server URL"),
        )
        .await
        .expect("run environment test plugin");
        assert_eq!(status, 0);
        let observed: Value = serde_json::from_slice(
            &fs::read(package.package_path.join("observed.json"))
                .expect("read observed plugin environment"),
        )
        .expect("decode observed plugin environment");
        for forbidden in [
            "TREER_WORKLOAD_CREDENTIAL",
            "TREER_OPERATOR_CREDENTIAL",
            "TREER_MACHINE_TOKEN",
            "UNRELATED_SECRET",
            INTERNAL_PLUGIN_ID_ENV,
            INTERNAL_PLUGIN_SESSION_ENV,
        ] {
            assert!(observed.get(forbidden).is_none(), "{forbidden} leaked");
        }
        for required in [
            PLUGIN_BROKER_SOCKET_ENV,
            PLUGIN_BROKER_TOKEN_ENV,
            "TREER_PLUGIN_ID",
            "TREER_PLUGIN_VERSION",
            "TREER_PLUGIN_STATE_DIR",
            "TREER_PLUGIN_CONFIG",
            "TREER_CLI",
        ] {
            assert!(observed.get(required).is_some(), "{required} is missing");
        }
        fs::remove_dir_all(&root).expect("remove environment test directory");
    }
}
