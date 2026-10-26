use std::env;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::ffi::OsStr;
use std::fs;
use std::io::ErrorKind;
use std::net::{Ipv4Addr, SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use treer_host_protocol::HostDaemonConfig;
use treer_protocol::{MachineSupervision, MachineSupervisionMode};
use url::Url;
use uuid::Uuid;

#[path = "service_platform.rs"]
mod platforms;
use platforms::platform;

const FIRST_AUTOMATIC_PORT: u16 = 8790;
const UPDATE_DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(120);
const UPDATE_VALIDATION_TIMEOUT: Duration = Duration::from_secs(10);
const CONTROLLER_START_TIMEOUT: Duration = Duration::from_secs(15);
#[cfg(unix)]
const NOHUP_STOP_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ServiceMode {
    Auto,
    Systemd,
    Launchd,
    Nohup,
    Foreground,
    #[cfg(windows)]
    Detached,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum NetworkMode {
    Plain,
    Transparent,
    NativeExperimental,
    ProxyEnv,
}

impl std::fmt::Display for NetworkMode {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Plain => "plain",
            Self::Transparent => "transparent",
            Self::NativeExperimental => "native-experimental",
            Self::ProxyEnv => "proxy-env",
        })
    }
}

impl std::fmt::Display for ServiceMode {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Auto => "auto",
            Self::Systemd => "systemd",
            Self::Launchd => "launchd",
            Self::Nohup => "nohup",
            Self::Foreground => "foreground",
            #[cfg(windows)]
            Self::Detached => "detached",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ServiceManager {
    SystemdUser,
    Launchd,
    Nohup,
    Foreground,
    #[cfg(windows)]
    Detached,
}

impl std::fmt::Display for ServiceManager {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::SystemdUser => "systemd-user",
            Self::Launchd => "launchd",
            Self::Nohup => "nohup",
            Self::Foreground => "foreground",
            #[cfg(windows)]
            Self::Detached => "detached",
        })
    }
}

fn default_service_manager() -> ServiceManager {
    #[cfg(target_os = "linux")]
    {
        ServiceManager::SystemdUser
    }
    #[cfg(target_os = "macos")]
    {
        ServiceManager::Launchd
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        ServiceManager::Foreground
    }
}

#[derive(Debug)]
pub struct ServiceSelection {
    pub manager: ServiceManager,
    pub fallback_reason: Option<String>,
}

impl ServiceSelection {
    pub fn announce(&self) {
        if let Some(reason) = &self.fallback_reason {
            eprintln!("treer: warning: persistent user service unavailable: {reason}");
            match self.manager {
                ServiceManager::Nohup => eprintln!(
                    "treer: warning: falling back to nohup mode; the Host will not restart after exit or reboot"
                ),
                ServiceManager::Foreground => eprintln!(
                    "treer: warning: falling back to foreground mode; keep this terminal open"
                ),
                ServiceManager::SystemdUser | ServiceManager::Launchd => {}
                #[cfg(windows)]
                ServiceManager::Detached => {}
            }
        } else if self.manager == ServiceManager::Nohup {
            eprintln!(
                "treer: nohup service mode selected; the Host will not restart after exit or reboot"
            );
        } else if self.manager == ServiceManager::Foreground {
            eprintln!(
                "treer: foreground service mode selected; keep this terminal open or run the command under a process supervisor"
            );
        }
        #[cfg(windows)]
        if self.manager == ServiceManager::Detached {
            eprintln!("treer: Windows detached mode selected; run restore after Host exit or machine restart");
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceConfig {
    pub proxy: String,
    pub workspace: String,
    pub server_id: String,
    pub machine_token: String,
    #[serde(default)]
    pub operator_credential: String,
    pub root: PathBuf,
    pub listen: String,
    pub host_socket: PathBuf,
    pub install_hostname: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network_mode: Option<NetworkMode>,
    #[serde(default = "default_service_manager")]
    pub service_manager: ServiceManager,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_fallback_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MachineIdentity {
    pub installation_id: String,
    pub name: String,
}

impl MachineIdentity {
    pub fn new(name: String) -> Self {
        Self {
            installation_id: format!("mid_{}", Uuid::new_v4().simple()),
            name,
        }
    }

    fn load(path: &Path) -> Result<Self> {
        let bytes = fs::read(path)
            .with_context(|| format!("failed to read machine identity {}", path.display()))?;
        let identity: Self = serde_json::from_slice(&bytes)
            .with_context(|| format!("invalid machine identity {}", path.display()))?;
        validate_machine_identity(&identity)?;
        Ok(identity)
    }

    fn save(&self, path: &Path) -> Result<()> {
        validate_machine_identity(self)?;
        save_json(self, path)
    }
}

pub fn load_machine_identity() -> Result<Option<MachineIdentity>> {
    let path = machine_identity_path()?;
    match fs::metadata(&path) {
        Ok(_) => MachineIdentity::load(&path).map(Some),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("failed to inspect {}", path.display())),
    }
}

pub fn save_machine_identity(identity: &MachineIdentity) -> Result<()> {
    identity.save(&machine_identity_path()?)
}

fn machine_identity_path() -> Result<PathBuf> {
    Ok(state_dir()?
        .join("machines")
        .join(node_key()?)
        .join("machine-identity.json"))
}

pub fn current_hostname() -> Result<String> {
    let output = Command::new("hostname")
        .output()
        .context("failed to determine the local hostname")?;
    require_success(output.status, "hostname")?;
    let hostname =
        String::from_utf8(output.stdout).context("hostname returned non-UTF-8 output")?;
    validate_hostname(hostname.trim())
}

fn validate_hostname(hostname: &str) -> Result<String> {
    if hostname.is_empty()
        || hostname.len() > 253
        || !hostname
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
    {
        bail!("local hostname contains unsupported characters");
    }
    Ok(hostname.to_string())
}

fn node_key() -> Result<String> {
    Ok(component_key(&current_hostname()?))
}

fn validate_machine_identity(identity: &MachineIdentity) -> Result<()> {
    if !identity.installation_id.starts_with("mid_")
        || identity.installation_id.len() != 36
        || !identity.installation_id[4..]
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        bail!("machine installation identity is invalid");
    }
    validate_machine_name(&identity.name)
}

/// Fixed code for the Windows product rule: Treer assumes an account in the
/// Administrators group; other accounts are refused before anything is
/// written or started, never half-supported.
pub const WINDOWS_REQUIRES_ADMINISTRATOR: &str = "windows_requires_administrator";

// Only Windows classifies a live token; other platforms exercise this through
// the unit test alone.
#[cfg_attr(not(windows), allow(dead_code))]
fn administrator_requirement(class: treer_platform::WindowsAccountClass) -> Result<()> {
    use treer_platform::WindowsAccountClass::*;
    match class {
        Elevated | AdminCapable => Ok(()),
        Standard => bail!(
            "{WINDOWS_REQUIRES_ADMINISTRATOR}: Treer on Windows requires an account in the Administrators group. Sign in as an administrator (elevation is not required) and retry. Nothing was changed."
        ),
    }
}

/// Windows: refuse to install, enroll or start under a non-administrator
/// account. No-op elsewhere.
pub fn require_windows_administrator() -> Result<()> {
    #[cfg(windows)]
    {
        let class = treer_platform::account_class()
            .context("failed to classify the Windows account token")?;
        administrator_requirement(class)
    }
    #[cfg(not(windows))]
    Ok(())
}

pub fn validate_machine_name(name: &str) -> Result<()> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 80 || name.chars().any(char::is_control) {
        bail!("machine name must contain 1 to 80 printable characters");
    }
    Ok(())
}

impl ServiceConfig {
    pub fn load(path: &Path) -> Result<Self> {
        let bytes = fs::read(path)
            .with_context(|| format!("failed to read service config {}", path.display()))?;
        serde_json::from_slice(&bytes)
            .with_context(|| format!("invalid service config {}", path.display()))
    }

    fn save(&self, path: &Path) -> Result<()> {
        let parent = path
            .parent()
            .context("service config path has no parent directory")?;
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
        let bytes = serde_json::to_vec_pretty(self).context("failed to encode service config")?;
        write_atomic(path, &bytes)
    }

    pub fn supervision(&self) -> MachineSupervision {
        MachineSupervision {
            mode: match self.service_manager {
                ServiceManager::SystemdUser => MachineSupervisionMode::SystemdUser,
                ServiceManager::Launchd => MachineSupervisionMode::Launchd,
                ServiceManager::Nohup => MachineSupervisionMode::Nohup,
                ServiceManager::Foreground => MachineSupervisionMode::Foreground,
                #[cfg(windows)]
                ServiceManager::Detached => MachineSupervisionMode::Detached,
            },
            fallback_reason: self.service_fallback_reason.clone(),
        }
    }
}

pub async fn resolve_listen(workspace: &str, requested: Option<SocketAddr>) -> Result<SocketAddr> {
    validate_workspace(workspace)?;
    if let Some(requested) = requested {
        anyhow::ensure!(
            address_is_available(requested),
            "requested local Controller port is already in use; choose a different --listen or omit it for automatic allocation (enrollment key was not exchanged). This is a best-effort preflight: a concurrent bind after the check can still make activation fail closed"
        );
        return Ok(requested);
    }

    if let Some(installed) = registered_config(workspace)? {
        let address = installed
            .listen
            .parse::<SocketAddr>()
            .context("invalid listen address in installed service config")?;
        if address_is_available(address)
            || local_api_matches(&address, &installed.workspace, &installed.server_id).await
        {
            return Ok(address);
        }
    }

    allocate_loopback_address()
}

/// Names only: do not read another enrollment's config or credentials.
pub fn has_other_controller_config(directory: &Path, server_id: &str) -> Result<bool> {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error).context("cannot inspect local enrollment names"),
    };
    let own_name = format!("{server_id}-controller.json");
    for entry in entries {
        let entry = entry?;
        if let Some(name) = entry.file_name().to_str() {
            if name.ends_with("-controller.json") && name != own_name {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

pub fn warn_shared_account(server_id: &str) -> Result<()> {
    if has_other_controller_config(&config_dir()?, server_id)? {
        eprintln!("Warning: another Treer enrollment exists under this OS account. One OS account is one trust domain: its workspace Agents/exec can read and impersonate other local workspace identities. Use separate OS accounts or containers for isolation.");
    }
    Ok(())
}

fn allocate_loopback_address() -> Result<SocketAddr> {
    for port in FIRST_AUTOMATIC_PORT..=u16::MAX {
        let address = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
        if address_is_available(address) {
            return Ok(address);
        }
    }

    bail!("no available loopback port from {FIRST_AUTOMATIC_PORT} through 65535")
}

fn address_is_available(address: SocketAddr) -> bool {
    TcpListener::bind(address).is_ok()
}

async fn local_api_matches(address: &SocketAddr, workspace: &str, server_id: &str) -> bool {
    occupying_controller(*address)
        .await
        .is_some_and(|occupant| {
            occupant.workspace_id == workspace && occupant.server_id == server_id
        })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OccupyingController {
    pub server_id: String,
    pub workspace_id: String,
    pub pid: Option<u32>,
}

pub async fn occupying_controller(address: SocketAddr) -> Option<OccupyingController> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_millis(500))
        .no_proxy()
        .build()
        .ok()?;
    let response = client
        .get(format!("http://{address}/api/health"))
        .send()
        .await
        .ok()?;
    let value = response.json::<serde_json::Value>().await.ok()?;
    if value.get("service").and_then(|value| value.as_str()) != Some("treer-agent-server") {
        return None;
    }
    Some(OccupyingController {
        workspace_id: value.get("workspace_id")?.as_str()?.to_string(),
        server_id: value.get("server_id")?.as_str()?.to_string(),
        pid: listen_pid(address),
    })
}

fn listen_pid(address: SocketAddr) -> Option<u32> {
    let output = Command::new("lsof")
        .args([
            "-nP",
            "-t",
            "-sTCP:LISTEN",
            &format!("-iTCP:{}", address.port()),
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout)
        .ok()?
        .lines()
        .find_map(|line| line.trim().parse().ok())
}

/// Applied only to a new enrollment. Saved roots are never implicitly migrated.
pub fn new_enrollment_root(requested: Option<&Path>) -> Result<PathBuf> {
    let root = match requested {
        Some(path) => path.to_path_buf(),
        None => {
            #[cfg(windows)]
            {
                let base = env::var_os("LOCALAPPDATA")
                    .or_else(|| env::var_os("USERPROFILE"))
                    .map(PathBuf::from)
                    .context("Windows user directory is unavailable; specify --root explicitly")?;
                if !base.is_absolute() {
                    bail!("Windows user directory must be absolute; specify --root explicitly");
                }
                let root = base.join("Treer/root");
                // Refuse a pre-existing reparse chain before changing its ACL.
                let mut existing = root.as_path();
                while !existing.exists() {
                    existing = existing.parent().context("invalid user root")?;
                }
                treer_platform::machine_file::Root::open(existing, &[])
                    .context("default Windows root must not traverse reparse points")?;
                treer_platform::secure_directory(&root)?;
                root
            }
            #[cfg(not(windows))]
            {
                PathBuf::from(".")
            }
        }
    };
    let root = fs::canonicalize(&root)
        .with_context(|| format!("invalid workspace root {}", root.display()))?;
    ensure_root_outside_state_dir(&root)?;
    Ok(root)
}

/// The state directory is reserved for file transfer by identity and name, so a
/// root equal to it or below it would make every transfer denied or the
/// Controller's own files the workload tree. Refuse such a root with a fixed,
/// actionable error at enrollment, relocation and Controller start instead of
/// degrading silently. If the state directory cannot be resolved at all the
/// caller fails closed through `state_dir()`.
pub fn ensure_root_outside_state_dir(root: &Path) -> Result<()> {
    root_outside_state_dir(root, &state_dir()?)
}

pub(crate) fn root_outside_state_dir(root: &Path, state: &Path) -> Result<()> {
    let state = canonical_prefix(state);
    let root = canonical_prefix(root);
    if path_is_equal_or_below(&root, &state) {
        bail!(
            "root_inside_treer_state_directory: the workspace root must not be the Treer state directory or inside it ({}); choose another --root",
            state.display()
        );
    }
    Ok(())
}

/// Component-wise prefix test. On case-insensitive filesystems (macOS,
/// Windows) a still-absent tail cannot be canonicalized, so its components are
/// compared ASCII case-insensitively; elsewhere the comparison is exact.
fn path_is_equal_or_below(path: &Path, ancestor: &Path) -> bool {
    let mut path_components = path.components();
    for expected in ancestor.components() {
        let Some(actual) = path_components.next() else {
            return false;
        };
        let (expected, actual) = (expected.as_os_str(), actual.as_os_str());
        let same = if cfg!(any(target_os = "macos", windows)) {
            expected
                .to_string_lossy()
                .eq_ignore_ascii_case(&actual.to_string_lossy())
        } else {
            expected == actual
        };
        if !same {
            return false;
        }
    }
    true
}

/// Canonicalizes the longest existing ancestor and re-appends the missing
/// tail, so absent paths compare on the same physical prefix as existing ones
/// (macOS `/var` versus `/private/var`, symlinked homes).
fn canonical_prefix(path: &Path) -> PathBuf {
    let mut existing = path;
    let mut tail = Vec::new();
    loop {
        if let Ok(canonical) = fs::canonicalize(existing) {
            return tail
                .iter()
                .rev()
                .fold(canonical, |acc, component| acc.join(component));
        }
        match (existing.parent(), existing.file_name()) {
            (Some(parent), Some(name)) => {
                tail.push(name.to_owned());
                existing = parent;
            }
            _ => return path.to_path_buf(),
        }
    }
}

pub fn register(config: ServiceConfig) -> Result<()> {
    validate_workspace(&config.workspace)?;
    require_install_hostname(&config)?;
    let paths = ServicePaths::new(&config.server_id)?;
    require_host_binary(&paths)?;
    fs::create_dir_all(&paths.state_dir)
        .with_context(|| format!("failed to create {}", paths.state_dir.display()))?;
    let previous = match fs::metadata(&paths.config) {
        Ok(_) => Some(ServiceConfig::load(&paths.config)?),
        Err(error) if error.kind() == ErrorKind::NotFound => None,
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to inspect {}", paths.config.display()))
        }
    };
    config.save(&paths.config)?;
    let host_config = HostDaemonConfig {
        socket_path: config.host_socket.clone(),
        controller_path: paths.executable.clone(),
        controller_config_path: paths.config.clone(),
        root: config.root.clone(),
    };
    save_json(&host_config, &paths.host_config)?;
    if let Err(error) = platform::transition(previous.as_ref(), &config) {
        if let Some(previous) = previous {
            previous
                .save(&paths.config)
                .context("failed to restore the previous service configuration")?;
        }
        return Err(error);
    }
    platform::register(&paths, &config)?;
    println!("treer: agent Host service registered");
    println!("treer: configured local API address: {}", config.listen);
    Ok(())
}

fn host_is_running(socket: &Path) -> bool {
    treer_platform::local_endpoint_active(socket)
}

pub enum ServiceActivation {
    Managed,
    Foreground(Box<tokio::process::Child>),
}

pub async fn refresh_registration_and_wait(mut config: ServiceConfig) -> Result<ServiceActivation> {
    let preferred_host_socket = host_socket_path(&config.server_id)?;
    if should_migrate_host_socket(&config.host_socket, &preferred_host_socket) {
        eprintln!(
            "treer: migrating unavailable Host socket from {} to {}",
            config.host_socket.display(),
            preferred_host_socket.display()
        );
        config.host_socket = preferred_host_socket;
    }
    let workspace = config.workspace.clone();
    register(config)?;
    let (paths, installed) = installed_service(&workspace)?;
    activate_registered_service(&paths, &installed).await
}

async fn activate_registered_service(
    paths: &ServicePaths,
    config: &ServiceConfig,
) -> Result<ServiceActivation> {
    // Registration can stop the old Host when switching supervisors. Probe
    // afterwards so a cold start never sends a restart request to a missing socket.
    if !host_is_running(&config.host_socket) {
        return start_service_and_wait(paths, config).await;
    }

    if let Err(error) = restart_controller_at(paths, &config.host_socket) {
        let diagnostic = format!("Controller hot restart failed ({error:#}); Host recovery failed");
        let recovery = async {
            // The Host may have exited between the probe and the request.
            if !host_is_running(&config.host_socket) {
                return start_service_and_wait(paths, config).await;
            }
            if config.service_manager == ServiceManager::Foreground {
                // A live foreground Host belongs to another caller. Do not
                // launch a second Host or terminate its agents as a fallback.
                bail!("the live foreground Host cannot be replaced during recovery");
            }
            eprintln!("treer: restarting the Host service after a failed Controller restart");
            platform::restart(paths, config)?;
            wait_for_controller_and_proxy(config).await?;
            Ok(ServiceActivation::Managed)
        };
        // Preserve the original diagnostic if recovery also fails, without
        // printing a subprocess error before a successful recovery.
        return recovery.await.context(diagnostic);
    }
    wait_for_controller_and_proxy(config).await?;
    Ok(ServiceActivation::Managed)
}

pub fn registered_config(workspace: &str) -> Result<Option<ServiceConfig>> {
    validate_workspace(workspace)?;
    Ok(find_installed_service(workspace)?.map(|(_, config)| config))
}

/// Local-host enrollments for launcher selection. These records contain secrets:
/// callers must only display the bounded identifier/address projection.
pub(crate) fn launcher_configs() -> Result<Vec<ServiceConfig>> {
    Ok(installed_services()?
        .into_iter()
        .map(|(_, config)| config)
        .collect())
}

/// Maintenance must resolve installed paths from the saved Host binding, not
/// ServicePaths::new/current_exe: a detached updater runs a job-local copy.
#[cfg(unix)]
pub(crate) fn self_update_inventory() -> Result<crate::self_update::selection::Inventory> {
    let directory =
        config_dir().map_err(|_| anyhow::anyhow!("update_config_directory_unavailable"))?;
    // Worker bootstrap is synchronous and may not fork an ambient PATH helper.
    let hostname = nix::unistd::gethostname()
        .ok()
        .and_then(|name| name.into_string().ok())
        .ok_or_else(|| anyhow::anyhow!("update_hostname_unavailable"))?;
    crate::self_update::selection::Inventory::load(&directory, &hostname)
}

/// Internal worker locator only: opening it must not create/chmod existing
/// state or accept a caller-supplied receipt directory.
#[cfg(unix)]
pub(crate) fn self_update_jobs_directory() -> Result<PathBuf> {
    Ok(state_dir()
        .map_err(|_| anyhow::anyhow!("update_state_directory_unavailable"))?
        .join("self-update")
        .join("jobs"))
}

pub(crate) fn controller_update_config_directory() -> Result<PathBuf> {
    config_dir().map_err(|_| anyhow::anyhow!("controller_update_catalog_unavailable"))
}

pub fn preflight_registration(workspace: &str, mode: ServiceMode) -> Result<ServiceSelection> {
    validate_workspace(workspace)?;
    require_host_binary(&ServicePaths::new("preflight")?)?;
    host_socket_path("preflight")?;
    let selection = select_service_manager(mode)?;
    #[cfg(unix)]
    if selection.manager == ServiceManager::Nohup {
        require_nohup()?;
    }
    Ok(selection)
}

fn require_host_binary(paths: &ServicePaths) -> Result<()> {
    if paths.host_executable.is_file() {
        Ok(())
    } else {
        bail!(
            "treer-agent-host was not found next to {}",
            paths.executable.display()
        )
    }
}

#[cfg(target_os = "linux")]
fn select_service_manager(mode: ServiceMode) -> Result<ServiceSelection> {
    select_linux_service_manager(mode, probe_systemd_user)
}

#[cfg(target_os = "linux")]
fn select_linux_service_manager(
    mode: ServiceMode,
    probe: impl FnOnce() -> Result<()>,
) -> Result<ServiceSelection> {
    match mode {
        ServiceMode::Auto | ServiceMode::Nohup => Ok(ServiceSelection {
            manager: ServiceManager::Nohup,
            fallback_reason: None,
        }),
        ServiceMode::Systemd => {
            probe().context(
                "systemd user service mode was requested but the user manager is unavailable; use `--service-mode nohup` or enable a user systemd session",
            )?;
            Ok(ServiceSelection {
                manager: ServiceManager::SystemdUser,
                fallback_reason: None,
            })
        }
        ServiceMode::Foreground => Ok(ServiceSelection {
            manager: ServiceManager::Foreground,
            fallback_reason: None,
        }),
        ServiceMode::Launchd => bail!("launchd service mode is available only on macOS"),
    }
}

#[cfg(target_os = "linux")]
fn probe_systemd_user() -> Result<()> {
    let executable = env::var_os("TREER_SYSTEMCTL").unwrap_or_else(|| "systemctl".into());
    probe_systemd_user_with(&executable)
}

#[cfg(target_os = "linux")]
fn probe_systemd_user_with(executable: &OsStr) -> Result<()> {
    let output = Command::new(executable)
        .args(["--user", "show-environment"])
        .output()
        .with_context(|| {
            format!(
                "failed to run {} --user show-environment",
                PathBuf::from(executable).display()
            )
        })?;
    if output.status.success() {
        return Ok(());
    }
    let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if detail.is_empty() {
        bail!(
            "systemctl --user show-environment exited with {}",
            output.status
        );
    }
    bail!("systemctl --user is unavailable: {detail}")
}

#[cfg(target_os = "linux")]
fn systemctl_command() -> Command {
    Command::new(env::var_os("TREER_SYSTEMCTL").unwrap_or_else(|| "systemctl".into()))
}

#[cfg(target_os = "macos")]
fn select_service_manager(mode: ServiceMode) -> Result<ServiceSelection> {
    match mode {
        ServiceMode::Auto | ServiceMode::Nohup => Ok(ServiceSelection {
            manager: ServiceManager::Nohup,
            fallback_reason: None,
        }),
        ServiceMode::Launchd => Ok(ServiceSelection {
            manager: ServiceManager::Launchd,
            fallback_reason: None,
        }),
        ServiceMode::Foreground => Ok(ServiceSelection {
            manager: ServiceManager::Foreground,
            fallback_reason: None,
        }),
        ServiceMode::Systemd => bail!("systemd service mode is available only on Linux"),
    }
}

#[cfg(windows)]
fn select_service_manager(mode: ServiceMode) -> Result<ServiceSelection> {
    let manager = match mode {
        ServiceMode::Auto | ServiceMode::Detached => ServiceManager::Detached,
        ServiceMode::Foreground => ServiceManager::Foreground,
        _ => bail!("use --service-mode detached or foreground on Windows"),
    };
    Ok(ServiceSelection {
        manager,
        fallback_reason: None,
    })
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn select_service_manager(mode: ServiceMode) -> Result<ServiceSelection> {
    match mode {
        ServiceMode::Auto => Ok(ServiceSelection {
            manager: ServiceManager::Foreground,
            fallback_reason: Some(
                "this platform has no supported persistent user service manager".to_string(),
            ),
        }),
        ServiceMode::Foreground => Ok(ServiceSelection {
            manager: ServiceManager::Foreground,
            fallback_reason: None,
        }),
        ServiceMode::Systemd | ServiceMode::Launchd | ServiceMode::Nohup => {
            bail!("the requested service manager is not available on this platform")
        }
    }
}

pub fn host_socket_path(server_id: &str) -> Result<PathBuf> {
    validate_server_id(server_id)?;
    #[cfg(windows)]
    return Ok(treer_platform::endpoint_name(
        server_id,
        &treer_platform::current_user_sid()?,
    ));
    #[cfg(unix)]
    {
        let name = host_socket_filename(server_id);
        let path = fit_unix_socket_path(runtime_dir()?.join(&name), &name)?;
        Ok(path)
    }
}

pub fn start(workspace: &str) -> Result<()> {
    let (paths, config) = installed_service(workspace)?;
    #[cfg(windows)]
    require_root_alignment(&paths, &config)?;
    platform::start(&paths, &config)
}

pub async fn start_and_wait(workspace: &str) -> Result<ServiceActivation> {
    let (paths, config) = installed_service(workspace)?;
    start_service_and_wait(&paths, &config).await
}

async fn start_service_and_wait(
    paths: &ServicePaths,
    config: &ServiceConfig,
) -> Result<ServiceActivation> {
    #[cfg(windows)]
    require_root_alignment(paths, config)?;
    let mut activation = if config.service_manager == ServiceManager::Foreground {
        eprintln!(
            "treer: starting Host in foreground mode; keep this terminal open or run the command under a process supervisor"
        );
        let child = tokio::process::Command::new(&paths.host_executable)
            .env_clear()
            .envs(treer_platform::inherited_environment())
            .arg("run")
            .arg("--config")
            .arg(&paths.host_config)
            .spawn()
            .with_context(|| {
                format!(
                    "failed to start foreground Host {}",
                    paths.host_executable.display()
                )
            })?;
        ServiceActivation::Foreground(Box::new(child))
    } else {
        platform::start(paths, config)?;
        ServiceActivation::Managed
    };

    if let Err(error) = wait_for_controller_and_proxy(config).await {
        if let ServiceActivation::Foreground(child) = &mut activation {
            let _ = child.kill().await;
            let _ = child.wait().await;
        } else if config.service_manager == ServiceManager::Nohup {
            let _ = platform::stop(paths, config);
        }
        #[cfg(windows)]
        if config.service_manager == ServiceManager::Detached {
            let _ = platform::stop(paths, config);
        }
        return Err(error);
    }
    if let ServiceActivation::Foreground(child) = &mut activation {
        if let Some(status) = child
            .try_wait()
            .context("failed to inspect foreground Host status")?
        {
            bail!("foreground treer-agent-host exited before startup completed with {status}");
        }
    }
    println!("treer: Controller and Proxy connection are ready");
    Ok(activation)
}

pub async fn restore(workspace: &str) -> Result<()> {
    let (_, config) = installed_service(workspace)?;
    let activation = if controller_health_document(&config).await.is_some() {
        ServiceActivation::Managed
    } else {
        start_and_wait(workspace).await?
    };
    wait_for_controller_and_proxy(&config).await?;
    let mut url = controller_health_url(&config).context("invalid Controller URL")?;
    url.set_path("/api/runtime/restore");
    let response = reqwest::Client::builder()
        .no_proxy()
        .build()?
        .post(url)
        .header("x-treer-operator-credential", &config.operator_credential)
        .json(&serde_json::json!({}))
        .send()
        .await
        .context("runtime restore request failed")?;
    let status = response.status();
    let body = response.text().await?;
    if !status.is_success() {
        bail!("runtime restore returned {status}: {body}");
    }
    println!("{body}");
    wait_for_foreground(activation).await
}

fn should_migrate_host_socket(current: &Path, preferred: &Path) -> bool {
    if current == preferred {
        return false;
    }
    !host_is_running(current)
}

pub async fn wait_for_foreground(activation: ServiceActivation) -> Result<()> {
    let ServiceActivation::Foreground(mut child) = activation else {
        return Ok(());
    };
    let status = child
        .wait()
        .await
        .context("failed to wait for foreground Host")?;
    require_success(status, "foreground treer-agent-host")
}

pub async fn repair_and_wait(
    workspace: &str,
    mode: ServiceMode,
    network_mode: NetworkMode,
) -> Result<ServiceActivation> {
    let lock = lock_machine_registration(workspace)?;
    finish_machine_replacement(&lock, workspace)?;
    let (_, mut config) = installed_service(workspace)?;
    let selection = preflight_registration(workspace, mode)?;
    selection.announce();
    config.service_manager = selection.manager;
    config.service_fallback_reason = selection.fallback_reason;
    config.network_mode = Some(network_mode);
    let activation = refresh_registration_and_wait(config).await?;
    drop(lock);
    println!("treer: service registration repaired without a new enrollment key");
    Ok(activation)
}

pub fn stop(workspace: &str) -> Result<()> {
    warn_managed_agent_service_stop();
    let (paths, config) = installed_service(workspace)?;
    platform::stop(&paths, &config)
}

pub fn stop_remotely(workspace: &str) -> Result<()> {
    let (paths, config) = installed_service(workspace)?;
    platform::stop_remotely(&paths, &config)
}

pub fn restart(workspace: &str) -> Result<()> {
    warn_managed_agent_service_stop();
    let (paths, config) = installed_service(workspace)?;
    #[cfg(windows)]
    require_root_alignment(&paths, &config)?;
    platform::restart(&paths, &config)
}

pub fn restart_controller(workspace: &str) -> Result<()> {
    let (paths, config) = installed_service(workspace)?;
    restart_controller_at(&paths, &config.host_socket)
}

pub fn relocate_root(workspace: &str, root: &Path) -> Result<()> {
    #[cfg(not(windows))]
    {
        let _ = (workspace, root);
        bail!("service relocate-root currently supports Windows only");
    }
    #[cfg(windows)]
    {
        let (paths, config) = installed_service(workspace)?;
        let _lock = treer_platform::LocalLock::acquire(&config.server_id)?;
        relocate_root_at(&paths, &config, root)
    }
}

#[cfg(any(windows, test))]
fn relocate_root_at(paths: &ServicePaths, config: &ServiceConfig, root: &Path) -> Result<()> {
    if host_is_running(&config.host_socket) {
        bail!(
            "stop the Host from a host terminal before relocating root; this terminates workloads"
        );
    }
    #[cfg(windows)]
    {
        let input = if root.is_absolute() {
            root.to_path_buf()
        } else {
            env::current_dir()?.join(root)
        };
        treer_platform::machine_file::Root::open(&input, &[])
            .context("new root must not traverse reparse points")?;
    }
    let root = fs::canonicalize(root).context("new root must be an existing physical directory")?;
    treer_platform::machine_file::Root::open(&root, &[])
        .context("new root must not traverse symlinks/reparse points")?;
    ensure_root_outside_state_dir(&root)?;
    let old_host_bytes = fs::read(&paths.host_config)?;
    let mut host: HostDaemonConfig = serde_json::from_slice(&old_host_bytes)?;
    let mut next = config.clone();
    next.root = root.clone();
    host.root = root;
    // Each file is atomic. Startup validates agreement so a crash between the
    // writes fails closed until this command is rerun or the old root restored.
    next.save(&paths.config)?;
    if let Err(error) = save_json(&host, &paths.host_config) {
        config
            .save(&paths.config)
            .context("root migration failed; restoring Controller configuration also failed")?;
        return Err(error);
    }
    println!("treer: root updated; no files were moved and no service was started");
    println!("treer: reinstall the launcher in the new root and review saved absolute startup/profile paths; the old .treer directory was not moved or cleaned");
    Ok(())
}

#[cfg(any(windows, test))]
fn require_root_alignment(paths: &ServicePaths, config: &ServiceConfig) -> Result<()> {
    let host: HostDaemonConfig = serde_json::from_slice(&fs::read(&paths.host_config)?)?;
    if fs::canonicalize(&host.root)? != fs::canonicalize(&config.root)? {
        bail!("Host/Controller roots disagree; rerun service relocate-root while stopped before starting");
    }
    Ok(())
}

fn warn_managed_agent_service_stop() {
    if env::var_os("TREER_AGENT_ID").is_some() {
        eprintln!("treer: warning: stopping/restarting the Host will terminate the current managed Agent. Run this from the host terminal or a detached process session; execution will continue.");
    }
}

pub async fn update(
    release_base_url: Url,
    channel: String,
    public_key: Option<PathBuf>,
) -> Result<()> {
    if cfg!(windows) {
        bail!("Windows in-place updates are not supported; stop Treer, reinstall the binaries, then run restore");
    }
    // Trust preflight must precede update-lock creation, staging, or activation.
    let public_key_path = match &public_key {
        Some(path) => path.clone(),
        None => config_dir()?
            .parent()
            .context("configuration directory has no parent")?
            .join("release-signing-key.pub.pem"),
    };
    let key = crate::release_update::resolve_public_key(
        &release_base_url,
        public_key.as_deref(),
        &public_key_path,
        Some(crate::release_update::OFFICIAL_PUBLIC_KEY_PEM),
    )?;
    let services = installed_services()?;
    let (paths, _) = services.first().context(
        "no agent-server services are installed on this host; connect a machine before updating",
    )?;
    let activation_services = activation_services(&services)?;
    let treer_executable = installed_treer_binary(&paths.executable).with_context(|| {
        format!(
            "could not find the installed treer CLI for {}",
            paths.executable.display()
        )
    })?;
    #[cfg(unix)]
    let _native_update_guard = crate::self_update::web_adapter::legacy_update_guard(
        &services
            .iter()
            .map(|(paths, config)| (paths.executable.clone(), config.workspace.clone()))
            .collect::<Vec<_>>(),
        &treer_executable,
    )?;
    let platform = artifact_platform(std::env::consts::OS, std::env::consts::ARCH)?;
    let client = reqwest::Client::builder()
        .timeout(UPDATE_DOWNLOAD_TIMEOUT)
        // Agent terminals inherit Treer's workload HTTP proxy. Updating the
        // Controller must not depend on the data plane it is replacing.
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .context("failed to create update client")?;

    println!(
        "treer: downloading signed {channel} {platform} Controller and CLI from {release_base_url}"
    );
    // Both signatures and both binary digests must pass before staging can
    // execute either downloaded program, including its --help smoke check.
    let (controller, cli) =
        crate::release_update::download(&client, release_base_url, &channel, &key, platform)
            .await?;
    let controller_bytes = controller.bytes();
    let treer_bytes = cli.bytes();
    let installed_controller = fs::read(&paths.executable)
        .with_context(|| format!("failed to read {}", paths.executable.display()))?;
    let installed_treer = fs::read(&treer_executable)
        .with_context(|| format!("failed to read {}", treer_executable.display()))?;
    let controller_changed = installed_controller.as_slice() != controller_bytes;
    let treer_changed = installed_treer.as_slice() != treer_bytes;

    let staged_controller = controller_changed
        .then(|| stage_executable(&paths.executable, controller_bytes, "update", true))
        .transpose()?;
    let staged_treer = treer_changed
        .then(|| stage_executable(&treer_executable, treer_bytes, "update", true))
        .transpose()?;
    let mut rollback_controller = controller_changed
        .then(|| stage_executable(&paths.executable, &installed_controller, "rollback", false))
        .transpose()?;
    let mut rollback_treer = treer_changed
        .then(|| stage_executable(&treer_executable, &installed_treer, "rollback", false))
        .transpose()?;
    let mut previous_epochs = Vec::with_capacity(activation_services.len());
    for (_, config) in &activation_services {
        previous_epochs.push(controller_epoch(config).await);
    }

    if let Some(staged) = staged_controller {
        staged.install(&paths.executable)?;
        println!(
            "treer: updated Controller at {}",
            paths.executable.display()
        );
    }
    if let Some(staged) = staged_treer {
        if let Err(error) = staged.install(&treer_executable) {
            if let Some(rollback) = rollback_controller.take() {
                rollback.install(&paths.executable).with_context(|| {
                    format!(
                        "failed to restore the old Controller after CLI installation failed: {error:#}"
                    )
                })?;
            }
            return Err(error);
        }
        println!("treer: updated CLI at {}", treer_executable.display());
    }

    let activation = activate_controllers(&activation_services, &previous_epochs).await;
    if let Err(error) = activation {
        let had_replacements = rollback_controller.is_some() || rollback_treer.is_some();
        if let Some(rollback) = rollback_controller.take() {
            rollback
                .install(&paths.executable)
                .context("failed to restore the old Controller")?;
        }
        if let Some(rollback) = rollback_treer.take() {
            rollback
                .install(&treer_executable)
                .context("failed to restore the old treer CLI")?;
        }
        if had_replacements {
            let rollback_epochs = vec![None; activation_services.len()];
            if let Err(rollback_error) =
                activate_controllers(&activation_services, &rollback_epochs).await
            {
                bail!(
                    "new Controller failed to activate ({error:#}); restored the old binaries but failed to restart the old Controller set: {rollback_error:#}"
                );
            }
            bail!("new Controller failed to activate; old binaries were restored: {error:#}");
        }
        return Err(error);
    }

    if !controller_changed && !treer_changed {
        println!("treer: binaries were already current; Controller activation completed");
    } else {
        println!("treer: update activated; Host and running agents were preserved");
    }
    Ok(())
}

async fn activate_controllers(
    services: &[&(ServicePaths, ServiceConfig)],
    previous_epochs: &[Option<String>],
) -> Result<()> {
    for ((paths, config), previous_epoch) in services.iter().zip(previous_epochs) {
        restart_controller_at(paths, &config.host_socket)
            .with_context(|| format!("failed to restart Controller for {}", config.server_id))?;
        wait_for_controller(config, previous_epoch.as_deref())
            .await
            .with_context(|| format!("Controller {} did not activate", config.server_id))?;
    }
    Ok(())
}

fn activation_services(
    services: &[(ServicePaths, ServiceConfig)],
) -> Result<Vec<&(ServicePaths, ServiceConfig)>> {
    let managed_server_id = env::var("TREER_SERVER_ID")
        .ok()
        .filter(|value| !value.is_empty());
    activation_services_for(services, managed_server_id.as_deref())
}

fn activation_services_for<'a>(
    services: &'a [(ServicePaths, ServiceConfig)],
    managed_server_id: Option<&str>,
) -> Result<Vec<&'a (ServicePaths, ServiceConfig)>> {
    if let Some(server_id) = managed_server_id {
        let service = services
            .iter()
            .find(|(_, config)| config.server_id == server_id)
            .with_context(|| {
                format!("managed Agent server {server_id} is not installed on this host")
            })?;
        return Ok(vec![service]);
    }
    Ok(services.iter().collect())
}

pub fn installed_treer_binary(controller: &Path) -> Option<PathBuf> {
    if let Some(path) = env::var_os("TREER_BIN").map(PathBuf::from) {
        if path.is_file() {
            return Some(path);
        }
    }

    let binary_name = format!("treer{}", env::consts::EXE_SUFFIX);
    let sibling = controller.with_file_name(&binary_name);
    if sibling.is_file() {
        return Some(sibling);
    }

    let local_root = controller.parent()?.parent()?.parent()?;
    let conventional = local_root.join("bin").join(&binary_name);
    if conventional.is_file() {
        return Some(conventional);
    }

    env::var_os("PATH").and_then(|path| {
        env::split_paths(&path)
            .map(|directory| directory.join(&binary_name))
            .find(|candidate| candidate.is_file())
    })
}

fn artifact_platform(os: &str, arch: &str) -> Result<&'static str> {
    match (os, arch) {
        ("linux", "x86_64") => Ok("linux-x86_64"),
        ("linux", "aarch64") => Ok("linux-aarch64"),
        ("macos", "x86_64") => Ok("darwin-x86_64"),
        ("macos", "aarch64") => Ok("darwin-aarch64"),
        _ => bail!("automatic updates are not supported on {os}/{arch}"),
    }
}

#[derive(Debug)]
struct StagedExecutable {
    path: PathBuf,
}

impl StagedExecutable {
    fn install(mut self, destination: &Path) -> Result<()> {
        fs::rename(&self.path, destination)
            .with_context(|| format!("failed to replace {}", destination.display()))?;
        self.path = PathBuf::new();
        sync_parent(destination);
        Ok(())
    }
}

impl Drop for StagedExecutable {
    fn drop(&mut self) {
        if !self.path.as_os_str().is_empty() {
            let _ = fs::remove_file(&self.path);
        }
    }
}

fn stage_executable(
    destination: &Path,
    bytes: &[u8],
    purpose: &str,
    validate: bool,
) -> Result<StagedExecutable> {
    let parent = destination
        .parent()
        .context("executable path has no parent directory")?;
    let file_name = destination
        .file_name()
        .and_then(|name| name.to_str())
        .context("executable path has no UTF-8 file name")?;
    let temporary = parent.join(format!(
        ".{file_name}.{purpose}.{}",
        Uuid::new_v4().simple()
    ));
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o755);
    }
    let result = (|| -> Result<()> {
        use std::io::Write;

        {
            // End the writable handle's scope before any validation process
            // starts. Concurrent forks can still briefly inherit a CLOEXEC
            // descriptor, so validation also handles transient ETXTBSY below.
            let mut file = options
                .open(&temporary)
                .with_context(|| format!("failed to stage {}", destination.display()))?;
            file.write_all(bytes)
                .with_context(|| format!("failed to stage {}", destination.display()))?;
            file.sync_all()
                .with_context(|| format!("failed to sync {}", temporary.display()))?;
        }
        if validate {
            validate_executable(&temporary, file_name)?;
        }
        Ok(())
    })();
    if let Err(error) = result {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }
    Ok(StagedExecutable { path: temporary })
}

fn validate_executable(path: &Path, file_name: &str) -> Result<()> {
    let deadline = Instant::now() + UPDATE_VALIDATION_TIMEOUT;
    let mut command = Command::new(path);
    command
        .arg("--help")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child = retry_busy_executable(|| command.spawn(), deadline)
        .with_context(|| format!("downloaded {file_name} is not executable on this machine"))?;
    loop {
        if let Some(status) = child
            .try_wait()
            .with_context(|| format!("failed to validate downloaded {file_name}"))?
        {
            return require_success(status, &format!("downloaded {file_name} --help"));
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            bail!(
                "downloaded {file_name} did not respond to --help within {} seconds",
                UPDATE_VALIDATION_TIMEOUT.as_secs()
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn retry_busy_executable<T>(
    mut spawn: impl FnMut() -> std::io::Result<T>,
    deadline: Instant,
) -> std::io::Result<T> {
    loop {
        match spawn() {
            Ok(child) => return Ok(child),
            Err(error) => {
                #[cfg(unix)]
                if error.raw_os_error() == Some(libc::ETXTBSY) && Instant::now() < deadline {
                    std::thread::sleep(
                        Duration::from_millis(50)
                            .min(deadline.saturating_duration_since(Instant::now())),
                    );
                    continue;
                }
                #[cfg(not(unix))]
                let _ = deadline;
                return Err(error);
            }
        }
    }
}

fn sync_parent(path: &Path) {
    let Some(parent) = path.parent() else {
        return;
    };
    if let Ok(directory) = fs::File::open(parent) {
        let _ = directory.sync_all();
    }
}

async fn controller_epoch(config: &ServiceConfig) -> Option<String> {
    let health_url = controller_health_url(config)?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_millis(500))
        .no_proxy()
        .build()
        .ok()?;
    let value = client
        .get(health_url)
        .send()
        .await
        .ok()?
        .json::<serde_json::Value>()
        .await
        .ok()?;
    let matches = value.get("service").and_then(|value| value.as_str())
        == Some("treer-agent-server")
        && value.get("workspace_id").and_then(|value| value.as_str())
            == Some(config.workspace.as_str())
        && value.get("server_id").and_then(|value| value.as_str())
            == Some(config.server_id.as_str());
    matches
        .then(|| value.get("controller_epoch")?.as_str().map(str::to_owned))
        .flatten()
}

fn controller_health_url(config: &ServiceConfig) -> Option<Url> {
    let managed_server_id = env::var("TREER_SERVER_ID").ok();
    let managed_server_url = env::var("TREER_AGENT_SERVER_URL").ok();
    controller_health_url_for(
        config,
        managed_server_id.as_deref(),
        managed_server_url.as_deref(),
    )
}

fn controller_health_url_for(
    config: &ServiceConfig,
    managed_server_id: Option<&str>,
    managed_server_url: Option<&str>,
) -> Option<Url> {
    let mut url = if managed_server_id == Some(config.server_id.as_str()) {
        Url::parse(managed_server_url?).ok()?
    } else {
        let address = config.listen.parse::<SocketAddr>().ok()?;
        Url::parse(&format!("http://{address}/")).ok()?
    };
    url.set_path("/api/health");
    url.set_query(None);
    url.set_fragment(None);
    Some(url)
}

async fn wait_for_controller(config: &ServiceConfig, previous_epoch: Option<&str>) -> Result<()> {
    let deadline = Instant::now() + CONTROLLER_START_TIMEOUT;
    loop {
        if let Some(epoch) = controller_epoch(config).await {
            if previous_epoch.is_none_or(|previous| previous != epoch) {
                return Ok(());
            }
        }
        if Instant::now() >= deadline {
            bail!(
                "Controller did not become healthy with a new epoch within {} seconds",
                CONTROLLER_START_TIMEOUT.as_secs()
            );
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

async fn controller_health_document(config: &ServiceConfig) -> Option<serde_json::Value> {
    let health_url = controller_health_url(config)?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_millis(900))
        .no_proxy()
        .build()
        .ok()?;
    let value = client
        .get(health_url)
        .send()
        .await
        .ok()?
        .json::<serde_json::Value>()
        .await
        .ok()?;
    let matches = value.get("service").and_then(|value| value.as_str())
        == Some("treer-agent-server")
        && value.get("workspace_id").and_then(|value| value.as_str())
            == Some(config.workspace.as_str())
        && value.get("server_id").and_then(|value| value.as_str())
            == Some(config.server_id.as_str());
    matches.then_some(value)
}

async fn wait_for_controller_and_proxy(config: &ServiceConfig) -> Result<()> {
    let deadline = Instant::now() + CONTROLLER_START_TIMEOUT;
    loop {
        let health = controller_health_document(config).await;
        let controller_ready = health.is_some();
        let proxy_connected = health.as_ref().is_some_and(|value| {
            value
                .get("proxy_connected")
                .and_then(|value| value.as_bool())
                == Some(true)
        });
        if controller_ready && proxy_connected {
            return Ok(());
        }
        if Instant::now() >= deadline {
            let waiting_for = if controller_ready {
                "Proxy lease"
            } else {
                "Controller health"
            };
            bail!(
                "{waiting_for} did not become ready within {} seconds",
                CONTROLLER_START_TIMEOUT.as_secs()
            );
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

#[derive(Debug)]
pub struct AmbiguousServices {
    pub hostname: String,
    pub services: Vec<ServiceConfig>,
}

impl std::fmt::Display for AmbiguousServices {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{}",
            format_ambiguous_services(&self.hostname, &self.services)
        )
    }
}

impl std::error::Error for AmbiguousServices {}

#[derive(Debug)]
#[allow(clippy::large_enum_variant)]
pub(crate) enum ServiceCommandTarget {
    Install(ServiceConfig),
    Inventory(Vec<ServiceConfig>),
}

pub(crate) fn resolve_service_command(
    requested: Option<&str>,
    list_when_omitted: bool,
) -> Result<ServiceCommandTarget> {
    let services = installed_services()?;
    match classify_service_request(
        requested,
        list_when_omitted,
        &services
            .iter()
            .map(|(_, config)| config.workspace.as_str())
            .collect::<Vec<_>>(),
    ) {
        ServiceRequestClass::Inventory => Ok(ServiceCommandTarget::Inventory(
            services.into_iter().map(|(_, config)| config).collect(),
        )),
        ServiceRequestClass::Unique => {
            let (_, config) = services
                .into_iter()
                .next()
                .expect("unique classification requires one install");
            Ok(ServiceCommandTarget::Install(config))
        }
        ServiceRequestClass::Selected => {
            let workspace = requested.expect("selected classification requires a workspace");
            installed_service(workspace).map(|(_, config)| ServiceCommandTarget::Install(config))
        }
        ServiceRequestClass::NoneInstalled => {
            bail!("{}", no_install_hint())
        }
        ServiceRequestClass::Ambiguous => Err(AmbiguousServices {
            hostname: current_hostname().unwrap_or_else(|_| "this host".to_string()),
            services: services.into_iter().map(|(_, config)| config).collect(),
        }
        .into()),
        ServiceRequestClass::Missing(workspace) => {
            let hostname = current_hostname().unwrap_or_else(|_| "this host".to_string());
            if services.is_empty() {
                bail!("{}", no_install_hint());
            }
            bail!(
                "no agent-server service for workspace {workspace} is installed on {hostname}\n{}",
                format_service_table(
                    &hostname,
                    &services
                        .iter()
                        .map(|(_, config)| config)
                        .collect::<Vec<_>>()
                )
            )
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum ServiceRequestClass {
    Inventory,
    Unique,
    Selected,
    NoneInstalled,
    Ambiguous,
    Missing(String),
}

fn classify_service_request(
    requested: Option<&str>,
    list_when_omitted: bool,
    workspaces: &[&str],
) -> ServiceRequestClass {
    match requested.map(str::trim).filter(|value| !value.is_empty()) {
        None => {
            if list_when_omitted {
                ServiceRequestClass::Inventory
            } else if workspaces.is_empty() {
                ServiceRequestClass::NoneInstalled
            } else if workspaces.len() == 1 {
                ServiceRequestClass::Unique
            } else {
                ServiceRequestClass::Ambiguous
            }
        }
        Some(workspace) => {
            if workspaces.contains(&workspace) {
                ServiceRequestClass::Selected
            } else {
                ServiceRequestClass::Missing(workspace.to_string())
            }
        }
    }
}

fn no_install_hint() -> String {
    let hostname = current_hostname().unwrap_or_else(|_| "this host".to_string());
    format!(
        "no agent-server service is installed on {hostname}\nConnect this machine from the workspace Add machine dialog, then run the copied connect command, or:\n  treer-agent-server connect --key 'enr_v1_…' --proxy '<proxy-url>'"
    )
}

fn format_service_table(hostname: &str, services: &[&ServiceConfig]) -> String {
    let mut lines = vec![format!("installed agent-server services on {hostname}:")];
    lines.push("workspace  server_id  listen  proxy  manager".to_string());
    for config in services {
        lines.push(format!(
            "{}  {}  {}  {}  {}",
            config.workspace, config.server_id, config.listen, config.proxy, config.service_manager
        ));
    }
    lines.join("\n")
}

fn format_ambiguous_services(hostname: &str, services: &[ServiceConfig]) -> String {
    format!(
        "{}\npass --workspace <workspace_id> to select one of these installs",
        format_service_table(hostname, &services.iter().collect::<Vec<_>>())
    )
}

pub(crate) async fn print_installed_services(services: &[ServiceConfig]) -> Result<()> {
    let hostname = current_hostname().unwrap_or_else(|_| "this host".to_string());
    if services.is_empty() {
        println!("{}", no_install_hint());
        return Ok(());
    }
    println!("installed agent-server services on {hostname}");
    println!(
        "{:<36}  {:<36}  {:<18}  {:<10}  proxy",
        "workspace", "server_id", "listen", "manager"
    );
    for config in services {
        let health = controller_health_document(config).await;
        let proxy = match health.as_ref().and_then(|value| {
            value
                .get("connection_state")
                .and_then(|value| value.as_str())
        }) {
            Some(state) => state.to_string(),
            None if health.is_some() => {
                if health.as_ref().and_then(|value| {
                    value
                        .get("proxy_connected")
                        .and_then(|value| value.as_bool())
                }) == Some(true)
                {
                    "online".to_string()
                } else {
                    "local".to_string()
                }
            }
            None => "stopped".to_string(),
        };
        println!(
            "{:<36}  {:<36}  {:<18}  {:<10}  {proxy}",
            config.workspace, config.server_id, config.listen, config.service_manager
        );
    }
    Ok(())
}

pub fn status(workspace: &str) -> Result<()> {
    let (paths, config) = installed_service(workspace)?;
    platform::status(&paths, &config)
}

pub fn logs(workspace: &str, lines: usize, follow: bool) -> Result<()> {
    let (paths, config) = installed_service(workspace)?;
    platform::logs(&paths, &config, lines, follow)
}

pub fn uninstall(workspace: &str) -> Result<()> {
    let (paths, config) = installed_service(workspace)?;
    platform::uninstall(&paths, &config)?;
    remove_if_exists(&paths.config)?;
    remove_if_exists(&paths.host_config)?;
    println!("treer: agent server service uninstalled");
    Ok(())
}

#[derive(Debug)]
pub(crate) struct ServicePaths {
    executable: PathBuf,
    host_executable: PathBuf,
    config: PathBuf,
    host_config: PathBuf,
    state_dir: PathBuf,
}

impl ServicePaths {
    fn new(server_id: &str) -> Result<Self> {
        Self::in_directories(&config_dir()?, state_dir()?, server_id)
    }

    fn in_directories(config_dir: &Path, state_dir: PathBuf, server_id: &str) -> Result<Self> {
        let key = component_key(server_id);
        let executable = env::current_exe()
            .context("failed to find the treer-agent-server executable")?
            .canonicalize()
            .context("failed to resolve the treer-agent-server executable")?;
        let host_executable =
            executable.with_file_name(format!("treer-agent-host{}", std::env::consts::EXE_SUFFIX));
        Ok(Self {
            executable,
            host_executable,
            config: config_dir.join(format!("{key}-controller.json")),
            host_config: config_dir.join(format!("{key}-host.json")),
            state_dir,
        })
    }
}

fn installed_service(workspace: &str) -> Result<(ServicePaths, ServiceConfig)> {
    validate_workspace(workspace)?;
    find_installed_service(workspace)?.with_context(|| {
        format!(
            "no agent-server service for workspace {workspace} is installed on {}",
            current_hostname().unwrap_or_else(|_| "this host".to_string())
        )
    })
}

fn find_installed_service(workspace: &str) -> Result<Option<(ServicePaths, ServiceConfig)>> {
    let mut matches: Vec<(ServicePaths, ServiceConfig)> = installed_services()?
        .into_iter()
        .filter(|(_, config)| config.workspace == workspace)
        .collect();
    if matches.len() > 1 {
        // An interrupted machine replacement leaves the previous and the new
        // configuration side by side; the journal names the one to keep.
        if let Some(journal) = load_replacement_journal(&config_dir()?, workspace)? {
            matches.retain(|(_, config)| config.server_id == journal.server_id);
        }
    }
    if matches.len() > 1 {
        bail!(
            "multiple agent-server services for workspace {workspace} are installed on {}",
            current_hostname()?
        );
    }
    Ok(matches.pop())
}

/// Durable record of a machine replacement in progress: the Proxy minted a
/// new machine ID for this installation after the previous machine was
/// deleted, and the local registration is being switched to it.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct MachineReplacementJournal {
    workspace: String,
    previous_server_id: String,
    server_id: String,
    created_at: String,
}

fn replacement_journal_path(config_dir: &Path, workspace: &str) -> PathBuf {
    config_dir.join(format!(
        "{}-machine-replacement.json",
        component_key(workspace)
    ))
}

fn load_replacement_journal(
    config_dir: &Path,
    workspace: &str,
) -> Result<Option<MachineReplacementJournal>> {
    let path = replacement_journal_path(config_dir, workspace);
    match fs::read(&path) {
        Ok(bytes) => {
            // The journal is fully written and synced before any other step,
            // so an unparsable journal means nothing was switched: discard it.
            let Ok(journal) = serde_json::from_slice::<MachineReplacementJournal>(&bytes) else {
                eprintln!(
                    "treer: discarding an incomplete machine replacement journal {}",
                    path.display()
                );
                remove_if_exists(&path)?;
                return Ok(None);
            };
            if journal.workspace != workspace {
                bail!(
                    "machine replacement journal {} names another workspace",
                    path.display()
                );
            }
            validate_proxy_server_id(&journal.server_id)?;
            validate_proxy_server_id(&journal.previous_server_id)?;
            Ok(Some(journal))
        }
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("failed to read {}", path.display())),
    }
}

/// Proxy machine IDs: `srv_` followed by 32 hex digits. The Controller never
/// adopts any other shape from an enrollment response.
pub fn validate_proxy_server_id(value: &str) -> Result<()> {
    if value.len() != 36
        || !value.starts_with("srv_")
        || !value[4..].bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        bail!("Proxy returned an invalid machine ID");
    }
    Ok(())
}

/// Publish `path` exclusively (it must not exist) with complete, synced,
/// private content: the bytes go to a sibling temporary file first and are
/// then hard-linked into place, so no reader ever observes a partial file and
/// a concurrent publisher fails with `AlreadyExists`, returned unchanged.
fn create_private_file_exclusively(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .context("configuration path has no parent directory")?;
    fs::create_dir_all(parent).with_context(|| format!("failed to create {}", parent.display()))?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .context("configuration path has no file name")?;
    let temporary = parent.join(format!(".{file_name}.{}.tmp", Uuid::new_v4().simple()));
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let result = (|| -> Result<()> {
        let mut file = options
            .open(&temporary)
            .with_context(|| format!("failed to create {}", temporary.display()))?;
        #[cfg(windows)]
        treer_platform::secure_file(&temporary)?;
        use std::io::Write;
        file.write_all(bytes)
            .with_context(|| format!("failed to write {}", temporary.display()))?;
        file.sync_all()
            .with_context(|| format!("failed to sync {}", temporary.display()))?;
        drop(file);
        match fs::hard_link(&temporary, path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == ErrorKind::AlreadyExists => Err(error.into()),
            Err(error) => {
                Err(error).with_context(|| format!("failed to publish {}", path.display()))
            }
        }
    })();
    let _ = fs::remove_file(&temporary);
    result
}

/// Copy a configuration file to a sibling `.bak-<timestamp>[-n]` that never
/// overwrites an existing backup and stays private.
fn backup_config_file(path: &Path) -> Result<Option<PathBuf>> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to read {}", path.display()))
        }
    };
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default();
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .context("configuration path has no file name")?;
    for attempt in 0..100u32 {
        let suffix = if attempt == 0 {
            format!("{file_name}.bak-{stamp}")
        } else {
            format!("{file_name}.bak-{stamp}-{attempt}")
        };
        let backup = path.with_file_name(suffix);
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(&backup) {
            Ok(mut file) => {
                #[cfg(windows)]
                treer_platform::secure_file(&backup)?;
                use std::io::Write;
                file.write_all(&bytes)
                    .with_context(|| format!("failed to write {}", backup.display()))?;
                file.sync_all()
                    .with_context(|| format!("failed to sync {}", backup.display()))?;
                return Ok(Some(backup));
            }
            Err(error) if error.kind() == ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(error).with_context(|| format!("failed to create {}", backup.display()))
            }
        }
    }
    bail!(
        "too many configuration backups exist for {}",
        path.display()
    );
}

/// Cross-process, per-workspace exclusion around enrollment recovery, machine
/// selection, the claim and the configuration replacement. Held by `connect`
/// and `service repair` until the switch and registration are complete, so a
/// concurrent run can never mistake an active replacement for crash residue.
/// Unix: an advisory lock on a stable, never-unlinked 0600 file in the config
/// directory (CLOEXEC, released by the OS when the process ends). Windows:
/// the named per-user mutex `LocalLock`.
pub struct RegistrationLock {
    workspace: String,
    #[cfg(unix)]
    _file: fs::File,
    #[cfg(windows)]
    _lock: treer_platform::LocalLock,
}

impl RegistrationLock {
    fn check(&self, workspace: &str) -> Result<()> {
        if self.workspace != workspace {
            bail!("registration lock belongs to another workspace");
        }
        Ok(())
    }
}

fn registration_busy(workspace: &str) -> anyhow::Error {
    anyhow::anyhow!(
        "another connect or repair for workspace {workspace} is in progress; retry after it finishes"
    )
}

pub fn lock_machine_registration(workspace: &str) -> Result<RegistrationLock> {
    validate_workspace(workspace)?;
    lock_machine_registration_in(&config_dir()?, workspace)
}

fn lock_machine_registration_in(config_dir: &Path, workspace: &str) -> Result<RegistrationLock> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        fs::create_dir_all(config_dir)
            .with_context(|| format!("failed to create {}", config_dir.display()))?;
        let path = config_dir.join(format!(
            "{}-machine-registration.lock",
            component_key(workspace)
        ));
        // std opens with O_CLOEXEC: spawned Hosts/Controllers never inherit it.
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&path)
            .with_context(|| format!("failed to open {}", path.display()))?;
        let locked = treer_platform::try_lock_file_exclusive(&file)
            .with_context(|| format!("failed to lock {}", path.display()))?;
        if !locked {
            return Err(registration_busy(workspace));
        }
        Ok(RegistrationLock {
            workspace: workspace.to_string(),
            _file: file,
        })
    }
    #[cfg(windows)]
    {
        let _ = config_dir;
        let lock = treer_platform::LocalLock::acquire(&format!(
            "machine-registration-{}",
            component_key(workspace)
        ))
        .map_err(|error| {
            if error.kind() == ErrorKind::TimedOut {
                registration_busy(workspace)
            } else {
                anyhow::Error::new(error)
            }
        })?;
        Ok(RegistrationLock {
            workspace: workspace.to_string(),
            _lock: lock,
        })
    }
}

/// Switch this installation's registration from `previous` to `next` (a new
/// machine ID the Proxy minted for it). Steps, each durable before the next:
/// journal, backups of the previous files, new configuration files, stop and
/// remove the previous registration, remove the journal, register and start.
/// The previous machine's state (startup store, private state) is neither
/// read nor migrated: a deleted machine's Agents never come back.
pub async fn replace_machine_registration(
    lock: &RegistrationLock,
    previous: &ServiceConfig,
    next: ServiceConfig,
) -> Result<ServiceActivation> {
    let config_dir = config_dir()?;
    replacement_transaction(
        lock,
        &config_dir,
        state_dir()?,
        previous,
        &next,
        platform::uninstall,
    )?;
    register(next.clone())?;
    let (paths, installed) = installed_service(&next.workspace)?;
    activate_registered_service(&paths, &installed).await
}

fn replacement_transaction(
    lock: &RegistrationLock,
    config_dir: &Path,
    state_dir: PathBuf,
    previous: &ServiceConfig,
    next: &ServiceConfig,
    uninstall_previous: impl Fn(&ServicePaths, &ServiceConfig) -> Result<()>,
) -> Result<()> {
    lock.check(&next.workspace)?;
    validate_proxy_server_id(&next.server_id)?;
    if next.workspace != previous.workspace {
        bail!("machine replacement must stay within the enrolled workspace");
    }
    if next.server_id == previous.server_id {
        bail!("machine replacement requires a new machine ID");
    }
    let journal_path = replacement_journal_path(config_dir, &next.workspace);
    let previous_paths =
        ServicePaths::in_directories(config_dir, state_dir.clone(), &previous.server_id)?;
    let next_paths = ServicePaths::in_directories(config_dir, state_dir, &next.server_id)?;
    let journal = MachineReplacementJournal {
        workspace: next.workspace.clone(),
        previous_server_id: previous.server_id.clone(),
        server_id: next.server_id.clone(),
        created_at: chrono::Utc::now().to_rfc3339(),
    };
    // Exclusive creation: a concurrent connect on the same host sees the
    // journal and is refused instead of racing the switch.
    create_private_file_exclusively(
        &journal_path,
        &serde_json::to_vec_pretty(&journal).context("failed to encode the replacement journal")?,
    )
    .map_err(|error| {
        if error
            .downcast_ref::<std::io::Error>()
            .is_some_and(|error| error.kind() == ErrorKind::AlreadyExists)
        {
            anyhow::anyhow!(
                "an unfinished machine replacement exists for workspace {}; run `treer-agent-server service --workspace {} repair` first",
                next.workspace,
                next.workspace
            )
        } else {
            error
        }
    })?;
    for path in [&next_paths.config, &next_paths.host_config] {
        if path.exists() {
            remove_if_exists(&journal_path)?;
            bail!(
                "configuration for machine {} already exists at {}",
                next.server_id,
                path.display()
            );
        }
    }
    backup_config_file(&previous_paths.config)?;
    backup_config_file(&previous_paths.host_config)?;
    // New files first: from here on an interruption is finished by
    // `finish_machine_replacement`, which prefers the journal's new machine.
    next.save(&next_paths.config)?;
    save_json(
        &HostDaemonConfig {
            socket_path: next.host_socket.clone(),
            controller_path: next_paths.executable.clone(),
            controller_config_path: next_paths.config.clone(),
            root: next.root.clone(),
        },
        &next_paths.host_config,
    )?;
    complete_machine_replacement(
        config_dir,
        &previous_paths,
        &journal_path,
        uninstall_previous,
    )
}

fn complete_machine_replacement(
    config_dir: &Path,
    previous_paths: &ServicePaths,
    journal_path: &Path,
    uninstall_previous: impl Fn(&ServicePaths, &ServiceConfig) -> Result<()>,
) -> Result<()> {
    let _ = config_dir;
    match ServiceConfig::load(&previous_paths.config) {
        Ok(previous) => {
            eprintln!(
                "treer: stopping and removing the previous machine {} (its Host and any Agents still running on it end now)",
                previous.server_id
            );
            uninstall_previous(previous_paths, &previous).with_context(|| {
                format!(
                    "the previous machine {} could not be stopped; rerun `treer-agent-server service --workspace {} repair` once it is stopped",
                    previous.server_id, previous.workspace
                )
            })?;
            remove_if_exists(&previous_paths.config)?;
            remove_if_exists(&previous_paths.host_config)?;
        }
        Err(error) if previous_paths.config.exists() => return Err(error),
        Err(_) => {}
    }
    remove_if_exists(journal_path)
}

/// Finish an interrupted machine replacement before any other service work.
/// Without new configuration files nothing was switched: the journal is
/// discarded and the previous registration stays authoritative.
pub fn finish_machine_replacement(lock: &RegistrationLock, workspace: &str) -> Result<()> {
    validate_workspace(workspace)?;
    let config_dir = config_dir()?;
    finish_machine_replacement_in(
        lock,
        &config_dir,
        state_dir()?,
        workspace,
        platform::uninstall,
    )
}

fn finish_machine_replacement_in(
    lock: &RegistrationLock,
    config_dir: &Path,
    state_dir: PathBuf,
    workspace: &str,
    uninstall_previous: impl Fn(&ServicePaths, &ServiceConfig) -> Result<()>,
) -> Result<()> {
    lock.check(workspace)?;
    let Some(journal) = load_replacement_journal(config_dir, workspace)? else {
        return Ok(());
    };
    let journal_path = replacement_journal_path(config_dir, workspace);
    let next_paths =
        ServicePaths::in_directories(config_dir, state_dir.clone(), &journal.server_id)?;
    if !next_paths.config.exists() || !next_paths.host_config.exists() {
        remove_if_exists(&next_paths.config)?;
        remove_if_exists(&next_paths.host_config)?;
        remove_if_exists(&journal_path)?;
        eprintln!(
            "treer: discarded an unfinished machine replacement for workspace {workspace}; the previous registration stays in place"
        );
        return Ok(());
    }
    let previous_paths =
        ServicePaths::in_directories(config_dir, state_dir, &journal.previous_server_id)?;
    complete_machine_replacement(
        config_dir,
        &previous_paths,
        &journal_path,
        uninstall_previous,
    )?;
    eprintln!(
        "treer: finished the machine replacement for workspace {workspace}; machine {} is registered",
        journal.server_id
    );
    Ok(())
}

fn installed_services() -> Result<Vec<(ServicePaths, ServiceConfig)>> {
    let hostname = current_hostname()?;
    let directory = config_dir()?;
    let entries = match fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to read {}", directory.display()))
        }
    };
    let mut services = Vec::new();
    for entry in entries {
        let entry = entry.with_context(|| format!("failed to read {}", directory.display()))?;
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !name.ends_with("-controller.json") {
            continue;
        }
        let config = ServiceConfig::load(&path)?;
        if config.install_hostname != hostname {
            continue;
        }
        let paths = ServicePaths::new(&config.server_id)?;
        services.push((paths, config));
    }
    services.sort_by(|(_, left), (_, right)| left.server_id.cmp(&right.server_id));
    Ok(services)
}

pub fn require_install_hostname(config: &ServiceConfig) -> Result<()> {
    let current = current_hostname()?;
    if config.install_hostname != current {
        bail!(
            "service for machine {} is pinned to host {}, not {current}",
            config.server_id,
            config.install_hostname
        );
    }
    Ok(())
}

fn config_dir() -> Result<PathBuf> {
    treer_platform::config_directory().context("user configuration directory is unavailable")
}

/// Canonical Treer state directory. Every file below it (startup stores,
/// private state, provider CLI results, service logs, machine identity) is
/// Controller-owned; the machine file-transfer root reserves it by identity.
pub fn state_dir() -> Result<PathBuf> {
    #[cfg(windows)]
    return env::var_os("TREER_STATE_DIR")
        .map(PathBuf::from)
        .or_else(|| env::var_os("LOCALAPPDATA").map(|root| PathBuf::from(root).join("Treer/state")))
        .context("LOCALAPPDATA is required to store Treer state");
    #[cfg(not(windows))]
    {
        let home = home_dir()?;
        Ok(env::var_os("TREER_STATE_DIR")
            .map(PathBuf::from)
            .or_else(|| env::var_os("XDG_STATE_HOME").map(|path| PathBuf::from(path).join("treer")))
            .unwrap_or_else(|| home.join(".local/state/treer")))
    }
}

pub fn startup_state_path(server_id: &str) -> Result<PathBuf> {
    Ok(state_dir()?.join(format!("{server_id}-agent-startup.json")))
}

/// Controller-owned root for per-Agent private state; see `private_state`.
pub fn agent_private_state_root() -> Result<PathBuf> {
    Ok(state_dir()?.join(crate::private_state::ROOT_DIRECTORY_NAME))
}

/// Controller-owned root for provider CLI jobs and results; see `provider_cli`.
pub fn provider_cli_root() -> Result<PathBuf> {
    Ok(state_dir()?.join(crate::provider_cli::ROOT_DIRECTORY_NAME))
}

#[cfg(unix)]
fn runtime_dir() -> Result<PathBuf> {
    if let Some(path) = env::var_os("TREER_RUNTIME_DIR") {
        return Ok(PathBuf::from(path));
    }
    let uid = current_uid()?;
    #[cfg(unix)]
    let uid_number = uid
        .parse::<u32>()
        .context("id -u returned an invalid user id")?;
    if let Some(path) = env::var_os("XDG_RUNTIME_DIR") {
        let path = PathBuf::from(path);
        #[cfg(unix)]
        if runtime_base_is_private_and_writable(&path, uid_number) {
            return Ok(path.join("treer"));
        }
        #[cfg(not(unix))]
        return Ok(path.join("treer"));
    }
    #[cfg(target_os = "linux")]
    {
        if let Some(path) = linux_user_runtime_dir(Path::new("/run/user"), &uid, uid_number) {
            return Ok(path);
        }
        private_state_runtime_dir()
    }
    #[cfg(not(target_os = "linux"))]
    Ok(env::temp_dir().join(format!("treer-{uid}")))
}

#[cfg(target_os = "linux")]
fn linux_user_runtime_dir(run_user_root: &Path, uid: &str, uid_number: u32) -> Option<PathBuf> {
    let path = run_user_root.join(uid);
    if runtime_base_is_private_and_writable(&path, uid_number) {
        Some(path.join("treer"))
    } else {
        None
    }
}

#[cfg(unix)]
fn runtime_base_is_private_and_writable(path: &Path, uid: u32) -> bool {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    metadata.is_dir()
        && metadata.uid() == uid
        && metadata.permissions().mode() & 0o300 == 0o300
        && metadata.permissions().mode() & 0o077 == 0
}

#[cfg(target_os = "linux")]
fn private_state_runtime_dir() -> Result<PathBuf> {
    prepare_private_runtime_dir(state_dir()?.join("run"))
}

#[cfg(any(target_os = "linux", test))]
#[cfg(unix)]
fn prepare_private_runtime_dir(path: PathBuf) -> Result<PathBuf> {
    fs::create_dir_all(&path).with_context(|| {
        format!(
            "failed to create fallback runtime directory {}",
            path.display()
        )
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).with_context(|| {
            format!(
                "failed to secure fallback runtime directory {}",
                path.display()
            )
        })?;
    }
    Ok(path)
}

#[cfg(unix)]
fn current_uid() -> Result<String> {
    let output = Command::new("id")
        .arg("-u")
        .output()
        .context("failed to determine the current user id")?;
    require_success(output.status, "id -u")?;
    Ok(String::from_utf8(output.stdout)
        .context("id -u returned non-UTF-8 output")?
        .trim()
        .to_string())
}

#[cfg(unix)]
fn host_socket_filename(server_id: &str) -> String {
    format!("h-{:016x}.sock", fnv1a64(server_id.as_bytes()))
}

#[cfg(unix)]
fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

/// Usable `sun_path` bytes, excluding the trailing NUL.
#[cfg(any(target_os = "macos", target_os = "ios"))]
const MAX_UNIX_SOCKET_PATH_BYTES: usize = 103;
#[cfg(all(unix, not(any(target_os = "macos", target_os = "ios"))))]
const MAX_UNIX_SOCKET_PATH_BYTES: usize = 107;

#[cfg(unix)]
fn fit_unix_socket_path(preferred: PathBuf, filename: &str) -> Result<PathBuf> {
    if unix_path_byte_len(&preferred) <= MAX_UNIX_SOCKET_PATH_BYTES {
        return Ok(preferred);
    }
    let fallback = PathBuf::from("/tmp")
        .join(format!("treer-{}", current_uid()?))
        .join(filename);
    if unix_path_byte_len(&fallback) <= MAX_UNIX_SOCKET_PATH_BYTES {
        return Ok(fallback);
    }
    bail!(
        "host socket path exceeds the unix sockaddr limit ({} bytes): {}",
        MAX_UNIX_SOCKET_PATH_BYTES,
        fallback.display()
    )
}

#[cfg(unix)]
fn unix_path_byte_len(path: &Path) -> usize {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        path.as_os_str().as_bytes().len()
    }
    #[cfg(not(unix))]
    {
        path.to_string_lossy().len()
    }
}

#[cfg(unix)]
fn home_dir() -> Result<PathBuf> {
    env::var_os("HOME")
        .map(PathBuf::from)
        .context("HOME is required to manage the agent-server service")
}

fn validate_workspace(workspace: &str) -> Result<()> {
    if workspace.trim().is_empty() {
        bail!("workspace must not be empty");
    }
    Ok(())
}

fn validate_server_id(server_id: &str) -> Result<()> {
    if server_id.trim().is_empty()
        || server_id.len() > 128
        || !server_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        bail!("server ID contains unsupported characters");
    }
    Ok(())
}

#[cfg(test)]
fn workspace_key(workspace: &str) -> String {
    component_key(workspace)
}

fn component_key(value: &str) -> String {
    value
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

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    #[cfg(windows)]
    treer_platform::secure_directory(path.parent().context("configuration has no parent")?)?;
    let temporary = path.with_extension("tmp");
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&temporary)
        .with_context(|| format!("failed to write {}", temporary.display()))?;
    #[cfg(windows)]
    treer_platform::secure_file(&temporary)?;
    use std::io::Write;
    file.write_all(bytes)
        .with_context(|| format!("failed to write {}", temporary.display()))?;
    file.sync_all()
        .with_context(|| format!("failed to sync {}", temporary.display()))?;
    drop(file);
    treer_platform::replace_file(&temporary, path)
        .with_context(|| format!("failed to replace {}", path.display()))
}

fn save_json(value: &impl Serialize, path: &Path) -> Result<()> {
    let parent = path
        .parent()
        .context("configuration path has no parent directory")?;
    fs::create_dir_all(parent).with_context(|| format!("failed to create {}", parent.display()))?;
    let bytes = serde_json::to_vec_pretty(value).context("failed to encode configuration")?;
    write_atomic(path, &bytes)
}

fn restart_controller_at(paths: &ServicePaths, socket: &Path) -> Result<()> {
    let output = Command::new(&paths.host_executable)
        .arg("restart-controller")
        .arg("--socket")
        .arg(socket)
        .output()
        .context("failed to run treer-agent-host restart-controller")?;
    if !output.status.success() {
        bail!(
            "treer-agent-host restart-controller exited with {}: {}{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    use std::io::Write;
    std::io::stdout().write_all(&output.stdout)?;
    std::io::stderr().write_all(&output.stderr)?;
    Ok(())
}

fn remove_if_exists(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("failed to remove {}", path.display())),
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[derive(Debug, Serialize, Deserialize)]
struct NohupProcess {
    pid: u32,
    started_at: String,
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn nohup_executable() -> std::ffi::OsString {
    env::var_os("TREER_NOHUP").unwrap_or_else(|| "nohup".into())
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn require_nohup() -> Result<()> {
    let executable = nohup_executable();
    require_executable(
        &executable,
        "install coreutils before connecting this machine",
    )?;
    require_executable(
        OsStr::new("sh"),
        "install a POSIX shell before connecting this machine",
    )?;
    process_started_at(std::process::id())?
        .context("ps did not return this process's start identity; install procps on Linux before connecting this machine")?;
    require_executable(
        OsStr::new("kill"),
        "install procps on Linux before connecting this machine",
    )?;
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn require_executable(executable: &OsStr, hint: &str) -> Result<()> {
    let path = Path::new(&executable);
    if path.components().count() > 1 {
        if path.is_file() {
            return Ok(());
        }
        bail!("executable was not found at {}; {hint}", path.display());
    }
    let found = env::var_os("PATH").is_some_and(|value| {
        env::split_paths(&value)
            .map(|directory| directory.join(path))
            .any(|candidate| candidate.is_file())
    });
    if found {
        Ok(())
    } else {
        bail!("{} was not found on PATH; {hint}", path.display())
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn nohup_pid_path(paths: &ServicePaths, config: &ServiceConfig) -> PathBuf {
    paths.state_dir.join(format!(
        "agent-host-{}.pid",
        component_key(&config.server_id)
    ))
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn service_log_path(paths: &ServicePaths, config: &ServiceConfig) -> PathBuf {
    paths.state_dir.join(format!(
        "agent-server-{}.log",
        component_key(&config.server_id)
    ))
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn read_nohup_process(
    paths: &ServicePaths,
    config: &ServiceConfig,
) -> Result<Option<NohupProcess>> {
    let path = nohup_pid_path(paths, config);
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to read {}", path.display()))
        }
    };
    serde_json::from_slice(&bytes)
        .with_context(|| format!("invalid nohup PID file {}", path.display()))
        .map(Some)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn process_started_at(pid: u32) -> Result<Option<String>> {
    let output = Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "lstart="])
        .output()
        .context("failed to inspect the nohup Host process with ps; install procps on Linux before connecting this machine")?;
    if !output.status.success() {
        return Ok(None);
    }
    let started_at = String::from_utf8(output.stdout)
        .context("ps returned non-UTF-8 process metadata")?
        .trim()
        .to_string();
    Ok((!started_at.is_empty()).then_some(started_at))
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn nohup_process_is_current(process: &NohupProcess) -> Result<bool> {
    Ok(process_started_at(process.pid)?.as_deref() == Some(process.started_at.as_str()))
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn start_nohup(paths: &ServicePaths, config: &ServiceConfig) -> Result<()> {
    require_nohup()?;
    start_nohup_with(&nohup_executable(), paths, config)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn start_nohup_with(nohup: &OsStr, paths: &ServicePaths, config: &ServiceConfig) -> Result<()> {
    let pid_path = nohup_pid_path(paths, config);
    if let Some(process) = read_nohup_process(paths, config)? {
        if nohup_process_is_current(&process)? {
            if host_is_running(&config.host_socket) {
                println!(
                    "treer: nohup Host is already running with PID {}",
                    process.pid
                );
                return Ok(());
            }
            bail!(
                "nohup Host PID {} is running but its socket is unavailable; wait for startup or stop it before retrying",
                process.pid
            );
        }
        remove_if_exists(&pid_path)?;
    }
    if host_is_running(&config.host_socket) {
        bail!(
            "a Host is already using {} but Treer has no matching nohup PID file",
            config.host_socket.display()
        );
    }

    fs::create_dir_all(&paths.state_dir)
        .with_context(|| format!("failed to create {}", paths.state_dir.display()))?;
    let log_path = service_log_path(paths, config);
    let mut options = fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let log = options
        .open(&log_path)
        .with_context(|| format!("failed to open {}", log_path.display()))?;
    drop(log);
    let mut launcher = Command::new("sh");
    launcher
        .env_clear()
        .envs(treer_platform::inherited_environment())
        .arg("-c")
        .arg(r#""$1" "$2" run --config "$3" </dev/null >>"$4" 2>&1 & printf '%s\n' "$!""#)
        .arg("treer-nohup")
        .arg(nohup)
        .arg(&paths.host_executable)
        .arg(&paths.host_config)
        .arg(&log_path);
    // The Host must outlive whatever started it. A caller's process group can
    // be reaped as a unit (launchd kills a job's process group when the job
    // exits), so the launcher shell and the Host it backgrounds start in their
    // own process group. nohup already covers terminal hangup.
    {
        use std::os::unix::process::CommandExt;
        launcher.process_group(0);
    }
    let output = launcher
        .output()
        .with_context(|| format!("failed to start {}", PathBuf::from(nohup).display()))?;
    require_success(output.status, "nohup Host launcher")?;
    let pid = String::from_utf8(output.stdout)
        .context("nohup Host launcher returned a non-UTF-8 PID")?
        .trim()
        .parse::<u32>()
        .context("nohup Host launcher returned an invalid PID")?;
    let started_at = process_started_at(pid)?.with_context(|| {
        format!("nohup Host PID {pid} exited before its process metadata could be recorded")
    })?;
    let process = NohupProcess { pid, started_at };
    if let Err(error) = save_json(&process, &pid_path) {
        let _ = Command::new("kill")
            .arg("-TERM")
            .arg(pid.to_string())
            .status();
        return Err(error).context("failed to record the nohup Host PID");
    }
    println!("treer: started nohup Host with PID {pid}");
    println!("treer: Host log: {}", log_path.display());
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn stop_nohup(paths: &ServicePaths, config: &ServiceConfig, wait: bool) -> Result<()> {
    let pid_path = nohup_pid_path(paths, config);
    let Some(process) = read_nohup_process(paths, config)? else {
        if host_is_running(&config.host_socket) {
            bail!(
                "a Host is using {} but its nohup PID file is missing; refusing to signal an unknown process",
                config.host_socket.display()
            );
        }
        return Ok(());
    };
    if !nohup_process_is_current(&process)? {
        remove_if_exists(&pid_path)?;
        if host_is_running(&config.host_socket) {
            bail!("the nohup PID file is stale while the Host socket is still active");
        }
        return Ok(());
    }
    let status = Command::new("kill")
        .arg("-TERM")
        .arg(process.pid.to_string())
        .status()
        .with_context(|| format!("failed to signal nohup Host PID {}", process.pid))?;
    require_success(status, "kill -TERM")?;
    if !wait {
        return Ok(());
    }
    let deadline = Instant::now() + NOHUP_STOP_TIMEOUT;
    while nohup_process_is_current(&process)? {
        if Instant::now() >= deadline {
            bail!(
                "nohup Host PID {} did not stop within {} seconds",
                process.pid,
                NOHUP_STOP_TIMEOUT.as_secs()
            );
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    remove_if_exists(&pid_path)?;
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn status_nohup(paths: &ServicePaths, config: &ServiceConfig) -> Result<()> {
    let process = read_nohup_process(paths, config)?;
    let process_running = process
        .as_ref()
        .map(nohup_process_is_current)
        .transpose()?
        .unwrap_or(false);
    let socket_running = host_is_running(&config.host_socket);
    println!("Manager: nohup");
    println!(
        "PID: {}",
        process
            .as_ref()
            .map_or_else(|| "unknown".to_string(), |process| process.pid.to_string())
    );
    println!("Log: {}", service_log_path(paths, config).display());
    match (process_running, socket_running) {
        (true, true) => {
            println!("Status: running");
            Ok(())
        }
        (true, false) => bail!("Status: process is running but the Host socket is unavailable"),
        (false, true) => bail!("Status: Host socket is active but the recorded PID is not running"),
        (false, false) => bail!("Status: stopped"),
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn logs_nohup(
    paths: &ServicePaths,
    config: &ServiceConfig,
    lines: usize,
    follow: bool,
) -> Result<()> {
    let mut command = Command::new("tail");
    command.args(["-n", &lines.to_string()]);
    if follow {
        command.arg("-f");
    }
    command.arg(service_log_path(paths, config));
    run_checked(&mut command, "tail")
}

#[cfg(unix)]
fn run_checked(command: &mut Command, description: &str) -> Result<()> {
    let status = command
        .status()
        .with_context(|| format!("failed to run {description}"))?;
    require_success(status, description)
}

fn require_success(status: ExitStatus, description: &str) -> Result<()> {
    if status.success() {
        Ok(())
    } else {
        bail!("{description} exited with {status}")
    }
}

#[cfg(any(target_os = "linux", test))]
fn systemd_unit(
    executable: &Path,
    config: &Path,
    workspace: &str,
    install_hostname: &str,
) -> String {
    format!(
        "[Unit]\nDescription=Treer agent server ({workspace})\nConditionHost={install_hostname}\nAfter=network-online.target\nWants=network-online.target\n\n[Service]\nType=simple\nExecStart={} run --config {}\nRestart=always\nRestartSec=2\n\n[Install]\nWantedBy=default.target\n",
        systemd_quote(executable),
        systemd_quote(config)
    )
}

#[cfg(any(target_os = "linux", test))]
fn systemd_quote(path: &Path) -> String {
    let escaped = path
        .to_string_lossy()
        .replace('%', "%%")
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    format!("\"{escaped}\"")
}

#[cfg(any(target_os = "macos", test))]
fn launchd_plist(
    executable: &Path,
    config: &Path,
    label: &str,
    stdout_path: &Path,
    stderr_path: &Path,
) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{label}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{executable}</string>
    <string>run</string>
    <string>--config</string>
    <string>{config}</string>
  </array>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <true/>
  <key>ProcessType</key>
  <string>Background</string>
  <key>StandardOutPath</key>
  <string>{stdout_path}</string>
  <key>StandardErrorPath</key>
  <string>{stderr_path}</string>
</dict>
</plist>
"#,
        label = xml_escape(label),
        executable = xml_escape(&executable.to_string_lossy()),
        config = xml_escape(&config.to_string_lossy()),
        stdout_path = xml_escape(&stdout_path.to_string_lossy()),
        stderr_path = xml_escape(&stderr_path.to_string_lossy()),
    )
}

#[cfg(any(target_os = "macos", test))]
fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// `launchctl bootstrap` of a disabled LaunchAgent fails with
/// "Could not find service … Bootstrap failed: 5: Input/output error".
#[cfg(any(target_os = "macos", test))]
fn launchd_start_steps(loaded: bool, domain: &str, target: &str, plist: &Path) -> Vec<Vec<String>> {
    let mut steps = vec![vec!["enable".to_string(), target.to_string()]];
    if loaded {
        steps.push(vec!["kickstart".to_string(), target.to_string()]);
    } else {
        steps.push(vec![
            "bootstrap".to_string(),
            domain.to_string(),
            plist.to_string_lossy().into_owned(),
        ]);
    }
    steps
}

#[cfg(test)]
#[path = "service_tests.rs"]
mod tests;

#[cfg(all(test, unix))]
#[path = "service_environment_tests.rs"]
mod environment_tests;
