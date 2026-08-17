use std::time::{Duration, Instant};

use anyhow::{bail, Context};
use clap::{Parser, Subcommand, ValueEnum};
use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
use reqwest::Method;
use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use treer_protocol::{
    AgentInfo, AgentStatus, CreateAgentRequest, InputAgentRequest, RenameRequest,
};
use url::Url;

const WAIT_POLL_INTERVAL: Duration = Duration::from_millis(150);
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
    #[arg(
        long,
        env = "TREER_AGENT_SERVER_URL",
        default_value = "http://127.0.0.1:8790"
    )]
    url: Url,
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
}

impl ApiClient {
    fn new(base: Url) -> Self {
        Self {
            http: reqwest::Client::new(),
            base,
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
    let client = ApiClient::new(args.url);
    let value = match command {
        Command::Agent { command } => run_agent_command(&client, command).await?,
        Command::Machine { command } => run_machine_command(&client, command).await?,
        Command::Whoami => {
            let agent_id = std::env::var("TREER_AGENT_ID").context(
                "TREER_AGENT_ID is not set; `treer whoami` must run inside a managed agent",
            )?;
            serde_json::to_value(client.get_agent(&agent_id).await?)?
        }
        Command::Discover => client.value(Method::GET, "api/discovery", None).await?,
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
        Command::Stop { target } => stop_agent(&client, &target).await?,
    };
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}

async fn run_agent_command(client: &ApiClient, command: AgentCommand) -> anyhow::Result<Value> {
    match command {
        AgentCommand::List => client.value(Method::GET, "api/agents", None).await,
        AgentCommand::Get { target } => Ok(serde_json::to_value(client.get_agent(&target).await?)?),
        AgentCommand::Rename { target, name } => rename_agent(client, &target, name).await,
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

async fn rename_machine(client: &ApiClient, target: &str, name: String) -> anyhow::Result<Value> {
    let target = if matches!(target, "self" | ".") {
        std::env::var("TREER_SERVER_ID")
            .context("self target requires TREER_SERVER_ID inside a managed agent")?
    } else {
        target.to_string()
    };
    client
        .value(
            Method::PATCH,
            &format!("api/machines/{}", path_segment(&target)),
            Some(serde_json::to_value(RenameRequest { name })?),
        )
        .await
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
    fn skill_flags_work_without_a_subcommand() {
        for flag in ["--skill", "--skills"] {
            let args = Args::try_parse_from(["treer", flag]).expect("skill flag should parse");
            assert!(args.skill);
            assert!(args.command.is_none());
        }
        assert!(SKILL.starts_with("---\nname: treer\n"));
        assert!(!SKILL.contains("TODO"));
    }
}
