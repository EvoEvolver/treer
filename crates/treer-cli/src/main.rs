use std::io::IsTerminal;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{bail, Context};
use clap::{Parser, Subcommand, ValueEnum};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, size as terminal_size};
use futures_util::{SinkExt, StreamExt};
use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
use reqwest::Method;
use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_tungstenite::tungstenite::Message;
use treer_protocol::{
    AgentInboxRequest, AgentInfo, AgentStatus, CreateAgentRequest, CreateMachineServiceRequest,
    CreateVirtualNetworkHostRequest, InputAgentRequest, MachineServiceProtocol, RenameRequest,
    SendAgentMailRequest, ServerInfo, TerminalClientMessage, TerminalServerMessage,
    UpdateMachineServiceRequest, WorkloadIdentityTokenRequest, WorkloadIdentityTokenResponse,
    WorkspaceSnapshot, AGENT_ID_HEADER, WORKLOAD_CREDENTIAL_HEADER,
};
use url::Url;

const WAIT_POLL_INTERVAL: Duration = Duration::from_millis(150);
const ATTACH_DETACH_BYTE: u8 = 0x1d;
const SKILL: &str = include_str!("../../../skills/treer/SKILL.md");

#[derive(Debug, Parser)]
#[command(
    name = "treer",
    about = "Discover and coordinate Treer agents",
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
    #[command(about = "Obtain a short-lived identity token for a workspace service")]
    Identity {
        #[command(subcommand)]
        command: IdentityCommand,
    },
    #[command(about = "Show the current managed agent identity")]
    Whoami,
    #[command(about = "Show this workspace, its machines, and its agents")]
    Discover,
    #[command(about = "Send durable mail without interrupting recipient agents")]
    Mail {
        #[arg(short = 't', long = "to")]
        recipients: Vec<String>,
        #[arg(long = "to-human")]
        human_recipients: Vec<String>,
        #[arg(short = 'c', long = "context")]
        context_ids: Vec<String>,
        body: String,
    },
    #[command(about = "Read and mark the current agent's unread mail")]
    Inbox {
        #[arg(long, default_value_t = 50, value_parser = clap::value_parser!(u16).range(1..=100))]
        limit: u16,
    },
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
}

impl ApiClient {
    fn new(base: Url) -> Self {
        Self {
            http: reqwest::Client::new(),
            base,
            source_agent_id: std::env::var("TREER_AGENT_ID").ok(),
            workload_credential: std::env::var("TREER_WORKLOAD_CREDENTIAL").ok(),
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
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().await.context("request failed")?;
        let status = response.status();
        let value: Value = response.json().await.context("invalid JSON response")?;
        if !status.is_success() {
            let message = value
                .pointer("/error/message")
                .and_then(Value::as_str)
                .unwrap_or("request failed");
            bail!("{}: {}", status, message);
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

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    if args.skill {
        print!("{SKILL}");
        return Ok(());
    }
    let command = args
        .command
        .context("a command is required; run `treer --help` for usage")?;
    let client = ApiClient::new(resolve_server_url(args.url, &args.workspace)?);
    let value = match command {
        Command::Agent { command } => run_agent_command(&client, command).await?,
        Command::Human { command } => match command {
            HumanCommand::List => client.value(Method::GET, "api/humans", None).await?,
        },
        Command::Machine { command } => run_machine_command(&client, command).await?,
        Command::VirtualHost { command } => run_virtual_host_command(&client, command).await?,
        Command::Service { command } => run_service_command(&client, command).await?,
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
        Command::Whoami => whoami(&client).await?,
        Command::Discover => discover(&client).await?,
        Command::Mail {
            recipients,
            human_recipients,
            context_ids,
            body,
        } => {
            validate_mail_recipients(&recipients, &human_recipients)?;
            let recipients = recipients
                .into_iter()
                .map(|target| normalize_target(&target))
                .collect::<anyhow::Result<Vec<_>>>()?;
            client
                .value(
                    Method::POST,
                    "api/mail",
                    Some(serde_json::to_value(SendAgentMailRequest {
                        recipients,
                        human_recipients,
                        context_ids,
                        body,
                    })?),
                )
                .await?
        }
        Command::Inbox { limit } => {
            client
                .value(
                    Method::POST,
                    "api/inbox",
                    Some(serde_json::to_value(AgentInboxRequest { limit })?),
                )
                .await?
        }
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
    let outcome = relay_terminal(url, &target, true).await?;
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

    let (socket, _) = tokio_tungstenite::connect_async(url.as_str())
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

fn validate_mail_recipients(agents: &[String], humans: &[String]) -> anyhow::Result<()> {
    if agents.is_empty() && humans.is_empty() {
        bail!("mail requires at least one --to or --to-human recipient");
    }
    Ok(())
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
    fn mail_and_inbox_commands_parse_agent_friendly_repeated_options() {
        let mail = Args::try_parse_from([
            "treer",
            "mail",
            "--to",
            "reviewer",
            "-t",
            "tester",
            "--context",
            "msg_one",
            "-c",
            "msg_two",
            "Review complete.",
        ])
        .expect("mail command should parse");
        assert!(matches!(
            mail.command,
            Some(Command::Mail {
                recipients,
                human_recipients,
                context_ids,
                body,
            }) if recipients == ["reviewer", "tester"]
                && human_recipients.is_empty()
                && context_ids == ["msg_one", "msg_two"]
                && body == "Review complete."
        ));
        let no_recipient = Args::try_parse_from(["treer", "mail", "no recipient"])
            .expect("recipient validation happens after parsing");
        let Some(Command::Mail {
            recipients,
            human_recipients,
            ..
        }) = no_recipient.command
        else {
            panic!("expected mail command");
        };
        assert!(validate_mail_recipients(&recipients, &human_recipients).is_err());

        let human_mail =
            Args::try_parse_from(["treer", "mail", "--to-human", "usr_123", "Human update."])
                .expect("human mail should parse");
        assert!(matches!(
            human_mail.command,
            Some(Command::Mail { human_recipients, .. }) if human_recipients == ["usr_123"]
        ));

        let humans =
            Args::try_parse_from(["treer", "human", "list"]).expect("human list should parse");
        assert!(matches!(
            humans.command,
            Some(Command::Human {
                command: HumanCommand::List
            })
        ));

        let inbox = Args::try_parse_from(["treer", "inbox", "--limit", "100"])
            .expect("inbox command should parse");
        assert!(matches!(inbox.command, Some(Command::Inbox { limit: 100 })));
        assert!(Args::try_parse_from(["treer", "inbox", "--limit", "101"]).is_err());
    }
}
