use std::io::IsTerminal;
use std::io::Read as StdRead;
#[cfg(test)]
use std::path::Path;
use std::path::PathBuf;
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
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::Message;
use treer_protocol::{
    AcknowledgeMessagesRequest, AgentInfo, AgentStatus, ApiError, CreateAgentLaunchProfileRequest,
    CreateAgentRequest, CreateMachineServiceRequest, CreateServiceIngressRequest,
    CreateVirtualNetworkHostRequest, GetMessageResponse, ImportMessagesRequest, InputAgentRequest,
    LaunchAgentProfileRequest, LegacyMailMessage, MachineServiceProtocol, MessageExternalSource,
    ReceiveMessagesRequest, RenameRequest, SendMessageRequest, ServerInfo, ServiceIngressAccess,
    SetAgentUiRequest, TerminalClientMessage, TerminalServerMessage,
    UpdateAgentLaunchProfileRequest, UpdateMachineServiceRequest, UpdateServiceIngressRequest,
    WorkloadIdentityTokenRequest, WorkloadIdentityTokenResponse, WorkspaceSnapshot,
    AGENT_ID_HEADER, OPERATOR_CREDENTIAL_HEADER, WORKLOAD_CREDENTIAL_HEADER,
};
use url::Url;

const WAIT_POLL_INTERVAL: Duration = Duration::from_millis(150);
const ATTACH_DETACH_BYTE: u8 = 0x1d;
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
    #[command(about = "Discover human members of the workspace organization")]
    Member {
        #[command(subcommand)]
        command: MemberCommand,
    },
    #[command(about = "Manage workspace machines")]
    Machine {
        #[command(subcommand)]
        command: MachineCommand,
    },
    #[command(about = "Manage private networking and published services")]
    Network {
        #[command(subcommand)]
        command: NetworkCommand,
    },
    #[command(about = "Show this Agent's custom web interface instead of its terminal")]
    Ui {
        #[command(subcommand)]
        command: UiCommand,
    },
    #[command(about = "Obtain a short-lived identity token for a workspace service")]
    Token {
        #[command(subcommand)]
        command: TokenCommand,
    },
    #[command(about = "Exchange durable contextual Messages")]
    Message {
        #[command(subcommand)]
        command: MessageCommand,
    },
    #[command(about = "Show the current managed agent identity")]
    Whoami,
    #[command(about = "Show this workspace, its machines, and its agents")]
    Status,
}

#[derive(Debug, Subcommand)]
enum MemberCommand {
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
enum AgentCommand {
    #[command(about = "List agents in the current workspace")]
    List,
    #[command(about = "Show one agent by unique name or id")]
    Show { target: String },
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
    #[command(about = "Manage Agent lifecycle and launch profiles")]
    Admin {
        #[command(subcommand)]
        command: AgentAdminCommand,
    },
}

#[derive(Debug, Subcommand)]
enum AgentAdminCommand {
    #[command(about = "Create an agent")]
    Create {
        #[arg(long)]
        machine: Option<String>,
        #[arg(long)]
        kind: String,
        #[arg(long)]
        name: String,
        #[arg(long, default_value = ".")]
        cwd: String,
        #[arg(
            long = "publish",
            value_name = "PORT",
            help = "Publish a Linux network-sandbox TCP port on 127.0.0.1"
        )]
        publish_ports: Vec<u16>,
        #[arg(last = true)]
        args: Vec<String>,
    },
    #[command(about = "Rename an agent")]
    Rename { target: String, name: String },
    #[command(about = "Stop an agent")]
    Stop { target: String },
    #[command(about = "Stop and permanently remove an agent")]
    Delete { target: String },
    #[command(about = "Manage reusable Agent launch profiles")]
    Profile {
        #[command(subcommand)]
        command: ProfileCommand,
    },
}

#[derive(Debug, Subcommand)]
enum ProfileCommand {
    #[command(about = "List launch profiles in the current workspace")]
    List,
    #[command(about = "Show a launch profile by unique name or id")]
    Show { target: String },
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
    #[command(about = "List machines in the current workspace")]
    List,
    #[command(about = "Show one machine by unique name or id")]
    Show { target: String },
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
    Create { hostname: String, service: String },
    #[command(about = "Delete a workspace virtual host")]
    Delete { hostname: String },
}

#[derive(Debug, Subcommand)]
enum ServiceCommand {
    #[command(about = "List registered machine services")]
    List,
    #[command(about = "Register a long-running service")]
    Create {
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
enum TokenCommand {
    #[command(about = "Create an audience-bound Bearer token")]
    Create {
        #[arg(value_name = "SERVICE")]
        audience: String,
        #[arg(long, help = "Print the complete JSON token response")]
        json: bool,
    },
}

#[derive(Debug, Subcommand)]
enum UiCommand {
    #[command(about = "Show the current custom interface declaration")]
    Show,
    #[command(about = "Use an HTTP machine service as this Agent's interface")]
    Set {
        service: String,
        #[arg(long, default_value = "/")]
        path: String,
    },
    #[command(about = "Return this Agent to the terminal interface")]
    Clear,
}

#[derive(Debug, Subcommand)]
enum NetworkCommand {
    #[command(about = "Manage long-running machine services")]
    Service {
        #[command(subcommand)]
        command: ServiceCommand,
    },
    #[command(about = "Manage workspace virtual hosts")]
    Host {
        #[command(subcommand)]
        command: VirtualHostCommand,
    },
    #[command(about = "Publish services through wildcard HTTPS ingress")]
    Publish {
        #[command(subcommand)]
        command: PublishCommand,
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
        Self {
            http: reqwest::Client::new(),
            base,
            source_agent_id,
            workload_credential,
            operator_credential,
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
    let client = ApiClient::new(
        resolve_server_url(args.url, &args.workspace)?,
        &args.workspace,
    );
    let value = match command {
        Command::Agent { command } => run_agent_command(&client, command).await?,
        Command::Member { command } => match command {
            MemberCommand::List => client.value(Method::GET, "api/humans", None).await?,
        },
        Command::Machine { command } => run_machine_command(&client, command).await?,
        Command::Network { command } => run_network_command(&client, command).await?,
        Command::Ui { command } => run_ui_command(&client, command).await?,
        Command::Token { command } => {
            let TokenCommand::Create { audience, json } = command;
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
        Command::Whoami => whoami(&client).await?,
        Command::Status => status(&client).await?,
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

async fn status(client: &ApiClient) -> anyhow::Result<Value> {
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
        AgentCommand::Show { target } => {
            Ok(serde_json::to_value(client.get_agent(&target).await?)?)
        }
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
        AgentCommand::Admin { command } => run_agent_admin_command(client, command).await,
    }
}

async fn run_agent_admin_command(
    client: &ApiClient,
    command: AgentAdminCommand,
) -> anyhow::Result<Value> {
    match command {
        AgentAdminCommand::Create {
            machine,
            kind,
            name,
            cwd,
            publish_ports,
            args,
        } => {
            let server_id = resolve_service_machine(client, machine.as_deref()).await?;
            client
                .value(
                    Method::POST,
                    "api/agents",
                    Some(serde_json::to_value(CreateAgentRequest {
                        server_id: Some(server_id),
                        kind,
                        name,
                        cwd,
                        args,
                        cols: 120,
                        rows: 36,
                        publish_ports,
                    })?),
                )
                .await
        }
        AgentAdminCommand::Rename { target, name } => rename_agent(client, &target, name).await,
        AgentAdminCommand::Stop { target } => stop_agent(client, &target).await,
        AgentAdminCommand::Delete { target } => delete_agent(client, &target).await,
        AgentAdminCommand::Profile { command } => run_profile_command(client, command).await,
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

async fn run_profile_command(client: &ApiClient, command: ProfileCommand) -> anyhow::Result<Value> {
    match command {
        ProfileCommand::List => client.value(Method::GET, "api/launch-profiles", None).await,
        ProfileCommand::Show { target } => {
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
        MachineCommand::List => {
            let snapshot: WorkspaceSnapshot =
                client.request(Method::GET, "api/discovery", None).await?;
            Ok(serde_json::to_value(snapshot.servers)?)
        }
        MachineCommand::Show { target } => Ok(serde_json::to_value(
            resolve_machine(client, &target).await?,
        )?),
        MachineCommand::Rename { target, name } => rename_machine(client, &target, name).await,
        MachineCommand::Delete { target } => delete_machine(client, &target).await,
    }
}

async fn run_network_command(client: &ApiClient, command: NetworkCommand) -> anyhow::Result<Value> {
    match command {
        NetworkCommand::Service { command } => run_service_command(client, command).await,
        NetworkCommand::Host { command } => run_virtual_host_command(client, command).await,
        NetworkCommand::Publish { command } => run_publish_command(client, command).await,
    }
}

async fn run_ui_command(client: &ApiClient, command: UiCommand) -> anyhow::Result<Value> {
    match command {
        UiCommand::Show => client.value(Method::GET, "api/ui", None).await,
        UiCommand::Set { service, path } => {
            client
                .value(
                    Method::PUT,
                    "api/ui",
                    Some(serde_json::to_value(SetAgentUiRequest {
                        service_id: service,
                        path,
                    })?),
                )
                .await
        }
        UiCommand::Clear => client.value(Method::DELETE, "api/ui", None).await,
    }
}

async fn run_virtual_host_command(
    client: &ApiClient,
    command: VirtualHostCommand,
) -> anyhow::Result<Value> {
    match command {
        VirtualHostCommand::List => client.value(Method::GET, "api/virtual-hosts", None).await,
        VirtualHostCommand::Create { hostname, service } => {
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
        ServiceCommand::Create {
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
                        TerminalServerMessage::Ready { .. }
                        | TerminalServerMessage::Cursor { .. } => {}
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
        let nested = Args::try_parse_from(["treer", "agent", "attach", "reviewer"])
            .expect("agent attach should parse");
        assert!(matches!(
            nested.command,
            Some(Command::Agent {
                command: AgentCommand::Attach { target }
            }) if target == "reviewer"
        ));
        assert!(Args::try_parse_from(["treer", "attach", "reviewer"]).is_err());
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
            "agent",
            "admin",
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
            Some(Command::Agent {
                command: AgentCommand::Admin {
                    command: AgentAdminCommand::Profile {
                        command: ProfileCommand::Create {
                            name,
                            executable,
                            args,
                            ..
                        }
                    }
                }
            }) if name == "reviewer" && executable == "codex" && args == ["review", "--base", "main"]
        ));

        let update = Args::try_parse_from([
            "treer",
            "agent",
            "admin",
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
            Some(Command::Agent {
                command: AgentCommand::Admin {
                    command: AgentAdminCommand::Profile {
                        command: ProfileCommand::Update { args, .. }
                    }
                }
            }) if args == ["--quiet", "check"]
        ));

        let launch = Args::try_parse_from([
            "treer",
            "agent",
            "admin",
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
            Some(Command::Agent {
                command: AgentCommand::Admin {
                    command: AgentAdminCommand::Profile {
                        command: ProfileCommand::Launch { target, machine, name }
                    }
                }
            }) if target == "reviewer" && machine.as_deref() == Some("builder") && name.as_deref() == Some("review-42")
        ));
        assert!(Args::try_parse_from(["treer", "profile", "list"]).is_err());
    }

    #[test]
    fn virtual_host_commands_parse() {
        let add = Args::try_parse_from([
            "treer",
            "network",
            "host",
            "create",
            "api.internal",
            "api-service",
        ])
        .expect("virtual host add should parse");
        assert!(matches!(
            add.command,
            Some(Command::Network {
                command: NetworkCommand::Host {
                    command: VirtualHostCommand::Create {
                        hostname,
                        service,
                    }
                }
            }) if hostname == "api.internal"
                && service == "api-service"
        ));

        let delete = Args::try_parse_from(["treer", "network", "host", "delete", "api.internal"])
            .expect("network host delete should parse");
        assert!(matches!(
            delete.command,
            Some(Command::Network {
                command: NetworkCommand::Host {
                    command: VirtualHostCommand::Delete { hostname }
                }
            }) if hostname == "api.internal"
        ));
    }

    #[test]
    fn publish_commands_parse() {
        let args = Args::try_parse_from([
            "treer",
            "network",
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
            Some(Command::Network {
                command: NetworkCommand::Publish {
                    command: PublishCommand::Create {
                        service,
                        slug: Some(slug),
                        access: CliIngressAccess::Workspace,
                    }
                }
            }) if service == "api" && slug == "issue-tracker"
        ));

        let args =
            Args::try_parse_from(["treer", "network", "publish", "disable", "demo.apps.test"])
                .expect("network publish disable should parse");
        assert!(matches!(
            args.command,
            Some(Command::Network {
                command: NetworkCommand::Publish {
                    command: PublishCommand::Disable { target }
                }
            }) if target == "demo.apps.test"
        ));
    }

    #[test]
    fn service_commands_parse() {
        let register = Args::try_parse_from([
            "treer",
            "network",
            "service",
            "create",
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
            Some(Command::Network {
                command: NetworkCommand::Service {
                    command: ServiceCommand::Create {
                        name,
                        machine: Some(machine),
                        port: 8080,
                        protocol: CliServiceProtocol::Http,
                        ..
                    }
                }
            }) if name == "api" && machine == "builder"
        ));
    }

    #[test]
    fn agent_ui_commands_parse() {
        let set = Args::try_parse_from(["treer", "ui", "set", "dashboard", "--path", "/treer/"])
            .expect("Agent UI set should parse");
        assert!(matches!(
            set.command,
            Some(Command::Ui {
                command: UiCommand::Set { service, path }
            }) if service == "dashboard" && path == "/treer/"
        ));

        let clear =
            Args::try_parse_from(["treer", "ui", "clear"]).expect("Agent UI clear should parse");
        assert!(matches!(
            clear.command,
            Some(Command::Ui {
                command: UiCommand::Clear
            })
        ));
    }

    #[test]
    fn identity_token_command_parses() {
        let args = Args::try_parse_from(["treer", "token", "create", "api"])
            .expect("token create should parse");
        assert!(matches!(
            args.command,
            Some(Command::Token {
                command: TokenCommand::Create {
                    audience,
                    json: false,
                }
            }) if audience == "api"
        ));
    }

    #[test]
    fn member_directory_command_parses() {
        let members =
            Args::try_parse_from(["treer", "member", "list"]).expect("member list should parse");
        assert!(matches!(
            members.command,
            Some(Command::Member {
                command: MemberCommand::List
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
    fn agent_admin_commands_parse() {
        let create = Args::try_parse_from([
            "treer",
            "agent",
            "admin",
            "create",
            "--machine",
            "builder",
            "--kind",
            "command",
            "--name",
            "shell",
            "--",
            "/bin/sh",
        ])
        .expect("agent admin create should parse");
        assert!(matches!(
            create.command,
            Some(Command::Agent {
                command: AgentCommand::Admin {
                    command: AgentAdminCommand::Create {
                        machine: Some(machine),
                        name,
                        ..
                    }
                }
            }) if machine == "builder" && name == "shell"
        ));

        let delete = Args::try_parse_from(["treer", "agent", "admin", "delete", "reviewer"])
            .expect("agent admin delete should parse");
        assert!(matches!(
            delete.command,
            Some(Command::Agent {
                command: AgentCommand::Admin {
                    command: AgentAdminCommand::Delete { target }
                }
            }) if target == "reviewer"
        ));
        assert!(Args::try_parse_from(["treer", "agent", "delete", "reviewer"]).is_err());

        let publish = Args::try_parse_from([
            "treer",
            "agent",
            "admin",
            "create",
            "--machine",
            "builder",
            "--kind",
            "command",
            "--name",
            "codex-ui",
            "--publish",
            "4173",
            "--",
            "/opt/codex-agent-ui/scripts/treer-agent.sh",
        ])
        .expect("agent admin create --publish should parse");
        assert!(matches!(
            publish.command,
            Some(Command::Agent {
                command: AgentCommand::Admin {
                    command: AgentAdminCommand::Create {
                        publish_ports,
                        ..
                    }
                }
            }) if publish_ports == vec![4173]
        ));
    }
}
