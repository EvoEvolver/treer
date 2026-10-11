use std::env;
use std::io::IsTerminal;
use std::io::Read as StdRead;
use std::path::Path;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{bail, Context};
use chrono::{DateTime, Utc};
use clap::{Parser, Subcommand, ValueEnum};
use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
use reqwest::Method;
use serde::de::DeserializeOwned;
use serde_json::{json, Value};

#[path = "cli_workload_tree.rs"]
mod workload_tree;
use treer_protocol::{
    AcknowledgeMessagesRequest, AgentInfo, AgentStatus, ApiError, CreateAgentLaunchProfileRequest,
    CreateAgentRequest, CreateAppDeploymentRequest, CreateMachineServiceRequest,
    GetMessageResponse, ImportMessagesRequest, InputAgentRequest, LaunchAgentProfileRequest,
    LegacyMailMessage, MachineExecRequest, MachineService, MachineServiceProtocol,
    MessageExternalSource, ReceiveMessagesRequest, RegisterAgentInterfaceRequest, RenameRequest,
    SendMessageRequest, ServerInfo, SetAgentStartupRequest, UpdateAgentLaunchProfileRequest,
    WorkloadIdentityTokenRequest, WorkloadIdentityTokenResponse, WorkspaceSnapshot,
    AGENT_ID_HEADER, AGENT_INTERFACE_PROTOCOL_V1, INSTALL_SKILL, OPERATOR_CREDENTIAL_HEADER,
    WORKLOAD_CREDENTIAL_HEADER,
};
use url::Url;

const WAIT_POLL_INTERVAL: Duration = Duration::from_millis(150);
const SKILL: &str = include_str!("../../../skills/treer/SKILL.md");
const MACOS_CONTAINER_SKILL: &str = include_str!("../../../skills/treer-macos-container/SKILL.md");

#[cfg(test)]
mod session_contract_tests {
    use super::*;
    #[test]
    fn self_session_uses_authenticated_caller_and_fails_without_it() {
        let argv = ["agent".to_owned(), "attach".to_owned(), "self".to_owned()];
        assert!(session_action(&argv, None).is_err());
        assert!(
            matches!(session_action(&argv, Some("agent-a")).unwrap(), Some(treer_protocol::cli::CliOutput::Session { path, terminal: true }) if path == "/api/agents/agent%2Da/terminal")
        );
    }
    #[test]
    fn interpreter_cannot_override_context_but_keeps_executed_argv() {
        assert!(Args::try_parse_from(["treer", "--url", "http://elsewhere", "status"]).is_err());
        assert!(Args::try_parse_from(["treer", "--workspace", "other", "status"]).is_err());
        let args = Args::try_parse_from([
            "treer",
            "exec",
            "--machine",
            "builder",
            "--",
            "tool",
            "--url",
            "literal",
            "--body-file",
            "client-path",
        ])
        .unwrap();
        assert!(
            matches!(args.command, Some(Command::Exec { command, .. }) if command == ["tool", "--url", "literal", "--body-file", "client-path"])
        );
        assert!(read_text_file_or_stdin(Path::new("/should-not-be-read"))
            .unwrap_err()
            .to_string()
            .contains("client-local files"));
    }
}

pub fn session_action(
    argv: &[String],
    source_agent_id: Option<&str>,
) -> anyhow::Result<Option<treer_protocol::cli::CliOutput>> {
    let Ok(args) =
        Args::try_parse_from(std::iter::once("treer".to_owned()).chain(argv.iter().cloned()))
    else {
        return Ok(None);
    };
    if let Some(Command::Service {
        command: ServiceCommand::Forward { service, listen },
    }) = &args.command
    {
        return Ok(Some(treer_protocol::cli::CliOutput::Forward {
            path: format!("/api/services/{}/connect", path_segment(service)),
            listen: listen.clone(),
        }));
    }
    if let Some(Command::Agent {
        command: AgentCommand::Attach { target },
    }) = args.command
    {
        let target = if matches!(target.as_str(), "self" | ".") {
            source_agent_id
                .context("self target requires an authenticated Agent identity")?
                .to_owned()
        } else {
            target
        };
        return Ok(Some(treer_protocol::cli::CliOutput::Session {
            path: format!("/api/agents/{}/terminal", path_segment(&target)),
            terminal: true,
        }));
    }
    Ok(None)
}

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
        num_args = 0..=1,
        default_missing_value = "treer",
        value_name = "NAME",
        help = "Print a bundled skill and exit (`treer`, `install`, or `macos-container`)"
    )]
    skill: Option<String>,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    #[command(about = "Register and explicitly forward workspace service endpoints")]
    Service {
        #[command(subcommand)]
        command: ServiceCommand,
    },
    Runtime {
        #[command(subcommand)]
        command: RuntimeCommand,
    },
    #[command(about = "Run a bounded command on a workspace machine and return its output")]
    Exec {
        #[arg(long)]
        machine: String,
        #[arg(long, default_value = ".")]
        cwd: String,
        #[arg(long, default_value_t = 30_000, value_name = "MS")]
        timeout: u64,
        #[arg(last = true, required = true, value_name = "COMMAND")]
        command: Vec<String>,
    },
    #[command(about = "Control and communicate with workspace agents")]
    Agent {
        #[command(subcommand)]
        command: AgentCommand,
    },
    #[command(about = "Manage long-running workspace Apps")]
    App {
        #[command(subcommand)]
        command: AppCommand,
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
    #[command(about = "Connect through the Agent's private Treer network")]
    Network {
        #[command(subcommand)]
        command: NetworkCommand,
    },
    #[command(about = "Register this Agent's semantic interface server")]
    Interface {
        #[command(subcommand)]
        command: InterfaceCommand,
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
enum ServiceCommand {
    List,
    Register {
        #[arg(long)]
        name: String,
        #[arg(long)]
        port: u16,
        #[arg(long)]
        machine: Option<String>,
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
        #[arg(long, default_value = "tcp")]
        protocol: String,
        #[arg(
            long,
            help = "Register or update this Agent's stable primary HTTP endpoint"
        )]
        primary: bool,
    },
    Forward {
        service: String,
        #[arg(long, default_value = "127.0.0.1:8080")]
        listen: String,
    },
    Publish {
        service: String,
        #[arg(long, default_value = "workspace")]
        access: String,
        #[arg(long)]
        tcp: bool,
    },
    Delete {
        service: String,
    },
    Unpublish {
        service: String,
        #[arg(long)]
        tcp: bool,
    },
}

#[derive(Debug, Subcommand)]
enum RuntimeCommand {
    Restore {
        #[arg(long, required = true)]
        all: bool,
        #[arg(long)]
        server: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
enum AppCommand {
    #[command(about = "List Apps in the current workspace")]
    List,
    #[command(about = "Show an App by unique name or id")]
    Show { target: String },
    #[command(about = "Create and start an App with one HTTP UI endpoint")]
    Create {
        #[arg(long)]
        machine: Option<String>,
        #[arg(long)]
        name: String,
        #[arg(long, default_value = ".")]
        cwd: String,
        #[arg(long)]
        port: u16,
        #[arg(
            long,
            help = "Expose the App without Treer authentication on its managed public URL"
        )]
        public: bool,
        #[arg(value_name = "COMMAND")]
        executable: String,
        #[arg(last = true)]
        args: Vec<String>,
    },
    #[command(about = "Start a stopped App")]
    Start { target: String },
    #[command(about = "Stop an App without deleting its service or virtual host")]
    Stop { target: String },
    #[command(about = "Replace an App runtime while preserving its service and virtual host")]
    Restart { target: String },
    #[command(about = "Stop and permanently delete an App and its routes")]
    Delete { target: String },
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
        #[arg(long, value_name = "PATH", conflicts_with = "body", hide = true)]
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
        #[arg(long, value_name = "PATH", conflicts_with = "body", hide = true)]
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
        #[arg(long, value_name = "PATH", hide = true)]
        body_file: Option<PathBuf>,
        #[arg(long)]
        operation_id: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
enum AgentCommand {
    #[command(about = "Create a workspace folder (paths start at workspace root)")]
    Mkdir {
        path: String,
        #[arg(short = 'p', long)]
        parents: bool,
    },
    #[command(about = "List folders and Agent/App workloads")]
    Ls {
        #[arg(default_value = "/")]
        path: String,
        #[arg(short = 'R', long)]
        recursive: bool,
    },
    #[command(about = "Move an Agent/App into a folder, or move/rename a folder")]
    Mv { source: String, destination: String },
    #[command(about = "Remove an empty workspace folder")]
    Rmdir { path: String },
    #[command(about = "Start an Agent or App from its saved launch")]
    Start { target: String },
    #[command(about = "Stop an Agent or App")]
    Stop { target: String },
    #[command(about = "Restart an Agent or App preserving its identity")]
    Restart { target: String },
    #[command(about = "List agents in the current workspace")]
    List,
    #[command(about = "Show one agent by unique name or id")]
    Show { target: String },
    #[command(about = "Print an Agent's primary HTTP service URL for curl or other HTTP clients")]
    Address {
        #[arg(default_value = "self")]
        target: String,
        #[arg(long)]
        json: bool,
    },
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
    #[command(about = "Read a structured transcript, one conversation turn per page")]
    Transcript {
        target: String,
        /// 0-based conversation turn. A turn starts at a user prompt.
        #[arg(long)]
        page: Option<u32>,
        /// Alias for `--page`.
        #[arg(long)]
        cursor: Option<String>,
        /// Number of turns to return. Default is one turn.
        #[arg(long, default_value_t = 1)]
        limit: usize,
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
    #[command(about = "Manage this Agent's command for recovery after a Host restart")]
    Startup {
        #[command(subcommand)]
        command: AgentStartupCommand,
    },
    #[command(about = "Manage Agent lifecycle and launch profiles")]
    Admin {
        #[command(subcommand)]
        command: AgentAdminCommand,
    },
}

#[derive(Debug, Subcommand)]
enum AgentStartupCommand {
    #[command(about = "Register this Agent's Host-restart command")]
    Set {
        #[arg(long, default_value = ".")]
        cwd: String,
        #[arg(
            long = "publish",
            value_name = "PORT",
            help = "Retired; register process ports with treer service register instead"
        )]
        publish_ports: Vec<u16>,
        #[arg(last = true, required = true, value_name = "COMMAND")]
        command: Vec<String>,
    },
    #[command(about = "Show this Agent's registered Host-restart command")]
    Show,
    #[command(about = "Disable and remove this Agent's Host-restart command")]
    Clear,
}

#[derive(Debug, Subcommand)]
enum AgentAdminCommand {
    Start {
        target: String,
    },
    Restart {
        target: String,
    },
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
            help = "Retired; register process ports with treer service register instead"
        )]
        publish_ports: Vec<u16>,
        #[arg(
            long,
            value_name = "URL",
            help = "Retired; launch an explicit installer command or profile instead"
        )]
        recipe: Option<String>,
        #[arg(last = true)]
        args: Vec<String>,
    },
    #[command(about = "Rename an agent")]
    Rename {
        target: String,
        name: String,
    },
    #[command(about = "Stop an agent")]
    Stop {
        target: String,
    },
    #[command(about = "Stop and permanently remove an agent")]
    Delete {
        target: String,
    },
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
        #[arg(long, help = "Override the profile working directory for this launch")]
        cwd: Option<String>,
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
enum InterfaceCommand {
    #[command(about = "Show this Agent's registered interface")]
    Show,
    #[command(about = "Register or refresh this Agent's interface")]
    Register {
        #[arg(long)]
        port: u16,
        #[arg(long)]
        instance_id: String,
        #[arg(long = "capability")]
        capabilities: Vec<String>,
        #[arg(long)]
        ui_path: Option<String>,
    },
    #[command(about = "Remove this Agent's interface registration")]
    Clear,
}

#[derive(Debug, Subcommand)]
enum NetworkCommand {
    #[command(about = "Bridge stdin/stdout to a host through the Agent's Treer network")]
    Connect { host: String, port: u16 },
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
    fn new(base: Url, _workspace: &str) -> Self {
        let source_agent_id = env::var("TREER_AGENT_ID").ok();
        let workload_credential = env::var("TREER_WORKLOAD_CREDENTIAL").ok();
        let operator_credential = if source_agent_id.is_none() && workload_credential.is_none() {
            env::var("TREER_OPERATOR_CREDENTIAL").ok()
        } else {
            None
        };
        Self {
            http: reqwest::Client::builder()
                .no_proxy()
                .build()
                .expect("local API client"),
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
        let bytes = response
            .bytes()
            .await
            .context("failed to read API response")?;
        if !status.is_success() {
            let failure = serde_json::from_slice::<ApiError>(&bytes)
                .map(|error| CliFailure {
                    code: error.error.code,
                    message: error.error.message,
                })
                .unwrap_or_else(|_| CliFailure {
                    code: "invalid_api_error".to_string(),
                    message: format!("Treer API returned {status} without a valid JSON error"),
                });
            return Err(failure.into());
        }
        let value: Value = serde_json::from_slice(&bytes)
            .with_context(|| format!("Treer API returned {status} with invalid JSON"))?;
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

pub async fn maybe_execute() -> anyhow::Result<bool> {
    if env::args().nth(1).as_deref() != Some("--execute-cli") {
        return Ok(false);
    }
    execute().await;
    Ok(true)
}

async fn execute() {
    let result = run_cli().await;
    if let Err(error) = result {
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
    let raw = env::args().skip(2).collect::<Vec<_>>();
    let args = match Args::try_parse_from(std::iter::once("treer".to_owned()).chain(raw)) {
        Ok(args) => args,
        Err(error) => {
            let code = error.exit_code();
            error.print()?;
            std::process::exit(code);
        }
    };
    if let Some(skill) = args.skill {
        match skill.as_str() {
            "treer" => print!("{SKILL}"),
            "install" | "treer-install" => print!("{INSTALL_SKILL}"),
            "macos-container" | "macos_container" | "treer-macos-container" => {
                print!("{MACOS_CONTAINER_SKILL}")
            }
            other => {
                bail!("unknown skill {other}; available skills: treer, install, macos-container")
            }
        }
        return Ok(());
    }
    let command = args
        .command
        .context("a command is required; run `treer --help` for usage")?;
    if matches!(
        &command,
        Command::Message {
            command: MessageCommand::Send {
                body_file: Some(path),
                ..
            } | MessageCommand::Reply {
                body_file: Some(path),
                ..
            } | MessageCommand::Import {
                body_file: Some(path),
                ..
            }
        } if path.as_os_str() != "-"
    ) {
        bail!(
            "client-local files cannot be read by the Controller; pipe the body to stdin instead"
        );
    }
    if let Command::Network {
        command: NetworkCommand::Connect { host, port },
    } = &command
    {
        let _ = (host, port);
        bail!("transparent network connect has been retired; register a service and use treer service forward instead");
    }
    let client = ApiClient::new(
        Url::parse(
            &env::var("TREER_AGENT_SERVER_URL")
                .context("Controller command endpoint is missing")?,
        )?,
        &env::var("TREER_WORKSPACE_ID").context("Controller workspace context is missing")?,
    );
    let value = match command {
        Command::Service { command } => match command {
            ServiceCommand::List => client.value(Method::GET, "api/services", None).await?,
            ServiceCommand::Register {
                name,
                port,
                machine,
                host,
                protocol,
                primary,
            } => {
                let protocol = match protocol.as_str() {
                    "http" => MachineServiceProtocol::Http,
                    "tcp" => MachineServiceProtocol::Tcp,
                    _ => bail!("service protocol must be tcp or http; UDP is retired"),
                };
                let server_id = resolve_service_machine(&client, machine.as_deref()).await?;
                register_service(
                    &client,
                    CreateMachineServiceRequest {
                        name,
                        server_id,
                        target_agent_id: None,
                        primary,
                        target_host: host,
                        target_port: port,
                        protocol,
                    },
                )
                .await?
            }
            ServiceCommand::Forward { .. } => {
                bail!("service forwarding requires the thin client transport")
            }
            ServiceCommand::Publish {
                service,
                access,
                tcp,
            } => {
                if !matches!(access.as_str(), "workspace" | "public") {
                    bail!("access must be workspace or public");
                }
                if tcp {
                    if access != "public" {
                        bail!("raw TCP publication requires explicit --access public");
                    }
                    client
                        .value(
                            Method::POST,
                            &format!("api/services/{}/public-tcp", path_segment(&service)),
                            Some(json!({"access":access})),
                        )
                        .await?
                } else {
                    client
                        .value(
                            Method::POST,
                            "api/publish",
                            Some(json!({"service_id":service,"access":access})),
                        )
                        .await?
                }
            }
            ServiceCommand::Delete { service } => {
                client
                    .value(
                        Method::DELETE,
                        &format!("api/services/{}", path_segment(&service)),
                        None,
                    )
                    .await?
            }
            ServiceCommand::Unpublish { service, tcp } => {
                if tcp {
                    client
                        .value(
                            Method::DELETE,
                            &format!("api/services/{}/public-tcp", path_segment(&service)),
                            None,
                        )
                        .await?
                } else {
                    let value = client.value(Method::GET, "api/publish", None).await?;
                    let ingresses = value
                        .get("ingresses")
                        .and_then(Value::as_array)
                        .context("unexpected ingress list")?;
                    let mut removed = Vec::new();
                    for ingress in ingresses.iter().filter(|ingress| {
                        ingress.get("service_id").and_then(Value::as_str) == Some(service.as_str())
                    }) {
                        let id = ingress
                            .get("ingress_id")
                            .and_then(Value::as_str)
                            .context("ingress identity is missing")?;
                        client
                            .value(
                                Method::DELETE,
                                &format!("api/publish/{}", path_segment(id)),
                                None,
                            )
                            .await?;
                        removed.push(id.to_owned());
                    }
                    json!({"service_id": service, "removed_ingress_ids": removed})
                }
            }
        },
        Command::Exec {
            machine,
            cwd,
            timeout,
            mut command,
        } => {
            let server_id = resolve_service_machine(&client, Some(&machine)).await?;
            let executable = command.remove(0);
            client
                .value(
                    Method::POST,
                    &format!("api/machines/{}/exec", path_segment(&server_id)),
                    Some(serde_json::to_value(MachineExecRequest {
                        cwd,
                        command: executable,
                        args: command,
                        timeout_ms: timeout,
                    })?),
                )
                .await?
        }
        Command::Runtime {
            command: RuntimeCommand::Restore { all: _, server },
        } => {
            client
                .value(
                    Method::POST,
                    "api/runtime/restore",
                    Some(json!({ "server_id": server })),
                )
                .await?
        }
        Command::Agent {
            command: AgentCommand::Address { target, json },
        } => {
            let agent = client.get_agent(&target).await?;
            let services = list_services(&client).await?;
            let service = primary_service(&services, &agent.agent_id)?
                .with_context(|| format!("Agent {} has no primary HTTP service; register one from its runtime with treer service register --primary --protocol http --name NAME --port PORT", agent.name))?;
            let address = format!("http://{}.treer/", service.service_id);
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&json!({
                        "agent_id": agent.agent_id, "service_id": service.service_id, "address": address
                    }))?
                );
            } else {
                println!("{address}");
            }
            return Ok(());
        }
        Command::Agent { command } => run_agent_command(&client, command).await?,
        Command::App { command } => run_app_command(&client, command).await?,
        Command::Member { command } => match command {
            MemberCommand::List => client.value(Method::GET, "api/humans", None).await?,
        },
        Command::Machine { command } => run_machine_command(&client, command).await?,
        Command::Network { command } => run_network_command(&client, command).await?,
        Command::Interface { command } => run_interface_command(&client, command).await?,
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
    let agent_id = client.source_agent_id.clone();
    let server_id = agent_id
        .as_ref()
        .and_then(|_| std::env::var("TREER_SERVER_ID").ok());
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

async fn run_app_command(client: &ApiClient, command: AppCommand) -> anyhow::Result<Value> {
    match command {
        AppCommand::List => client.value(Method::GET, "api/apps", None).await,
        AppCommand::Show { target } => {
            let target = normalize_target(&target)?;
            client
                .value(
                    Method::GET,
                    &format!("api/apps/{}", path_segment(&target)),
                    None,
                )
                .await
        }
        AppCommand::Create {
            machine,
            name,
            cwd,
            port,
            public,
            executable,
            args,
        } => {
            let server_id = resolve_service_machine(client, machine.as_deref()).await?;
            client
                .value(
                    Method::POST,
                    "api/apps",
                    Some(serde_json::to_value(CreateAppDeploymentRequest {
                        server_id: Some(server_id),
                        name,
                        command: executable,
                        args,
                        cwd,
                        port,
                        hostname: String::new(),
                        public,
                    })?),
                )
                .await
        }
        AppCommand::Start { target } => app_action(client, &target, "start").await,
        AppCommand::Stop { target } => app_action(client, &target, "stop").await,
        AppCommand::Restart { target } => app_action(client, &target, "restart").await,
        AppCommand::Delete { target } => {
            let target = normalize_target(&target)?;
            client
                .value(
                    Method::DELETE,
                    &format!("api/apps/{}", path_segment(&target)),
                    None,
                )
                .await
        }
    }
}

async fn app_action(client: &ApiClient, target: &str, action: &str) -> anyhow::Result<Value> {
    let target = normalize_target(target)?;
    client
        .value(
            Method::POST,
            &format!("api/apps/{}/{}", path_segment(&target), action),
            Some(json!({})),
        )
        .await
}

async fn run_agent_command(client: &ApiClient, command: AgentCommand) -> anyhow::Result<Value> {
    match command {
        AgentCommand::Address { .. } => {
            unreachable!("address output is handled before JSON rendering")
        }
        AgentCommand::Mkdir { path, parents } => workload_tree::mkdir(client, &path, parents).await,
        AgentCommand::Ls { path, recursive } => workload_tree::ls(client, &path, recursive).await,
        AgentCommand::Mv {
            source,
            destination,
        } => workload_tree::mv(client, &source, &destination).await,
        AgentCommand::Rmdir { path } => workload_tree::rmdir(client, &path).await,
        AgentCommand::Start { target } => lifecycle_agent(client, &target, "start").await,
        AgentCommand::Stop { target } => stop_agent(client, &target).await,
        AgentCommand::Restart { target } => lifecycle_agent(client, &target, "restart").await,
        AgentCommand::List => client.value(Method::GET, "api/agents", None).await,
        AgentCommand::Show { target } => {
            Ok(serde_json::to_value(client.get_agent(&target).await?)?)
        }
        AgentCommand::Attach { .. } => {
            bail!("terminal attach requires the thin client session transport")
        }
        AgentCommand::Prompt { target, text, wait } => {
            prompt_and_maybe_wait(client, &target, text, wait).await
        }
        AgentCommand::Read { target, lines } => read_agent(client, &target, lines).await,
        AgentCommand::Transcript {
            target,
            page,
            cursor,
            limit,
        } => read_agent_transcript(client, &target, page, cursor, limit).await,
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
        AgentCommand::Startup { command } => match command {
            AgentStartupCommand::Set {
                cwd,
                publish_ports,
                mut command,
            } => {
                if !publish_ports.is_empty() {
                    bail!("sandbox port publication is retired; register process ports with treer service register");
                }
                let executable = command.remove(0);
                client
                    .value(
                        Method::PUT,
                        "api/agent/startup",
                        Some(serde_json::to_value(SetAgentStartupRequest {
                            cwd,
                            command: executable,
                            args: command,
                            publish_ports,
                        })?),
                    )
                    .await
            }
            AgentStartupCommand::Show => client.value(Method::GET, "api/agent/startup", None).await,
            AgentStartupCommand::Clear => {
                client
                    .value(Method::DELETE, "api/agent/startup", None)
                    .await
            }
        },
        AgentCommand::Admin { command } => run_agent_admin_command(client, command).await,
    }
}

async fn run_agent_admin_command(
    client: &ApiClient,
    command: AgentAdminCommand,
) -> anyhow::Result<Value> {
    match command {
        AgentAdminCommand::Start { target } => lifecycle_agent(client, &target, "start").await,
        AgentAdminCommand::Restart { target } => lifecycle_agent(client, &target, "restart").await,
        AgentAdminCommand::Create {
            machine,
            kind,
            name,
            cwd,
            publish_ports,
            recipe,
            args,
        } => {
            if recipe.is_some() {
                bail!("automatic recipe runtimes are retired; launch an explicit installer command or profile and supply the install skill");
            }
            if !publish_ports.is_empty() {
                bail!("sandbox port publication is retired; register process ports with treer service register");
            }
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
                        recipe,
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

async fn lifecycle_agent(client: &ApiClient, target: &str, action: &str) -> anyhow::Result<Value> {
    let target = normalize_target(target)?;
    client
        .value(
            Method::POST,
            &format!("api/agents/{}/{action}", path_segment(&target)),
            Some(json!({})),
        )
        .await
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
            let messages =
                read_legacy_import(body_file.as_deref().unwrap_or_else(|| Path::new("-")))?;
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
        (None, None) => bail!("message body is required through --body or piped stdin"),
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

fn read_legacy_import(path: &Path) -> anyhow::Result<Vec<LegacyMailMessage>> {
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

fn read_text_file_or_stdin(path: &Path) -> anyhow::Result<String> {
    if path.as_os_str() == "-" {
        read_stdin_text()
    } else {
        bail!("client-local files cannot be read by the Controller; pipe the contents to stdin and use '-' instead")
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
            cwd,
        } => {
            let target = normalize_target(&target)?;
            let server_id = match machine {
                Some(machine) => Some(resolve_machine(client, &machine).await?.server_id),
                None => None,
            };
            client
                .value(
                    Method::POST,
                    &format!("api/launch-profiles/{}/launch", path_segment(&target)),
                    Some(serde_json::to_value(LaunchAgentProfileRequest {
                        server_id,
                        agent_name: name,
                        cwd,
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

async fn run_network_command(
    _client: &ApiClient,
    command: NetworkCommand,
) -> anyhow::Result<Value> {
    match command {
        NetworkCommand::Connect { .. } => {
            bail!("network connect must run before API client initialization")
        }
    }
}

async fn run_interface_command(
    client: &ApiClient,
    command: InterfaceCommand,
) -> anyhow::Result<Value> {
    match command {
        InterfaceCommand::Show => client.value(Method::GET, "api/interface", None).await,
        InterfaceCommand::Register {
            port,
            instance_id,
            capabilities,
            ui_path,
        } => {
            client
                .value(
                    Method::PUT,
                    "api/interface",
                    Some(serde_json::to_value(RegisterAgentInterfaceRequest {
                        protocol: AGENT_INTERFACE_PROTOCOL_V1.to_string(),
                        instance_id,
                        port,
                        capabilities,
                        ui_path,
                    })?),
                )
                .await
        }
        InterfaceCommand::Clear => client.value(Method::DELETE, "api/interface", None).await,
    }
}

async fn register_service(
    client: &ApiClient,
    request: CreateMachineServiceRequest,
) -> anyhow::Result<Value> {
    if request.primary {
        if request.protocol != MachineServiceProtocol::Http {
            bail!("a primary endpoint requires --protocol http");
        }
        let agent_id = client
            .source_agent_id
            .as_deref()
            .context("--primary requires a managed Agent identity")?;
        let services = list_services(client).await?;
        let existing = primary_service(&services, agent_id)?.or_else(|| {
            services.iter().find(|service| {
                service.target_agent_id.as_deref() == Some(agent_id)
                    && service.name.eq_ignore_ascii_case(request.name.trim())
            })
        });
        if let Some(service) = existing {
            if service.server_id != request.server_id {
                bail!("an Agent's primary service cannot be moved to another machine");
            }
            // Reuse service.update ownership/Policy checks and preserve the ID.
            return client
                .value(
                    Method::PATCH,
                    &format!("api/services/{}", path_segment(&service.service_id)),
                    Some(
                        json!({"name": request.name, "target_host": request.target_host,
                    "target_port": request.target_port, "protocol": "http", "primary": true}),
                    ),
                )
                .await;
        }
    }
    client
        .value(
            Method::POST,
            "api/services",
            Some(serde_json::to_value(request)?),
        )
        .await
}

async fn list_services(client: &ApiClient) -> anyhow::Result<Vec<MachineService>> {
    #[derive(serde::Deserialize)]
    struct Services {
        services: Vec<MachineService>,
    }
    Ok(client
        .request::<Services>(Method::GET, "api/services", None)
        .await?
        .services)
}

fn primary_service<'a>(
    services: &'a [MachineService],
    agent_id: &str,
) -> anyhow::Result<Option<&'a MachineService>> {
    let mut matches = services
        .iter()
        .filter(|service| service.primary && service.target_agent_id.as_deref() == Some(agent_id));
    let Some(service) = matches.next() else {
        return Ok(None);
    };
    if matches.next().is_some() || service.protocol != MachineServiceProtocol::Http {
        bail!("Agent {agent_id} has an invalid primary HTTP service binding");
    }
    Ok(Some(service))
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

async fn read_agent_transcript(
    client: &ApiClient,
    target: &str,
    page: Option<u32>,
    cursor: Option<String>,
    limit: usize,
) -> anyhow::Result<Value> {
    let target = normalize_target(target)?;
    let mut path = format!(
        "api/agents/{}/transcript?limit={}",
        path_segment(&target),
        limit.min(1000)
    );
    let page = page
        .map(|value| value.to_string())
        .or_else(|| cursor.filter(|value| !value.is_empty()));
    if let Some(page) = page {
        path.push_str("&page=");
        path.push_str(&path_segment(&page));
        path.push_str("&cursor=");
        path.push_str(&path_segment(&page));
    }
    client.value(Method::GET, &path, None).await
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
                "available_agents": ["claude"],
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
    fn skill_flags_work_without_a_subcommand() {
        for flag in ["--skill", "--skills"] {
            let args = Args::try_parse_from(["treer", flag]).expect("skill flag should parse");
            assert_eq!(args.skill.as_deref(), Some("treer"));
            assert!(args.command.is_none());
        }
        let install = Args::try_parse_from(["treer", "--skill", "install"])
            .expect("install skill should parse");
        assert_eq!(install.skill.as_deref(), Some("install"));
        let macos = Args::try_parse_from(["treer", "--skill", "macos-container"])
            .expect("macos-container skill should parse");
        assert_eq!(macos.skill.as_deref(), Some("macos-container"));
        assert!(SKILL.starts_with("---\nname: treer\n"));
        assert!(INSTALL_SKILL.starts_with("---\nname: treer-install\n"));
        assert!(MACOS_CONTAINER_SKILL.starts_with("---\nname: treer-macos-container\n"));
        assert!(!SKILL.contains("TODO"));
        assert!(!INSTALL_SKILL.contains("TODO"));
        assert!(!MACOS_CONTAINER_SKILL.contains("TODO"));
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
    fn machine_exec_command_preserves_argv() {
        let args = Args::try_parse_from([
            "treer",
            "exec",
            "--machine",
            "builder",
            "--cwd",
            "repo",
            "--timeout",
            "1500",
            "--",
            "sh",
            "-lc",
            "printf 'ok'",
        ])
        .expect("machine exec should parse");
        assert!(matches!(
            args.command,
            Some(Command::Exec {
                machine,
                cwd,
                timeout: 1500,
                command,
            }) if machine == "builder"
                && cwd == "repo"
                && command == ["sh", "-lc", "printf 'ok'"]
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
            "--cwd",
            "packages/api",
        ])
        .expect("profile launch should parse");
        assert!(matches!(
            launch.command,
            Some(Command::Agent {
                command: AgentCommand::Admin {
                    command: AgentAdminCommand::Profile {
                        command: ProfileCommand::Launch { target, machine, name, cwd }
                    }
                }
            }) if target == "reviewer"
                && machine.as_deref() == Some("builder")
                && name.as_deref() == Some("review-42")
                && cwd.as_deref() == Some("packages/api")
        ));
        assert!(Args::try_parse_from(["treer", "profile", "list"]).is_err());
    }

    #[test]
    fn network_publication_commands_are_not_exposed() {
        for args in [
            vec!["treer", "network", "service", "list"],
            vec![
                "treer", "network", "service", "create", "api", "--agent", "self", "--port", "8080",
            ],
            vec!["treer", "network", "host", "list"],
            vec!["treer", "network", "host", "create", "api.internal", "api"],
            vec!["treer", "network", "publish", "list"],
        ] {
            assert!(
                Args::try_parse_from(args).is_err(),
                "Agent CLI must not expose service publication commands"
            );
        }
    }

    #[test]
    fn network_connect_command_parses() {
        let args = Args::try_parse_from(["treer", "network", "connect", "git.internal", "9418"])
            .expect("network connect should parse");
        assert!(matches!(
            args.command,
            Some(Command::Network {
                command: NetworkCommand::Connect { host, port }
            }) if host == "git.internal" && port == 9418
        ));
    }

    #[test]
    fn agent_interface_commands_parse() {
        let register = Args::try_parse_from([
            "treer",
            "interface",
            "register",
            "--port",
            "4180",
            "--instance-id",
            "pi-one",
            "--capability",
            "prompt.submit",
            "--capability",
            "transcript.read",
            "--ui-path",
            "/",
        ])
        .expect("Agent Interface registration should parse");
        assert!(matches!(
            register.command,
            Some(Command::Interface {
                command: InterfaceCommand::Register {
                    port: 4180,
                    instance_id,
                    capabilities,
                    ui_path: Some(ui_path),
                }
            }) if instance_id == "pi-one"
                && capabilities == ["prompt.submit", "transcript.read"]
                && ui_path == "/"
        ));

        let transcript = Args::try_parse_from([
            "treer",
            "agent",
            "transcript",
            "reviewer",
            "--page",
            "2",
            "--cursor",
            "10",
            "--limit",
            "25",
        ])
        .expect("Agent transcript should parse");
        assert!(matches!(
            transcript.command,
            Some(Command::Agent {
                command: AgentCommand::Transcript {
                    target,
                    page: Some(2),
                    cursor: Some(cursor),
                    limit: 25,
                }
            }) if target == "reviewer" && cursor == "10"
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
        for action in ["start", "stop", "restart"] {
            Args::try_parse_from(["treer", "agent", action, "workload"])
                .expect("unified lifecycle parses");
            Args::try_parse_from(["treer", "agent", "admin", action, "workload"])
                .expect("admin compatibility parses");
        }
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

        let recipe = Args::try_parse_from([
            "treer",
            "agent",
            "admin",
            "create",
            "--machine",
            "builder",
            "--kind",
            "auto",
            "--name",
            "installer",
            "--recipe",
            "https://github.com/example/recipe.git",
        ])
        .expect("agent admin create --recipe should parse");
        assert!(matches!(
            recipe.command,
            Some(Command::Agent {
                command: AgentCommand::Admin {
                    command: AgentAdminCommand::Create {
                        recipe: Some(url),
                        ..
                    }
                }
            }) if url == "https://github.com/example/recipe.git"
        ));
    }

    #[test]
    fn agent_startup_command_preserves_argv() {
        let args = Args::try_parse_from([
            "treer",
            "agent",
            "startup",
            "set",
            "--cwd",
            "repo",
            "--publish",
            "4180",
            "--",
            "sh",
            "-lc",
            "exec ./agent --serve",
        ])
        .expect("Agent startup command should parse");
        assert!(matches!(
            args.command,
            Some(Command::Agent {
                command: AgentCommand::Startup {
                    command: AgentStartupCommand::Set {
                        cwd,
                        publish_ports,
                        command,
                    }
                }
            }) if cwd == "repo"
                && publish_ports == [4180]
                && command == ["sh", "-lc", "exec ./agent --serve"]
        ));
    }

    #[test]
    fn managed_app_commands_parse() {
        let create = Args::try_parse_from([
            "treer",
            "app",
            "create",
            "--machine",
            "builder",
            "--name",
            "soul",
            "--port",
            "9420",
            "--public",
            "python3",
            "--",
            "apps/soul/soul.py",
        ])
        .expect("App create should parse");
        assert!(matches!(
            create.command,
            Some(Command::App {
                command: AppCommand::Create {
                    machine: Some(machine),
                    name,
                    port: 9420,
                    public: true,
                    executable,
                    args,
                    ..
                }
            }) if machine == "builder"
                && name == "soul"
                && executable == "python3"
                && args == ["apps/soul/soul.py"]
        ));

        let restart = Args::try_parse_from(["treer", "app", "restart", "soul"])
            .expect("App restart should parse");
        assert!(matches!(
            restart.command,
            Some(Command::App {
                command: AppCommand::Restart { target }
            }) if target == "soul"
        ));
    }
}
