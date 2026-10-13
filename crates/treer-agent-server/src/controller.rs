use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use base64::Engine;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::sync::broadcast;
use tracing::warn;
use treer_host_protocol::{
    HostCommand, HostOutputChunk, HostOutputReplay, HostProcessInfo, HostResponse,
    HostSpawnRequest, HostWrite,
};
use treer_protocol::{
    AgentInfo, AgentInterfaceDescriptor, AgentPrompt, AgentPromptQueueResponse, AgentStartupSpec,
    AgentStatus, AgentTranscriptResponse, CreateAgentRequest, MachineExecRequest,
    MachineExecResponse, ProtocolError, ReadAgentOutputResponse, RegisterAgentInterfaceRequest,
    ReplaceAgentStartupRequest, SetAgentStartupRequest, TerminalCursor, UploadMachineFileResponse,
    VirtualNetworkHostsSnapshot, AGENT_INTERFACE_PROTOCOL_V1,
};
#[cfg(test)]
use uuid::Uuid;

use crate::host_client::{HostClient, HostEvents};
use crate::interface_cache::{CachedAgentInterface, InterfaceCache};
use crate::startup_store::{StartupStore, StoredAgentStartup};

#[path = "controller_launch.rs"]
mod launch;
use launch::*;
#[path = "controller_output.rs"]
mod output;
use output::*;

const OUTPUT_LIMIT_BYTES: usize = 512 * 1024;
const OUTPUT_TRIM_SLACK_BYTES: usize = 64 * 1024;
const STATUS_SCAN_LIMIT_BYTES: usize = 16 * 1024;
const QUIET_IDLE_AFTER: Duration = Duration::from_millis(900);
const OUTPUT_METADATA_INTERVAL: Duration = Duration::from_millis(150);
const PROMPT_SUBMIT_DELAY: Duration = Duration::from_millis(300);
const AGENT_INTERFACE_FAILURE_LIMIT: u8 = 5;
const MACHINE_EXEC_TIMEOUT_MAX_MS: u64 = 30_000;
const MACHINE_EXEC_STREAM_LIMIT_BYTES: usize = 64 * 1024;
const MACHINE_UPLOAD_CHUNK_MAX_BYTES: usize = 192 * 1024;
const BRIDGE_PROMPT_QUEUE_LIMIT: usize = 256;
const BRIDGE_PROMPT_READ_LIMIT: usize = 100;
const BRIDGE_PROMPT_MAX_BYTES: usize = 64 * 1024;

fn validate_interface_ui_path(value: &str) -> Result<String, ProtocolError> {
    let value = value.trim();
    if value.is_empty()
        || !value.starts_with('/')
        || value.len() > 1024
        || value.contains("//")
        || value.split('/').any(|segment| segment == "..")
        || value
            .bytes()
            .any(|byte| byte.is_ascii_control() || byte.is_ascii_whitespace())
    {
        return Err(ProtocolError::new(
            "invalid_agent_interface_ui_path",
            "Agent Interface ui_path must be an absolute path without whitespace or parent traversal",
        ));
    }
    Ok(value.to_string())
}

#[derive(Debug)]
struct AgentLaunch {
    command: String,
    args: Vec<String>,
    initial_writes: Vec<HostWrite>,
    publish_ports: Vec<u16>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AgentMetadata {
    agent_id: String,
    #[serde(default)]
    run_generation: u64,
    workspace_id: String,
    server_id: String,
    kind: String,
    name: String,
    cwd: String,
    #[serde(default)]
    workload_credential: String,
    #[serde(default)]
    http_proxy_port: Option<u16>,
}

#[derive(Debug, Clone, Default)]
pub struct ProxyLinkStatus {
    pub connected: bool,
    pub last_error: Option<String>,
    pub last_error_code: Option<String>,
}

impl ProxyLinkStatus {
    pub fn connection_state(&self) -> &'static str {
        if self.connected {
            "online"
        } else if self.last_error_code.as_deref() == Some("duplicate_machine_connection") {
            "fenced"
        } else {
            "local"
        }
    }
}

#[derive(Clone)]
pub struct ControllerRuntime {
    inner: Arc<ControllerInner>,
}

pub struct ControllerConfig {
    pub protected_paths: Vec<PathBuf>,
    pub workspace_id: String,
    pub server_id: String,
    pub agent_server_url: String,
    pub proxy_public_url: String,
    pub treer_binary: Option<PathBuf>,
    pub interface_cache_path: PathBuf,
    pub startup_store_path: PathBuf,
    pub root: PathBuf,
}

struct ControllerInner {
    host: HostClient,
    workspace_id: String,
    server_id: String,
    agent_server_url: String,
    proxy_public_url: String,
    treer_binary: Option<PathBuf>,
    root: PathBuf,
    host_epoch: String,
    interface_cache: InterfaceCache,
    startup_store: StartupStore,
    recovery_lock: tokio::sync::Mutex<()>,
    uploads: Mutex<HashMap<String, PendingUpload>>,
    file_transfers: crate::machine_file_transfer::Transfers,
    agents: RwLock<HashMap<String, Arc<Mutex<ControllerAgent>>>>,
    events: broadcast::Sender<AgentInfo>,
    terminal_events: broadcast::Sender<TerminalOutput>,
    process_events: broadcast::Sender<HostProcessInfo>,
    virtual_hosts: RwLock<Option<VirtualNetworkHostsSnapshot>>,
    proxy_link: RwLock<ProxyLinkStatus>,
    workload_proxies: crate::workload_proxy::WorkloadProxies,
}

struct PendingUpload {
    file: File,
    temp_path: PathBuf,
    final_path: PathBuf,
    display_path: String,
    overwrite: bool,
    bytes_written: u64,
}

struct ControllerAgent {
    stream_epoch: String,
    info: AgentInfo,
    workload_credential: String,
    text: String,
    prompt_queue: VecDeque<AgentPrompt>,
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
    pub stream_epoch: String,
    pub revision: u64,
    pub gap: bool,
    pub data: Vec<u8>,
}

impl ControllerRuntime {
    pub(crate) fn machine_root(&self) -> &Path {
        &self.inner.root
    }
    pub(crate) fn agent_server_url(&self) -> &str {
        &self.inner.agent_server_url
    }
    pub fn from_sync(
        host: HostClient,
        sync: HostResponse,
        events: HostEvents,
        config: ControllerConfig,
    ) -> Result<(Self, tokio::sync::watch::Receiver<bool>), ProtocolError> {
        let HostResponse::Synced {
            host_epoch,
            processes,
            replay,
            ..
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
                file_transfers: crate::machine_file_transfer::Transfers::new(
                    config.root.clone(),
                    config.workspace_id.clone(),
                    [
                        Some(config.startup_store_path.clone()),
                        Some(config.interface_cache_path.clone()),
                        treer_platform::config_directory(),
                    ]
                    .into_iter()
                    .flatten()
                    .chain(config.protected_paths.clone())
                    .collect(),
                    std::env::var("TREER_MACHINE_FILE_MAX_BYTES")
                        .ok()
                        .and_then(|value| value.parse().ok())
                        .unwrap_or_else(crate::machine_file_transfer::Transfers::default_limit),
                ),
                host,
                workspace_id: config.workspace_id,
                server_id: config.server_id,
                workload_proxies: crate::workload_proxy::WorkloadProxies::new(
                    config.agent_server_url.clone(),
                ),
                agent_server_url: config.agent_server_url,
                proxy_public_url: config.proxy_public_url,
                treer_binary: config.treer_binary,
                root: config.root,
                host_epoch,
                interface_cache: InterfaceCache::load(config.interface_cache_path),
                startup_store: StartupStore::load(config.startup_store_path).map_err(|error| {
                    ProtocolError::new(
                        "startup_store_error",
                        format!("failed to load Agent startup store: {error}"),
                    )
                })?,
                recovery_lock: tokio::sync::Mutex::new(()),
                uploads: Mutex::new(HashMap::new()),
                agents: RwLock::new(HashMap::new()),
                events: agent_events,
                terminal_events,
                process_events,
                virtual_hosts: RwLock::new(None),
                proxy_link: RwLock::new(ProxyLinkStatus::default()),
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
            runtime.migrate_legacy_live_generation(&process)?;
            process.stream_epoch.clone_from(&replay.stream_epoch);
            process.next_revision = replay.next_revision;
            runtime.restore_process(process, replay)?;
        }
        for entry in runtime.inner.startup_store.entries() {
            if entry.workspace_id == runtime.inner.workspace_id
                && entry.spec.server_id == runtime.inner.server_id
            {
                runtime.materialize_startup(&entry)?;
            }
        }
        let disconnected = events.disconnected.clone();
        runtime.start_event_tasks(events);
        runtime.start_idle_monitor();
        Ok((runtime, disconnected))
    }

    // Only initial inventory may normalize an old configuration revision into
    // a process generation. Never call this while reconciling later events:
    // generation-zero exits after a replacement intent are stale.
    fn migrate_legacy_live_generation(
        &self,
        process: &HostProcessInfo,
    ) -> Result<(), ProtocolError> {
        let metadata: AgentMetadata = serde_json::from_str(&process.metadata)
            .map_err(|error| protocol_error("invalid_host_metadata", error))?;
        if process.running
            && metadata.run_generation == 0
            && metadata.workspace_id == self.inner.workspace_id
            && metadata.server_id == self.inner.server_id
            && self.inner.startup_store.get(&process.process_id).is_some()
        {
            self.inner
                .startup_store
                .update(&process.process_id, |entry| entry.spec.generation = 0)
                .map_err(startup_store_error)?;
        }
        Ok(())
    }

    pub async fn restore_cached_interfaces(&self) {
        let mut restored = Vec::new();
        for mut cached in self.inner.interface_cache.entries() {
            let matches_process = self
                .get(&cached.agent_id)
                .ok()
                .and_then(|agent| {
                    agent.lock().ok().map(|agent| {
                        agent.info.pid == Some(cached.pid)
                            && agent.info.started_at == cached.started_at
                            && !agent.info.status.is_terminal()
                    })
                })
                .unwrap_or(false);
            if !matches_process {
                continue;
            }
            cached.interface.registered_at = Utc::now();
            if let Err(error) = self
                .validate_interface_manifest(&cached.agent_id, &cached.interface)
                .await
            {
                warn!(
                    agent_id = %cached.agent_id,
                    code = %error.code,
                    "discarding stale Agent Interface cache entry"
                );
                continue;
            }
            let installed = self
                .get(&cached.agent_id)
                .ok()
                .and_then(|agent| {
                    agent.lock().ok().and_then(|mut agent| {
                        if agent.info.pid != Some(cached.pid)
                            || agent.info.started_at != cached.started_at
                            || agent.info.status.is_terminal()
                        {
                            return None;
                        }
                        agent.info.interface = Some(cached.interface.clone());
                        Some(agent.info.clone())
                    })
                })
                .is_some();
            if installed {
                self.start_interface_status_monitor(
                    cached.agent_id.clone(),
                    cached.interface.clone(),
                );
                restored.push(cached);
            }
        }
        if let Err(error) = self.inner.interface_cache.replace_all(restored) {
            warn!(%error, "failed to update Agent Interface cache after recovery");
        }
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

    pub fn proxy_link_status(&self) -> ProxyLinkStatus {
        self.inner
            .proxy_link
            .read()
            .map(|status| status.clone())
            .unwrap_or_default()
    }

    pub fn set_proxy_link_status(&self, status: ProxyLinkStatus) {
        if !status.connected {
            self.inner.file_transfers.clear();
        }
        if let Ok(mut current) = self.inner.proxy_link.write() {
            *current = status;
        }
    }

    pub async fn transfer_machine_file(
        &self,
        actor: treer_protocol::MachineTransferActor,
        path: String,
        request: treer_protocol::MachineFileTransferRequest,
    ) -> Result<serde_json::Value, ProtocolError> {
        let runtime = self.clone();
        let generation = runtime.inner.file_transfers.generation();
        tokio::task::spawn_blocking(move || {
            runtime
                .inner
                .file_transfers
                .execute_generation(generation, actor, &path, request)
        })
        .await
        .map_err(|_| ProtocolError::new("machine_file_io_failed", "file transfer task failed"))?
    }
    pub fn machine_file_transfer_info(&self) -> serde_json::Value {
        self.inner.file_transfers.info()
    }

    pub fn available_agent_kinds(&self) -> Vec<String> {
        vec!["shell".to_string(), "command".to_string()]
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
        let _guard = self.inner.recovery_lock.lock().await;
        if request.name.trim().is_empty() {
            return Err(ProtocolError::new(
                "invalid_request",
                "agent name cannot be empty",
            ));
        }
        let (kind, launch) = resolve_launch(&request)?;
        if !valid_workload_credential(&workload_credential) {
            return Err(ProtocolError::new(
                "invalid_workload_credential",
                "Proxy supplied an invalid workload credential",
            ));
        }
        // Persist the resolved launch, rather than a mutable profile reference,
        // before asking the Host to start the workload.
        if self.inner.startup_store.get(&agent_id).is_none() {
            self.inner
                .startup_store
                .upsert(StoredAgentStartup {
                    spec: AgentStartupSpec {
                        agent_id: agent_id.clone(),
                        server_id: self.inner.server_id.clone(),
                        kind: kind.clone(),
                        name: request.name.clone(),
                        cwd: request.cwd.clone(),
                        command: launch.command.clone(),
                        args: launch.args.clone(),
                        publish_ports: launch.publish_ports.clone(),
                        enabled: true,
                        generation: 1,
                    },
                    workspace_id: self.inner.workspace_id.clone(),
                    workload_credential: workload_credential.clone(),
                    last_host_epoch: self.inner.host_epoch.clone(),
                    initial_writes: launch.initial_writes.clone(),
                    cols: request.cols,
                    rows: request.rows,
                })
                .map_err(startup_store_error)?;
        }
        let proxy_port =
            self.inner
                .workload_proxies
                .ensure(&agent_id, &workload_credential, None)?;
        let metadata = AgentMetadata {
            agent_id: agent_id.clone(),
            run_generation: self
                .inner
                .startup_store
                .get(&agent_id)
                .map(|entry| entry.spec.generation)
                .unwrap_or(1),
            workspace_id: self.inner.workspace_id.clone(),
            server_id: self.inner.server_id.clone(),
            kind,
            name: request.name,
            cwd: request.cwd.clone(),
            workload_credential: workload_credential.clone(),
            http_proxy_port: Some(proxy_port),
        };
        let mut env = self.process_environment(Some((&agent_id, &workload_credential)));
        crate::workload_proxy::inject_environment(&mut env, proxy_port);
        let response = self
            .inner
            .host
            .request(
                HostCommand::Spawn {
                    request: HostSpawnRequest {
                        process_id: agent_id.clone(),
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
            .map_err(|error| {
                if !self.get(&agent_id).is_ok_and(|agent| {
                    agent
                        .lock()
                        .is_ok_and(|agent| !agent.info.status.is_terminal())
                }) {
                    self.inner.workload_proxies.remove(&agent_id);
                }
                protocol_error("host_error", error)
            })?;
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

    fn materialize_startup(&self, entry: &StoredAgentStartup) -> Result<AgentInfo, ProtocolError> {
        if let Ok(agent) = self.get(&entry.spec.agent_id) {
            return agent
                .lock()
                .map(|agent| agent.info.clone())
                .map_err(|_| ProtocolError::new("state_error", "agent lock poisoned"));
        }
        let now = Utc::now();
        let info = AgentInfo {
            path: None,
            agent_id: entry.spec.agent_id.clone(),
            run_generation: entry.spec.generation,
            workspace_id: entry.workspace_id.clone(),
            server_id: entry.spec.server_id.clone(),
            kind: entry.spec.kind.clone(),
            name: entry.spec.name.clone(),
            cwd: entry.spec.cwd.clone(),
            status: AgentStatus::Exited,
            pid: None,
            started_at: now,
            updated_at: now,
            exited_at: None,
            exit_code: None,
            output_revision: 0,
            interface: None,
        };
        self.inner
            .agents
            .write()
            .map_err(|_| ProtocolError::new("state_error", "agent registry lock poisoned"))?
            .insert(
                info.agent_id.clone(),
                Arc::new(Mutex::new(ControllerAgent {
                    stream_epoch: String::new(),
                    info: info.clone(),
                    workload_credential: entry.workload_credential.clone(),
                    text: String::new(),
                    prompt_queue: VecDeque::new(),
                    bracketed_paste: false,
                    last_output: Instant::now(),
                    last_metadata_event: Instant::now(),
                })),
            );
        let _ = self.inner.events.send(info.clone());
        Ok(info)
    }

    /// Declare a legacy workload without replacing its identity or live process.
    #[allow(clippy::too_many_arguments)]
    pub async fn ensure_lifecycle(
        &self,
        operation_id: &str,
        agent_id: String,
        workload_credential: Option<String>,
        request: CreateAgentRequest,
        enabled: bool,
        start_missing: bool,
    ) -> Result<AgentInfo, ProtocolError> {
        let _guard = self.inner.recovery_lock.lock().await;
        let inventory = self
            .inner
            .host
            .request(
                HostCommand::Sync {
                    cursors: Default::default(),
                },
                None,
            )
            .await
            .map_err(|error| protocol_error("host_error", error))?;
        if let HostResponse::Synced { processes, .. } = inventory {
            for process in processes {
                if process.process_id == agent_id && process.running {
                    self.upsert_process(process, None)?;
                }
            }
        }
        let newly_declared = self.inner.startup_store.get(&agent_id).is_none();
        if newly_declared {
            let (kind, launch) = resolve_launch(&request)?;
            let credential = self
                .get(&agent_id)
                .ok()
                .and_then(|agent| {
                    agent
                        .lock()
                        .ok()
                        .map(|agent| agent.workload_credential.clone())
                })
                .or(workload_credential)
                .ok_or_else(|| {
                    ProtocolError::new(
                        "lifecycle_credential_missing",
                        "no saved or live workload credential",
                    )
                })?;
            if !valid_workload_credential(&credential) {
                return Err(ProtocolError::new(
                    "invalid_workload_credential",
                    "invalid lifecycle credential",
                ));
            }
            self.inner
                .startup_store
                .upsert(StoredAgentStartup {
                    spec: AgentStartupSpec {
                        agent_id: agent_id.clone(),
                        server_id: self.inner.server_id.clone(),
                        kind,
                        name: request.name.clone(),
                        cwd: request.cwd.clone(),
                        command: launch.command,
                        args: launch.args,
                        publish_ports: launch.publish_ports,
                        enabled,
                        generation: self
                            .get(&agent_id)
                            .ok()
                            .and_then(|agent| {
                                agent.lock().ok().map(|agent| agent.info.run_generation)
                            })
                            .unwrap_or(if start_missing { 1 } else { 0 }),
                    },
                    workspace_id: self.inner.workspace_id.clone(),
                    workload_credential: credential,
                    last_host_epoch: self.inner.host_epoch.clone(),
                    initial_writes: launch.initial_writes,
                    cols: request.cols,
                    rows: request.rows,
                })
                .map_err(startup_store_error)?;
        }
        if !start_missing {
            // Explicit lifecycle requests declare first, then start/restart.
            // Do not let the periodic supervisor launch between those commands.
            self.inner
                .startup_store
                .update(&agent_id, |entry| entry.spec.enabled = false)
                .map_err(startup_store_error)?;
        }
        if !enabled {
            self.inner
                .startup_store
                .update(&agent_id, |entry| entry.spec.enabled = false)
                .map_err(startup_store_error)?;
            let running = self.get(&agent_id).is_ok_and(|agent| {
                agent
                    .lock()
                    .is_ok_and(|agent| !agent.info.status.is_terminal())
            });
            if running {
                let response = self
                    .inner
                    .host
                    .request(
                        HostCommand::Stop {
                            process_id: agent_id.clone(),
                        },
                        Some(format!("{operation_id}:stop")),
                    )
                    .await
                    .map_err(|error| protocol_error("host_error", error))?;
                return self.process_response(response, AgentStatus::Exited);
            }
        }
        if let Ok(agent) = self.get(&agent_id) {
            let info = agent
                .lock()
                .map_err(|_| ProtocolError::new("state_error", "agent lock poisoned"))?
                .info
                .clone();
            if !info.status.is_terminal() || !enabled {
                return Ok(info);
            }
        }
        let entry = self
            .inner
            .startup_store
            .get(&agent_id)
            .ok_or_else(|| ProtocolError::new("startup_not_found", &agent_id))?;
        if !entry.spec.enabled || !enabled || !start_missing {
            return self.materialize_startup(&entry);
        }
        let mut entry = entry;
        if !newly_declared {
            entry.spec.generation = entry.spec.generation.saturating_add(1);
            self.inner
                .startup_store
                .upsert(entry.clone())
                .map_err(startup_store_error)?;
        }
        self.spawn_startup(&entry).await
    }

    pub async fn start(
        &self,
        operation_id: &str,
        agent_id: &str,
        restart: bool,
    ) -> Result<AgentInfo, ProtocolError> {
        let _guard = self.inner.recovery_lock.lock().await;
        self.start_locked(operation_id, agent_id, restart).await
    }

    async fn start_locked(
        &self,
        operation_id: &str,
        agent_id: &str,
        restart: bool,
    ) -> Result<AgentInfo, ProtocolError> {
        // Inventory is authoritative even when a previous spawn ACK was lost.
        let response = self
            .inner
            .host
            .request(
                HostCommand::Sync {
                    cursors: Default::default(),
                },
                None,
            )
            .await
            .map_err(|error| protocol_error("host_error", error))?;
        if let HostResponse::Synced { processes, .. } = response {
            for process in processes {
                if process.process_id == agent_id && process.running {
                    let info = self.upsert_process(process, None)?;
                    if !restart {
                        self.inner
                            .startup_store
                            .update(agent_id, |entry| {
                                entry.spec.enabled = true;
                                entry.last_host_epoch.clone_from(&self.inner.host_epoch);
                            })
                            .map_err(startup_store_error)?;
                        return Ok(info);
                    }
                    let response = self
                        .inner
                        .host
                        .request(
                            HostCommand::Stop {
                                process_id: agent_id.to_string(),
                            },
                            Some(format!("{operation_id}:stop")),
                        )
                        .await
                        .map_err(|error| protocol_error("host_error", error))?;
                    self.process_response(response, AgentStatus::Exited)?;
                }
            }
        }
        let mut entry = self.inner.startup_store.get(agent_id).ok_or_else(|| {
            ProtocolError::new("startup_not_found", "no saved launch for workload")
        })?;
        entry.spec.enabled = true;
        entry.spec.generation = entry.spec.generation.saturating_add(1);
        entry.last_host_epoch.clone_from(&self.inner.host_epoch);
        self.inner
            .startup_store
            .upsert(entry.clone())
            .map_err(startup_store_error)?;
        self.spawn_startup(&entry).await
    }

    pub async fn replace_startup(
        &self,
        agent_id: &str,
        request: ReplaceAgentStartupRequest,
    ) -> Result<AgentStartupSpec, ProtocolError> {
        let _guard = self.inner.recovery_lock.lock().await;
        self.replace_startup_locked(agent_id, request)
    }

    pub async fn restart_with_startup(
        &self,
        operation_id: &str,
        agent_id: &str,
        request: ReplaceAgentStartupRequest,
    ) -> Result<AgentInfo, ProtocolError> {
        let _guard = self.inner.recovery_lock.lock().await;
        // Persist before touching the process. The lock covers replacement,
        // inventory adoption, stop, generation allocation, and spawn.
        self.replace_startup_locked(agent_id, request)?;
        self.start_locked(operation_id, agent_id, true).await
    }

    pub fn get_admin_startup(&self, agent_id: &str) -> Result<AgentStartupSpec, ProtocolError> {
        let spec = self.get_startup(agent_id)?;
        if spec.kind == "app" {
            return Err(ProtocolError::new(
                "app_startup_managed",
                "App startup is managed by its deployment; edit the App deployment instead",
            ));
        }
        Ok(spec)
    }

    fn replace_startup_locked(
        &self,
        agent_id: &str,
        request: ReplaceAgentStartupRequest,
    ) -> Result<AgentStartupSpec, ProtocolError> {
        request.validate()?;
        let mut entry = match self.inner.startup_store.get(agent_id) {
            Some(entry) => entry,
            None => {
                let agent = self.get(agent_id)?;
                let agent = agent
                    .lock()
                    .map_err(|_| ProtocolError::new("state_error", "agent state lock poisoned"))?;
                StoredAgentStartup {
                    spec: AgentStartupSpec {
                        agent_id: agent_id.into(),
                        server_id: self.inner.server_id.clone(),
                        kind: agent.info.kind.clone(),
                        name: agent.info.name.clone(),
                        cwd: agent.info.cwd.clone(),
                        command: String::new(),
                        args: vec![],
                        publish_ports: vec![],
                        enabled: !agent.info.status.is_terminal(),
                        generation: agent.info.run_generation,
                    },
                    workspace_id: self.inner.workspace_id.clone(),
                    workload_credential: agent.workload_credential.clone(),
                    last_host_epoch: self.inner.host_epoch.clone(),
                    initial_writes: vec![],
                    cols: 120,
                    rows: 36,
                }
            }
        };
        if entry.spec.kind == "app" {
            return Err(ProtocolError::new(
                "app_startup_managed",
                "App startup is managed by its deployment; edit the App deployment instead",
            ));
        }
        if entry.workspace_id != self.inner.workspace_id
            || entry.spec.server_id != self.inner.server_id
        {
            return Err(ProtocolError::new(
                "host_identity_mismatch",
                "saved startup belongs to another workspace or Controller",
            ));
        }
        // Workload credentials are inherited from the private store, never
        // serialized into the public startup spec or command line.
        if !entry.workload_credential.is_empty()
            && std::iter::once(&request.command)
                .chain(request.args.iter())
                .any(|arg| arg.contains(&entry.workload_credential))
        {
            return Err(ProtocolError::new(
                "invalid_agent_startup",
                "startup argv must not contain the workload credential",
            ));
        }
        if let Some(cwd) = request.cwd {
            self.resolve_machine_directory(&cwd)?;
            entry.spec.cwd = if cwd.trim().is_empty() {
                ".".into()
            } else {
                cwd
            };
        }
        entry.spec.command = request.command;
        entry.spec.args = request.args;
        entry.initial_writes.clear();
        let spec = entry.spec.clone();
        self.inner
            .startup_store
            .upsert(entry)
            .map_err(startup_store_error)?;
        Ok(spec)
    }

    pub async fn set_startup(
        &self,
        agent_id: &str,
        request: SetAgentStartupRequest,
    ) -> Result<AgentStartupSpec, ProtocolError> {
        let _guard = self.inner.recovery_lock.lock().await;
        if request.command.trim().is_empty() {
            return Err(ProtocolError::new(
                "invalid_agent_startup",
                "startup command cannot be empty",
            ));
        }
        self.resolve_machine_directory(&request.cwd)?;
        let publish_ports = validate_publish_ports(&request.publish_ports)?;
        let agent = self.get(agent_id)?;
        let (info, workload_credential) = agent
            .lock()
            .map(|agent| (agent.info.clone(), agent.workload_credential.clone()))
            .map_err(|_| ProtocolError::new("state_error", "agent state lock poisoned"))?;
        if info.status.is_terminal() {
            return Err(ProtocolError::new(
                "agent_not_running",
                "only a running Agent can register its startup command",
            ));
        }
        let generation = self
            .inner
            .startup_store
            .get(agent_id)
            .map_or(info.run_generation, |entry| entry.spec.generation);
        let spec = AgentStartupSpec {
            agent_id: agent_id.to_string(),
            server_id: self.inner.server_id.clone(),
            kind: info.kind,
            name: info.name,
            cwd: if request.cwd.trim().is_empty() {
                ".".to_string()
            } else {
                request.cwd
            },
            command: request.command,
            args: request.args,
            publish_ports,
            enabled: true,
            generation,
        };
        self.inner
            .startup_store
            .upsert(StoredAgentStartup {
                spec: spec.clone(),
                workspace_id: self.inner.workspace_id.clone(),
                workload_credential,
                last_host_epoch: self.inner.host_epoch.clone(),
                initial_writes: vec![],
                cols: 120,
                rows: 36,
            })
            .map_err(startup_store_error)?;
        Ok(spec)
    }

    pub fn get_startup(&self, agent_id: &str) -> Result<AgentStartupSpec, ProtocolError> {
        self.inner
            .startup_store
            .get(agent_id)
            .filter(|entry| {
                entry.workspace_id == self.inner.workspace_id
                    && entry.spec.server_id == self.inner.server_id
            })
            .map(|entry| entry.spec)
            .ok_or_else(|| {
                ProtocolError::new(
                    "agent_startup_not_found",
                    "Agent has not registered a startup command",
                )
            })
    }

    pub fn clear_startup(&self, agent_id: &str) -> Result<bool, ProtocolError> {
        self.inner
            .startup_store
            .remove(agent_id)
            .map_err(startup_store_error)
    }

    pub fn persist_stopped_startup(&self, agent_id: &str) -> Result<(), ProtocolError> {
        if let Some(mut entry) = self.inner.startup_store.get(agent_id) {
            entry.spec.enabled = false;
            self.inner
                .startup_store
                .upsert(entry)
                .map_err(startup_store_error)?;
        }
        Ok(())
    }

    pub async fn apply_stopped_startups(&self, agent_ids: &[String]) -> Result<(), ProtocolError> {
        for id in agent_ids {
            self.stop(&format!("proxy-stopped:{}", uuid::Uuid::new_v4()), id)
                .await?;
        }
        Ok(())
    }

    pub async fn restore_startup_agents(
        &self,
        active_agent_ids: &[String],
    ) -> Vec<(String, Result<AgentInfo, ProtocolError>)> {
        let _guard = self.inner.recovery_lock.lock().await;
        let mut running_ids = HashSet::new();
        // A spawn ACK can be lost. The Host inventory, rather than cached
        // Controller state or a PID, decides whether an instance is missing.
        match self
            .inner
            .host
            .request(
                HostCommand::Sync {
                    cursors: Default::default(),
                },
                None,
            )
            .await
        {
            Ok(HostResponse::Synced { processes, .. }) => {
                for process in processes {
                    if process.running {
                        running_ids.insert(process.process_id.clone());
                        if self.get(&process.process_id).is_ok_and(|agent| {
                            agent.lock().is_ok_and(|agent| {
                                agent.stream_epoch == process.stream_epoch
                                    && !agent.info.status.is_terminal()
                            })
                        }) {
                            continue;
                        }
                    }
                    let _ = self.upsert_process(process, None);
                }
            }
            Ok(_) => {
                return vec![(
                    "host".into(),
                    Err(ProtocolError::new(
                        "host_protocol_error",
                        "inventory returned an unexpected response",
                    )),
                )]
            }
            Err(error) => return vec![("host".into(), Err(protocol_error("host_error", error)))],
        }
        let active: HashSet<_> = active_agent_ids.iter().map(String::as_str).collect();
        let entries = self.inner.startup_store.entries();
        let mut results = Vec::new();
        for mut entry in entries {
            let agent_id = entry.spec.agent_id.clone();
            if entry.workspace_id != self.inner.workspace_id
                || entry.spec.server_id != self.inner.server_id
                || !entry.spec.enabled
                || !active.contains(agent_id.as_str())
            {
                continue;
            }
            if running_ids.contains(&agent_id) {
                if let Ok(agent) = self.get(&agent_id) {
                    if let Ok(agent) = agent.lock() {
                        if !agent.info.status.is_terminal() {
                            results.push((agent_id, Ok(agent.info.clone())));
                            continue;
                        }
                    }
                }
            }
            // Cached success can refer to a process that has since exited.
            // Persist a new attempt before spawning a missing instance.
            entry.spec.generation = entry.spec.generation.saturating_add(1);
            if let Err(error) = self.inner.startup_store.upsert(entry.clone()) {
                results.push((agent_id, Err(startup_store_error(error))));
                continue;
            }
            let result = match self.spawn_startup(&entry).await {
                Ok(info) => self
                    .inner
                    .startup_store
                    .update(&agent_id, |current| {
                        current.last_host_epoch.clone_from(&self.inner.host_epoch)
                    })
                    .map(|()| info)
                    .map_err(startup_store_error),
                Err(error) => Err(error),
            };
            results.push((agent_id, result));
        }
        results
    }

    pub fn startup_candidate_ids(&self) -> Vec<String> {
        let mut ids: Vec<_> = self
            .inner
            .startup_store
            .entries()
            .into_iter()
            .filter(|entry| {
                entry.workspace_id == self.inner.workspace_id
                    && entry.spec.server_id == self.inner.server_id
                    && entry.spec.enabled
            })
            .map(|entry| entry.spec.agent_id)
            .collect();
        ids.sort();
        ids
    }

    pub fn startup_validation_ids(&self) -> Vec<String> {
        self.inner
            .startup_store
            .entries()
            .into_iter()
            .filter(|entry| {
                entry.workspace_id == self.inner.workspace_id
                    && entry.spec.server_id == self.inner.server_id
            })
            .map(|entry| entry.spec.agent_id)
            .collect()
    }

    pub fn automatic_startup_candidate_ids(&self) -> Vec<String> {
        self.inner.startup_store.candidate_ids(
            &self.inner.workspace_id,
            &self.inner.server_id,
            &self.inner.host_epoch,
        )
    }

    pub async fn restore_runtime(&self, active: &[String]) -> treer_protocol::RuntimeRestoreReport {
        use treer_protocol::{RuntimeRestoreItem, RuntimeRestoreReport, RuntimeRestoreState};
        let requested_at = Utc::now();
        let results = self.restore_startup_agents(active).await;
        let mut items: Vec<_> = results
            .into_iter()
            .map(|(id, result)| {
                let (state, message) = match result {
                    Ok(info) => (
                        if info.started_at <= requested_at {
                            RuntimeRestoreState::Attached
                        } else {
                            RuntimeRestoreState::Started
                        },
                        None,
                    ),
                    Err(error) => (
                        RuntimeRestoreState::Failed,
                        Some(format!("{}: {}", error.code, error.message)),
                    ),
                };
                RuntimeRestoreItem {
                    workload_id: id,
                    kind: "agent".into(),
                    state,
                    message,
                }
            })
            .collect();
        for entry in self.inner.startup_store.entries() {
            if entry.workspace_id != self.inner.workspace_id
                || entry.spec.server_id != self.inner.server_id
            {
                continue;
            }
            if !entry.spec.enabled || !active.contains(&entry.spec.agent_id) {
                items.push(RuntimeRestoreItem {
                    workload_id: entry.spec.agent_id,
                    kind: "agent".into(),
                    state: if entry.spec.enabled {
                        RuntimeRestoreState::IdentityRevoked
                    } else {
                        RuntimeRestoreState::SkippedStopped
                    },
                    message: None,
                });
            }
        }
        items.sort_by(|a, b| a.workload_id.cmp(&b.workload_id));
        RuntimeRestoreReport {
            workspace_id: self.inner.workspace_id.clone(),
            server_id: self.inner.server_id.clone(),
            items,
        }
    }

    async fn spawn_startup(&self, entry: &StoredAgentStartup) -> Result<AgentInfo, ProtocolError> {
        if !entry.spec.publish_ports.is_empty() {
            return Err(ProtocolError::new("startup_migration_required", "legacy namespace port publications cannot be restored; update the startup command and register an explicit machine service"));
        }
        let agent_id = &entry.spec.agent_id;
        let proxy_port =
            self.inner
                .workload_proxies
                .ensure(agent_id, &entry.workload_credential, None)?;
        let metadata = AgentMetadata {
            agent_id: agent_id.clone(),
            run_generation: entry.spec.generation,
            workspace_id: self.inner.workspace_id.clone(),
            server_id: self.inner.server_id.clone(),
            kind: entry.spec.kind.clone(),
            name: entry.spec.name.clone(),
            cwd: entry.spec.cwd.clone(),
            workload_credential: entry.workload_credential.clone(),
            http_proxy_port: Some(proxy_port),
        };
        let mut env = self.process_environment(Some((agent_id, &entry.workload_credential)));
        crate::workload_proxy::inject_environment(&mut env, proxy_port);
        let launch = AgentLaunch {
            command: entry.spec.command.clone(),
            args: entry.spec.args.clone(),
            initial_writes: entry.initial_writes.clone(),
            publish_ports: entry.spec.publish_ports.clone(),
        };
        let response = self
            .inner
            .host
            .request(
                HostCommand::Spawn {
                    request: HostSpawnRequest {
                        process_id: agent_id.clone(),
                        command: launch.command,
                        args: launch.args,
                        cwd: entry.spec.cwd.clone(),
                        env,
                        cols: entry.cols,
                        rows: entry.rows,
                        metadata: serde_json::to_string(&metadata)
                            .map_err(|error| protocol_error("metadata_error", error))?,
                    },
                },
                Some(format!(
                    "startup:{}:{agent_id}:{}",
                    self.inner.host_epoch, entry.spec.generation
                )),
            )
            .await
            .map_err(|error| {
                if !self.get(agent_id).is_ok_and(|agent| {
                    agent
                        .lock()
                        .is_ok_and(|agent| !agent.info.status.is_terminal())
                }) {
                    self.inner.workload_proxies.remove(agent_id);
                }
                protocol_error("host_error", error)
            })?;
        let HostResponse::Process { process } = response else {
            return Err(ProtocolError::new(
                "host_protocol_error",
                "startup spawn returned an unexpected response",
            ));
        };
        let info = self.upsert_process(process, None)?;
        if !launch.initial_writes.is_empty() {
            self.inner
                .host
                .request(
                    HostCommand::Write {
                        process_id: agent_id.clone(),
                        writes: launch.initial_writes,
                    },
                    Some(format!(
                        "startup:{}:{agent_id}:{}:launch",
                        self.inner.host_epoch, entry.spec.generation
                    )),
                )
                .await
                .map_err(|error| protocol_error("host_error", error))?;
        }
        Ok(info)
    }

    pub async fn exec_machine(
        &self,
        request: MachineExecRequest,
    ) -> Result<MachineExecResponse, ProtocolError> {
        if request.command.trim().is_empty() {
            return Err(ProtocolError::new(
                "invalid_machine_exec",
                "command must not be empty",
            ));
        }
        if request.timeout_ms == 0 || request.timeout_ms > MACHINE_EXEC_TIMEOUT_MAX_MS {
            return Err(ProtocolError::new(
                "invalid_machine_exec_timeout",
                format!("timeout_ms must be between 1 and {MACHINE_EXEC_TIMEOUT_MAX_MS}"),
            ));
        }
        let cwd = self.resolve_machine_directory(&request.cwd)?;
        let display_cwd = machine_relative_path(&self.inner.root, &cwd);
        let started = Instant::now();
        let mut command = tokio::process::Command::new(&request.command);
        command
            .args(&request.args)
            .current_dir(&cwd)
            .env_clear()
            .envs(self.machine_exec_environment())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = command.spawn().map_err(|error| {
            ProtocolError::new(
                "machine_exec_spawn_failed",
                format!("failed to start {}: {error}", request.command),
            )
        })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            ProtocolError::new("machine_exec_failed", "stdout pipe was not created")
        })?;
        let stderr = child.stderr.take().ok_or_else(|| {
            ProtocolError::new("machine_exec_failed", "stderr pipe was not created")
        })?;
        let mut stdout_task = tokio::spawn(read_bounded(stdout, MACHINE_EXEC_STREAM_LIMIT_BYTES));
        let mut stderr_task = tokio::spawn(read_bounded(stderr, MACHINE_EXEC_STREAM_LIMIT_BYTES));
        let wait =
            tokio::time::timeout(Duration::from_millis(request.timeout_ms), child.wait()).await;
        let (status, timed_out) = match wait {
            Ok(result) => (
                result.map_err(|error| {
                    ProtocolError::new("machine_exec_failed", error.to_string())
                })?,
                false,
            ),
            Err(_) => {
                child.kill().await.map_err(|error| {
                    ProtocolError::new("machine_exec_kill_failed", error.to_string())
                })?;
                (
                    child.wait().await.map_err(|error| {
                        ProtocolError::new("machine_exec_failed", error.to_string())
                    })?,
                    true,
                )
            }
        };
        let (stdout, stdout_truncated) = finish_bounded_read(&mut stdout_task).await;
        let (stderr, stderr_truncated) = finish_bounded_read(&mut stderr_task).await;
        Ok(MachineExecResponse {
            server_id: self.inner.server_id.clone(),
            cwd: display_cwd,
            command: request.command,
            args: request.args,
            exit_code: status.code(),
            stdout: String::from_utf8_lossy(&stdout).into_owned(),
            stderr: String::from_utf8_lossy(&stderr).into_owned(),
            truncated: stdout_truncated || stderr_truncated,
            timed_out,
            duration_ms: started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
        })
    }

    pub fn begin_upload(
        &self,
        upload_id: &str,
        directory: &str,
        file_name: &str,
        overwrite: bool,
    ) -> Result<(), ProtocolError> {
        validate_upload_id(upload_id)?;
        validate_upload_file_name(file_name)?;
        let directory = self.resolve_machine_directory(directory)?;
        let final_path = directory.join(file_name);
        if final_path.exists() && !overwrite {
            return Err(ProtocolError::new(
                "machine_file_exists",
                format!(
                    "{} already exists",
                    machine_relative_path(&self.inner.root, &final_path)
                ),
            ));
        }
        let mut uploads = self
            .inner
            .uploads
            .lock()
            .map_err(|_| ProtocolError::new("state_error", "upload registry lock poisoned"))?;
        if uploads.contains_key(upload_id) {
            return Err(ProtocolError::new(
                "machine_upload_exists",
                "upload ID is already active",
            ));
        }
        let temp_path = directory.join(format!(".treer-upload-{upload_id}.tmp"));
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)
            .map_err(|error| {
                ProtocolError::new(
                    "machine_upload_failed",
                    format!("failed to create upload staging file: {error}"),
                )
            })?;
        let pending = PendingUpload {
            file,
            temp_path,
            display_path: machine_relative_path(&self.inner.root, &final_path),
            final_path,
            overwrite,
            bytes_written: 0,
        };
        uploads.insert(upload_id.to_string(), pending);
        Ok(())
    }

    pub fn append_upload_chunk(
        &self,
        upload_id: &str,
        content_base64: &str,
    ) -> Result<u64, ProtocolError> {
        let content = base64::engine::general_purpose::STANDARD
            .decode(content_base64)
            .map_err(|_| {
                ProtocolError::new("invalid_machine_upload", "chunk is not valid base64")
            })?;
        if content.len() > MACHINE_UPLOAD_CHUNK_MAX_BYTES {
            return Err(ProtocolError::new(
                "machine_upload_chunk_too_large",
                format!("upload chunks may contain at most {MACHINE_UPLOAD_CHUNK_MAX_BYTES} bytes"),
            ));
        }
        let mut uploads = self
            .inner
            .uploads
            .lock()
            .map_err(|_| ProtocolError::new("state_error", "upload registry lock poisoned"))?;
        let upload = uploads.get_mut(upload_id).ok_or_else(|| {
            ProtocolError::new("machine_upload_not_found", "upload is not active")
        })?;
        upload.file.write_all(&content).map_err(|error| {
            ProtocolError::new(
                "machine_upload_failed",
                format!("failed to write chunk: {error}"),
            )
        })?;
        upload.bytes_written = upload.bytes_written.saturating_add(content.len() as u64);
        Ok(upload.bytes_written)
    }

    pub fn commit_upload(
        &self,
        upload_id: &str,
    ) -> Result<UploadMachineFileResponse, ProtocolError> {
        let mut upload = self.take_upload(upload_id)?;
        upload.file.flush().map_err(|error| {
            ProtocolError::new(
                "machine_upload_failed",
                format!("failed to flush upload: {error}"),
            )
        })?;
        drop(upload.file);
        if upload.final_path.exists() && !upload.overwrite {
            let _ = std::fs::remove_file(&upload.temp_path);
            return Err(ProtocolError::new(
                "machine_file_exists",
                format!("{} already exists", upload.display_path),
            ));
        }
        #[cfg(windows)]
        if upload.overwrite && upload.final_path.exists() {
            std::fs::remove_file(&upload.final_path).map_err(|error| {
                ProtocolError::new(
                    "machine_upload_failed",
                    format!("failed to replace existing file: {error}"),
                )
            })?;
        }
        if let Err(error) = std::fs::rename(&upload.temp_path, &upload.final_path) {
            let _ = std::fs::remove_file(&upload.temp_path);
            return Err(ProtocolError::new(
                "machine_upload_failed",
                format!("failed to install uploaded file: {error}"),
            ));
        }
        Ok(UploadMachineFileResponse {
            server_id: self.inner.server_id.clone(),
            path: upload.display_path,
            bytes_written: upload.bytes_written,
        })
    }

    pub fn abort_upload(&self, upload_id: &str) -> Result<bool, ProtocolError> {
        let mut uploads = self
            .inner
            .uploads
            .lock()
            .map_err(|_| ProtocolError::new("state_error", "upload registry lock poisoned"))?;
        let Some(upload) = uploads.remove(upload_id) else {
            return Ok(false);
        };
        drop(upload.file);
        let _ = std::fs::remove_file(upload.temp_path);
        Ok(true)
    }

    fn take_upload(&self, upload_id: &str) -> Result<PendingUpload, ProtocolError> {
        self.inner
            .uploads
            .lock()
            .map_err(|_| ProtocolError::new("state_error", "upload registry lock poisoned"))?
            .remove(upload_id)
            .ok_or_else(|| ProtocolError::new("machine_upload_not_found", "upload is not active"))
    }

    fn resolve_machine_directory(&self, requested: &str) -> Result<PathBuf, ProtocolError> {
        let requested = if requested.trim().is_empty() {
            "."
        } else {
            requested
        };
        let requested = Path::new(requested);
        if requested.is_absolute() {
            return Err(ProtocolError::new(
                "invalid_machine_path",
                "directory must be relative to the machine root",
            ));
        }
        let directory =
            std::fs::canonicalize(self.inner.root.join(requested)).map_err(|error| {
                ProtocolError::new(
                    "invalid_machine_path",
                    format!("directory is unavailable: {error}"),
                )
            })?;
        if !directory.starts_with(&self.inner.root) || !directory.is_dir() {
            return Err(ProtocolError::new(
                "invalid_machine_path",
                "directory resolves outside the machine root or is not a directory",
            ));
        }
        Ok(directory)
    }

    fn process_environment(&self, agent: Option<(&str, &str)>) -> BTreeMap<String, String> {
        let mut env = BTreeMap::from([
            (
                "TREER_WORKSPACE_ID".to_string(),
                self.inner.workspace_id.clone(),
            ),
            ("TREER_SERVER_ID".to_string(), self.inner.server_id.clone()),
            (
                "TREER_PROXY_URL".to_string(),
                self.inner.proxy_public_url.clone(),
            ),
            (
                "TREER_AGENT_SERVER_URL".to_string(),
                self.inner.agent_server_url.clone(),
            ),
        ]);
        if let Some((agent_id, workload_credential)) = agent {
            env.insert("TREER_IDENTITY_MODE".to_string(), "agent".to_string());
            env.insert("TREER_AGENT_ID".to_string(), agent_id.to_string());
            env.insert(
                "TREER_WORKLOAD_CREDENTIAL".to_string(),
                workload_credential.to_string(),
            );
        }
        if let Some(treer_binary) = &self.inner.treer_binary {
            env.insert("TREER_BIN".to_string(), treer_binary.display().to_string());
        }
        env.insert(
            "PATH".to_string(),
            join_agent_path(self.inner.treer_binary.as_deref()),
        );
        env
    }

    fn machine_exec_environment(&self) -> BTreeMap<std::ffi::OsString, std::ffi::OsString> {
        treer_platform::machine_exec_environment()
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

    pub async fn submit_prompt_delivery(
        &self,
        operation_id: &str,
        agent_id: &str,
        request: treer_protocol::SubmitAgentPromptRequest,
        sender: Option<treer_protocol::PromptSender>,
    ) -> Result<serde_json::Value, ProtocolError> {
        use treer_protocol::{AgentInterfacePromptRequest, PromptDelivery};
        if request.text.is_empty() {
            return Err(ProtocolError::new(
                "invalid_request",
                "agent prompt cannot be empty",
            ));
        }
        let interface = self.interface_for(agent_id, "prompt.queue")?;
        let Some(interface) = interface else {
            if request.delivery == PromptDelivery::Steer
                && request.notify.is_none()
                && !request.require_receipt
            {
                return serde_json::to_value(
                    self.prompt(operation_id, agent_id, &request.text).await?,
                )
                .map_err(|error| ProtocolError::new("serialization_error", error.to_string()));
            }
            return Err(ProtocolError::new("queue_unsupported", "Target does not expose prompt.queue; use --steer explicitly for legacy terminal input"));
        };
        if request.notify.is_some() && self.interface_for(agent_id, "prompt.notify")?.is_none() {
            return Err(ProtocolError::new(
                "queue_unsupported",
                "Target does not expose prompt.notify",
            ));
        }
        if request.require_receipt && self.interface_for(agent_id, "prompt.queue.read")?.is_none() {
            return Err(ProtocolError::new(
                "queue_unsupported",
                "Target does not expose prompt.queue.read required by --wait",
            ));
        }
        let receipt = crate::agent_interface::submit_delivery(
            agent_id,
            &interface,
            &AgentInterfacePromptRequest {
                operation_id: operation_id.to_string(),
                text: request.text,
                mode: "prompt".into(),
                delivery: request.delivery,
                sender,
                notify: request.notify,
            },
        )
        .await?;
        let info = self
            .get(agent_id)?
            .lock()
            .map_err(|_| ProtocolError::new("state_error", "agent state lock poisoned"))?
            .info
            .clone();
        let mut value = serde_json::to_value(info)
            .map_err(|error| ProtocolError::new("serialization_error", error.to_string()))?;
        value["submission"] = serde_json::to_value(receipt.submission)
            .map_err(|error| ProtocolError::new("serialization_error", error.to_string()))?;
        Ok(value)
    }

    pub async fn interface_status(
        &self,
        agent_id: &str,
    ) -> Result<serde_json::Value, ProtocolError> {
        let interface = self
            .interface_for(agent_id, "state.observe")?
            .ok_or_else(|| {
                ProtocolError::new("queue_unsupported", "Target does not expose state.observe")
            })?;
        serde_json::to_value(crate::agent_interface::status(agent_id, &interface).await?)
            .map_err(|error| ProtocolError::new("serialization_error", error.to_string()))
    }

    pub async fn prompt_queue_request(
        &self,
        agent_id: &str,
        submission_id: Option<&str>,
        cancel: bool,
    ) -> Result<serde_json::Value, ProtocolError> {
        let interface = self
            .interface_for(agent_id, "prompt.queue.read")?
            .ok_or_else(|| {
                ProtocolError::new(
                    "queue_unsupported",
                    "Target does not expose prompt.queue.read",
                )
            })?;
        crate::agent_interface::queue_request(agent_id, &interface, submission_id, cancel).await
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
        let agent = self.get(agent_id)?;
        let is_bridge = agent
            .lock()
            .map_err(|_| ProtocolError::new("state_error", "agent state lock poisoned"))?
            .info
            .kind
            == "bridge";
        if is_bridge {
            if text.len() > BRIDGE_PROMPT_MAX_BYTES {
                return Err(ProtocolError::new(
                    "agent_prompt_too_large",
                    "bridge agent prompt exceeds 64 KiB",
                ));
            }
            let info = {
                let mut agent = agent
                    .lock()
                    .map_err(|_| ProtocolError::new("state_error", "agent state lock poisoned"))?;
                if agent.prompt_queue.len() >= BRIDGE_PROMPT_QUEUE_LIMIT {
                    return Err(ProtocolError::new(
                        "agent_prompt_queue_full",
                        "bridge agent prompt queue is full",
                    ));
                }
                agent.prompt_queue.push_back(AgentPrompt {
                    prompt_id: operation_id.to_string(),
                    text: text.to_string(),
                    created_at: Utc::now(),
                });
                agent.info.status = AgentStatus::Working;
                agent.info.updated_at = Utc::now();
                agent.info.clone()
            };
            let _ = self.inner.events.send(info.clone());
            return Ok(info);
        }
        if let Some(interface) = self.interface_for(agent_id, "prompt.submit")? {
            crate::agent_interface::submit_prompt(agent_id, &interface, operation_id, text).await?;
            return self.update_agent_status(agent_id, AgentStatus::Working);
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

    pub async fn abort(
        &self,
        operation_id: &str,
        agent_id: &str,
    ) -> Result<AgentInfo, ProtocolError> {
        let interface = self.interface_for(agent_id, "abort")?.ok_or_else(|| {
            ProtocolError::new(
                "agent_interface_capability_unavailable",
                "Agent does not expose abort",
            )
        })?;
        crate::agent_interface::abort(agent_id, &interface, operation_id).await?;
        let agent = self.get(agent_id)?;
        agent
            .lock()
            .map_err(|_| ProtocolError::new("state_error", "agent state lock poisoned"))
            .map(|guard| guard.info.clone())
    }

    pub async fn transcript(
        &self,
        agent_id: &str,
        cursor: Option<&str>,
        limit: Option<usize>,
    ) -> Result<AgentTranscriptResponse, ProtocolError> {
        let interface = self
            .interface_for(agent_id, "transcript.read")?
            .ok_or_else(|| {
                ProtocolError::new(
                    "agent_interface_capability_unavailable",
                    "Agent does not expose transcript.read",
                )
            })?;
        crate::agent_interface::transcript(agent_id, &interface, cursor, limit).await
    }

    pub async fn register_interface(
        &self,
        agent_id: &str,
        request: RegisterAgentInterfaceRequest,
    ) -> Result<AgentInterfaceDescriptor, ProtocolError> {
        if request.protocol != AGENT_INTERFACE_PROTOCOL_V1 {
            return Err(ProtocolError::new(
                "agent_interface_protocol_unsupported",
                format!("unsupported Agent Interface protocol {}", request.protocol),
            ));
        }
        if request.instance_id.trim().is_empty() || request.port == 0 {
            return Err(ProtocolError::new(
                "invalid_request",
                "Agent Interface requires a non-empty instance ID and non-zero port",
            ));
        }
        let mut capabilities = request.capabilities;
        capabilities.sort();
        capabilities.dedup();
        if capabilities.iter().any(|capability| {
            capability.is_empty()
                || !capability
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        }) {
            return Err(ProtocolError::new(
                "invalid_request",
                "Agent Interface capabilities must use letters, numbers, dot, dash, or underscore",
            ));
        }
        let ui_path = request
            .ui_path
            .as_deref()
            .map(validate_interface_ui_path)
            .transpose()?;
        let descriptor = AgentInterfaceDescriptor {
            protocol: request.protocol,
            instance_id: request.instance_id,
            port: request.port,
            capabilities,
            ui_path,
            registered_at: Utc::now(),
        };
        self.validate_interface_manifest(agent_id, &descriptor)
            .await?;
        let agent = self.get(agent_id)?;
        let info = {
            let mut agent = agent
                .lock()
                .map_err(|_| ProtocolError::new("state_error", "agent state lock poisoned"))?;
            let start_monitor = agent
                .info
                .interface
                .as_ref()
                .is_none_or(|current| current.instance_id != descriptor.instance_id);
            let publish_changed = agent.info.interface.as_ref().is_none_or(|current| {
                current.protocol != descriptor.protocol
                    || current.instance_id != descriptor.instance_id
                    || current.port != descriptor.port
                    || current.capabilities != descriptor.capabilities
                    || current.ui_path != descriptor.ui_path
            });
            agent.info.interface = Some(descriptor.clone());
            if publish_changed {
                agent.info.updated_at = Utc::now();
            }
            (agent.info.clone(), start_monitor, publish_changed)
        };
        self.cache_interface(&info.0);
        if info.2 {
            let _ = self.inner.events.send(info.0);
        }
        if info.1 {
            self.start_interface_status_monitor(agent_id.to_string(), descriptor.clone());
        }
        Ok(descriptor)
    }

    async fn validate_interface_manifest(
        &self,
        agent_id: &str,
        descriptor: &AgentInterfaceDescriptor,
    ) -> Result<(), ProtocolError> {
        if descriptor.protocol != AGENT_INTERFACE_PROTOCOL_V1
            || descriptor.instance_id.trim().is_empty()
            || descriptor.port == 0
            || descriptor
                .ui_path
                .as_deref()
                .map(validate_interface_ui_path)
                .transpose()?
                != descriptor.ui_path
            || descriptor
                .capabilities
                .windows(2)
                .any(|pair| pair[0] >= pair[1])
            || descriptor.capabilities.iter().any(|capability| {
                capability.is_empty()
                    || !capability.bytes().all(|byte| {
                        byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-')
                    })
            })
        {
            return Err(ProtocolError::new(
                "invalid_agent_interface_cache",
                "cached Agent Interface descriptor is invalid",
            ));
        }
        let manifest = crate::agent_interface::manifest(agent_id, descriptor).await?;
        let mut manifest_capabilities = manifest.capabilities;
        manifest_capabilities.sort();
        manifest_capabilities.dedup();
        if manifest.protocol != descriptor.protocol
            || manifest.instance_id != descriptor.instance_id
            || manifest_capabilities != descriptor.capabilities
            || manifest.ui_path != descriptor.ui_path
        {
            return Err(ProtocolError::new(
                "agent_interface_manifest_mismatch",
                "Agent Interface manifest does not match its registration request",
            ));
        }
        Ok(())
    }

    pub fn clear_interface(
        &self,
        agent_id: &str,
    ) -> Result<Option<AgentInterfaceDescriptor>, ProtocolError> {
        let agent = self.get(agent_id)?;
        let (descriptor, info) = {
            let mut agent = agent
                .lock()
                .map_err(|_| ProtocolError::new("state_error", "agent state lock poisoned"))?;
            let descriptor = agent.info.interface.take();
            agent.info.updated_at = Utc::now();
            (descriptor, agent.info.clone())
        };
        self.remove_cached_interface(agent_id);
        let _ = self.inner.events.send(info);
        Ok(descriptor)
    }

    pub fn interface(
        &self,
        agent_id: &str,
    ) -> Result<Option<AgentInterfaceDescriptor>, ProtocolError> {
        let agent = self.get(agent_id)?;
        let interface = agent
            .lock()
            .map_err(|_| ProtocolError::new("state_error", "agent state lock poisoned"))?
            .info
            .interface
            .clone();
        Ok(interface)
    }

    fn interface_for(
        &self,
        agent_id: &str,
        capability: &str,
    ) -> Result<Option<AgentInterfaceDescriptor>, ProtocolError> {
        Ok(self
            .interface(agent_id)?
            .filter(|interface| interface.supports(capability)))
    }

    fn update_agent_status(
        &self,
        agent_id: &str,
        status: AgentStatus,
    ) -> Result<AgentInfo, ProtocolError> {
        let agent = self.get(agent_id)?;
        let (info, changed) = {
            let mut agent = agent
                .lock()
                .map_err(|_| ProtocolError::new("state_error", "agent state lock poisoned"))?;
            let changed = agent.info.status != status;
            agent.info.status = status;
            if changed {
                agent.info.updated_at = Utc::now();
            }
            (agent.info.clone(), changed)
        };
        if changed {
            let _ = self.inner.events.send(info.clone());
        }
        Ok(info)
    }

    fn start_interface_status_monitor(
        &self,
        agent_id: String,
        descriptor: AgentInterfaceDescriptor,
    ) {
        let runtime = self.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(1));
            let mut consecutive_failures = 0_u8;
            loop {
                interval.tick().await;
                let current = match runtime.interface(&agent_id) {
                    Ok(Some(current)) if current.instance_id == descriptor.instance_id => current,
                    _ => break,
                };
                match crate::agent_interface::status(&agent_id, &current).await {
                    Ok(status) => {
                        consecutive_failures = 0;
                        let terminal = runtime
                            .get(&agent_id)
                            .ok()
                            .and_then(|agent| {
                                agent
                                    .lock()
                                    .ok()
                                    .map(|agent| agent.info.status.is_terminal())
                            })
                            .unwrap_or(true);
                        if !terminal && current.supports("state.observe") {
                            let _ = runtime.update_agent_status(&agent_id, status.status);
                        }
                    }
                    Err(error) => {
                        consecutive_failures = consecutive_failures.saturating_add(1);
                        warn!(%agent_id, code = %error.code, "Agent Interface status probe failed");
                        if consecutive_failures >= AGENT_INTERFACE_FAILURE_LIMIT {
                            let _ = runtime.expire_interface(
                                &agent_id,
                                &current.instance_id,
                                current.registered_at,
                            );
                            break;
                        }
                    }
                }
            }
        });
    }

    fn expire_interface(
        &self,
        agent_id: &str,
        instance_id: &str,
        registered_at: chrono::DateTime<Utc>,
    ) -> Result<(), ProtocolError> {
        let agent = self.get(agent_id)?;
        let info = {
            let mut agent = agent
                .lock()
                .map_err(|_| ProtocolError::new("state_error", "agent state lock poisoned"))?;
            let should_expire = agent.info.interface.as_ref().is_some_and(|current| {
                current.instance_id == instance_id && current.registered_at == registered_at
            });
            if !should_expire {
                return Ok(());
            }
            agent.info.interface = None;
            agent.info.updated_at = Utc::now();
            agent.info.clone()
        };
        self.remove_cached_interface(agent_id);
        let _ = self.inner.events.send(info);
        Ok(())
    }

    fn cache_interface(&self, info: &AgentInfo) {
        let Some(pid) = info.pid else {
            self.remove_cached_interface(&info.agent_id);
            return;
        };
        let Some(interface) = info.interface.clone() else {
            self.remove_cached_interface(&info.agent_id);
            return;
        };
        if let Err(error) = self.inner.interface_cache.upsert(CachedAgentInterface {
            agent_id: info.agent_id.clone(),
            pid,
            started_at: info.started_at,
            interface,
        }) {
            warn!(agent_id = %info.agent_id, %error, "failed to persist Agent Interface cache");
        }
    }

    fn remove_cached_interface(&self, agent_id: &str) {
        if let Err(error) = self.inner.interface_cache.remove(agent_id) {
            warn!(%agent_id, %error, "failed to remove Agent Interface cache entry");
        }
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
        if agent.info.kind == "bridge" {
            return Err(ProtocolError::new(
                "agent_output_unavailable",
                "bridge agent output is not readable",
            ));
        }
        let text = select_lines(recent_text(&agent.text, OUTPUT_LIMIT_BYTES), lines);
        Ok(ReadAgentOutputResponse {
            agent_id: agent_id.to_string(),
            revision: agent.info.output_revision,
            text,
            truncated: agent.text.len() >= OUTPUT_LIMIT_BYTES,
        })
    }

    pub fn read_prompt_queue(
        &self,
        agent_id: &str,
        limit: Option<usize>,
    ) -> Result<AgentPromptQueueResponse, ProtocolError> {
        let agent = self.get(agent_id)?;
        let (prompts, remaining, info) = {
            let mut agent = agent
                .lock()
                .map_err(|_| ProtocolError::new("state_error", "agent state lock poisoned"))?;
            if agent.info.kind != "bridge" {
                return Err(ProtocolError::new(
                    "agent_prompt_queue_unavailable",
                    "prompt queue is available only for bridge agents",
                ));
            }
            let count = limit
                .unwrap_or(BRIDGE_PROMPT_READ_LIMIT)
                .min(BRIDGE_PROMPT_READ_LIMIT);
            let prompts: Vec<_> = agent.prompt_queue.drain(..count).collect();
            let remaining = agent.prompt_queue.len();
            if remaining == 0 && agent.info.status == AgentStatus::Working {
                agent.info.status = AgentStatus::Idle;
                agent.info.updated_at = Utc::now();
            }
            (prompts, remaining, agent.info.clone())
        };
        let _ = self.inner.events.send(info);
        Ok(AgentPromptQueueResponse {
            agent_id: agent_id.to_string(),
            prompts,
            remaining,
        })
    }

    pub async fn terminal_snapshot(
        &self,
        agent_id: &str,
        cursor: Option<&TerminalCursor>,
    ) -> Result<TerminalSnapshot, ProtocolError> {
        let response = self
            .inner
            .host
            .request(
                HostCommand::Read {
                    process_id: agent_id.to_string(),
                    cursor: cursor.map(|cursor| treer_host_protocol::OutputCursor {
                        stream_epoch: cursor.stream_epoch.clone(),
                        revision: cursor.revision,
                    }),
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
            stream_epoch: replay.stream_epoch.clone(),
            revision: replay.next_revision.saturating_sub(1),
            gap: replay.gap,
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
        let _guard = self.inner.recovery_lock.lock().await;
        self.persist_stopped_startup(agent_id)?;
        let inventory = self
            .inner
            .host
            .request(
                HostCommand::Sync {
                    cursors: Default::default(),
                },
                None,
            )
            .await
            .map_err(|error| protocol_error("host_error", error))?;
        let mut running = false;
        if let HostResponse::Synced { processes, .. } = inventory {
            for process in processes {
                if process.process_id == agent_id {
                    running = process.running;
                    self.upsert_process(process, None)?;
                }
            }
        }
        if !running {
            if let Ok(agent) = self.get(agent_id) {
                let info = agent
                    .lock()
                    .map_err(|_| ProtocolError::new("state_error", "agent lock poisoned"))?
                    .info
                    .clone();
                return Ok(info);
            }
        }
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
        if let Ok(current) = self.get(&process.process_id) {
            if let Ok(current) = current.lock() {
                if current.info.run_generation > metadata.run_generation
                    || (current.info.pid.is_some() && current.info.started_at > process.started_at)
                {
                    return Ok(current.info.clone());
                }
            }
        }
        if metadata.workspace_id != self.inner.workspace_id
            || metadata.server_id != self.inner.server_id
        {
            return Err(ProtocolError::new(
                "host_identity_mismatch",
                "host process metadata belongs to another controller",
            ));
        }
        if process.running {
            if let Some(port) = metadata.http_proxy_port {
                self.inner.workload_proxies.ensure(
                    &metadata.agent_id,
                    &metadata.workload_credential,
                    Some(port),
                )?;
            }
        } else {
            self.inner.workload_proxies.remove(&metadata.agent_id);
        }
        let inventory_only = restored_text.is_none();
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
        let interface = process
            .running
            .then(|| self.interface(&metadata.agent_id).ok().flatten())
            .flatten();
        let info = AgentInfo {
            path: None,
            agent_id: metadata.agent_id.clone(),
            run_generation: metadata.run_generation,
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
            interface,
        };
        let mut agent = ControllerAgent {
            stream_epoch: process.stream_epoch,
            info: info.clone(),
            workload_credential: metadata.workload_credential,
            text,
            prompt_queue: VecDeque::new(),
            bracketed_paste: process.bracketed_paste,
            last_output: Instant::now(),
            last_metadata_event: Instant::now(),
        };
        let info = {
            let mut agents =
                self.inner.agents.write().map_err(|_| {
                    ProtocolError::new("state_error", "agent registry lock poisoned")
                })?;
            if let Some(current) = agents.get(&metadata.agent_id) {
                let mut current = current
                    .lock()
                    .map_err(|_| ProtocolError::new("state_error", "agent state lock poisoned"))?;
                if current.info.run_generation > agent.info.run_generation
                    || (current.info.pid.is_some()
                        && current.info.started_at > agent.info.started_at)
                {
                    return Ok(current.info.clone());
                }
                if current.stream_epoch == agent.stream_epoch
                    && current.info.run_generation == agent.info.run_generation
                {
                    // Inventory describes process state, not a replacement of
                    // its output. Keep the same-run cache, including exited
                    // output, and never rewind output that arrived after Sync.
                    if inventory_only || current.info.output_revision > agent.info.output_revision {
                        agent.text = std::mem::take(&mut current.text);
                        agent.info.output_revision =
                            agent.info.output_revision.max(current.info.output_revision);
                    }
                    agent.prompt_queue = std::mem::take(&mut current.prompt_queue);
                }
                // Update the existing Arc under its lock so an output event
                // cannot append to a detached cache while inventory replaces it.
                *current = agent;
                current.info.clone()
            } else {
                let info = agent.info.clone();
                agents.insert(metadata.agent_id, Arc::new(Mutex::new(agent)));
                info
            }
        };
        if !process.running {
            self.remove_cached_interface(&info.agent_id);
            if process.exit_code == Some(0) && info.kind != "app" {
                if let Some(mut entry) = self.inner.startup_store.get(&info.agent_id) {
                    if entry.spec.generation <= info.run_generation {
                        entry.spec.enabled = false;
                        self.inner
                            .startup_store
                            .upsert(entry)
                            .map_err(startup_store_error)?;
                    }
                }
            }
        }
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
        let terminal = status.is_terminal();
        agent.info.status = status;
        agent.info.updated_at = Utc::now();
        agent.info.exited_at = process.exited_at;
        agent.info.exit_code = process.exit_code;
        if terminal {
            agent.info.interface = None;
        }
        let info = agent.info.clone();
        drop(agent);
        if terminal {
            self.remove_cached_interface(&info.agent_id);
            self.inner.workload_proxies.remove(&info.agent_id);
        }
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
        let weak = Arc::downgrade(&self.inner);
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(1));
            loop {
                interval.tick().await;
                let Some(inner) = weak.upgrade() else {
                    break;
                };
                inner.file_transfers.expire();
            }
        });
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
                if agent.stream_epoch != chunk.stream_epoch {
                    return;
                }
                let previous_status = agent.info.status;
                let plain = strip_ansi_escapes::strip(&chunk.data);
                agent.text.push_str(&String::from_utf8_lossy(&plain));
                trim_text(&mut agent.text);
                agent.bracketed_paste = chunk.bracketed_paste;
                agent.last_output = Instant::now();
                agent.info.output_revision = chunk.revision;
                let interface_owns_status = agent
                    .info
                    .interface
                    .as_ref()
                    .is_some_and(|interface| interface.supports("state.observe"));
                if !agent.info.status.is_terminal() && !interface_owns_status {
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
        let generation = serde_json::from_str::<AgentMetadata>(&process.metadata)
            .map(|metadata| metadata.run_generation)
            .unwrap_or(0);
        // A restart persists the next generation before its spawn ACK/event.
        // An old exit during that window must not cancel the new running intent.
        if self
            .inner
            .startup_store
            .get(&process.process_id)
            .is_some_and(|entry| entry.spec.generation > generation)
        {
            return;
        }
        let mut new_run = false;
        if let Ok(agent) = self.get(&process.process_id) {
            if let Ok(agent) = agent.lock() {
                if agent.info.run_generation > generation
                    || (agent.info.pid.is_some() && agent.info.started_at > process.started_at)
                    || (agent.stream_epoch != process.stream_epoch
                        && agent.info.started_at == process.started_at)
                {
                    return;
                }
                new_run = generation > agent.info.run_generation
                    || (process.running && agent.stream_epoch != process.stream_epoch);
            }
        }
        if new_run {
            let _ = self.inner.process_events.send(process.clone());
            let _ = self.upsert_process(process, None);
            return;
        }
        if !process.running
            && process.exit_code == Some(0)
            && self
                .inner
                .startup_store
                .get(&process.process_id)
                .is_some_and(|entry| entry.spec.kind != "app")
        {
            if let Some(mut entry) = self.inner.startup_store.get(&process.process_id) {
                entry.spec.enabled = false;
                if let Err(error) = self.inner.startup_store.upsert(entry) {
                    warn!(%error, "failed to persist completed Agent state");
                }
            }
        }
        if !process.running {
            self.inner.workload_proxies.remove(&process.process_id);
        }
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
            agent.info.interface = None;
        }
        agent.info.updated_at = Utc::now();
        let info = agent.info.clone();
        drop(agent);
        if !process.running {
            self.remove_cached_interface(&info.agent_id);
        }
        let _ = self.inner.events.send(info);
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
                    let interface_owns_status = agent
                        .info
                        .interface
                        .as_ref()
                        .is_some_and(|interface| interface.supports("state.observe"));
                    if !interface_owns_status
                        && agent.last_output.elapsed() >= QUIET_IDLE_AFTER
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

async fn read_bounded<R>(mut reader: R, limit: usize) -> std::io::Result<(Vec<u8>, bool)>
where
    R: AsyncRead + Unpin,
{
    let mut retained = Vec::with_capacity(limit.min(8192));
    let mut buffer = [0_u8; 8192];
    let mut truncated = false;
    loop {
        let read = reader.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        let remaining = limit.saturating_sub(retained.len());
        retained.extend_from_slice(&buffer[..read.min(remaining)]);
        truncated |= read > remaining;
    }
    Ok((retained, truncated))
}

async fn finish_bounded_read(
    task: &mut tokio::task::JoinHandle<std::io::Result<(Vec<u8>, bool)>>,
) -> (Vec<u8>, bool) {
    match tokio::time::timeout(Duration::from_secs(2), &mut *task).await {
        Ok(Ok(Ok(result))) => result,
        _ => {
            task.abort();
            (Vec::new(), true)
        }
    }
}

fn validate_upload_id(upload_id: &str) -> Result<(), ProtocolError> {
    if upload_id.is_empty()
        || upload_id.len() > 80
        || !upload_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    {
        return Err(ProtocolError::new(
            "invalid_machine_upload",
            "upload ID is invalid",
        ));
    }
    Ok(())
}

fn validate_upload_file_name(file_name: &str) -> Result<(), ProtocolError> {
    let mut components = Path::new(file_name).components();
    let valid = matches!(components.next(), Some(Component::Normal(_)))
        && components.next().is_none()
        && !file_name.contains(['/', '\\'])
        && !file_name.chars().any(char::is_control);
    if !valid {
        return Err(ProtocolError::new(
            "invalid_machine_file_name",
            "file_name must be one file name without path separators",
        ));
    }
    Ok(())
}

fn machine_relative_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
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

#[cfg(test)]
#[path = "controller_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "controller_exec_environment_tests.rs"]
mod exec_environment_tests;
