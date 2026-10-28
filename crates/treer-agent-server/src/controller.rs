use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
#[cfg(windows)]
use std::path::Component;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

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
use crate::private_state::{PrivateStateRoot, QuotaLedger, QuotaLimits};
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

enum StartupRestoreOutcome {
    Started(AgentInfo),
    Attached(AgentInfo),
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

/// Per-Agent private state as the Controller sees it for one workload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrivateStateStatus {
    Available(PathBuf),
    /// Missing (never started or purged) or an I/O failure.
    Unavailable,
}

impl PrivateStateStatus {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Available(_) => "available",
            Self::Unavailable => "agent_private_state_unavailable",
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PrivateStateReport {
    pub status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<crate::private_state::AgentUsage>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PrivateStateReservation {
    pub reservation_id: String,
    pub bytes: u64,
    pub expires_at: chrono::DateTime<Utc>,
}

/// Interface registrations an Agent may perform per window; a Controller
/// restart re-registers each Agent once and a Proxy reconnect does not
/// re-register at all, so normal operation stays far below the limit.
pub const INTERFACE_REGISTRATIONS_PER_WINDOW: u32 = 10;
pub const INTERFACE_REGISTRATION_WINDOW: Duration = Duration::from_secs(60);
/// Distinct Agents tracked at once; the oldest window is evicted beyond it so
/// an enumeration cannot grow memory.
const INTERFACE_REGISTRATION_TRACKED_AGENTS: usize = 1024;

#[derive(Default)]
struct InterfaceRegistrationLimiter {
    windows: HashMap<String, RegistrationWindow>,
}

struct RegistrationWindow {
    started: Instant,
    count: u32,
    warned: bool,
}

impl InterfaceRegistrationLimiter {
    /// Counts one registration for the authenticated Agent and says whether
    /// it is within the window; `now` is a monotonic instant. The first
    /// refusal in a window is reported once.
    fn admit(&mut self, agent_id: &str, now: Instant) -> Result<(), bool> {
        self.windows
            .retain(|_, window| now.duration_since(window.started) < INTERFACE_REGISTRATION_WINDOW);
        if !self.windows.contains_key(agent_id)
            && self.windows.len() >= INTERFACE_REGISTRATION_TRACKED_AGENTS
        {
            if let Some(oldest) = self
                .windows
                .iter()
                .min_by_key(|(_, window)| window.started)
                .map(|(id, _)| id.clone())
            {
                self.windows.remove(&oldest);
            }
        }
        let window = self
            .windows
            .entry(agent_id.to_string())
            .or_insert(RegistrationWindow {
                started: now,
                count: 0,
                warned: false,
            });
        if window.count >= INTERFACE_REGISTRATIONS_PER_WINDOW {
            let first = !window.warned;
            window.warned = true;
            return Err(first);
        }
        window.count += 1;
        Ok(())
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
    /// Controller-derived `agent-private` directory (below the Treer state dir).
    pub private_state_root: PathBuf,
    pub root: PathBuf,
}

struct ControllerInner {
    // No field, setter, wire/config option or read of this value in production.
    // Only isolated Windows exec unit-test fixtures opt in (never real Hosts).
    #[cfg(all(test, windows))]
    hosted_test_module_cache: Mutex<Option<std::ffi::OsString>>,
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
    /// `Err(code)` when the root could not be opened safely: private state is
    /// then unavailable for every Agent, but the Controller keeps running.
    private_state: Result<PrivateStateRoot, &'static str>,
    private_state_ledger: Mutex<QuotaLedger>,
    recovery_lock: tokio::sync::Mutex<()>,
    uploads: crate::machine_upload::Uploads,
    /// Per-Agent interface (re)registration windows (#164 Controller item,
    /// security 3555 P3-2): keyed by the authenticated Agent id, bounded in
    /// size, one warning per window.
    interface_registrations: Mutex<InterfaceRegistrationLimiter>,
    file_transfers: crate::machine_file_transfer::Transfers,
    agents: RwLock<HashMap<String, Arc<Mutex<ControllerAgent>>>>,
    events: broadcast::Sender<AgentInfo>,
    terminal_events: broadcast::Sender<TerminalOutput>,
    process_events: broadcast::Sender<HostProcessInfo>,
    virtual_hosts: RwLock<Option<VirtualNetworkHostsSnapshot>>,
    proxy_link: RwLock<ProxyLinkStatus>,
    workload_proxies: crate::workload_proxy::WorkloadProxies,
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
    pub(crate) fn host(&self) -> HostClient {
        self.inner.host.clone()
    }

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
        // Only an invalid server identity fails Controller start. An
        // unavailable root degrades private state to "unavailable" for every
        // Agent (variable omitted, quota refused) while recovery proceeds.
        let private_state = match PrivateStateRoot::open(
            &config.private_state_root,
            &config.server_id,
        ) {
            Ok(root) => Ok(root),
            Err(error) if error.code == "agent_private_state_invalid_identity" => {
                return Err(error)
            }
            Err(error) => {
                warn!(code = %error.code, "Agent private state root is unavailable; private state disabled");
                Err("agent_private_state_unavailable")
            }
        };
        // Crash/restart accounting: measure what is on disk now; in-flight
        // reservations of a previous Controller are gone and any leftover
        // temporary files count as usage until the owning launcher sweeps them.
        let mut ledger = QuotaLedger::new(QuotaLimits::default());
        if let Ok(root) = &private_state {
            match root.agent_ids() {
                Ok(ids) => ledger.replace_inventory(
                    ids.into_iter()
                        .map(|agent_id| {
                            let measurement = root.measure(&agent_id).ok();
                            (agent_id, measurement)
                        })
                        .collect(),
                ),
                Err(error) => {
                    // Unknown existing usage must not read as zero: refuse new
                    // reservations until a later enumeration succeeds.
                    warn!(%error, "Agent private state directories could not be listed; reservations paused");
                    ledger.mark_inventory_incomplete();
                }
            }
        }
        let protected_paths: Vec<PathBuf> = [
            Some(config.startup_store_path.clone()),
            Some(config.interface_cache_path.clone()),
            Some(config.private_state_root.clone()),
            treer_platform::config_directory(),
        ]
        .into_iter()
        .flatten()
        .chain(config.protected_paths.clone())
        .collect();
        let runtime_result = Self {
            inner: Arc::new(ControllerInner {
                #[cfg(all(test, windows))]
                hosted_test_module_cache: Mutex::new(None),
                uploads: crate::machine_upload::Uploads::new(
                    config.root.clone(),
                    config.server_id.clone(),
                    protected_paths.clone(),
                ),
                interface_registrations: Mutex::new(InterfaceRegistrationLimiter::default()),
                file_transfers: crate::machine_file_transfer::Transfers::new(
                    config.root.clone(),
                    config.workspace_id.clone(),
                    protected_paths,
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
                private_state,
                private_state_ledger: Mutex::new(ledger),
                recovery_lock: tokio::sync::Mutex::new(()),
                agents: RwLock::new(HashMap::new()),
                events: agent_events,
                terminal_events,
                process_events,
                virtual_hosts: RwLock::new(None),
                proxy_link: RwLock::new(ProxyLinkStatus::default()),
            }),
        };
        let runtime = runtime_result;
        // Sweep staging files recorded by a previous Controller instance now,
        // not on the first upload command (#166).
        runtime.inner.uploads.recover_at_startup();
        let replays: HashMap<_, _> = replay
            .into_iter()
            .map(|replay| (replay.process_id.clone(), replay))
            .collect();
        for mut process in processes {
            if !process.running
                && runtime
                    .inner
                    .startup_store
                    .is_denied(&process.process_id)
                    .map_err(startup_store_error)?
            {
                continue;
            }
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

    fn require_not_denied(&self, agent_id: &str) -> Result<(), ProtocolError> {
        if self
            .inner
            .startup_store
            .is_denied(agent_id)
            .map_err(startup_store_error)?
        {
            tracing::warn!(agent_id, "refused launch for a fenced Agent identity");
            return Err(ProtocolError::new(
                "agent_identity_denied",
                "Agent identity was permanently denied",
            ));
        }
        Ok(())
    }

    pub async fn fence_absent_agent_reservation(
        &self,
        agent_id: &str,
    ) -> Result<(), ProtocolError> {
        if agent_id.is_empty()
            || agent_id.len() > 128
            || !agent_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
        {
            return Err(ProtocolError::new(
                "invalid_agent_identity",
                "A stable Agent ID is required",
            ));
        }
        let _guard = self.inner.recovery_lock.lock().await;
        let observed = ProtocolError::new(
            "agent_reservation_observed",
            "Agent has workload state; use normal lifecycle controls",
        );
        match self.get(agent_id) {
            Err(error) if error.code == "agent_not_found" => {}
            Err(error) => return Err(error),
            Ok(_) => return Err(observed),
        }
        if self.inner.startup_store.get(agent_id).is_some() {
            return Err(observed);
        }
        let inventory = self
            .inner
            .host
            .sync(Default::default())
            .await
            .map_err(|_| {
                ProtocolError::new("host_unavailable", "Host absence could not be confirmed")
            })?;
        let HostResponse::Synced { processes, .. } = inventory else {
            return Err(ProtocolError::new(
                "host_protocol_error",
                "Host absence could not be confirmed",
            ));
        };
        if processes
            .iter()
            .any(|process| process.process_id == agent_id)
        {
            return Err(observed);
        }
        self.inner
            .startup_store
            .deny_absent(agent_id)
            .map_err(startup_store_error)
    }

    pub async fn create(
        &self,
        operation_id: &str,
        agent_id: String,
        workload_credential: String,
        request: CreateAgentRequest,
    ) -> Result<AgentInfo, ProtocolError> {
        let _guard = self.inner.recovery_lock.lock().await;
        self.require_not_denied(&agent_id)?;
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
        let mut env = self.process_environment(Some((&agent_id, &workload_credential)));
        let command = resolve_agent_command(&launch.command, &env).map_err(|mut error| {
            if error.code == "command_not_found"
                && self.inner.startup_store.get(&agent_id).is_none()
                && self
                    .get(&agent_id)
                    .is_err_and(|error| error.code == "agent_not_found")
            {
                error.message = "Required executable is not installed on this machine".into();
                error.details = Some(Box::new(treer_protocol::AgentCreationErrorDetails {
                    command: treer_protocol::agent_install_command_name(&launch.command),
                    machine_id: None,
                    agent_id: Some(agent_id.clone()),
                    creation_state: treer_protocol::AgentCreationFailureState::NotStarted,
                }));
            }
            error
        })?;
        // Persist the resolved launch, rather than a mutable profile reference,
        // before asking the Host to start the workload.
        if self.inner.startup_store.get(&agent_id).is_none() {
            self.inner
                .startup_store
                .upsert(StoredAgentStartup {
                    app_revision: 0,
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
        self.inject_private_state(&mut env, &agent_id)?;
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
        crate::workload_proxy::inject_environment(&mut env, proxy_port);
        let response = self
            .inner
            .host
            .request(
                HostCommand::Spawn {
                    request: HostSpawnRequest {
                        process_id: agent_id.clone(),
                        command,
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
        self.ensure_app_lifecycle(
            operation_id,
            agent_id,
            workload_credential,
            request,
            enabled,
            start_missing,
            None,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn ensure_app_lifecycle(
        &self,
        operation_id: &str,
        agent_id: String,
        workload_credential: Option<String>,
        request: CreateAgentRequest,
        enabled: bool,
        start_missing: bool,
        revision: Option<u64>,
    ) -> Result<AgentInfo, ProtocolError> {
        let _guard = self.inner.recovery_lock.lock().await;
        self.require_not_denied(&agent_id)?;
        if let Some(revision) = revision {
            if revision == 0 || request.kind != "app" {
                return Err(ProtocolError::new(
                    "invalid_app_startup",
                    "versioned lifecycle declaration must target an App",
                ));
            }
            let replacement = ReplaceAgentStartupRequest {
                cwd: Some(request.cwd.clone()),
                command: request.args.first().cloned().unwrap_or_default(),
                args: request.args.iter().skip(1).cloned().collect(),
            };
            if self.inner.startup_store.get(&agent_id).is_some() {
                let (_, changed) =
                    self.save_app_startup_locked(&agent_id, replacement, revision)?;
                if changed && enabled && start_missing {
                    return self.start_locked(operation_id, &agent_id, true).await;
                }
            } else {
                replacement.validate()?;
                self.resolve_machine_directory(&request.cwd)?;
            }
        }
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
                    app_revision: revision.unwrap_or(0),
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
                    .map_err(host_stop_error)?;
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

    fn checked_app_startup(
        &self,
        agent_id: &str,
        request: &ReplaceAgentStartupRequest,
    ) -> Result<StoredAgentStartup, ProtocolError> {
        request.validate()?;
        let entry = self.inner.startup_store.get(agent_id).ok_or_else(|| {
            ProtocolError::new(
                "startup_not_found",
                "App has no saved startup; retry after lifecycle synchronization",
            )
        })?;
        if entry.spec.kind != "app"
            || entry.workspace_id != self.inner.workspace_id
            || entry.spec.server_id != self.inner.server_id
        {
            return Err(ProtocolError::new(
                "app_startup_mismatch",
                "startup does not belong to this App Controller",
            ));
        }
        if !entry.workload_credential.is_empty()
            && std::iter::once(&request.command)
                .chain(request.args.iter())
                .chain(request.cwd.iter())
                .any(|value| value.contains(&entry.workload_credential))
        {
            return Err(ProtocolError::new(
                "invalid_agent_startup",
                "startup must not contain the workload credential",
            ));
        }
        self.resolve_machine_directory(request.cwd.as_deref().unwrap_or(&entry.spec.cwd))?;
        Ok(entry)
    }

    pub async fn check_app_startup(
        &self,
        agent_id: &str,
        request: ReplaceAgentStartupRequest,
    ) -> Result<(), ProtocolError> {
        let _guard = self.inner.recovery_lock.lock().await;
        self.checked_app_startup(agent_id, &request).map(|_| ())
    }

    pub async fn update_app_startup(
        &self,
        operation_id: &str,
        agent_id: &str,
        request: ReplaceAgentStartupRequest,
        revision: u64,
    ) -> Result<AgentStartupSpec, ProtocolError> {
        let _guard = self.inner.recovery_lock.lock().await;
        let (entry, changed) = self.save_app_startup_locked(agent_id, request, revision)?;
        if changed && entry.spec.enabled {
            self.start_locked(operation_id, agent_id, true).await?;
        }
        self.get_startup(agent_id)
    }

    fn save_app_startup_locked(
        &self,
        agent_id: &str,
        request: ReplaceAgentStartupRequest,
        revision: u64,
    ) -> Result<(StoredAgentStartup, bool), ProtocolError> {
        let mut entry = self.checked_app_startup(agent_id, &request)?;
        if revision < entry.app_revision || revision == 0 {
            return Err(ProtocolError::new(
                "stale_app_startup",
                "a newer App startup specification is already saved",
            ));
        }
        if revision == entry.app_revision {
            // Duplicate delivery must not allocate another generation or restart.
            if entry.spec.command != request.command
                || entry.spec.args != request.args
                || request
                    .cwd
                    .as_ref()
                    .is_some_and(|cwd| cwd != &entry.spec.cwd)
            {
                return Err(ProtocolError::new(
                    "app_revision_conflict",
                    "App revision has conflicting launch fields",
                ));
            }
            return Ok((entry, false));
        }
        entry.spec.command = request.command;
        entry.spec.args = request.args;
        if let Some(cwd) = request.cwd {
            entry.spec.cwd = cwd;
        }
        entry.app_revision = revision;
        entry.initial_writes.clear();
        self.inner
            .startup_store
            .upsert(entry.clone())
            .map_err(startup_store_error)?;
        Ok((entry, true))
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
                    app_revision: 0,
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
                app_revision: 0,
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
        self.restore_startup_outcomes(active_agent_ids)
            .await
            .into_iter()
            .map(|(id, result)| {
                (
                    id,
                    result.map(|outcome| match outcome {
                        StartupRestoreOutcome::Started(info)
                        | StartupRestoreOutcome::Attached(info) => info,
                    }),
                )
            })
            .collect()
    }

    async fn restore_startup_outcomes(
        &self,
        active_agent_ids: &[String],
    ) -> Vec<(String, Result<StartupRestoreOutcome, ProtocolError>)> {
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
                            results.push((
                                agent_id,
                                Ok(StartupRestoreOutcome::Attached(agent.info.clone())),
                            ));
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
                    .map(|()| StartupRestoreOutcome::Started(info))
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

    /// Identities the Proxy must confirm: startup entries plus every private
    /// state directory under this server, so deletions confirmed while this
    /// Controller was offline are still cleaned up.
    pub fn startup_validation_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self
            .inner
            .startup_store
            .entries()
            .into_iter()
            .filter(|entry| {
                entry.workspace_id == self.inner.workspace_id
                    && entry.spec.server_id == self.inner.server_id
            })
            .map(|entry| entry.spec.agent_id)
            .collect();
        match self
            .inner
            .private_state
            .as_ref()
            .map(PrivateStateRoot::agent_ids)
        {
            Ok(Ok(private)) => ids.extend(private),
            Ok(Err(error)) => {
                warn!(%error, "Agent private state directories could not be listed")
            }
            Err(_) => {}
        }
        ids.sort();
        ids.dedup();
        ids
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
        // Capture the actual action under recovery_lock. Host timestamps have
        // millisecond wire precision and cannot order a spawn against this call
        // (nor can wall clocks reliably order concurrent recovery requests).
        let results = self.restore_startup_outcomes(active).await;
        let mut items: Vec<_> = results
            .into_iter()
            .map(|(id, result)| {
                let (state, message) = match result {
                    Ok(StartupRestoreOutcome::Started(_)) => (RuntimeRestoreState::Started, None),
                    Ok(StartupRestoreOutcome::Attached(_)) => (RuntimeRestoreState::Attached, None),
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
        let mut env = self.process_environment(Some((agent_id, &entry.workload_credential)));
        let command = resolve_agent_command(&entry.spec.command, &env)?;
        self.inject_private_state(&mut env, agent_id)?;
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
                        command,
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

    /// Capabilities this Controller advertises on registration: the static
    /// list plus features whose embedded data validated on this machine.
    pub fn advertised_capabilities(&self) -> Vec<String> {
        let mut capabilities: Vec<String> = treer_protocol::CONTROLLER_CAPABILITIES
            .iter()
            .map(|value| (*value).to_string())
            .collect();
        // Chunked uploads (#166) are advertised only where the Controller also
        // accepts the commands (#171 for Windows); the dispatch refuses any
        // command whose required capability is not advertised.
        if !crate::machine_upload::chunked_upload_supported() {
            capabilities.retain(|value| value != treer_protocol::CAPABILITY_MACHINE_UPLOAD_V3);
        }
        if crate::controller_update::capability_available() {
            capabilities.push(treer_protocol::controller_update::CAPABILITY.to_string());
        }
        capabilities
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
        size: Option<u64>,
    ) -> Result<(), ProtocolError> {
        self.inner
            .uploads
            .begin(upload_id, directory, file_name, overwrite, size)
    }

    pub fn append_upload_chunk(
        &self,
        upload_id: &str,
        offset: Option<u64>,
        content_base64: &str,
    ) -> Result<u64, ProtocolError> {
        self.inner.uploads.chunk(upload_id, offset, content_base64)
    }

    pub fn commit_upload_part(
        &self,
        upload_id: &str,
        offset: u64,
        length: u64,
        sha256: &str,
    ) -> Result<serde_json::Value, ProtocolError> {
        self.inner
            .uploads
            .part_commit(upload_id, offset, length, sha256)
    }

    pub fn commit_upload(
        &self,
        upload_id: &str,
    ) -> Result<UploadMachineFileResponse, ProtocolError> {
        self.inner.uploads.commit(upload_id)
    }

    pub fn upload_result(&self, upload_id: &str) -> Result<serde_json::Value, ProtocolError> {
        self.inner.uploads.result(upload_id)
    }

    pub fn abort_upload(&self, upload_id: &str) -> Result<bool, ProtocolError> {
        self.inner.uploads.abort(upload_id)
    }

    /// Deletes staging files of abandoned uploads (#166); see `machine_upload`.
    pub fn reap_uploads(&self) -> usize {
        self.inner.uploads.reap()
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

    /// Inject the Controller-owned private state directory. Only an invalid
    /// identity fails the launch. An unavailable directory omits the variable
    /// with a fixed code: the launcher then keeps private-state features
    /// disabled, and the state stays visible through `private_state_status`.
    fn inject_private_state(
        &self,
        env: &mut BTreeMap<String, String>,
        agent_id: &str,
    ) -> Result<(), ProtocolError> {
        match self.private_state_status(agent_id, true)? {
            PrivateStateStatus::Available(directory) => {
                // `private_state_status` only reports Unicode-representable
                // paths as available, so this always succeeds.
                if let Some(value) = directory.to_str() {
                    env.insert(
                        crate::private_state::ENVIRONMENT_VARIABLE.to_string(),
                        value.to_string(),
                    );
                }
            }
            status => {
                warn!(
                    agent_id,
                    code = status.code(),
                    "Agent private state is not injected"
                );
            }
        }
        Ok(())
    }

    /// Derive the Agent's private state. `create` prepares missing Treer
    /// levels (process starts); otherwise an absent directory is reported as
    /// unavailable so the local API never recreates a purged directory.
    fn private_state_status(
        &self,
        agent_id: &str,
        create: bool,
    ) -> Result<PrivateStateStatus, ProtocolError> {
        let prepared = self.private_state_root().and_then(|root| {
            if create {
                root.prepare(agent_id)
            } else {
                root.verify_existing(agent_id)
            }
        });
        let directory = match prepared {
            Ok(directory) => directory,
            Err(error) if error.code == "agent_private_state_unavailable" => {
                return Ok(PrivateStateStatus::Unavailable)
            }
            Err(error) => return Err(error),
        };
        if directory.to_str().is_none() {
            // The environment carries UTF-8 only; a non-Unicode path is not
            // advertised and the launcher keeps private state disabled.
            warn!(
                agent_id,
                "Agent private state directory path is not valid Unicode"
            );
            return Ok(PrivateStateStatus::Unavailable);
        }
        if create {
            // A fresh start re-admits an identity that was purged earlier.
            let measurement = self
                .private_state_root()
                .and_then(|root| root.measure(agent_id))
                .ok();
            if let Ok(mut ledger) = self.inner.private_state_ledger.lock() {
                ledger.record(agent_id, measurement);
            }
        }
        Ok(PrivateStateStatus::Available(directory))
    }

    fn private_state_root(&self) -> Result<&PrivateStateRoot, ProtocolError> {
        self.inner.private_state.as_ref().map_err(|code| {
            ProtocolError::new(
                *code,
                "Agent private state root is unavailable on this Controller",
            )
        })
    }

    /// Private state is reported only for workloads this Controller knows.
    fn require_known_workload(&self, agent_id: &str) -> Result<(), ProtocolError> {
        if self.inner.startup_store.get(agent_id).is_none() {
            self.get(agent_id)?;
        }
        Ok(())
    }

    fn private_state_ledger(
        &self,
    ) -> Result<std::sync::MutexGuard<'_, QuotaLedger>, ProtocolError> {
        self.inner
            .private_state_ledger
            .lock()
            .map_err(|_| ProtocolError::new("state_error", "private state ledger lock poisoned"))
    }

    /// After a failed startup enumeration, retry the complete inventory before
    /// any admission decision; stays paused while the listing keeps failing.
    fn complete_private_state_inventory(&self) -> Result<(), ProtocolError> {
        if !self.private_state_ledger()?.inventory_incomplete() {
            return Ok(());
        }
        let root = self.private_state_root()?;
        let ids = root.agent_ids().map_err(|error| {
            warn!(%error, "Agent private state directories still cannot be listed; reservations stay paused");
            ProtocolError::new(
                "agent_private_state_unavailable",
                "Agent private state inventory is incomplete; reservations resume after a successful re-enumeration",
            )
        })?;
        let measured = ids
            .into_iter()
            .map(|agent_id| {
                let measurement = root.measure(&agent_id).ok();
                (agent_id, measurement)
            })
            .collect();
        self.private_state_ledger()?.replace_inventory(measured);
        Ok(())
    }

    /// Re-measure the caller's directory and refresh the ledger. Only the
    /// Controller's own measurement ever enters the accounting.
    fn refresh_private_state_measurement(&self, agent_id: &str) -> Result<(), ProtocolError> {
        self.complete_private_state_inventory()?;
        let measurement = self
            .private_state_root()
            .and_then(|root| root.measure(agent_id));
        let mut ledger = self.private_state_ledger()?;
        match measurement {
            Ok(measurement) => ledger.record(agent_id, Some(measurement)),
            Err(error) => {
                warn!(agent_id, code = %error.code, "Agent private state could not be measured");
                ledger.record(agent_id, None);
            }
        }
        Ok(())
    }

    /// Status and usage for the authenticated Agent only. Never lists other
    /// Agents; the global headroom is folded into `available_bytes`.
    pub fn private_state_report(
        &self,
        agent_id: &str,
    ) -> Result<PrivateStateReport, ProtocolError> {
        if self.private_state_ledger()?.is_purged(agent_id) {
            return Ok(PrivateStateReport {
                status: PrivateStateStatus::Unavailable.code(),
                usage: None,
            });
        }
        self.require_known_workload(agent_id)?;
        let status = self.private_state_status(agent_id, false)?;
        let usage = if matches!(status, PrivateStateStatus::Available(_)) {
            self.refresh_private_state_measurement(agent_id)?;
            Some(self.private_state_ledger()?.usage(agent_id))
        } else {
            None
        };
        Ok(PrivateStateReport {
            status: status.code(),
            usage,
        })
    }

    fn require_private_state_available(&self, agent_id: &str) -> Result<PathBuf, ProtocolError> {
        if self.private_state_ledger()?.is_purged(agent_id) {
            return Err(ProtocolError::new(
                "agent_private_state_unavailable",
                "Agent private state was removed after deletion",
            ));
        }
        self.require_known_workload(agent_id)?;
        match self.private_state_status(agent_id, false)? {
            PrivateStateStatus::Available(directory) => Ok(directory),
            status => Err(ProtocolError::new(
                status.code(),
                "Agent private state is not available; attachments stay disabled",
            )),
        }
    }

    /// Admission before the launcher writes `bytes` of new private content.
    pub fn reserve_private_state(
        &self,
        agent_id: &str,
        bytes: u64,
    ) -> Result<PrivateStateReservation, ProtocolError> {
        let directory = self.require_private_state_available(agent_id)?;
        self.refresh_private_state_measurement(agent_id)?;
        let free_bytes = treer_platform::available_disk_bytes(&directory).map_err(|error| {
            warn!(agent_id, %error, "free disk space for private state is unknown");
            ProtocolError::new(
                "agent_private_state_unavailable",
                "free disk space could not be determined; inspect the Controller log",
            )
        })?;
        let now = Instant::now();
        let mut ledger = self.private_state_ledger()?;
        let reservation = ledger.reserve(agent_id, bytes, free_bytes, now)?;
        let ttl = ledger.limits().reservation_ttl;
        Ok(PrivateStateReservation {
            reservation_id: reservation.id,
            bytes: reservation.bytes,
            expires_at: Utc::now() + chrono::Duration::from_std(ttl).unwrap_or_default(),
        })
    }

    /// Close a reservation after the launcher's content is durable, then let
    /// the Controller's own measurement decide whether the caps still hold.
    pub fn commit_private_state_reservation(
        &self,
        agent_id: &str,
        reservation_id: &str,
    ) -> Result<crate::private_state::AgentUsage, ProtocolError> {
        self.require_private_state_available(agent_id)?;
        self.private_state_ledger()?
            .commit(agent_id, reservation_id, Instant::now())?;
        self.refresh_private_state_measurement(agent_id)?;
        let ledger = self.private_state_ledger()?;
        ledger.check_caps(agent_id)?;
        Ok(ledger.usage(agent_id))
    }

    /// Idempotent release of the caller's own reservation.
    pub fn release_private_state_reservation(
        &self,
        agent_id: &str,
        reservation_id: &str,
    ) -> Result<bool, ProtocolError> {
        self.require_private_state_available(agent_id)?;
        let released =
            self.private_state_ledger()?
                .release(agent_id, reservation_id, Instant::now());
        self.refresh_private_state_measurement(agent_id)?;
        Ok(released)
    }

    /// Remove private state and the disabled startup entry of Agents whose
    /// permanent deletion the Proxy confirmed (`deleted_agents` tombstones).
    /// A still-running process is stopped first and its purge deferred to the
    /// next validation pass; nothing is forced while the Host reports it alive.
    pub async fn purge_deleted_private_state(
        &self,
        deleted_agent_ids: &[String],
    ) -> Vec<(String, Result<bool, ProtocolError>)> {
        let mut ids: Vec<&String> = deleted_agent_ids
            .iter()
            .filter(|id| crate::private_state::valid_identity(id))
            .collect();
        ids.sort();
        ids.dedup();
        if ids.is_empty() {
            return Vec::new();
        }
        let _guard = self.inner.recovery_lock.lock().await;
        let root = match self.private_state_root() {
            Ok(root) => root,
            Err(error) => {
                return ids
                    .into_iter()
                    .map(|id| (id.clone(), Err(error.clone())))
                    .collect()
            }
        };
        let inventory = match self.inner.host.sync(Default::default()).await {
            Ok(HostResponse::Synced { processes, .. }) => processes,
            Ok(_) | Err(_) => {
                let error = ProtocolError::new(
                    "host_unavailable",
                    "Host inventory is unavailable; private state purge deferred",
                );
                return ids
                    .into_iter()
                    .map(|id| (id.clone(), Err(error.clone())))
                    .collect();
            }
        };
        let mut results = Vec::with_capacity(ids.len());
        for agent_id in ids {
            let running = inventory
                .iter()
                .any(|process| process.process_id == *agent_id && process.running);
            if running {
                let stop = self
                    .inner
                    .host
                    .request(
                        HostCommand::Stop {
                            process_id: agent_id.clone(),
                        },
                        Some(format!("private-state-purge:{agent_id}")),
                    )
                    .await;
                match stop {
                    Ok(response) => {
                        let _ = self.process_response(response, AgentStatus::Exited);
                    }
                    Err(error) => {
                        warn!(%agent_id, %error, "stop before private state purge failed")
                    }
                }
                results.push((
                    agent_id.clone(),
                    Err(ProtocolError::new(
                        "agent_private_state_purge_deferred",
                        "the deleted Agent was still running; stop requested, purge retried later",
                    )),
                ));
                continue;
            }
            // A missing exit event must not turn cleanup into a force-delete
            // of a nonterminal local record. Refresh it from Host evidence.
            if let Some(process) = inventory.iter().find(|p| p.process_id == *agent_id) {
                let _ = self.upsert_process(process.clone(), None);
            }
            if self.get(agent_id).is_ok_and(|agent| {
                agent
                    .lock()
                    .map_or(true, |agent| !agent.info.status.is_terminal())
            }) {
                results.push((
                    agent_id.clone(),
                    Err(ProtocolError::new(
                        "agent_private_state_purge_deferred",
                        "the deleted Agent registry entry is not terminal; purge retried later",
                    )),
                ));
                continue;
            }
            let result = (|| {
                // Persist the fence before forgetting credentials in memory.
                // Host inventory can retain exited processes across reconnects.
                self.inner
                    .startup_store
                    .remove_deleted(agent_id)
                    .map_err(startup_store_error)?;
                self.inner
                    .agents
                    .write()
                    .map_err(|_| ProtocolError::new("state_error", "agent registry lock poisoned"))?
                    .remove(agent_id);
                self.inner.workload_proxies.remove(agent_id);
                self.remove_cached_interface(agent_id);
                root.purge(agent_id)
            })();
            if result.is_ok() {
                if let Ok(mut ledger) = self.inner.private_state_ledger.lock() {
                    ledger.forget(agent_id);
                }
            }
            results.push((agent_id.clone(), result));
        }
        results
    }

    fn machine_exec_environment(&self) -> BTreeMap<std::ffi::OsString, std::ffi::OsString> {
        let environment = treer_platform::machine_exec_environment();
        #[cfg(all(test, windows))]
        {
            let mut environment = environment;
            if std::env::var("GITHUB_ACTIONS").as_deref() == Ok("true")
                && std::env::var("CI").as_deref() == Ok("true")
            {
                if let Some(path) = self.inner.hosted_test_module_cache.lock().unwrap().as_ref() {
                    environment.insert("PSModuleAnalysisCachePath".into(), path.clone());
                }
            }
            environment
        }
        #[cfg(not(all(test, windows)))]
        environment
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
            origin_url: None,
            protocol: request.protocol,
            instance_id: request.instance_id,
            port: request.port,
            capabilities,
            ui_path,
            registered_at: Utc::now(),
        };
        let agent = self.get(agent_id)?;
        // Rate limit after authentication (the caller resolved the Agent from
        // its credential) and the cheap shape checks, but BEFORE the manifest
        // fetch from the Agent's port and before any mutation: a flood of
        // registrations costs no fetch once the window is spent, and an
        // excess registration leaves the existing one exactly as it was.
        {
            let mut limiter =
                self.inner.interface_registrations.lock().map_err(|_| {
                    ProtocolError::new("state_error", "registration limiter poisoned")
                })?;
            if let Err(first_refusal) = limiter.admit(agent_id, Instant::now()) {
                if first_refusal {
                    tracing::warn!(%agent_id, "Agent interface registrations exceed the per-minute limit; refusing until the window passes");
                }
                return Err(ProtocolError::new(
                    "agent_interface_rate_limited",
                    "too many Agent Interface registrations; retry after a minute",
                ));
            }
        }
        self.validate_interface_manifest(agent_id, &descriptor)
            .await?;
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
            // No live process and no local record: a launch that failed before
            // the Host spawned anything leaves only its saved declaration
            // (#150). Stop is idempotent, so report that workload as exited
            // instead of asking the Host to stop a process it never had; an
            // id with no declaration either is simply absent.
            return match self.inner.startup_store.get(agent_id) {
                Some(entry) => self.materialize_startup(&entry),
                None => Err(ProtocolError::new(
                    "agent_not_found",
                    "no live process or saved launch for workload",
                )),
            };
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
            .map_err(host_stop_error)?;
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
        self.require_not_denied(&process.process_id)?;
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
            // Serialize the final fence check and proxy/registry publication
            // with deletion cleanup; a delayed Host event cannot reinsert it.
            // Lock order is registry -> workload proxies. ensure synchronously
            // binds loopback and builds a client; it neither awaits nor calls
            // back into the Controller or takes the registry lock itself.
            self.require_not_denied(&metadata.agent_id)?;
            if process.running {
                if let Some(port) = metadata.http_proxy_port {
                    self.inner.workload_proxies.ensure(
                        &metadata.agent_id,
                        &agent.workload_credential,
                        Some(port),
                    )?;
                }
            } else {
                self.inner.workload_proxies.remove(&metadata.agent_id);
            }
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
                ControllerRuntime { inner }.reap_uploads();
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
#[path = "controller_private_state_tests.rs"]
pub(crate) mod private_state_tests;
#[cfg(test)]
#[path = "controller_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "controller_exec_environment_tests.rs"]
mod exec_environment_tests;
