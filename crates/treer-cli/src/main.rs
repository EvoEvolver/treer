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
    AgentInfo, AgentStatus, CreateAgentRequest, CreateVirtualNetworkHostRequest, InputAgentRequest,
    RenameRequest, ServerInfo, ServerStatus, TerminalClientMessage, TerminalServerMessage,
    TransferBinaryFrame, TransferServerMessage, TransferStats, WorkspaceSnapshot, AGENT_ID_HEADER,
};
use treer_transfer::TransferReceiver;
use url::Url;

const WAIT_POLL_INTERVAL: Duration = Duration::from_millis(150);
const ATTACH_DETACH_BYTE: u8 = 0x1d;
const TRANSFER_WINDOW_FRAMES: usize = 16;
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
    #[command(about = "Show the current managed agent identity")]
    Whoami,
    #[command(about = "Show this workspace, its machines, and its agents")]
    Discover,
    #[command(about = "Open a shell on another workspace machine")]
    Ssh {
        target: String,
        #[arg(long, default_value = ".")]
        cwd: String,
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<String>,
    },
    #[command(about = "Copy files to or from another workspace machine")]
    Scp {
        #[arg(short = 'r', long)]
        recursive: bool,
        source: String,
        destination: String,
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
    #[command(about = "Map a virtual hostname to a workspace machine")]
    Add {
        hostname: String,
        machine: String,
        #[arg(long, default_value = "127.0.0.1")]
        target_host: String,
        #[arg(long)]
        target_port: Option<u16>,
    },
    #[command(about = "Delete a workspace virtual host")]
    Delete { hostname: String },
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
}

impl ApiClient {
    fn new(base: Url) -> Self {
        Self {
            http: reqwest::Client::new(),
            base,
            source_agent_id: std::env::var("TREER_AGENT_ID").ok(),
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
        Command::Ssh {
            target,
            cwd,
            command,
        } => {
            let exit_code = ssh_machine(&client, &target, cwd, command).await?;
            if exit_code != 0 {
                std::process::exit(exit_code.clamp(1, 255));
            }
            return Ok(());
        }
        Command::Scp {
            recursive,
            source,
            destination,
        } => {
            scp(&client, recursive, &source, &destination).await?;
            return Ok(());
        }
        Command::Agent { command } => run_agent_command(&client, command).await?,
        Command::Machine { command } => run_machine_command(&client, command).await?,
        Command::VirtualHost { command } => run_virtual_host_command(&client, command).await?,
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
        VirtualHostCommand::Add {
            hostname,
            machine,
            target_host,
            target_port,
        } => {
            client
                .value(
                    Method::POST,
                    "api/virtual-hosts",
                    Some(serde_json::to_value(CreateVirtualNetworkHostRequest {
                        hostname,
                        destination_server_id: machine,
                        target_host,
                        target_port,
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

async fn ssh_machine(
    client: &ApiClient,
    target: &str,
    cwd: String,
    command: Vec<String>,
) -> anyhow::Result<i32> {
    let server = resolve_machine(client, target).await?;
    if server.status != ServerStatus::Online {
        bail!("machine {} is offline", server.name);
    }
    let interactive = command.is_empty();
    let mut url = client
        .base
        .join(&format!("api/ssh/{}", path_segment(&server.server_id)))
        .context("failed to build remote shell URL")?;
    url.query_pairs_mut().append_pair("cwd", &cwd);
    if !command.is_empty() {
        url.query_pairs_mut()
            .append_pair("command", &command.join(" "));
    }
    let outcome = relay_terminal(url, &server.name, interactive).await?;
    if interactive {
        return Ok(outcome.exit_code.unwrap_or(0));
    }
    Ok(outcome.exit_code.unwrap_or(1))
}

#[derive(Debug)]
struct RemotePath {
    machine: String,
    path: String,
}

async fn scp(
    client: &ApiClient,
    recursive: bool,
    source: &str,
    destination: &str,
) -> anyhow::Result<()> {
    let remote_source = parse_remote_path(source);
    let remote_destination = parse_remote_path(destination);
    match (remote_source, remote_destination) {
        (Some(_), Some(_)) => {
            bail!("remote-to-remote copies are not supported; exactly one path must be local")
        }
        (None, None) => bail!("exactly one path must use the machine:path form"),
        (None, Some(remote)) => upload_path(client, PathBuf::from(source), remote, recursive).await,
        (Some(remote), None) => {
            download_path(client, remote, PathBuf::from(destination), recursive).await
        }
    }
}

fn parse_remote_path(value: &str) -> Option<RemotePath> {
    if value.starts_with('/') || value.starts_with("./") || value.starts_with("../") {
        return None;
    }
    let (machine, path) = value.split_once(':')?;
    if machine.is_empty()
        || path.is_empty()
        || machine.contains('/')
        || machine.contains(std::path::MAIN_SEPARATOR)
    {
        return None;
    }
    Some(RemotePath {
        machine: machine.to_string(),
        path: path.to_string(),
    })
}

async fn upload_path(
    client: &ApiClient,
    source: PathBuf,
    remote: RemotePath,
    recursive: bool,
) -> anyhow::Result<()> {
    let metadata = tokio::fs::symlink_metadata(&source)
        .await
        .with_context(|| format!("source does not exist: {}", source.display()))?;
    if metadata.is_dir() && !recursive {
        bail!("{} is a directory; use --recursive", source.display());
    }
    let server = resolve_machine(client, &remote.machine).await?;
    require_online_machine(&server)?;
    let url = transfer_url(client, &server, "upload", &remote.path, recursive)?;
    let (mut socket, _) = tokio_tungstenite::connect_async(url.as_str())
        .await
        .with_context(|| format!("failed to connect to {}", server.name))?;
    let session_id = wait_for_transfer_ready(&mut socket).await?;
    let (frame_tx, mut frame_rx) = tokio::sync::mpsc::channel(16);
    let task_session = session_id.clone();
    let mut producer = tokio::spawn(async move {
        treer_transfer::stream_path(source, recursive, task_session, frame_tx).await
    });
    let (mut outgoing, mut incoming) = socket.split();
    let mut producer_finished = false;
    let mut local_stats = None;
    let mut in_flight = 0_usize;
    let outcome: anyhow::Result<TransferStats> = loop {
        tokio::select! {
            frame = frame_rx.recv(), if !producer_finished && in_flight < TRANSFER_WINDOW_FRAMES => {
                let Some(frame) = frame else {
                    producer_finished = true;
                    local_stats = Some(
                        (&mut producer)
                            .await
                            .context("file reader task failed")?
                            .map_err(|error| anyhow::anyhow!("{}: {}", error.code, error.message))?
                    );
                    continue;
                };
                let encoded = frame
                    .encode()
                    .map_err(|error| anyhow::anyhow!("{}: {}", error.code, error.message))?;
                outgoing
                    .send(Message::Binary(encoded.into()))
                    .await
                    .context("failed to send file data")?;
                in_flight += 1;
            }
            message = incoming.next() => {
                let Some(message) = message else {
                    break Err(anyhow::anyhow!("transfer connection closed"));
                };
                match message.context("failed to read transfer response")? {
                    Message::Text(text) => match serde_json::from_str::<TransferServerMessage>(&text)
                        .context("invalid transfer response")? {
                        TransferServerMessage::Complete { stats, .. } => break Ok(stats),
                        TransferServerMessage::Progress { .. } => {
                            in_flight = in_flight.saturating_sub(1);
                        }
                        TransferServerMessage::Error { error } => {
                            break Err(anyhow::anyhow!("{}: {}", error.code, error.message));
                        }
                        TransferServerMessage::Ready { .. } => {}
                    },
                    Message::Ping(data) => {
                        outgoing.send(Message::Pong(data)).await.context("failed to reply to ping")?;
                    }
                    Message::Close(_) => break Err(anyhow::anyhow!("transfer connection closed")),
                    Message::Binary(_) => {
                        break Err(anyhow::anyhow!("upload received unexpected file data"));
                    }
                    Message::Pong(_) | Message::Frame(_) => {}
                }
            }
        }
    };
    let remote_stats = match outcome {
        Ok(stats) => stats,
        Err(error) => {
            if !producer_finished {
                producer.abort();
                let _ = producer.await;
            }
            return Err(error);
        }
    };
    let local_stats = match local_stats {
        Some(stats) => stats,
        None => producer
            .await
            .context("file reader task failed")?
            .map_err(|error| anyhow::anyhow!("{}: {}", error.code, error.message))?,
    };
    if local_stats != remote_stats {
        bail!("remote transfer summary did not match the local file data");
    }
    eprintln!(
        "copied {} entr{} ({} bytes) to {}:{}",
        remote_stats.entries,
        if remote_stats.entries == 1 {
            "y"
        } else {
            "ies"
        },
        remote_stats.bytes,
        server.name,
        remote.path
    );
    Ok(())
}

async fn download_path(
    client: &ApiClient,
    remote: RemotePath,
    destination: PathBuf,
    recursive: bool,
) -> anyhow::Result<()> {
    let server = resolve_machine(client, &remote.machine).await?;
    require_online_machine(&server)?;
    let url = transfer_url(client, &server, "download", &remote.path, recursive)?;
    let (mut socket, _) = tokio_tungstenite::connect_async(url.as_str())
        .await
        .with_context(|| format!("failed to connect to {}", server.name))?;
    let session_id = wait_for_transfer_ready(&mut socket).await?;
    let mut receiver = TransferReceiver::new(destination, None, recursive, session_id)
        .await
        .map_err(|error| anyhow::anyhow!("{}: {}", error.code, error.message))?;
    let mut local_stats = None;
    let remote_stats = loop {
        let Some(message) = socket.next().await else {
            receiver.cancel().await;
            bail!("transfer connection closed");
        };
        match message.context("failed to read file data")? {
            Message::Binary(encoded) => {
                let frame = TransferBinaryFrame::decode(&encoded)
                    .map_err(|error| anyhow::anyhow!("{}: {}", error.code, error.message))?;
                if let Some(stats) = receiver
                    .receive(frame)
                    .await
                    .map_err(|error| anyhow::anyhow!("{}: {}", error.code, error.message))?
                {
                    local_stats = Some(stats);
                }
            }
            Message::Text(text) => match serde_json::from_str::<TransferServerMessage>(&text)
                .context("invalid transfer response")?
            {
                TransferServerMessage::Complete { stats, .. } => break stats,
                TransferServerMessage::Progress { .. } => {}
                TransferServerMessage::Error { error } => {
                    receiver.cancel().await;
                    bail!("{}: {}", error.code, error.message);
                }
                TransferServerMessage::Ready { .. } => {}
            },
            Message::Ping(data) => {
                socket
                    .send(Message::Pong(data))
                    .await
                    .context("failed to reply to ping")?;
            }
            Message::Close(_) => {
                receiver.cancel().await;
                bail!("transfer connection closed");
            }
            Message::Pong(_) | Message::Frame(_) => {}
        }
    };
    if local_stats != Some(remote_stats) {
        receiver.cancel().await;
        bail!("local transfer summary did not match the remote file data");
    }
    eprintln!(
        "copied {} entr{} ({} bytes) from {}:{}",
        remote_stats.entries,
        if remote_stats.entries == 1 {
            "y"
        } else {
            "ies"
        },
        remote_stats.bytes,
        server.name,
        remote.path
    );
    Ok(())
}

fn transfer_url(
    client: &ApiClient,
    server: &ServerInfo,
    direction: &str,
    path: &str,
    recursive: bool,
) -> anyhow::Result<Url> {
    let mut url = client
        .base
        .join(&format!("api/scp/{}", path_segment(&server.server_id)))
        .context("failed to build file transfer URL")?;
    url.query_pairs_mut()
        .append_pair("direction", direction)
        .append_pair("path", path)
        .append_pair("recursive", &recursive.to_string());
    let scheme = match url.scheme() {
        "http" => "ws",
        "https" => "wss",
        scheme => bail!("unsupported agent server URL scheme {scheme}"),
    };
    url.set_scheme(scheme)
        .map_err(|_| anyhow::anyhow!("invalid agent server URL scheme"))?;
    Ok(url)
}

async fn wait_for_transfer_ready(
    socket: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
) -> anyhow::Result<String> {
    loop {
        let Some(message) = socket.next().await else {
            bail!("transfer connection closed before it was ready");
        };
        match message.context("failed to initialize transfer")? {
            Message::Text(text) => match serde_json::from_str::<TransferServerMessage>(&text)
                .context("invalid transfer response")?
            {
                TransferServerMessage::Ready { session_id } => return Ok(session_id),
                TransferServerMessage::Progress { .. } => {
                    bail!("transfer reported progress before it became ready")
                }
                TransferServerMessage::Error { error } => {
                    bail!("{}: {}", error.code, error.message)
                }
                TransferServerMessage::Complete { .. } => {
                    bail!("transfer completed before it became ready")
                }
            },
            Message::Ping(data) => socket
                .send(Message::Pong(data))
                .await
                .context("failed to reply to ping")?,
            Message::Close(_) => bail!("transfer connection closed before it was ready"),
            Message::Binary(_) => bail!("transfer sent data before it was ready"),
            Message::Pong(_) | Message::Frame(_) => {}
        }
    }
}

fn require_online_machine(server: &ServerInfo) -> anyhow::Result<()> {
    if server.status != ServerStatus::Online {
        bail!("machine {} is offline", server.name);
    }
    Ok(())
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
    exit_code: Option<i32>,
}

async fn relay_terminal(
    mut url: Url,
    target: &str,
    interactive_required: bool,
) -> anyhow::Result<TerminalOutcome> {
    let interactive = std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
    if interactive_required && !interactive {
        bail!("remote shell requires an interactive terminal when no command is provided");
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
    Ok(TerminalOutcome { reason, exit_code })
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
            "builder",
            "--target-host",
            "127.0.0.1",
            "--target-port",
            "8080",
        ])
        .expect("virtual host add should parse");
        assert!(matches!(
            add.command,
            Some(Command::VirtualHost {
                command: VirtualHostCommand::Add {
                    hostname,
                    machine,
                    target_host,
                    target_port: Some(8080),
                }
            }) if hostname == "api.internal"
                && machine == "builder"
                && target_host == "127.0.0.1"
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
    fn ssh_commands_parse_interactive_and_remote_commands() {
        let interactive = Args::try_parse_from(["treer", "ssh", "builder"])
            .expect("interactive ssh should parse");
        assert!(matches!(
            interactive.command,
            Some(Command::Ssh { target, cwd, command })
                if target == "builder" && cwd == "." && command.is_empty()
        ));

        let command = Args::try_parse_from([
            "treer", "ssh", "builder", "--cwd", "src", "--", "cargo", "test", "-q",
        ])
        .expect("remote ssh command should parse");
        assert!(matches!(
            command.command,
            Some(Command::Ssh { target, cwd, command })
                if target == "builder"
                    && cwd == "src"
                    && command == ["cargo", "test", "-q"]
        ));
    }

    #[test]
    fn scp_commands_parse_uploads_and_recursive_downloads() {
        let upload = Args::try_parse_from(["treer", "scp", "notes.txt", "builder:notes.txt"])
            .expect("scp upload should parse");
        assert!(matches!(
            upload.command,
            Some(Command::Scp { recursive: false, source, destination })
                if source == "notes.txt" && destination == "builder:notes.txt"
        ));

        let download =
            Args::try_parse_from(["treer", "scp", "-r", "builder:results", "./downloaded"])
                .expect("recursive scp download should parse");
        assert!(matches!(
            download.command,
            Some(Command::Scp { recursive: true, source, destination })
                if source == "builder:results" && destination == "./downloaded"
        ));
    }

    #[test]
    fn scp_remote_paths_require_machine_prefixes() {
        let remote = parse_remote_path("builder:src/main.rs").expect("remote path");
        assert_eq!(remote.machine, "builder");
        assert_eq!(remote.path, "src/main.rs");
        assert!(parse_remote_path("./local:file").is_none());
        assert!(parse_remote_path("/tmp/local:file").is_none());
    }
}
