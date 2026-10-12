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
    ReplaceAgentStartupRequest, SendMessageRequest, ServerInfo, SetAgentStartupRequest,
    UpdateAgentLaunchProfileRequest, WorkloadIdentityTokenRequest, WorkloadIdentityTokenResponse,
    WorkspaceSnapshot, AGENT_ID_HEADER, AGENT_INTERFACE_PROTOCOL_V1, INSTALL_SKILL,
    OPERATOR_CREDENTIAL_HEADER, WORKLOAD_CREDENTIAL_HEADER,
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
    if let Some(message) = removed_command(&raw) {
        return Err(CliFailure {
            code: "command_removed".into(),
            message: message.into(),
        }
        .into());
    }
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

fn removed_command(argv: &[String]) -> Option<&'static str> {
    (argv.first().map(String::as_str) == Some("agent")
        && argv.get(1).map(String::as_str) == Some("list"))
    .then_some("treer agent list was removed; use treer agent ls -R / to list the workspace tree")
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

mod arguments;
use arguments::*;
mod apps;
use apps::*;
mod agents;
use agents::*;
mod messages;
use messages::*;
mod profiles;
use profiles::*;
mod machines;
use machines::*;
mod services;
use services::*;
mod agent_io;
use agent_io::*;
mod machine_targets;
use machine_targets::*;
#[cfg(test)]
#[path = "cli_command/tests.rs"]
mod tests;
