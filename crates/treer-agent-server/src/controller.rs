use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq;
use tokio::sync::broadcast;
use tracing::warn;
use treer_host_protocol::{
    HostCommand, HostOutputChunk, HostOutputReplay, HostProcessInfo, HostResponse,
    HostSpawnRequest, HostWrite,
};
use treer_protocol::{
    AgentInfo, AgentStatus, CreateAgentRequest, ProtocolError, ReadAgentOutputResponse,
    VirtualNetworkHostsSnapshot,
};
#[cfg(test)]
use uuid::Uuid;

use crate::host_client::{HostClient, HostEvents};

const OUTPUT_LIMIT_BYTES: usize = 512 * 1024;
const QUIET_IDLE_AFTER: Duration = Duration::from_millis(900);
const OUTPUT_METADATA_INTERVAL: Duration = Duration::from_millis(150);
const PROMPT_SUBMIT_DELAY: Duration = Duration::from_millis(300);
const AGENT_COMMAND_DELAY: Duration = Duration::from_millis(500);
const CLAUDE_TRUST_CONFIRM_DELAY: Duration = Duration::from_millis(1_500);

struct AgentLaunch {
    command: String,
    args: Vec<String>,
    initial_writes: Vec<HostWrite>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AgentMetadata {
    agent_id: String,
    workspace_id: String,
    server_id: String,
    kind: String,
    name: String,
    cwd: String,
    #[serde(default)]
    workload_credential: String,
}

#[derive(Clone)]
pub struct ControllerRuntime {
    inner: Arc<ControllerInner>,
}

pub struct ControllerConfig {
    pub workspace_id: String,
    pub server_id: String,
    pub agent_server_url: String,
    pub network_proxy_url: String,
    pub treer_binary: Option<PathBuf>,
    pub sandbox_executable: Option<PathBuf>,
}

struct ControllerInner {
    host: HostClient,
    workspace_id: String,
    server_id: String,
    agent_server_url: String,
    network_proxy_url: String,
    treer_binary: Option<PathBuf>,
    sandbox_executable: Option<PathBuf>,
    agents: RwLock<HashMap<String, Arc<Mutex<ControllerAgent>>>>,
    events: broadcast::Sender<AgentInfo>,
    terminal_events: broadcast::Sender<TerminalOutput>,
    process_events: broadcast::Sender<HostProcessInfo>,
    virtual_hosts: RwLock<Option<VirtualNetworkHostsSnapshot>>,
}

struct ControllerAgent {
    info: AgentInfo,
    workload_credential: String,
    text: String,
    bracketed_paste: bool,
    last_output: Instant,
    last_metadata_event: Instant,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalOutput {
    pub process_id: String,
    pub revision: u64,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalSnapshot {
    pub revision: u64,
    pub data: Vec<u8>,
}

impl ControllerRuntime {
    pub fn from_sync(
        host: HostClient,
        sync: HostResponse,
        events: HostEvents,
        config: ControllerConfig,
    ) -> Result<(Self, tokio::sync::watch::Receiver<bool>), ProtocolError> {
        let HostResponse::Synced {
            processes, replay, ..
        } = sync
        else {
            return Err(ProtocolError::new(
                "host_protocol_error",
                "expected sync response",
            ));
        };
        let (agent_events, _) = broadcast::channel(512);
        let (terminal_events, _) = broadcast::channel(2048);
        let (process_events, _) = broadcast::channel(512);
        let runtime = Self {
            inner: Arc::new(ControllerInner {
                host,
                workspace_id: config.workspace_id,
                server_id: config.server_id,
                agent_server_url: config.agent_server_url,
                network_proxy_url: config.network_proxy_url,
                treer_binary: config.treer_binary,
                sandbox_executable: config.sandbox_executable,
                agents: RwLock::new(HashMap::new()),
                events: agent_events,
                terminal_events,
                process_events,
                virtual_hosts: RwLock::new(None),
            }),
        };
        let replays: HashMap<_, _> = replay
            .into_iter()
            .map(|replay| (replay.process_id.clone(), replay))
            .collect();
        for mut process in processes {
            let Some(replay) = replays.get(&process.process_id) else {
                continue;
            };
            process.stream_epoch.clone_from(&replay.stream_epoch);
            process.next_revision = replay.next_revision;
            runtime.restore_process(process, replay)?;
        }
        let disconnected = events.disconnected.clone();
        runtime.start_event_tasks(events);
        runtime.start_idle_monitor();
        Ok((runtime, disconnected))
    }

    pub fn subscribe(&self) -> broadcast::Receiver<AgentInfo> {
        self.inner.events.subscribe()
    }

    pub fn subscribe_terminal(&self) -> broadcast::Receiver<TerminalOutput> {
        self.inner.terminal_events.subscribe()
    }

    pub fn subscribe_processes(&self) -> broadcast::Receiver<HostProcessInfo> {
        self.inner.process_events.subscribe()
    }

    pub fn replace_virtual_hosts(
        &self,
        snapshot: VirtualNetworkHostsSnapshot,
    ) -> Result<bool, ProtocolError> {
        if snapshot.workspace_id != self.inner.workspace_id {
            return Err(ProtocolError::new(
                "workspace_mismatch",
                "virtual-host snapshot belongs to another workspace",
            ));
        }
        let mut current =
            self.inner.virtual_hosts.write().map_err(|_| {
                ProtocolError::new("state_error", "virtual-host cache lock poisoned")
            })?;
        if !should_replace_virtual_hosts(current.as_ref(), &snapshot) {
            return Ok(false);
        }
        *current = Some(snapshot);
        Ok(true)
    }

    pub fn reset_virtual_hosts(&self) -> Result<(), ProtocolError> {
        *self
            .inner
            .virtual_hosts
            .write()
            .map_err(|_| ProtocolError::new("state_error", "virtual-host cache lock poisoned"))? =
            None;
        Ok(())
    }

    pub fn list(&self) -> Vec<AgentInfo> {
        let Ok(agents) = self.inner.agents.read() else {
            return Vec::new();
        };
        let mut result: Vec<_> = agents
            .values()
            .filter_map(|agent| agent.lock().ok().map(|agent| agent.info.clone()))
            .collect();
        result.sort_by(|left, right| left.agent_id.cmp(&right.agent_id));
        result
    }

    pub async fn create(
        &self,
        operation_id: &str,
        agent_id: String,
        workload_credential: String,
        request: CreateAgentRequest,
    ) -> Result<AgentInfo, ProtocolError> {
        if request.name.trim().is_empty() {
            return Err(ProtocolError::new(
                "invalid_request",
                "agent name cannot be empty",
            ));
        }
        let launch = resolve_launch(&request)?;
        if !valid_workload_credential(&workload_credential) {
            return Err(ProtocolError::new(
                "invalid_workload_credential",
                "Proxy supplied an invalid workload credential",
            ));
        }
        let metadata = AgentMetadata {
            agent_id: agent_id.clone(),
            workspace_id: self.inner.workspace_id.clone(),
            server_id: self.inner.server_id.clone(),
            kind: request.kind,
            name: request.name,
            cwd: request.cwd.clone(),
            workload_credential: workload_credential.clone(),
        };
        let env = self.process_environment(Some((&agent_id, &workload_credential)));
        let launch = sandbox_launch(
            self.inner.sandbox_executable.as_deref(),
            &agent_network_proxy_url(&self.inner.network_proxy_url, &agent_id),
            launch,
        );
        let response = self
            .inner
            .host
            .request(
                HostCommand::Spawn {
                    request: HostSpawnRequest {
                        process_id: agent_id,
                        command: launch.command,
                        args: launch.args,
                        cwd: request.cwd,
                        env,
                        cols: request.cols,
                        rows: request.rows,
                        metadata: serde_json::to_string(&metadata)
                            .map_err(|error| protocol_error("metadata_error", error))?,
                    },
                },
                Some(operation_id.to_string()),
            )
            .await
            .map_err(|error| protocol_error("host_error", error))?;
        let HostResponse::Process { process } = response else {
            return Err(ProtocolError::new(
                "host_protocol_error",
                "spawn returned an unexpected response",
            ));
        };
        let agent = if let Ok(agent) = self.get(&process.process_id) {
            agent
                .lock()
                .map(|agent| agent.info.clone())
                .map_err(|_| ProtocolError::new("state_error", "agent state lock poisoned"))?
        } else {
            self.upsert_process(process, None)?
        };
        if launch.initial_writes.is_empty() {
            return Ok(agent);
        }
        let response = self
            .inner
            .host
            .request(
                HostCommand::Write {
                    process_id: agent.agent_id.clone(),
                    writes: launch.initial_writes,
                },
                Some(format!("{operation_id}:launch")),
            )
            .await
            .map_err(|error| protocol_error("host_error", error))?;
        self.process_response(response, AgentStatus::Working)
    }

    fn process_environment(&self, agent: Option<(&str, &str)>) -> BTreeMap<String, String> {
        let network_proxy_url = agent.map_or_else(
            || self.inner.network_proxy_url.clone(),
            |(agent_id, _)| agent_network_proxy_url(&self.inner.network_proxy_url, agent_id),
        );
        let mut env = BTreeMap::from([
            (
                "TREER_WORKSPACE_ID".to_string(),
                self.inner.workspace_id.clone(),
            ),
            ("TREER_SERVER_ID".to_string(), self.inner.server_id.clone()),
            (
                "TREER_AGENT_SERVER_URL".to_string(),
                self.inner.agent_server_url.clone(),
            ),
        ]);
        env.extend(network_environment(
            network_proxy_url,
            self.inner.sandbox_executable.is_some(),
        ));
        if let Some((agent_id, workload_credential)) = agent {
            env.insert("TREER_AGENT_ID".to_string(), agent_id.to_string());
            env.insert(
                "TREER_WORKLOAD_CREDENTIAL".to_string(),
                workload_credential.to_string(),
            );
        }
        if let Some(treer_binary) = &self.inner.treer_binary {
            env.insert("TREER_BIN".to_string(), treer_binary.display().to_string());
            if let Some(parent) = treer_binary.parent() {
                let mut paths = vec![parent.to_path_buf()];
                if let Some(current_path) = std::env::var_os("PATH") {
                    paths.extend(std::env::split_paths(&current_path));
                }
                if let Ok(path) = std::env::join_paths(paths) {
                    env.insert("PATH".to_string(), path.to_string_lossy().into_owned());
                }
            }
        }
        env
    }

    pub fn authenticate_agent(
        &self,
        agent_id: &str,
        workload_credential: &str,
    ) -> Result<AgentInfo, ProtocolError> {
        let agents = self
            .inner
            .agents
            .read()
            .map_err(|_| ProtocolError::new("state_error", "agent registry lock poisoned"))?;
        let agent = agents
            .get(agent_id)
            .ok_or_else(|| ProtocolError::new("agent_not_found", agent_id))?
            .lock()
            .map_err(|_| ProtocolError::new("state_error", "agent lock poisoned"))?;
        if !workload_credential_matches(&agent.workload_credential, workload_credential) {
            return Err(ProtocolError::new(
                "invalid_workload_credential",
                "workload credential does not match the managed agent",
            ));
        }
        Ok(agent.info.clone())
    }

    pub async fn prompt(
        &self,
        operation_id: &str,
        agent_id: &str,
        text: &str,
    ) -> Result<AgentInfo, ProtocolError> {
        if text.is_empty() {
            return Err(ProtocolError::new(
                "invalid_request",
                "agent prompt cannot be empty",
            ));
        }
        let bracketed = self
            .get(agent_id)?
            .lock()
            .map_err(|_| ProtocolError::new("state_error", "agent state lock poisoned"))?
            .bracketed_paste;
        let response = self
            .inner
            .host
            .request(
                HostCommand::Write {
                    process_id: agent_id.to_string(),
                    writes: vec![
                        HostWrite {
                            data: encode_prompt_text(text, bracketed),
                            delay_ms: 0,
                        },
                        HostWrite {
                            data: vec![b'\r'],
                            delay_ms: PROMPT_SUBMIT_DELAY.as_millis() as u64,
                        },
                    ],
                },
                Some(operation_id.to_string()),
            )
            .await
            .map_err(|error| protocol_error("host_error", error))?;
        self.process_response(response, AgentStatus::Working)
    }

    pub async fn write_raw(
        &self,
        operation_id: &str,
        agent_id: &str,
        data: &[u8],
    ) -> Result<AgentInfo, ProtocolError> {
        let response = self
            .inner
            .host
            .request(
                HostCommand::Write {
                    process_id: agent_id.to_string(),
                    writes: vec![HostWrite {
                        data: data.to_vec(),
                        delay_ms: 0,
                    }],
                },
                Some(operation_id.to_string()),
            )
            .await
            .map_err(|error| protocol_error("host_error", error))?;
        self.process_response(response, AgentStatus::Working)
    }

    pub fn read(
        &self,
        agent_id: &str,
        lines: Option<usize>,
    ) -> Result<ReadAgentOutputResponse, ProtocolError> {
        let agent = self.get(agent_id)?;
        let agent = agent
            .lock()
            .map_err(|_| ProtocolError::new("state_error", "agent state lock poisoned"))?;
        let text = select_lines(&agent.text, lines);
        Ok(ReadAgentOutputResponse {
            agent_id: agent_id.to_string(),
            revision: agent.info.output_revision,
            text,
            truncated: agent.text.len() >= OUTPUT_LIMIT_BYTES,
        })
    }

    pub async fn terminal_snapshot(
        &self,
        agent_id: &str,
    ) -> Result<TerminalSnapshot, ProtocolError> {
        let response = self
            .inner
            .host
            .request(
                HostCommand::Read {
                    process_id: agent_id.to_string(),
                    cursor: None,
                },
                None,
            )
            .await
            .map_err(|error| protocol_error("host_error", error))?;
        let HostResponse::Output { replay } = response else {
            return Err(ProtocolError::new(
                "host_protocol_error",
                "read returned an unexpected response",
            ));
        };
        Ok(TerminalSnapshot {
            revision: replay.next_revision.saturating_sub(1),
            data: decode_replay(&replay)?,
        })
    }

    pub async fn resize(
        &self,
        operation_id: &str,
        agent_id: &str,
        cols: u16,
        rows: u16,
    ) -> Result<(), ProtocolError> {
        self.inner
            .host
            .request(
                HostCommand::Resize {
                    process_id: agent_id.to_string(),
                    cols,
                    rows,
                },
                Some(operation_id.to_string()),
            )
            .await
            .map_err(|error| protocol_error("host_error", error))?;
        Ok(())
    }

    pub async fn stop(
        &self,
        operation_id: &str,
        agent_id: &str,
    ) -> Result<AgentInfo, ProtocolError> {
        let response = self
            .inner
            .host
            .request(
                HostCommand::Stop {
                    process_id: agent_id.to_string(),
                },
                Some(operation_id.to_string()),
            )
            .await
            .map_err(|error| protocol_error("host_error", error))?;
        self.process_response(response, AgentStatus::Exited)
    }

    fn restore_process(
        &self,
        process: HostProcessInfo,
        replay: &HostOutputReplay,
    ) -> Result<(), ProtocolError> {
        let text = plain_text(replay)?;
        self.upsert_process(process, Some(text)).map(|_| ())
    }

    fn upsert_process(
        &self,
        process: HostProcessInfo,
        restored_text: Option<String>,
    ) -> Result<AgentInfo, ProtocolError> {
        let metadata: AgentMetadata = serde_json::from_str(&process.metadata)
            .map_err(|error| protocol_error("invalid_host_metadata", error))?;
        if metadata.workspace_id != self.inner.workspace_id
            || metadata.server_id != self.inner.server_id
        {
            return Err(ProtocolError::new(
                "host_identity_mismatch",
                "host process metadata belongs to another controller",
            ));
        }
        let text = restored_text.unwrap_or_default();
        let status = if process.running {
            detect_status(&text).unwrap_or_else(|| {
                if Utc::now()
                    .signed_duration_since(process.last_output_at)
                    .num_milliseconds()
                    >= QUIET_IDLE_AFTER.as_millis() as i64
                {
                    AgentStatus::Idle
                } else {
                    AgentStatus::Working
                }
            })
        } else {
            AgentStatus::Exited
        };
        let info = AgentInfo {
            agent_id: metadata.agent_id.clone(),
            workspace_id: metadata.workspace_id,
            server_id: metadata.server_id,
            kind: metadata.kind,
            name: metadata.name,
            cwd: process.cwd,
            status,
            pid: process.pid,
            started_at: process.started_at,
            updated_at: Utc::now(),
            exited_at: process.exited_at,
            exit_code: process.exit_code,
            output_revision: process.next_revision.saturating_sub(1),
        };
        let agent = Arc::new(Mutex::new(ControllerAgent {
            info: info.clone(),
            workload_credential: metadata.workload_credential,
            text,
            bracketed_paste: process.bracketed_paste,
            last_output: Instant::now(),
            last_metadata_event: Instant::now(),
        }));
        self.inner
            .agents
            .write()
            .map_err(|_| ProtocolError::new("state_error", "agent registry lock poisoned"))?
            .insert(metadata.agent_id, agent);
        let _ = self.inner.events.send(info.clone());
        Ok(info)
    }

    fn process_response(
        &self,
        response: HostResponse,
        status: AgentStatus,
    ) -> Result<AgentInfo, ProtocolError> {
        let HostResponse::Process { process } = response else {
            return Err(ProtocolError::new(
                "host_protocol_error",
                "operation returned an unexpected response",
            ));
        };
        let agent = self.get(&process.process_id)?;
        let mut agent = agent
            .lock()
            .map_err(|_| ProtocolError::new("state_error", "agent state lock poisoned"))?;
        agent.info.status = status;
        agent.info.updated_at = Utc::now();
        agent.info.exited_at = process.exited_at;
        agent.info.exit_code = process.exit_code;
        let info = agent.info.clone();
        let _ = self.inner.events.send(info.clone());
        Ok(info)
    }

    fn get(&self, agent_id: &str) -> Result<Arc<Mutex<ControllerAgent>>, ProtocolError> {
        self.inner
            .agents
            .read()
            .ok()
            .and_then(|agents| agents.get(agent_id).cloned())
            .ok_or_else(|| ProtocolError::new("agent_not_found", agent_id))
    }

    fn start_event_tasks(&self, mut events: HostEvents) {
        let output_runtime = self.clone();
        tokio::spawn(async move {
            loop {
                match events.output.recv().await {
                    Ok(chunk) => output_runtime.apply_output(chunk),
                    Err(broadcast::error::RecvError::Lagged(count)) => {
                        warn!(count, "controller output relay lagged")
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        });
        let process_runtime = self.clone();
        tokio::spawn(async move {
            loop {
                match events.processes.recv().await {
                    Ok(process) => process_runtime.apply_process(process),
                    Err(broadcast::error::RecvError::Lagged(count)) => {
                        warn!(count, "controller process relay lagged")
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        });
    }

    fn apply_output(&self, chunk: HostOutputChunk) {
        if let Ok(agent) = self.get(&chunk.process_id) {
            if let Ok(mut agent) = agent.lock() {
                let previous_status = agent.info.status;
                let plain = strip_ansi_escapes::strip(&chunk.data);
                agent.text.push_str(&String::from_utf8_lossy(&plain));
                trim_text(&mut agent.text);
                agent.bracketed_paste = chunk.bracketed_paste;
                agent.last_output = Instant::now();
                agent.info.output_revision = chunk.revision;
                if !agent.info.status.is_terminal() {
                    agent.info.status = detect_status(&agent.text).unwrap_or(AgentStatus::Working);
                }
                agent.info.updated_at = chunk.emitted_at;
                let status_changed = previous_status != agent.info.status;
                let should_emit = status_changed
                    || agent.last_metadata_event.elapsed() >= OUTPUT_METADATA_INTERVAL;
                if should_emit {
                    agent.last_metadata_event = Instant::now();
                    let _ = self.inner.events.send(agent.info.clone());
                }
            }
        }
        let _ = self.inner.terminal_events.send(TerminalOutput {
            process_id: chunk.process_id,
            revision: chunk.revision,
            data: chunk.data,
        });
    }

    fn apply_process(&self, process: HostProcessInfo) {
        let _ = self.inner.process_events.send(process.clone());
        let Ok(agent) = self.get(&process.process_id) else {
            let _ = self.upsert_process(process, None);
            return;
        };
        let Ok(mut agent) = agent.lock() else {
            return;
        };
        agent.info.pid = process.pid;
        agent.info.exited_at = process.exited_at;
        agent.info.exit_code = process.exit_code;
        if !process.running {
            agent.info.status = AgentStatus::Exited;
        }
        agent.info.updated_at = Utc::now();
        let _ = self.inner.events.send(agent.info.clone());
    }

    fn start_idle_monitor(&self) {
        let runtime = self.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_millis(200));
            loop {
                interval.tick().await;
                let agents = runtime
                    .inner
                    .agents
                    .read()
                    .ok()
                    .map(|agents| agents.values().cloned().collect::<Vec<_>>())
                    .unwrap_or_default();
                for agent in agents {
                    let Ok(mut agent) = agent.lock() else {
                        continue;
                    };
                    if agent.last_output.elapsed() >= QUIET_IDLE_AFTER
                        && matches!(
                            agent.info.status,
                            AgentStatus::Starting | AgentStatus::Working
                        )
                    {
                        agent.info.status = AgentStatus::Idle;
                        agent.info.updated_at = Utc::now();
                        let _ = runtime.inner.events.send(agent.info.clone());
                    }
                }
            }
        });
    }
}

#[cfg(test)]
fn new_workload_credential() -> String {
    format!("wlc_{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}

fn valid_workload_credential(credential: &str) -> bool {
    credential.starts_with("wlc_")
        && credential.len() == 68
        && credential[4..].bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn workload_credential_matches(expected: &str, supplied: &str) -> bool {
    !expected.is_empty()
        && expected.len() == supplied.len()
        && expected.as_bytes().ct_eq(supplied.as_bytes()).unwrap_u8() == 1
}

fn resolve_launch(request: &CreateAgentRequest) -> Result<AgentLaunch, ProtocolError> {
    match request.kind.as_str() {
        "codex" => Ok(shell_agent_launch(
            "codex",
            "--dangerously-bypass-approvals-and-sandbox",
            &request.args,
            false,
        )),
        "claude" => Ok(shell_agent_launch(
            "claude",
            "--dangerously-skip-permissions",
            &request.args,
            true,
        )),
        "shell" => Ok(request.args.split_first().map_or_else(
            || AgentLaunch {
                command: interactive_shell(),
                args: vec!["-i".to_string()],
                initial_writes: Vec::new(),
            },
            |(command, args)| interactive_shell_command_launch(command, args),
        )),
        "command" => {
            let (command, args) = request.args.split_first().map_or_else(
                || (interactive_shell(), vec!["-i".to_string()]),
                |(command, args)| (command.clone(), args.to_vec()),
            );
            Ok(AgentLaunch {
                command,
                args,
                initial_writes: Vec::new(),
            })
        }
        other => Err(ProtocolError::new(
            "invalid_request",
            format!("unsupported agent kind {other}"),
        )),
    }
}

fn shell_agent_launch(
    agent_command: &str,
    default_arg: &str,
    args: &[String],
    confirm_workspace_trust: bool,
) -> AgentLaunch {
    let launch_args = std::iter::once(default_arg.to_string())
        .chain(args.iter().cloned())
        .collect::<Vec<_>>();
    let mut launch = interactive_shell_command_launch(agent_command, &launch_args);
    if confirm_workspace_trust {
        launch.initial_writes.push(HostWrite {
            data: vec![b'\r'],
            delay_ms: CLAUDE_TRUST_CONFIRM_DELAY.as_millis() as u64,
        });
    }
    launch
}

fn interactive_shell_command_launch(command: &str, args: &[String]) -> AgentLaunch {
    let mut input = shell_join(command, args).into_bytes();
    input.push(b'\r');
    AgentLaunch {
        command: interactive_shell(),
        args: vec!["-i".to_string()],
        initial_writes: vec![HostWrite {
            data: input,
            delay_ms: AGENT_COMMAND_DELAY.as_millis() as u64,
        }],
    }
}

fn sandbox_launch(
    executable: Option<&std::path::Path>,
    network_proxy_url: &str,
    launch: AgentLaunch,
) -> AgentLaunch {
    let Some(executable) = executable else {
        return launch;
    };
    let mut args = vec![
        "sandbox-exec".to_string(),
        "--network-proxy".to_string(),
        network_proxy_url.to_string(),
        "--".to_string(),
        launch.command,
    ];
    args.extend(launch.args);
    AgentLaunch {
        command: executable.display().to_string(),
        args,
        initial_writes: launch.initial_writes,
    }
}

fn should_replace_virtual_hosts(
    current: Option<&VirtualNetworkHostsSnapshot>,
    incoming: &VirtualNetworkHostsSnapshot,
) -> bool {
    current.is_none_or(|current| incoming.revision > current.revision)
}

fn interactive_shell() -> String {
    std::env::var("SHELL")
        .ok()
        .filter(|shell| !shell.trim().is_empty())
        .unwrap_or_else(|| {
            if cfg!(target_os = "macos") {
                "/bin/zsh".to_string()
            } else if std::path::Path::new("/bin/bash").is_file() {
                "/bin/bash".to_string()
            } else {
                "/bin/sh".to_string()
            }
        })
}

fn shell_join(command: &str, args: &[String]) -> String {
    std::iter::once(command)
        .chain(args.iter().map(String::as_str))
        .map(shell_quote)
        .collect::<Vec<_>>()
        .join(" ")
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn encode_prompt_text(text: &str, bracketed_paste: bool) -> Vec<u8> {
    if !bracketed_paste {
        return text.as_bytes().to_vec();
    }
    let mut encoded = Vec::with_capacity(text.len() + 12);
    encoded.extend_from_slice(b"\x1b[200~");
    encoded.extend_from_slice(text.as_bytes());
    encoded.extend_from_slice(b"\x1b[201~");
    encoded
}

fn decode_replay(replay: &HostOutputReplay) -> Result<Vec<u8>, ProtocolError> {
    let mut result = Vec::new();
    for chunk in &replay.chunks {
        result.extend_from_slice(&chunk.data);
    }
    Ok(result)
}

fn plain_text(replay: &HostOutputReplay) -> Result<String, ProtocolError> {
    let raw = decode_replay(replay)?;
    Ok(String::from_utf8_lossy(&strip_ansi_escapes::strip(raw)).into_owned())
}

fn trim_text(text: &mut String) {
    if text.len() <= OUTPUT_LIMIT_BYTES {
        return;
    }
    let mut split = text.len().saturating_sub(OUTPUT_LIMIT_BYTES);
    while split < text.len() && !text.is_char_boundary(split) {
        split += 1;
    }
    text.drain(..split);
}

fn select_lines(text: &str, lines: Option<usize>) -> String {
    let Some(lines) = lines else {
        return text.to_string();
    };
    if lines == 0 {
        return String::new();
    }
    let line_count = text.lines().count();
    if line_count <= lines {
        text.to_string()
    } else {
        text.lines()
            .skip(line_count - lines)
            .collect::<Vec<_>>()
            .join("\n")
    }
}

fn detect_status(text: &str) -> Option<AgentStatus> {
    let lower = text.to_lowercase();
    let blocked = [
        "allow command?",
        "do you want to proceed",
        "press enter to confirm",
        "enter to submit answer",
        "[y/n]",
        "action required",
    ];
    if blocked.iter().any(|pattern| lower.contains(pattern)) {
        return Some(AgentStatus::Blocked);
    }
    let working = [
        "esc to interrupt",
        "working (",
        "thinking (",
        "running tool",
    ];
    if working.iter().any(|pattern| lower.contains(pattern)) {
        return Some(AgentStatus::Working);
    }
    let last = text.lines().next_back().unwrap_or_default().trim_end();
    if last.ends_with('❯') || last.ends_with('›') || last.ends_with('$') || last.ends_with('#')
    {
        return Some(AgentStatus::Idle);
    }
    None
}

fn protocol_error(code: &str, error: impl std::fmt::Display) -> ProtocolError {
    ProtocolError::new(code, error.to_string())
}

fn agent_network_proxy_url(base: &str, agent_id: &str) -> String {
    let Ok(mut url) = url::Url::parse(base) else {
        return base.to_string();
    };
    if url.set_username(agent_id).is_err() || url.set_password(Some("treer")).is_err() {
        return base.to_string();
    }
    url.to_string()
}

fn network_environment(network_proxy_url: String, transparent: bool) -> BTreeMap<String, String> {
    let mut env = BTreeMap::from([("TREER_NETWORK_PROXY".to_string(), network_proxy_url.clone())]);
    if transparent {
        // The Controller's loopback is outside the agent network namespace.
        // Proxy-aware applications must use the TUN path instead of dialing it directly.
        for name in [
            "ALL_PROXY",
            "all_proxy",
            "HTTP_PROXY",
            "http_proxy",
            "HTTPS_PROXY",
            "https_proxy",
        ] {
            env.insert(name.to_string(), String::new());
        }
    } else {
        env.insert("ALL_PROXY".to_string(), network_proxy_url.clone());
        env.insert("all_proxy".to_string(), network_proxy_url);
        for name in ["NO_PROXY", "no_proxy"] {
            env.insert(name.to_string(), "127.0.0.1,localhost,::1".to_string());
        }
    }
    env
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_detector_prefers_blockers() {
        assert_eq!(
            detect_status("Working (esc to interrupt)\nAllow command?"),
            Some(AgentStatus::Blocked)
        );
    }

    #[test]
    fn workload_credentials_are_unique_and_compared_exactly() {
        let first = new_workload_credential();
        let second = new_workload_credential();
        assert!(first.starts_with("wlc_"));
        assert_eq!(first.len(), 68);
        assert_ne!(first, second);
        assert!(workload_credential_matches(&first, &first));
        assert!(!workload_credential_matches(&first, &second));
        assert!(!workload_credential_matches("", ""));
    }

    #[test]
    fn host_metadata_preserves_the_workload_credential_for_controller_restarts() {
        let metadata = AgentMetadata {
            agent_id: "agent-a".to_string(),
            workspace_id: "workspace-a".to_string(),
            server_id: "server-a".to_string(),
            kind: "command".to_string(),
            name: "agent-a".to_string(),
            cwd: ".".to_string(),
            workload_credential: "wlc_secret".to_string(),
        };
        let encoded = serde_json::to_string(&metadata).expect("encode metadata");
        let restored: AgentMetadata = serde_json::from_str(&encoded).expect("restore metadata");
        assert_eq!(restored.workload_credential, "wlc_secret");
    }

    #[test]
    fn prompt_text_uses_bracketed_paste_when_enabled() {
        assert_eq!(
            encode_prompt_text("hello", true),
            b"\x1b[200~hello\x1b[201~"
        );
        assert_eq!(encode_prompt_text("hello", false), b"hello");
    }

    #[test]
    fn agent_commands_are_entered_in_an_interactive_shell() {
        let args = vec![
            "--model".to_string(),
            "gpt 5".to_string(),
            "it's".to_string(),
            String::new(),
        ];

        let launch = shell_agent_launch(
            "codex",
            "--dangerously-bypass-approvals-and-sandbox",
            &args,
            false,
        );

        assert!(!launch.command.is_empty());
        assert_eq!(launch.args, ["-i"]);
        assert_eq!(
            launch.initial_writes,
            [HostWrite {
                data: b"'codex' '--dangerously-bypass-approvals-and-sandbox' '--model' 'gpt 5' 'it'\\''s' ''\r".to_vec(),
                delay_ms: AGENT_COMMAND_DELAY.as_millis() as u64,
            }]
        );
    }

    #[test]
    fn claude_launch_skips_permissions_and_confirms_workspace_trust() {
        let request = CreateAgentRequest {
            server_id: None,
            kind: "claude".to_string(),
            name: "claude-test".to_string(),
            cwd: ".".to_string(),
            args: vec!["--model".to_string(), "sonnet".to_string()],
            cols: 120,
            rows: 36,
        };

        let launch = resolve_launch(&request).expect("resolve claude launch");

        assert_eq!(launch.initial_writes.len(), 2);
        assert_eq!(
            launch.initial_writes[0].data,
            b"'claude' '--dangerously-skip-permissions' '--model' 'sonnet'\r"
        );
        assert_eq!(launch.initial_writes[1].data, b"\r");
        assert_eq!(
            launch.initial_writes[1].delay_ms,
            CLAUDE_TRUST_CONFIRM_DELAY.as_millis() as u64
        );
    }

    #[test]
    fn shell_commands_are_entered_after_interactive_shell_startup() {
        let request = CreateAgentRequest {
            server_id: None,
            kind: "shell".to_string(),
            name: "profile".to_string(),
            cwd: ".".to_string(),
            args: vec![
                "opencode".to_string(),
                "--model".to_string(),
                "provider/model name".to_string(),
            ],
            cols: 120,
            rows: 36,
        };

        let launch = resolve_launch(&request).expect("resolve shell command launch");

        assert!(!launch.command.is_empty());
        assert_eq!(launch.args, ["-i"]);
        assert_eq!(
            launch.initial_writes,
            [HostWrite {
                data: b"'opencode' '--model' 'provider/model name'\r".to_vec(),
                delay_ms: AGENT_COMMAND_DELAY.as_millis() as u64,
            }]
        );
    }

    #[test]
    fn explicit_command_agents_still_spawn_directly() {
        let request = CreateAgentRequest {
            server_id: None,
            kind: "command".to_string(),
            name: "shell".to_string(),
            cwd: ".".to_string(),
            args: vec!["/bin/sh".to_string(), "-c".to_string(), "pwd".to_string()],
            cols: 120,
            rows: 36,
        };

        let launch = resolve_launch(&request).expect("resolve command launch");

        assert_eq!(launch.command, "/bin/sh");
        assert_eq!(launch.args, ["-c", "pwd"]);
        assert!(launch.initial_writes.is_empty());
    }

    #[test]
    fn empty_command_request_opens_an_unmodified_interactive_terminal() {
        let request = CreateAgentRequest {
            server_id: None,
            kind: "command".to_string(),
            name: "terminal".to_string(),
            cwd: ".".to_string(),
            args: Vec::new(),
            cols: 120,
            rows: 36,
        };

        let launch = resolve_launch(&request).expect("resolve terminal launch");

        assert!(!launch.command.is_empty());
        assert_eq!(launch.args, ["-i"]);
        assert!(launch.initial_writes.is_empty());
    }

    #[test]
    fn agent_proxy_urls_carry_policy_identity() {
        let url = agent_network_proxy_url("socks5h://127.0.0.1:8791", "agent-a");
        let url = url::Url::parse(&url).expect("agent proxy URL");
        assert_eq!(url.username(), "agent-a");
        assert_eq!(url.password(), Some("treer"));
    }

    #[test]
    fn transparent_networking_does_not_expose_the_host_loopback_proxy() {
        let env = network_environment("socks5h://agent-a:treer@127.0.0.1:8791".to_string(), true);

        assert_eq!(env.get("ALL_PROXY").map(String::as_str), Some(""));
        assert_eq!(env.get("all_proxy").map(String::as_str), Some(""));
        assert_eq!(env.get("HTTP_PROXY").map(String::as_str), Some(""));
        assert_eq!(env.get("http_proxy").map(String::as_str), Some(""));
        assert_eq!(env.get("HTTPS_PROXY").map(String::as_str), Some(""));
        assert_eq!(env.get("https_proxy").map(String::as_str), Some(""));
        assert_eq!(
            env.get("TREER_NETWORK_PROXY").map(String::as_str),
            Some("socks5h://agent-a:treer@127.0.0.1:8791")
        );
    }

    #[test]
    fn compatibility_networking_exposes_the_socks_proxy() {
        let proxy = "socks5h://agent-a:treer@127.0.0.1:8791";
        let env = network_environment(proxy.to_string(), false);

        assert_eq!(env.get("ALL_PROXY").map(String::as_str), Some(proxy));
        assert_eq!(env.get("all_proxy").map(String::as_str), Some(proxy));
        assert_eq!(
            env.get("NO_PROXY").map(String::as_str),
            Some("127.0.0.1,localhost,::1")
        );
        assert_eq!(
            env.get("no_proxy").map(String::as_str),
            Some("127.0.0.1,localhost,::1")
        );
        assert_eq!(
            env.get("TREER_NETWORK_PROXY").map(String::as_str),
            Some(proxy)
        );
    }

    #[test]
    fn transparent_sandbox_preserves_launch_and_initial_input() {
        let initial_writes = vec![HostWrite {
            data: b"codex\r".to_vec(),
            delay_ms: 500,
        }];
        let launch = sandbox_launch(
            Some(std::path::Path::new("/opt/treer-agent-server")),
            "socks5h://agent-a:treer@127.0.0.1:8791",
            AgentLaunch {
                command: "/bin/bash".to_string(),
                args: vec!["-i".to_string()],
                initial_writes: initial_writes.clone(),
            },
        );

        assert_eq!(launch.command, "/opt/treer-agent-server");
        assert_eq!(
            launch.args,
            [
                "sandbox-exec",
                "--network-proxy",
                "socks5h://agent-a:treer@127.0.0.1:8791",
                "--",
                "/bin/bash",
                "-i"
            ]
        );
        assert_eq!(launch.initial_writes, initial_writes);
    }

    #[test]
    fn virtual_host_snapshots_only_move_forward_on_one_connection() {
        let current = VirtualNetworkHostsSnapshot {
            workspace_id: "default".to_string(),
            revision: 8,
            hosts: Vec::new(),
        };
        assert!(!should_replace_virtual_hosts(Some(&current), &current));
        assert!(!should_replace_virtual_hosts(
            Some(&current),
            &VirtualNetworkHostsSnapshot {
                revision: 7,
                ..current.clone()
            }
        ));
        assert!(should_replace_virtual_hosts(
            Some(&current),
            &VirtualNetworkHostsSnapshot {
                revision: 9,
                ..current.clone()
            }
        ));
    }
}
