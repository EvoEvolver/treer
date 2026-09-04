use std::env;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::ffi::OsStr;
use std::fs;
use std::io::ErrorKind;
use std::net::{Ipv4Addr, SocketAddr, TcpListener};
#[cfg(unix)]
use std::os::unix::net::UnixStream;
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

const FIRST_AUTOMATIC_PORT: u16 = 8790;
const MAX_UPDATE_BINARY_BYTES: usize = 128 * 1024 * 1024;
const UPDATE_DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(120);
const UPDATE_VALIDATION_TIMEOUT: Duration = Duration::from_secs(10);
const CONTROLLER_START_TIMEOUT: Duration = Duration::from_secs(15);
const NOHUP_STOP_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ServiceMode {
    Auto,
    Systemd,
    Launchd,
    Nohup,
    Foreground,
}

impl std::fmt::Display for ServiceMode {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Auto => "auto",
            Self::Systemd => "systemd",
            Self::Launchd => "launchd",
            Self::Nohup => "nohup",
            Self::Foreground => "foreground",
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
}

impl std::fmt::Display for ServiceManager {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::SystemdUser => "systemd-user",
            Self::Launchd => "launchd",
            Self::Nohup => "nohup",
            Self::Foreground => "foreground",
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
            },
            fallback_reason: self.service_fallback_reason.clone(),
        }
    }
}

pub async fn resolve_listen(workspace: &str, requested: Option<SocketAddr>) -> Result<SocketAddr> {
    validate_workspace(workspace)?;
    if let Some(requested) = requested {
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

#[cfg(unix)]
fn host_is_running(socket: &Path) -> bool {
    UnixStream::connect(socket).is_ok()
}

pub enum ServiceActivation {
    Managed,
    Foreground(tokio::process::Child),
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
    let (_, installed) = installed_service(&workspace)?;
    if installed.service_manager == ServiceManager::Foreground {
        if restart_controller(&workspace).is_ok() {
            wait_for_controller_and_proxy(&installed).await?;
            return Ok(ServiceActivation::Managed);
        }
        return start_and_wait(&workspace).await;
    }
    if let Err(error) = restart_controller(&workspace) {
        eprintln!(
            "treer: warning: hot Controller restart failed ({error:#}); restarting the Host service"
        );
        restart(&workspace)?;
    }
    wait_for_controller_and_proxy(&installed).await?;
    Ok(ServiceActivation::Managed)
}

pub fn registered_config(workspace: &str) -> Result<Option<ServiceConfig>> {
    validate_workspace(workspace)?;
    Ok(find_installed_service(workspace)?.map(|(_, config)| config))
}

pub fn preflight_registration(workspace: &str, mode: ServiceMode) -> Result<ServiceSelection> {
    validate_workspace(workspace)?;
    require_host_binary(&ServicePaths::new("preflight")?)?;
    host_socket_path("preflight")?;
    let selection = select_service_manager(mode)?;
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

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
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
    let name = host_socket_filename(server_id);
    let path = fit_unix_socket_path(runtime_dir()?.join(&name), &name)?;
    Ok(path)
}

pub fn start(workspace: &str) -> Result<()> {
    let (paths, config) = installed_service(workspace)?;
    platform::start(&paths, &config)
}

pub async fn start_and_wait(workspace: &str) -> Result<ServiceActivation> {
    let (paths, config) = installed_service(workspace)?;
    let mut activation = if config.service_manager == ServiceManager::Foreground {
        eprintln!(
            "treer: starting Host in foreground mode; keep this terminal open or run the command under a process supervisor"
        );
        let child = tokio::process::Command::new(&paths.host_executable)
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
        ServiceActivation::Foreground(child)
    } else {
        platform::start(&paths, &config)?;
        ServiceActivation::Managed
    };

    if let Err(error) = wait_for_controller_and_proxy(&config).await {
        if let ServiceActivation::Foreground(child) = &mut activation {
            let _ = child.kill().await;
            let _ = child.wait().await;
        } else if config.service_manager == ServiceManager::Nohup {
            let _ = platform::stop(&paths, &config);
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

fn should_migrate_host_socket(current: &Path, preferred: &Path) -> bool {
    if current == preferred {
        return false;
    }
    #[cfg(unix)]
    return !host_is_running(current);
    #[cfg(not(unix))]
    true
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

pub async fn repair_and_wait(workspace: &str, mode: ServiceMode) -> Result<ServiceActivation> {
    let (_, mut config) = installed_service(workspace)?;
    let selection = preflight_registration(workspace, mode)?;
    selection.announce();
    config.service_manager = selection.manager;
    config.service_fallback_reason = selection.fallback_reason;
    let activation = refresh_registration_and_wait(config).await?;
    println!("treer: service registration repaired without a new enrollment key");
    Ok(activation)
}

pub fn stop(workspace: &str) -> Result<()> {
    let (paths, config) = installed_service(workspace)?;
    platform::stop(&paths, &config)
}

pub fn stop_remotely(workspace: &str) -> Result<()> {
    let (paths, config) = installed_service(workspace)?;
    platform::stop_remotely(&paths, &config)
}

pub fn restart(workspace: &str) -> Result<()> {
    let (paths, config) = installed_service(workspace)?;
    platform::restart(&paths, &config)
}

pub fn restart_controller(workspace: &str) -> Result<()> {
    let (paths, config) = installed_service(workspace)?;
    restart_controller_at(&paths, &config.host_socket)
}

pub async fn update(proxy_override: Option<Url>) -> Result<()> {
    let services = installed_services()?;
    let (paths, source_config) = services.first().context(
        "no agent-server services are installed on this host; connect a machine before updating",
    )?;
    let activation_services = activation_services(&services)?;
    let treer_executable = installed_treer_binary(&paths.executable).with_context(|| {
        format!(
            "could not find the installed treer CLI for {}",
            paths.executable.display()
        )
    })?;
    let platform = artifact_platform(std::env::consts::OS, std::env::consts::ARCH)?;
    let proxy = resolve_update_proxy(proxy_override, source_config)?;
    let controller_url = artifact_url(&proxy, platform, "treer-agent-server")?;
    let treer_url = artifact_url(&proxy, platform, "treer")?;
    let client = reqwest::Client::builder()
        .timeout(UPDATE_DOWNLOAD_TIMEOUT)
        // Agent terminals inherit Treer's socks5h proxy. Updating the
        // Controller must not depend on the data plane it is replacing.
        .no_proxy()
        .build()
        .context("failed to create update client")?;

    println!("treer: downloading latest {platform} Controller and CLI from {proxy}");
    let (controller_bytes, treer_bytes) = tokio::try_join!(
        download_binary(&client, controller_url, "treer-agent-server"),
        download_binary(&client, treer_url, "treer"),
    )?;
    let installed_controller = fs::read(&paths.executable)
        .with_context(|| format!("failed to read {}", paths.executable.display()))?;
    let installed_treer = fs::read(&treer_executable)
        .with_context(|| format!("failed to read {}", treer_executable.display()))?;
    let controller_changed = installed_controller != controller_bytes;
    let treer_changed = installed_treer != treer_bytes;

    let staged_controller = controller_changed
        .then(|| stage_executable(&paths.executable, &controller_bytes, "update", true))
        .transpose()?;
    let staged_treer = treer_changed
        .then(|| stage_executable(&treer_executable, &treer_bytes, "update", true))
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

fn resolve_update_proxy(proxy_override: Option<Url>, source_config: &ServiceConfig) -> Result<Url> {
    let proxy = proxy_override.map_or_else(
        || Url::parse(&source_config.proxy).context("invalid Proxy URL in service config"),
        Ok,
    )?;
    if !matches!(proxy.scheme(), "http" | "https") {
        bail!("update Proxy URL must use http or https");
    }
    Ok(proxy)
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

fn artifact_url(proxy: &Url, platform: &str, binary: &str) -> Result<Url> {
    proxy
        .join(&format!("artifacts/{platform}/{binary}"))
        .with_context(|| format!("failed to build the {binary} artifact URL"))
}

async fn download_binary(client: &reqwest::Client, url: Url, binary: &str) -> Result<Vec<u8>> {
    let response = client
        .get(url.clone())
        .send()
        .await
        .with_context(|| format!("failed to download {binary} from {url}"))?
        .error_for_status()
        .with_context(|| format!("failed to download {binary} from {url}"))?;
    if response
        .content_length()
        .is_some_and(|length| length > MAX_UPDATE_BINARY_BYTES as u64)
    {
        bail!("downloaded {binary} exceeds the 128 MiB update limit");
    }
    let bytes = response
        .bytes()
        .await
        .with_context(|| format!("failed to read downloaded {binary}"))?;
    if bytes.is_empty() {
        bail!("downloaded {binary} is empty");
    }
    if bytes.len() > MAX_UPDATE_BINARY_BYTES {
        bail!("downloaded {binary} exceeds the 128 MiB update limit");
    }
    Ok(bytes.to_vec())
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

        let mut file = options
            .open(&temporary)
            .with_context(|| format!("failed to stage {}", destination.display()))?;
        file.write_all(bytes)
            .with_context(|| format!("failed to stage {}", destination.display()))?;
        file.sync_all()
            .with_context(|| format!("failed to sync {}", temporary.display()))?;
        drop(file);
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
    let mut child = Command::new(path)
        .arg("--help")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("downloaded {file_name} is not executable on this machine"))?;
    let deadline = Instant::now() + UPDATE_VALIDATION_TIMEOUT;
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
        let home = home_dir()?;
        let config_home = env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".config"));
        let state_dir = state_dir()?;
        let key = component_key(server_id);
        let executable = env::current_exe()
            .context("failed to find the treer-agent-server executable")?
            .canonicalize()
            .context("failed to resolve the treer-agent-server executable")?;
        let host_executable =
            executable.with_file_name(format!("treer-agent-host{}", std::env::consts::EXE_SUFFIX));
        let config_dir = config_home.join("treer/agent-servers");
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
    let mut matched = None;
    for (paths, config) in installed_services()? {
        if config.workspace != workspace {
            continue;
        }
        if matched.is_some() {
            bail!(
                "multiple agent-server services for workspace {workspace} are installed on {}",
                current_hostname()?
            );
        }
        matched = Some((paths, config));
    }
    Ok(matched)
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
    let home = home_dir()?;
    let config_home = env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".config"));
    Ok(config_home.join("treer/agent-servers"))
}

fn state_dir() -> Result<PathBuf> {
    let home = home_dir()?;
    Ok(env::var_os("TREER_STATE_DIR")
        .map(PathBuf::from)
        .or_else(|| env::var_os("XDG_STATE_HOME").map(|path| PathBuf::from(path).join("treer")))
        .unwrap_or_else(|| home.join(".local/state/treer")))
}

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

fn host_socket_filename(server_id: &str) -> String {
    format!("h-{:016x}.sock", fnv1a64(server_id.as_bytes()))
}

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
#[cfg(not(any(target_os = "macos", target_os = "ios")))]
const MAX_UNIX_SOCKET_PATH_BYTES: usize = 107;

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
    use std::io::Write;
    file.write_all(bytes)
        .with_context(|| format!("failed to write {}", temporary.display()))?;
    file.sync_all()
        .with_context(|| format!("failed to sync {}", temporary.display()))?;
    fs::rename(&temporary, path).with_context(|| format!("failed to replace {}", path.display()))
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
    run_checked(
        Command::new(&paths.host_executable)
            .arg("restart-controller")
            .arg("--socket")
            .arg(socket),
        "treer-agent-host restart-controller",
    )
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
    let path = Path::new(&executable);
    if path.components().count() > 1 {
        if path.is_file() {
            return Ok(());
        }
        bail!("nohup executable was not found at {}", path.display());
    }
    let found = env::var_os("PATH").is_some_and(|value| {
        env::split_paths(&value)
            .map(|directory| directory.join(path))
            .any(|candidate| candidate.is_file())
    });
    if found {
        Ok(())
    } else {
        bail!("nohup was not found on PATH; install coreutils before connecting this machine")
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
        .context("failed to inspect the nohup Host process with ps")?;
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
    let output = Command::new("sh")
        .arg("-c")
        .arg(r#""$1" "$2" run --config "$3" </dev/null >>"$4" 2>&1 & printf '%s\n' "$!""#)
        .arg("treer-nohup")
        .arg(nohup)
        .arg(&paths.host_executable)
        .arg(&paths.host_config)
        .arg(&log_path)
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

#[cfg(target_os = "linux")]
mod platform {
    use super::*;

    fn unit_name(server_id: &str) -> String {
        format!("treer-agent-server-{}.service", component_key(server_id))
    }

    fn unit_path(server_id: &str) -> Result<PathBuf> {
        let home = home_dir()?;
        let config_home = env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".config"));
        Ok(config_home.join("systemd/user").join(unit_name(server_id)))
    }

    fn wants_path(server_id: &str) -> Result<PathBuf> {
        let unit_path = unit_path(server_id)?;
        let parent = unit_path
            .parent()
            .context("systemd user unit path has no parent")?;
        Ok(parent
            .join("default.target.wants")
            .join(unit_name(server_id)))
    }

    pub fn transition(previous: Option<&ServiceConfig>, next: &ServiceConfig) -> Result<()> {
        let Some(previous) = previous else {
            return Ok(());
        };
        if previous.service_manager == next.service_manager {
            return Ok(());
        }
        match (previous.service_manager, next.service_manager) {
            (ServiceManager::SystemdUser, ServiceManager::Foreground | ServiceManager::Nohup) => {
                cleanup_systemd_registration(
                    &unit_path(&previous.server_id)?,
                    &wants_path(&previous.server_id)?,
                    &unit_name(&previous.server_id),
                    host_is_running(&previous.host_socket),
                )
            }
            (ServiceManager::Foreground, ServiceManager::SystemdUser) => {
                if host_is_running(&previous.host_socket) {
                    bail!(
                        "cannot switch a running foreground Host to systemd; stop the foreground command with Ctrl-C, then run repair again"
                    );
                }
                Ok(())
            }
            (ServiceManager::Nohup, ServiceManager::SystemdUser) => {
                let paths = ServicePaths::new(&previous.server_id)?;
                stop_nohup(&paths, previous, true)
            }
            (ServiceManager::Foreground, ServiceManager::Nohup) => {
                if host_is_running(&previous.host_socket) {
                    bail!(
                        "cannot switch a running foreground Host to nohup; stop the foreground command with Ctrl-C, then run repair again"
                    );
                }
                Ok(())
            }
            (ServiceManager::Nohup, ServiceManager::Foreground) => {
                let paths = ServicePaths::new(&previous.server_id)?;
                stop_nohup(&paths, previous, true)
            }
            (_, ServiceManager::Launchd) | (ServiceManager::Launchd, _) => {
                bail!("launchd service configuration cannot run on Linux")
            }
            _ => Ok(()),
        }
    }

    fn cleanup_systemd_registration(
        unit_path: &Path,
        wants_path: &Path,
        unit: &str,
        host_running: bool,
    ) -> Result<()> {
        cleanup_systemd_registration_with(
            env::var_os("TREER_SYSTEMCTL").unwrap_or_else(|| "systemctl".into()),
            unit_path,
            wants_path,
            unit,
            host_running,
        )
    }

    pub(super) fn cleanup_systemd_registration_with(
        executable: impl AsRef<OsStr>,
        unit_path: &Path,
        wants_path: &Path,
        unit: &str,
        host_running: bool,
    ) -> Result<()> {
        let disable = run_checked(
            Command::new(executable.as_ref()).args(["--user", "disable", "--now", unit]),
            "systemctl --user disable --now",
        );
        if host_running {
            disable.context(
                "the existing systemd Host is still running, so Treer cannot safely switch supervision modes",
            )?;
        } else if let Err(error) = disable {
            eprintln!(
                "treer: warning: could not disable the inactive systemd unit ({error:#}); removing its files directly"
            );
        }
        remove_if_exists(wants_path)?;
        remove_if_exists(unit_path)?;
        if let Err(error) = run_checked(
            Command::new(executable.as_ref()).args(["--user", "daemon-reload"]),
            "systemctl --user daemon-reload",
        ) {
            eprintln!(
                "treer: warning: systemd daemon-reload remains unavailable after removing the stale unit: {error:#}"
            );
        }
        Ok(())
    }

    pub fn register(paths: &ServicePaths, config: &ServiceConfig) -> Result<()> {
        match config.service_manager {
            ServiceManager::Nohup | ServiceManager::Foreground => return Ok(()),
            ServiceManager::SystemdUser => {}
            ServiceManager::Launchd => bail!("launchd service configuration cannot run on Linux"),
        }
        register_systemd_with(
            env::var_os("TREER_SYSTEMCTL").unwrap_or_else(|| "systemctl".into()),
            paths,
            config,
            &unit_path(&config.server_id)?,
        )
    }

    pub(super) fn register_systemd_with(
        executable: impl AsRef<OsStr>,
        paths: &ServicePaths,
        config: &ServiceConfig,
        unit_path: &Path,
    ) -> Result<()> {
        let parent = unit_path
            .parent()
            .context("systemd user unit path has no parent")?;
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
        write_atomic(
            unit_path,
            systemd_unit(
                &paths.host_executable,
                &paths.host_config,
                &config.workspace,
                &config.install_hostname,
            )
            .as_bytes(),
        )?;
        run_checked(
            Command::new(executable.as_ref()).args(["--user", "daemon-reload"]),
            "systemctl --user daemon-reload",
        )?;
        let unit = unit_name(&config.server_id);
        run_checked(
            Command::new(executable.as_ref()).args(["--user", "enable", unit.as_str()]),
            "systemctl --user enable",
        )?;
        warn_if_linger_disabled();
        Ok(())
    }

    pub fn start(paths: &ServicePaths, config: &ServiceConfig) -> Result<()> {
        if config.service_manager == ServiceManager::Nohup {
            return start_nohup(paths, config);
        }
        require_systemd(config)?;
        let unit = unit_name(&config.server_id);
        run_checked(
            systemctl_command().args(["--user", "start", unit.as_str()]),
            "systemctl --user start",
        )
    }

    pub fn stop(paths: &ServicePaths, config: &ServiceConfig) -> Result<()> {
        if config.service_manager == ServiceManager::Nohup {
            return stop_nohup(paths, config, true);
        }
        require_systemd(config)?;
        let unit = unit_name(&config.server_id);
        run_checked(
            systemctl_command().args(["--user", "stop", unit.as_str()]),
            "systemctl --user stop",
        )
    }

    pub fn stop_remotely(paths: &ServicePaths, config: &ServiceConfig) -> Result<()> {
        if config.service_manager == ServiceManager::Nohup {
            return stop_nohup(paths, config, false);
        }
        require_systemd(config)?;
        let unit = unit_name(&config.server_id);
        run_checked(
            systemctl_command().args(["--user", "--no-block", "stop", unit.as_str()]),
            "systemctl --user --no-block stop",
        )
    }

    pub fn restart(paths: &ServicePaths, config: &ServiceConfig) -> Result<()> {
        if config.service_manager == ServiceManager::Nohup {
            stop_nohup(paths, config, true)?;
            return start_nohup(paths, config);
        }
        require_systemd(config)?;
        let unit = unit_name(&config.server_id);
        run_checked(
            systemctl_command().args(["--user", "restart", unit.as_str()]),
            "systemctl --user restart",
        )
    }

    pub fn status(paths: &ServicePaths, config: &ServiceConfig) -> Result<()> {
        if config.service_manager == ServiceManager::Nohup {
            return status_nohup(paths, config);
        }
        require_systemd(config)?;
        let unit = unit_name(&config.server_id);
        run_checked(
            systemctl_command().args(["--user", "status", "--no-pager", unit.as_str()]),
            "systemctl --user status",
        )
    }

    pub fn logs(
        paths: &ServicePaths,
        config: &ServiceConfig,
        lines: usize,
        follow: bool,
    ) -> Result<()> {
        if config.service_manager == ServiceManager::Nohup {
            return logs_nohup(paths, config, lines, follow);
        }
        require_systemd(config)?;
        let unit = unit_name(&config.server_id);
        let mut command = Command::new("journalctl");
        command.args([
            "--user",
            "-u",
            unit.as_str(),
            "--no-pager",
            "-n",
            &lines.to_string(),
        ]);
        if follow {
            command.arg("-f");
        }
        run_checked(&mut command, "journalctl")
    }

    pub fn uninstall(paths: &ServicePaths, config: &ServiceConfig) -> Result<()> {
        let unit = unit_name(&config.server_id);
        if config.service_manager == ServiceManager::Nohup {
            stop_nohup(paths, config, true)?;
            remove_if_exists(&wants_path(&config.server_id)?)?;
            remove_if_exists(&unit_path(&config.server_id)?)?;
            return Ok(());
        }
        if config.service_manager == ServiceManager::Foreground {
            remove_if_exists(&wants_path(&config.server_id)?)?;
            remove_if_exists(&unit_path(&config.server_id)?)?;
            return Ok(());
        }
        require_systemd(config)?;
        let _ = systemctl_command()
            .args(["--user", "disable", "--now", unit.as_str()])
            .status();
        remove_if_exists(&unit_path(&config.server_id)?)?;
        run_checked(
            systemctl_command().args(["--user", "daemon-reload"]),
            "systemctl --user daemon-reload",
        )
    }

    fn require_systemd(config: &ServiceConfig) -> Result<()> {
        match config.service_manager {
            ServiceManager::SystemdUser => Ok(()),
            ServiceManager::Nohup => bail!("this service uses nohup mode"),
            ServiceManager::Foreground => bail!(
                "this service uses foreground mode; run `treer-agent-server service --workspace {} start` in a terminal or process supervisor",
                config.workspace
            ),
            ServiceManager::Launchd => bail!("launchd service configuration cannot run on Linux"),
        }
    }

    fn warn_if_linger_disabled() {
        let Some(user) = env::var_os("USER") else {
            eprintln!("treer: warning: USER is unset; could not check systemd linger");
            return;
        };
        let output = Command::new("loginctl")
            .args([
                "show-user",
                user.to_string_lossy().as_ref(),
                "-p",
                "Linger",
                "--value",
            ])
            .output();
        if !matches!(output, Ok(output) if output.status.success() && String::from_utf8_lossy(&output.stdout).trim() == "yes")
        {
            eprintln!(
                "treer: warning: systemd linger is disabled; an administrator can run `loginctl enable-linger {}` to keep this service running after the last login session exits, or run the Controller in a fixed-host tmux session",
                user.to_string_lossy()
            );
        }
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use super::*;

    fn label(server_id: &str) -> String {
        format!("dev.treer.agent-server.{}", component_key(server_id))
    }

    fn plist_path(server_id: &str) -> Result<PathBuf> {
        Ok(home_dir()?
            .join("Library/LaunchAgents")
            .join(format!("{}.plist", label(server_id))))
    }

    fn domain() -> Result<String> {
        let output = Command::new("id")
            .arg("-u")
            .output()
            .context("failed to determine the current user id")?;
        require_success(output.status, "id -u")?;
        let uid = String::from_utf8(output.stdout)
            .context("id -u returned non-UTF-8 output")?
            .trim()
            .to_owned();
        Ok(format!("gui/{uid}"))
    }

    fn service_target(server_id: &str) -> Result<String> {
        Ok(format!("{}/{}", domain()?, label(server_id)))
    }

    pub fn transition(previous: Option<&ServiceConfig>, next: &ServiceConfig) -> Result<()> {
        let Some(previous) = previous else {
            return Ok(());
        };
        if previous.service_manager == next.service_manager {
            return Ok(());
        }
        match (previous.service_manager, next.service_manager) {
            (ServiceManager::Launchd, ServiceManager::Foreground | ServiceManager::Nohup) => {
                let target = service_target(&previous.server_id)?;
                let bootout = run_checked(
                    Command::new("launchctl").args(["bootout", target.as_str()]),
                    "launchctl bootout",
                );
                if host_is_running(&previous.host_socket) {
                    bootout.context(
                        "the existing LaunchAgent Host is still running, so Treer cannot safely switch supervision modes",
                    )?;
                }
                remove_if_exists(&plist_path(&previous.server_id)?)
            }
            (ServiceManager::Foreground, ServiceManager::Launchd) => {
                if host_is_running(&previous.host_socket) {
                    bail!(
                        "cannot switch a running foreground Host to launchd; stop the foreground command with Ctrl-C, then run repair again"
                    );
                }
                Ok(())
            }
            (ServiceManager::Nohup, ServiceManager::Launchd) => {
                let paths = ServicePaths::new(&previous.server_id)?;
                stop_nohup(&paths, previous, true)
            }
            (ServiceManager::Foreground, ServiceManager::Nohup) => {
                if host_is_running(&previous.host_socket) {
                    bail!(
                        "cannot switch a running foreground Host to nohup; stop the foreground command with Ctrl-C, then run repair again"
                    );
                }
                Ok(())
            }
            (ServiceManager::Nohup, ServiceManager::Foreground) => {
                let paths = ServicePaths::new(&previous.server_id)?;
                stop_nohup(&paths, previous, true)
            }
            (_, ServiceManager::SystemdUser) | (ServiceManager::SystemdUser, _) => {
                bail!("systemd user service configuration cannot run on macOS")
            }
            _ => Ok(()),
        }
    }

    pub fn register(paths: &ServicePaths, config: &ServiceConfig) -> Result<()> {
        match config.service_manager {
            ServiceManager::Nohup | ServiceManager::Foreground => return Ok(()),
            ServiceManager::Launchd => {}
            ServiceManager::SystemdUser => {
                bail!("systemd user service configuration cannot run on macOS")
            }
        }
        let plist_path = plist_path(&config.server_id)?;
        let parent = plist_path
            .parent()
            .context("LaunchAgent path has no parent")?;
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
        let log_path = service_log_path(paths, config);
        write_atomic(
            &plist_path,
            launchd_plist(
                &paths.host_executable,
                &paths.host_config,
                &label(&config.server_id),
                &log_path,
                &log_path,
            )
            .as_bytes(),
        )?;
        Ok(())
    }

    pub fn start(paths: &ServicePaths, config: &ServiceConfig) -> Result<()> {
        if config.service_manager == ServiceManager::Nohup {
            return start_nohup(paths, config);
        }
        require_launchd(config)?;
        let target = service_target(&config.server_id)?;
        let loaded = Command::new("launchctl")
            .args(["print", target.as_str()])
            .status()
            .context("failed to query LaunchAgent")?
            .success();
        let domain = domain()?;
        let plist = plist_path(&config.server_id)?;
        for args in launchd_start_steps(loaded, &domain, &target, &plist) {
            let description = format!(
                "launchctl {}",
                args.first().map(String::as_str).unwrap_or_default()
            );
            run_checked(Command::new("launchctl").args(&args), &description)?;
        }
        Ok(())
    }

    pub fn stop(paths: &ServicePaths, config: &ServiceConfig) -> Result<()> {
        if config.service_manager == ServiceManager::Nohup {
            return stop_nohup(paths, config, true);
        }
        require_launchd(config)?;
        let target = service_target(&config.server_id)?;
        run_checked(
            Command::new("launchctl").args(["bootout", target.as_str()]),
            "launchctl bootout",
        )
    }

    pub fn stop_remotely(paths: &ServicePaths, config: &ServiceConfig) -> Result<()> {
        if config.service_manager == ServiceManager::Nohup {
            return stop_nohup(paths, config, false);
        }
        require_launchd(config)?;
        stop(paths, config)
    }

    pub fn restart(paths: &ServicePaths, config: &ServiceConfig) -> Result<()> {
        if config.service_manager == ServiceManager::Nohup {
            stop_nohup(paths, config, true)?;
            return start_nohup(paths, config);
        }
        require_launchd(config)?;
        let target = service_target(&config.server_id)?;
        let _ = Command::new("launchctl")
            .args(["bootout", target.as_str()])
            .status();
        start(paths, config)
    }

    pub fn status(paths: &ServicePaths, config: &ServiceConfig) -> Result<()> {
        if config.service_manager == ServiceManager::Nohup {
            return status_nohup(paths, config);
        }
        require_launchd(config)?;
        let target = service_target(&config.server_id)?;
        run_checked(
            Command::new("launchctl").args(["print", target.as_str()]),
            "launchctl print",
        )
    }

    pub fn logs(
        paths: &ServicePaths,
        config: &ServiceConfig,
        lines: usize,
        follow: bool,
    ) -> Result<()> {
        if config.service_manager == ServiceManager::Nohup {
            return logs_nohup(paths, config, lines, follow);
        }
        require_launchd(config)?;
        let log_path = service_log_path(paths, config);
        let mut command = Command::new("tail");
        command.args(["-n", &lines.to_string()]);
        if follow {
            command.arg("-f");
        }
        command.arg(log_path);
        run_checked(&mut command, "tail")
    }

    pub fn uninstall(paths: &ServicePaths, config: &ServiceConfig) -> Result<()> {
        if config.service_manager == ServiceManager::Nohup {
            stop_nohup(paths, config, true)?;
            remove_if_exists(&plist_path(&config.server_id)?)?;
            return Ok(());
        }
        if config.service_manager == ServiceManager::Foreground {
            remove_if_exists(&plist_path(&config.server_id)?)?;
            return Ok(());
        }
        require_launchd(config)?;
        let target = service_target(&config.server_id)?;
        let _ = Command::new("launchctl")
            .args(["bootout", target.as_str()])
            .status();
        remove_if_exists(&plist_path(&config.server_id)?)
    }

    fn require_launchd(config: &ServiceConfig) -> Result<()> {
        match config.service_manager {
            ServiceManager::Launchd => Ok(()),
            ServiceManager::Nohup => bail!("this service uses nohup mode"),
            ServiceManager::Foreground => bail!(
                "this service uses foreground mode; run `treer-agent-server service --workspace {} start` in a terminal or process supervisor",
                config.workspace
            ),
            ServiceManager::SystemdUser => {
                bail!("systemd user service configuration cannot run on macOS")
            }
        }
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod platform {
    use super::*;

    fn unsupported() -> Result<()> {
        bail!("service management is currently supported on Linux and macOS")
    }

    pub fn transition(previous: Option<&ServiceConfig>, next: &ServiceConfig) -> Result<()> {
        if previous.is_none_or(|previous| previous.service_manager == next.service_manager)
            || next.service_manager == ServiceManager::Foreground
        {
            Ok(())
        } else {
            unsupported()
        }
    }

    pub fn register(_paths: &ServicePaths, config: &ServiceConfig) -> Result<()> {
        if config.service_manager == ServiceManager::Foreground {
            Ok(())
        } else {
            unsupported()
        }
    }

    pub fn start(_paths: &ServicePaths, _config: &ServiceConfig) -> Result<()> {
        unsupported()
    }

    pub fn stop(_paths: &ServicePaths, _config: &ServiceConfig) -> Result<()> {
        unsupported()
    }

    pub fn stop_remotely(_paths: &ServicePaths, _config: &ServiceConfig) -> Result<()> {
        unsupported()
    }

    pub fn restart(_paths: &ServicePaths, _config: &ServiceConfig) -> Result<()> {
        unsupported()
    }

    pub fn status(_paths: &ServicePaths, _config: &ServiceConfig) -> Result<()> {
        unsupported()
    }

    pub fn logs(
        _paths: &ServicePaths,
        _config: &ServiceConfig,
        _lines: usize,
        _follow: bool,
    ) -> Result<()> {
        unsupported()
    }

    pub fn uninstall(_paths: &ServicePaths, config: &ServiceConfig) -> Result<()> {
        if config.service_manager == ServiceManager::Foreground {
            Ok(())
        } else {
            unsupported()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::routing::get;
    use axum::{Json, Router};
    use serde_json::json;

    #[test]
    fn workspace_keys_are_safe_for_service_names() {
        assert_eq!(workspace_key("team one/alpha"), "team_one_alpha");
        assert_eq!(workspace_key("default"), "default");
    }

    #[test]
    fn identity_and_socket_paths_are_scoped_to_node_and_server() {
        let identity = machine_identity_path().expect("machine identity path");
        assert!(identity.ends_with("machine-identity.json"));
        assert!(identity
            .components()
            .any(|component| component.as_os_str() == "machines"));
        let hostname_key = node_key().unwrap();
        assert!(identity
            .components()
            .any(|component| component.as_os_str() == std::ffi::OsStr::new(&hostname_key)));

        let socket = host_socket_path("srv_0123456789abcdef").expect("host socket path");
        assert_eq!(
            socket.file_name().and_then(|name| name.to_str()),
            Some("h-bc326bb4e71ef28b.sock")
        );
        assert_eq!(
            socket.file_name(),
            host_socket_path("srv_0123456789abcdef")
                .expect("same server")
                .file_name()
        );
        assert_ne!(
            host_socket_path("srv_0123456789abcdef")
                .expect("first server")
                .file_name(),
            host_socket_path("srv_fedcba9876543210")
                .expect("second server")
                .file_name()
        );
        assert!(unix_path_byte_len(&socket) <= MAX_UNIX_SOCKET_PATH_BYTES);
    }

    #[test]
    fn host_socket_filename_is_a_stable_short_hash() {
        assert_eq!(
            host_socket_filename("srv_0123456789abcdef"),
            "h-bc326bb4e71ef28b.sock"
        );
        assert_eq!(
            host_socket_filename("srv_0123456789abcdef").len(),
            "h-0123456789abcdef.sock".len()
        );
    }

    #[test]
    fn host_socket_falls_back_when_the_runtime_directory_is_too_long() {
        let filename = host_socket_filename("srv_0123456789abcdef");
        let too_long =
            PathBuf::from(format!("/{}", "a".repeat(MAX_UNIX_SOCKET_PATH_BYTES))).join(&filename);
        let fitted = fit_unix_socket_path(too_long, &filename).expect("fallback");
        assert!(unix_path_byte_len(&fitted) <= MAX_UNIX_SOCKET_PATH_BYTES);
        assert_eq!(
            fitted.file_name().and_then(|name| name.to_str()),
            Some(filename.as_str())
        );
        assert!(fitted.starts_with("/tmp"));
    }

    #[cfg(unix)]
    #[test]
    fn runtime_base_requires_a_private_writable_directory_owned_by_the_user() {
        use std::os::unix::fs::PermissionsExt;

        let directory = env::temp_dir().join(format!(
            "treer-runtime-permissions-{}",
            Uuid::new_v4().simple()
        ));
        fs::create_dir(&directory).expect("temporary runtime directory");
        let uid = current_uid()
            .expect("current uid")
            .parse::<u32>()
            .expect("numeric uid");
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))
            .expect("private permissions");
        assert!(runtime_base_is_private_and_writable(&directory, uid));

        fs::set_permissions(&directory, fs::Permissions::from_mode(0o755))
            .expect("public permissions");
        assert!(!runtime_base_is_private_and_writable(&directory, uid));

        fs::set_permissions(&directory, fs::Permissions::from_mode(0o777))
            .expect("writable public permissions");
        assert!(!runtime_base_is_private_and_writable(&directory, uid));

        fs::set_permissions(&directory, fs::Permissions::from_mode(0o500))
            .expect("read-only permissions");
        assert!(!runtime_base_is_private_and_writable(&directory, uid));
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o600))
            .expect("non-searchable permissions");
        assert!(!runtime_base_is_private_and_writable(&directory, uid));
        assert!(!runtime_base_is_private_and_writable(
            &directory.join("missing"),
            uid
        ));
        fs::remove_dir(&directory).expect("remove temporary runtime directory");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_runtime_dir_falls_back_when_the_user_runtime_directory_is_missing() {
        use std::os::unix::fs::PermissionsExt;

        let directory = env::temp_dir().join(format!(
            "treer-runtime-selection-{}",
            Uuid::new_v4().simple()
        ));
        fs::create_dir(&directory).expect("temporary directory");
        let uid = current_uid().expect("current uid");
        let uid_number = uid.parse::<u32>().expect("numeric uid");
        assert_eq!(linux_user_runtime_dir(&directory, &uid, uid_number), None);

        let user_runtime = directory.join(&uid);
        fs::create_dir(&user_runtime).expect("user runtime directory");
        fs::set_permissions(&user_runtime, fs::Permissions::from_mode(0o700))
            .expect("private permissions");
        assert_eq!(
            linux_user_runtime_dir(&directory, &uid, uid_number),
            Some(user_runtime.join("treer"))
        );
        fs::remove_dir_all(&directory).expect("remove temporary directory");
    }

    #[cfg(unix)]
    #[test]
    fn fallback_runtime_directory_is_created_with_private_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let directory = env::temp_dir().join(format!(
            "treer-runtime-fallback-{}",
            Uuid::new_v4().simple()
        ));
        let runtime = directory.join("run");
        assert_eq!(
            prepare_private_runtime_dir(runtime.clone()).expect("prepare runtime directory"),
            runtime
        );
        assert_eq!(
            fs::metadata(&runtime)
                .expect("runtime metadata")
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        fs::remove_dir_all(directory).expect("remove temporary directory");
    }

    #[cfg(unix)]
    #[test]
    fn unavailable_legacy_socket_is_migrated_but_a_live_socket_is_not() {
        let directory = PathBuf::from("/tmp").join(format!(
            "treer-runtime-migration-{}",
            Uuid::new_v4().simple()
        ));
        fs::create_dir(&directory).expect("temporary directory");
        let current = directory.join("current.sock");
        let preferred = directory.join("preferred.sock");
        assert!(should_migrate_host_socket(&current, &preferred));
        assert!(!should_migrate_host_socket(&preferred, &preferred));

        let listener = std::os::unix::net::UnixListener::bind(&current).expect("live Host socket");
        assert!(!should_migrate_host_socket(&current, &preferred));
        drop(listener);
        fs::remove_dir_all(directory).expect("remove temporary directory");
    }

    #[test]
    fn automatic_address_uses_the_loopback_port_range() {
        let address = allocate_loopback_address().expect("allocate local API address");
        assert!(address.ip().is_loopback());
        assert!(address.port() >= FIRST_AUTOMATIC_PORT);
    }

    #[tokio::test]
    async fn local_api_identity_distinguishes_the_installed_controller() {
        let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("bind health server");
        let address = listener.local_addr().expect("health server address");
        let app = Router::new()
            .route(
                "/api/health",
                get(|| async {
                    Json(json!({
                        "status": "ok",
                        "service": "treer-agent-server",
                        "workspace_id": "default",
                        "server_id": "srv_test",
                        "controller_epoch": "epoch-test",
                        "proxy_connected": true,
                        "connection_state": "online",
                    }))
                }),
            )
            .route(
                "/api/agents",
                get(|| async { Json(json!({ "agents": [] })) }),
            );
        let server = tokio::spawn(async move { axum::serve(listener, app).await });

        assert!(local_api_matches(&address, "default", "srv_test").await);
        assert!(!local_api_matches(&address, "other", "srv_test").await);
        assert!(!local_api_matches(&address, "default", "srv_other").await);
        let config = ServiceConfig {
            proxy: "https://treer.example/".to_string(),
            workspace: "default".to_string(),
            server_id: "srv_test".to_string(),
            machine_token: "srv_test.secret".to_string(),
            operator_credential: "op_test".to_string(),
            root: PathBuf::from("/tmp"),
            listen: address.to_string(),
            host_socket: PathBuf::from("/tmp/host.sock"),
            install_hostname: current_hostname().expect("local hostname"),
            service_manager: default_service_manager(),
            service_fallback_reason: None,
        };
        assert_eq!(
            controller_epoch(&config).await.as_deref(),
            Some("epoch-test")
        );
        wait_for_controller_and_proxy(&config)
            .await
            .expect("Controller and Proxy readiness");
        let occupant = occupying_controller(address)
            .await
            .expect("occupying Controller");
        assert_eq!(occupant.server_id, "srv_test");
        assert_eq!(occupant.workspace_id, "default");

        server.abort();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn automatic_service_mode_uses_nohup_without_probing_systemd() {
        let automatic = select_linux_service_manager(ServiceMode::Auto, || {
            panic!("automatic nohup mode must not probe systemd")
        })
        .expect("automatic nohup selection");
        assert_eq!(automatic.manager, ServiceManager::Nohup);
        assert_eq!(automatic.fallback_reason, None);

        let explicit = select_linux_service_manager(ServiceMode::Systemd, || {
            bail!("Failed to connect to bus: No such file or directory")
        })
        .expect_err("explicit systemd must fail");
        assert!(explicit.to_string().contains("user manager is unavailable"));

        let foreground = select_linux_service_manager(ServiceMode::Foreground, || {
            panic!("foreground mode must not probe systemd")
        })
        .expect("foreground selection");
        assert_eq!(foreground.manager, ServiceManager::Foreground);

        let nohup = select_linux_service_manager(ServiceMode::Nohup, || {
            panic!("explicit nohup mode must not probe systemd")
        })
        .expect("nohup selection");
        assert_eq!(nohup.manager, ServiceManager::Nohup);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn service_canary_healthy_systemd_user_manager_is_selected() {
        use std::os::unix::fs::PermissionsExt;

        let selection = select_linux_service_manager(ServiceMode::Systemd, || Ok(()))
            .expect("explicit systemd selection");
        assert_eq!(selection.manager, ServiceManager::SystemdUser);
        assert_eq!(selection.fallback_reason, None);

        let directory = std::env::temp_dir().join(format!(
            "treer-systemd-canary-{}",
            uuid::Uuid::new_v4().simple()
        ));
        fs::create_dir(&directory).expect("create canary directory");
        let log = directory.join("systemctl.log");
        let systemctl = directory.join("systemctl");
        fs::write(
            &systemctl,
            format!("#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\n", log.display()),
        )
        .expect("write fake systemctl");
        fs::set_permissions(&systemctl, fs::Permissions::from_mode(0o755))
            .expect("make fake systemctl executable");
        let unit = directory.join("treer-agent-server-srv_canary.service");
        let paths = ServicePaths::new("srv_canary").expect("service paths");
        let config = ServiceConfig {
            proxy: "https://treer.example/".to_string(),
            workspace: "canary".to_string(),
            server_id: "srv_canary".to_string(),
            machine_token: "srv_canary.secret".to_string(),
            operator_credential: "op_canary".to_string(),
            root: PathBuf::from("/tmp"),
            listen: "127.0.0.1:8790".to_string(),
            host_socket: directory.join("host.sock"),
            install_hostname: current_hostname().expect("hostname"),
            service_manager: selection.manager,
            service_fallback_reason: selection.fallback_reason,
        };
        platform::register_systemd_with(systemctl.as_os_str(), &paths, &config, &unit)
            .expect("register systemd unit");
        let unit_contents = fs::read_to_string(&unit).expect("read generated unit");
        assert!(unit_contents.contains("Restart=always"));
        let calls = fs::read_to_string(log).expect("read systemctl calls");
        assert!(calls.contains("--user daemon-reload"));
        assert!(calls.contains("--user enable treer-agent-server-srv_canary.service"));

        fs::remove_dir_all(directory).expect("remove canary directory");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn service_canary_auto_ignores_a_missing_user_bus() {
        let selection = select_linux_service_manager(ServiceMode::Auto, || {
            panic!("auto mode must not inspect the user bus")
        })
        .expect("automatic nohup selection");
        assert_eq!(selection.manager, ServiceManager::Nohup);
        assert_eq!(selection.fallback_reason, None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn nohup_stop_discards_a_reused_pid_without_signaling_it() {
        let (directory, paths, config) = nohup_test_service("stale-pid");
        let pid_path = nohup_pid_path(&paths, &config);
        save_json(
            &NohupProcess {
                pid: std::process::id(),
                started_at: "not the current process start time".to_string(),
            },
            &pid_path,
        )
        .expect("write stale PID record");

        stop_nohup(&paths, &config, true).expect("discard stale PID record");
        assert!(!pid_path.exists());

        fs::remove_dir_all(directory).expect("remove nohup state directory");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn nohup_launcher_detaches_and_stops_the_recorded_process() {
        use std::os::unix::fs::PermissionsExt;

        let (directory, mut paths, config) = nohup_test_service("lifecycle");
        let fake_nohup = directory.join("fake nohup");
        let fake_host = directory.join("fake host");
        fs::write(&fake_nohup, "#!/bin/sh\nexec \"$@\"\n").expect("write fake nohup");
        fs::write(
            &fake_host,
            "#!/bin/sh\ntrap 'exit 0' TERM INT\nwhile :; do sleep 1; done\n",
        )
        .expect("write fake Host");
        fs::set_permissions(&fake_nohup, fs::Permissions::from_mode(0o755))
            .expect("make fake nohup executable");
        fs::set_permissions(&fake_host, fs::Permissions::from_mode(0o755))
            .expect("make fake Host executable");
        paths.host_executable = fake_host;

        start_nohup_with(fake_nohup.as_os_str(), &paths, &config)
            .expect("start detached fake Host");
        let process = read_nohup_process(&paths, &config)
            .expect("read PID record")
            .expect("PID record");
        assert!(nohup_process_is_current(&process).expect("inspect fake Host"));
        stop_nohup(&paths, &config, true).expect("stop detached fake Host");
        assert!(!nohup_pid_path(&paths, &config).exists());

        fs::remove_dir_all(directory).expect("remove nohup lifecycle directory");
    }

    #[cfg(target_os = "linux")]
    fn nohup_test_service(label: &str) -> (PathBuf, ServicePaths, ServiceConfig) {
        let directory = std::env::temp_dir().join(format!(
            "treer-nohup-{label}-{}",
            uuid::Uuid::new_v4().simple()
        ));
        fs::create_dir(&directory).expect("create nohup test directory");
        let paths = ServicePaths {
            executable: directory.join("treer-agent-server"),
            host_executable: directory.join("treer-agent-host"),
            config: directory.join("controller.json"),
            host_config: directory.join("host config.json"),
            state_dir: directory.clone(),
        };
        let config = ServiceConfig {
            proxy: "https://treer.example/".to_string(),
            workspace: "default".to_string(),
            server_id: format!("srv_nohup_{label}"),
            machine_token: "srv_nohup.secret".to_string(),
            operator_credential: "op_nohup".to_string(),
            root: directory.clone(),
            listen: "127.0.0.1:8790".to_string(),
            host_socket: directory.join("host socket.sock"),
            install_hostname: current_hostname().expect("hostname"),
            service_manager: ServiceManager::Nohup,
            service_fallback_reason: None,
        };
        (directory, paths, config)
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn service_canary_explicit_systemd_failure_then_repairs_partial_unit() {
        use std::os::unix::fs::PermissionsExt;

        let explicit = select_linux_service_manager(ServiceMode::Systemd, || {
            bail!("Failed to connect to bus: No medium found")
        })
        .expect_err("explicit systemd must fail before enrollment");
        assert!(explicit.to_string().contains("user manager is unavailable"));

        let directory = std::env::temp_dir().join(format!(
            "treer-service-canary-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let unit = directory.join("treer-agent-server-test.service");
        let wants = directory
            .join("default.target.wants")
            .join("treer-agent-server-test.service");
        fs::create_dir_all(wants.parent().expect("wants parent"))
            .expect("create canary directories");
        fs::write(&unit, "partial unit").expect("write partial unit");
        fs::write(&wants, "partial enablement").expect("write partial wants entry");
        let systemctl = directory.join("systemctl");
        fs::write(&systemctl, "#!/bin/sh\nexit 1\n").expect("write fake systemctl");
        fs::set_permissions(&systemctl, fs::Permissions::from_mode(0o755))
            .expect("make fake systemctl executable");

        platform::cleanup_systemd_registration_with(
            systemctl.as_os_str(),
            &unit,
            &wants,
            "treer-agent-server-test.service",
            false,
        )
        .expect("repair inactive partial registration without a user bus");
        assert!(!unit.exists());
        assert!(!wants.exists());

        fs::write(&unit, "active unit").expect("rewrite active unit");
        let error = platform::cleanup_systemd_registration_with(
            systemctl.as_os_str(),
            &unit,
            &wants,
            "treer-agent-server-test.service",
            true,
        )
        .expect_err("an active Host must not be orphaned when systemctl is unavailable");
        assert!(error
            .to_string()
            .contains("cannot safely switch supervision modes"));
        assert!(unit.exists());

        fs::remove_dir_all(directory).expect("remove canary directory");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn systemd_probe_preserves_the_actionable_bus_error() {
        use std::os::unix::fs::PermissionsExt;

        let directory = std::env::temp_dir().join(format!(
            "treer-fake-systemctl-{}",
            uuid::Uuid::new_v4().simple()
        ));
        fs::create_dir(&directory).expect("create fake systemctl directory");
        let executable = directory.join("systemctl");
        fs::write(
            &executable,
            b"#!/bin/sh\necho 'Failed to connect to bus: No such file or directory' >&2\nexit 1\n",
        )
        .expect("write fake systemctl");
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o755))
            .expect("make fake systemctl executable");

        let error = probe_systemd_user_with(executable.as_os_str())
            .expect_err("unavailable fake user manager");
        assert!(error
            .to_string()
            .contains("Failed to connect to bus: No such file or directory"));

        fs::remove_file(executable).expect("remove fake systemctl");
        fs::remove_dir(directory).expect("remove fake systemctl directory");
    }

    #[test]
    fn legacy_service_config_defaults_to_the_native_manager() {
        let config: ServiceConfig = serde_json::from_value(json!({
            "proxy": "https://treer.example/",
            "workspace": "default",
            "server_id": "srv_test",
            "machine_token": "token",
            "operator_credential": "operator",
            "root": "/tmp",
            "listen": "127.0.0.1:8790",
            "host_socket": "/tmp/host.sock",
            "install_hostname": "builder"
        }))
        .expect("legacy configuration");
        assert_eq!(config.service_manager, default_service_manager());
        assert_eq!(config.service_fallback_reason, None);
    }

    #[test]
    fn update_platforms_match_release_artifact_names() {
        assert_eq!(
            artifact_platform("linux", "x86_64").unwrap(),
            "linux-x86_64"
        );
        assert_eq!(
            artifact_platform("linux", "aarch64").unwrap(),
            "linux-aarch64"
        );
        assert_eq!(
            artifact_platform("macos", "x86_64").unwrap(),
            "darwin-x86_64"
        );
        assert_eq!(
            artifact_platform("macos", "aarch64").unwrap(),
            "darwin-aarch64"
        );
        assert!(artifact_platform("windows", "x86_64").is_err());
    }

    #[test]
    fn update_artifact_urls_are_relative_to_the_proxy_root() {
        let proxy = Url::parse("https://treer.example/").unwrap();
        assert_eq!(
            artifact_url(&proxy, "darwin-aarch64", "treer-agent-server")
                .unwrap()
                .as_str(),
            "https://treer.example/artifacts/darwin-aarch64/treer-agent-server"
        );
    }

    #[test]
    fn explicit_update_proxy_overrides_the_installed_source() {
        let config = ServiceConfig {
            proxy: "https://stable.treer.example/".to_string(),
            workspace: "default".to_string(),
            server_id: "srv_test".to_string(),
            machine_token: "srv_test.secret".to_string(),
            operator_credential: "op_test".to_string(),
            root: PathBuf::from("/tmp"),
            listen: "127.0.0.1:8790".to_string(),
            host_socket: PathBuf::from("/tmp/host.sock"),
            install_hostname: current_hostname().expect("local hostname"),
            service_manager: default_service_manager(),
            service_fallback_reason: None,
        };
        let explicit = Url::parse("https://canary.treer.example/").unwrap();

        assert_eq!(
            resolve_update_proxy(Some(explicit), &config)
                .unwrap()
                .as_str(),
            "https://canary.treer.example/"
        );
        assert_eq!(
            resolve_update_proxy(None, &config).unwrap().as_str(),
            "https://stable.treer.example/"
        );
        assert!(
            resolve_update_proxy(Some(Url::parse("file:///tmp/release").unwrap()), &config)
                .is_err()
        );
    }

    #[test]
    fn managed_agent_health_uses_the_injected_host_route() {
        let config = ServiceConfig {
            proxy: "https://treer.example/".to_string(),
            workspace: "default".to_string(),
            server_id: "srv_current".to_string(),
            machine_token: "srv_current.secret".to_string(),
            operator_credential: "op_test".to_string(),
            root: PathBuf::from("/tmp"),
            listen: "127.0.0.1:8790".to_string(),
            host_socket: PathBuf::from("/tmp/host.sock"),
            install_hostname: current_hostname().expect("local hostname"),
            service_manager: default_service_manager(),
            service_fallback_reason: None,
        };

        assert_eq!(
            controller_health_url_for(
                &config,
                Some("srv_current"),
                Some("http://192.0.2.1:8790/ignored?old=true"),
            )
            .unwrap()
            .as_str(),
            "http://192.0.2.1:8790/api/health"
        );
        assert_eq!(
            controller_health_url_for(&config, Some("srv_other"), Some("http://192.0.2.1:9999/"),)
                .unwrap()
                .as_str(),
            "http://127.0.0.1:8790/api/health"
        );
    }

    #[test]
    fn managed_agent_update_activates_only_its_controller() {
        let make_service = |server_id: &str| {
            let config = ServiceConfig {
                proxy: "https://treer.example/".to_string(),
                workspace: format!("workspace-{server_id}"),
                server_id: server_id.to_string(),
                machine_token: format!("{server_id}.secret"),
                operator_credential: "op_test".to_string(),
                root: PathBuf::from("/tmp"),
                listen: "127.0.0.1:8790".to_string(),
                host_socket: PathBuf::from(format!("/tmp/{server_id}.sock")),
                install_hostname: current_hostname().expect("local hostname"),
                service_manager: default_service_manager(),
                service_fallback_reason: None,
            };
            (ServicePaths::new(server_id).expect("service paths"), config)
        };
        let services = vec![make_service("srv_a"), make_service("srv_b")];

        let managed = activation_services_for(&services, Some("srv_b")).unwrap();
        assert_eq!(managed.len(), 1);
        assert_eq!(managed[0].1.server_id, "srv_b");

        let host = activation_services_for(&services, None).unwrap();
        assert_eq!(
            host.iter()
                .map(|(_, config)| config.server_id.as_str())
                .collect::<Vec<_>>(),
            ["srv_a", "srv_b"]
        );
    }

    #[cfg(unix)]
    #[test]
    fn staged_executable_is_validated_and_atomically_installed() {
        use std::os::unix::fs::PermissionsExt;

        let directory = std::env::temp_dir().join(format!(
            "treer-update-stage-{}",
            uuid::Uuid::new_v4().simple()
        ));
        fs::create_dir(&directory).expect("create update test directory");
        let destination = directory.join("treer-test");
        let bytes = b"#!/bin/sh\nexit 0\n";
        let staged =
            stage_executable(&destination, bytes, "update", true).expect("stage executable");
        staged.install(&destination).expect("install executable");

        assert_eq!(fs::read(&destination).expect("read executable"), bytes);
        assert_eq!(
            fs::metadata(&destination)
                .expect("executable metadata")
                .permissions()
                .mode()
                & 0o777,
            0o755
        );
        fs::remove_file(&destination).expect("remove executable");
        fs::remove_dir(&directory).expect("remove update test directory");
    }

    #[test]
    fn systemd_unit_quotes_paths_and_restarts() {
        let unit = systemd_unit(
            Path::new("/home/test user/bin/treer-agent-server"),
            Path::new("/home/test%user/config.json"),
            "team one",
            "build-node-1",
        );
        assert!(unit.contains("Restart=always"));
        assert!(unit.contains("ConditionHost=build-node-1"));
        assert!(unit.contains("\"/home/test user/bin/treer-agent-server\" run"));
        assert!(unit.contains("test%%user"));
    }

    #[test]
    fn launchd_plist_escapes_values_and_keeps_process_alive() {
        let plist = launchd_plist(
            Path::new("/Users/a&b/treer-agent-server"),
            Path::new("/Users/a&b/config.json"),
            "dev.treer.test",
            Path::new("/tmp/out.log"),
            Path::new("/tmp/error.log"),
        );
        assert!(plist.contains("<key>KeepAlive</key>"));
        assert!(plist.contains("/Users/a&amp;b/treer-agent-server"));
        assert!(plist.contains("<string>run</string>"));
    }

    #[test]
    fn launchd_start_enables_a_disabled_agent_before_bootstrap() {
        let target = "gui/502/dev.treer.agent-server.srv_94cc4ceea1dc4d8eaadc4d9b6fbe2958";
        let plist = Path::new(
            "/Users/mac/Library/LaunchAgents/dev.treer.agent-server.srv_94cc4ceea1dc4d8eaadc4d9b6fbe2958.plist",
        );
        let steps = launchd_start_steps(false, "gui/502", target, plist);
        assert_eq!(
            steps,
            vec![
                vec!["enable".to_string(), target.to_string()],
                vec![
                    "bootstrap".to_string(),
                    "gui/502".to_string(),
                    plist.to_string_lossy().into_owned(),
                ],
            ]
        );
    }

    #[test]
    fn launchd_start_enables_before_kickstart_when_already_loaded() {
        let target = "gui/502/dev.treer.agent-server.srv_94cc4ceea1dc4d8eaadc4d9b6fbe2958";
        let steps = launchd_start_steps(true, "gui/502", target, Path::new("/tmp/unused.plist"));
        assert_eq!(
            steps,
            vec![
                vec!["enable".to_string(), target.to_string()],
                vec!["kickstart".to_string(), target.to_string()],
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn atomic_config_files_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let path = std::env::temp_dir().join(format!(
            "treer-config-permissions-{}.json",
            uuid::Uuid::new_v4().simple()
        ));
        write_atomic(&path, b"secret").expect("write configuration");
        let mode = std::fs::metadata(&path)
            .expect("configuration metadata")
            .permissions()
            .mode()
            & 0o777;
        std::fs::remove_file(&path).expect("remove test configuration");
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn machine_identity_round_trips_without_hardware_identifiers() {
        let directory = std::env::temp_dir().join(format!(
            "treer-machine-identity-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let path = directory.join("machine-identity.json");
        let identity = MachineIdentity::new("Build machine".to_string());
        identity.save(&path).expect("save machine identity");
        let loaded = MachineIdentity::load(&path).expect("load machine identity");
        assert_eq!(loaded, identity);
        let encoded = fs::read_to_string(&path).expect("read machine identity");
        assert!(!encoded.contains("mac_address"));
        assert!(!encoded.contains("hardware_address"));
        fs::remove_dir_all(directory).expect("remove identity directory");
    }

    #[test]
    fn service_commands_list_or_select_local_installs() {
        assert_eq!(
            classify_service_request(None, true, &["ws_a", "ws_b"]),
            ServiceRequestClass::Inventory
        );
        assert_eq!(
            classify_service_request(None, false, &["ws_a"]),
            ServiceRequestClass::Unique
        );
        assert_eq!(
            classify_service_request(None, false, &["ws_a", "ws_b"]),
            ServiceRequestClass::Ambiguous
        );
        assert_eq!(
            classify_service_request(None, false, &[]),
            ServiceRequestClass::NoneInstalled
        );
        assert_eq!(
            classify_service_request(Some("default"), false, &["ws_a"]),
            ServiceRequestClass::Missing("default".to_string())
        );
        assert_eq!(
            classify_service_request(Some("ws_a"), false, &["ws_a", "ws_b"]),
            ServiceRequestClass::Selected
        );
        assert_eq!(
            classify_service_request(Some("default"), false, &["default"]),
            ServiceRequestClass::Selected
        );
    }

    #[test]
    fn service_table_includes_workspace_and_listen() {
        let config = ServiceConfig {
            proxy: "https://treer.example/".to_string(),
            workspace: "ws_a39c30b35d6043918353e321cdd8ce96".to_string(),
            server_id: "srv_94cc4cee0123456789abcdef01234567".to_string(),
            machine_token: "token".to_string(),
            operator_credential: "operator".to_string(),
            root: PathBuf::from("/tmp"),
            listen: "127.0.0.1:8794".to_string(),
            host_socket: PathBuf::from("/tmp/host.sock"),
            install_hostname: "Mac.home.com".to_string(),
            service_manager: default_service_manager(),
            service_fallback_reason: None,
        };
        let table = format_service_table("Mac.home.com", &[&config]);
        assert!(table.contains("ws_a39c30b35d6043918353e321cdd8ce96"));
        assert!(table.contains("127.0.0.1:8794"));
        assert!(!table.contains("pass --workspace"));
        let ambiguous = format_ambiguous_services("Mac.home.com", &[config]);
        assert!(ambiguous.contains("pass --workspace <workspace_id>"));
    }
}
