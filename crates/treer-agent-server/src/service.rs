use std::env;
#[cfg(target_os = "linux")]
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
use treer_protocol::OPERATOR_CREDENTIAL_HEADER;
use url::Url;
use uuid::Uuid;

const FIRST_AUTOMATIC_PORT: u16 = 8790;
const MAX_UPDATE_BINARY_BYTES: usize = 128 * 1024 * 1024;
const UPDATE_DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(120);
const UPDATE_VALIDATION_TIMEOUT: Duration = Duration::from_secs(10);
const CONTROLLER_START_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ServiceMode {
    Auto,
    Systemd,
    Launchd,
    Foreground,
}

impl std::fmt::Display for ServiceMode {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Auto => "auto",
            Self::Systemd => "systemd",
            Self::Launchd => "launchd",
            Self::Foreground => "foreground",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ServiceManager {
    SystemdUser,
    Launchd,
    Foreground,
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
    fallback_reason: Option<String>,
}

impl ServiceSelection {
    pub fn announce(&self) {
        if let Some(reason) = &self.fallback_reason {
            eprintln!("treer: warning: persistent user service unavailable: {reason}");
            eprintln!(
                "treer: warning: falling back to foreground mode; keep this terminal open or run the command under a process supervisor"
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
    let Ok(client) = reqwest::Client::builder()
        .timeout(Duration::from_millis(500))
        .no_proxy()
        .build()
    else {
        return false;
    };
    let Ok(response) = client
        .get(format!("http://{address}/api/health"))
        .send()
        .await
    else {
        return false;
    };
    let Ok(value) = response.json::<serde_json::Value>().await else {
        return false;
    };
    value.get("service").and_then(|value| value.as_str()) == Some("treer-agent-server")
        && value.get("workspace_id").and_then(|value| value.as_str()) == Some(workspace)
        && value
            .get("server_id")
            .and_then(|value| value.as_str())
            .is_none_or(|value| value == server_id)
}

pub fn register(config: ServiceConfig) -> Result<()> {
    validate_workspace(&config.workspace)?;
    require_install_hostname(&config)?;
    let paths = ServicePaths::new(&config.server_id)?;
    require_host_binary(&paths)?;
    fs::create_dir_all(&paths.state_dir)
        .with_context(|| format!("failed to create {}", paths.state_dir.display()))?;
    config.save(&paths.config)?;
    let host_config = HostDaemonConfig {
        socket_path: config.host_socket.clone(),
        controller_path: paths.executable.clone(),
        controller_config_path: paths.config.clone(),
        root: config.root.clone(),
    };
    save_json(&host_config, &paths.host_config)?;
    platform::register(&paths, &config)?;
    println!("treer: agent Host service registered");
    println!("treer: configured local API address: {}", config.listen);
    Ok(())
}

pub enum ServiceActivation {
    Managed,
    Foreground(tokio::process::Child),
}

pub async fn refresh_registration_and_wait(config: ServiceConfig) -> Result<ServiceActivation> {
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
    select_service_manager(mode)
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
        ServiceMode::Auto => match probe() {
            Ok(()) => Ok(ServiceSelection {
                manager: ServiceManager::SystemdUser,
                fallback_reason: None,
            }),
            Err(error) => Ok(ServiceSelection {
                manager: ServiceManager::Foreground,
                fallback_reason: Some(format!("{error:#}")),
            }),
        },
        ServiceMode::Systemd => {
            probe().context(
                "systemd user service mode was requested but the user manager is unavailable; use `--service-mode foreground` or enable a user systemd session",
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
        ServiceMode::Auto | ServiceMode::Launchd => Ok(ServiceSelection {
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
        ServiceMode::Systemd | ServiceMode::Launchd => {
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

async fn proxy_is_reachable(config: &ServiceConfig) -> bool {
    let Some(mut url) = controller_health_url(config) else {
        return false;
    };
    url.set_path("/api/agents");
    let Ok(client) = reqwest::Client::builder()
        .timeout(Duration::from_millis(900))
        .no_proxy()
        .build()
    else {
        return false;
    };
    matches!(
        client
            .get(url)
            .header(OPERATOR_CREDENTIAL_HEADER, &config.operator_credential)
            .send()
            .await,
        Ok(response) if response.status().is_success()
    )
}

async fn wait_for_controller_and_proxy(config: &ServiceConfig) -> Result<()> {
    let deadline = Instant::now() + CONTROLLER_START_TIMEOUT;
    loop {
        let controller_ready = controller_epoch(config).await.is_some();
        if controller_ready && proxy_is_reachable(config).await {
            return Ok(());
        }
        if Instant::now() >= deadline {
            let waiting_for = if controller_ready {
                "Proxy connection"
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
struct ServicePaths {
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
    if let Some(path) = env::var_os("XDG_RUNTIME_DIR") {
        return Ok(PathBuf::from(path).join("treer"));
    }
    let uid = current_uid()?;
    #[cfg(target_os = "linux")]
    {
        Ok(PathBuf::from("/run/user").join(uid).join("treer"))
    }
    #[cfg(not(target_os = "linux"))]
    Ok(env::temp_dir().join(format!("treer-{uid}")))
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

    pub fn register(paths: &ServicePaths, config: &ServiceConfig) -> Result<()> {
        match config.service_manager {
            ServiceManager::Foreground => return Ok(()),
            ServiceManager::SystemdUser => {}
            ServiceManager::Launchd => bail!("launchd service configuration cannot run on Linux"),
        }
        let unit_path = unit_path(&config.server_id)?;
        let parent = unit_path
            .parent()
            .context("systemd user unit path has no parent")?;
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
        write_atomic(
            &unit_path,
            systemd_unit(
                &paths.host_executable,
                &paths.host_config,
                &config.workspace,
                &config.install_hostname,
            )
            .as_bytes(),
        )?;
        run_checked(
            systemctl_command().args(["--user", "daemon-reload"]),
            "systemctl --user daemon-reload",
        )?;
        let unit = unit_name(&config.server_id);
        run_checked(
            systemctl_command().args(["--user", "enable", unit.as_str()]),
            "systemctl --user enable",
        )?;
        warn_if_linger_disabled();
        Ok(())
    }

    pub fn start(_paths: &ServicePaths, config: &ServiceConfig) -> Result<()> {
        require_systemd(config)?;
        let unit = unit_name(&config.server_id);
        run_checked(
            systemctl_command().args(["--user", "start", unit.as_str()]),
            "systemctl --user start",
        )
    }

    pub fn stop(_paths: &ServicePaths, config: &ServiceConfig) -> Result<()> {
        require_systemd(config)?;
        let unit = unit_name(&config.server_id);
        run_checked(
            systemctl_command().args(["--user", "stop", unit.as_str()]),
            "systemctl --user stop",
        )
    }

    pub fn stop_remotely(_paths: &ServicePaths, config: &ServiceConfig) -> Result<()> {
        require_systemd(config)?;
        let unit = unit_name(&config.server_id);
        run_checked(
            systemctl_command().args(["--user", "--no-block", "stop", unit.as_str()]),
            "systemctl --user --no-block stop",
        )
    }

    pub fn restart(_paths: &ServicePaths, config: &ServiceConfig) -> Result<()> {
        require_systemd(config)?;
        let unit = unit_name(&config.server_id);
        run_checked(
            systemctl_command().args(["--user", "restart", unit.as_str()]),
            "systemctl --user restart",
        )
    }

    pub fn status(_paths: &ServicePaths, config: &ServiceConfig) -> Result<()> {
        require_systemd(config)?;
        let unit = unit_name(&config.server_id);
        run_checked(
            systemctl_command().args(["--user", "status", "--no-pager", unit.as_str()]),
            "systemctl --user status",
        )
    }

    pub fn logs(
        _paths: &ServicePaths,
        config: &ServiceConfig,
        lines: usize,
        follow: bool,
    ) -> Result<()> {
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

    pub fn uninstall(_paths: &ServicePaths, config: &ServiceConfig) -> Result<()> {
        let unit = unit_name(&config.server_id);
        if config.service_manager == ServiceManager::Foreground {
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

    pub fn register(paths: &ServicePaths, config: &ServiceConfig) -> Result<()> {
        match config.service_manager {
            ServiceManager::Foreground => return Ok(()),
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
        let log_path = paths.state_dir.join(format!(
            "agent-server-{}.log",
            component_key(&config.server_id)
        ));
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

    pub fn start(_paths: &ServicePaths, config: &ServiceConfig) -> Result<()> {
        require_launchd(config)?;
        let target = service_target(&config.server_id)?;
        let loaded = Command::new("launchctl")
            .args(["print", target.as_str()])
            .status()
            .context("failed to query LaunchAgent")?
            .success();
        if loaded {
            run_checked(
                Command::new("launchctl").args(["kickstart", target.as_str()]),
                "launchctl kickstart",
            )
        } else {
            let domain = domain()?;
            let plist = plist_path(&config.server_id)?;
            run_checked(
                Command::new("launchctl").args([
                    "bootstrap",
                    domain.as_str(),
                    plist.to_string_lossy().as_ref(),
                ]),
                "launchctl bootstrap",
            )
        }
    }

    pub fn stop(_paths: &ServicePaths, config: &ServiceConfig) -> Result<()> {
        require_launchd(config)?;
        let target = service_target(&config.server_id)?;
        run_checked(
            Command::new("launchctl").args(["bootout", target.as_str()]),
            "launchctl bootout",
        )
    }

    pub fn stop_remotely(paths: &ServicePaths, config: &ServiceConfig) -> Result<()> {
        require_launchd(config)?;
        stop(paths, config)
    }

    pub fn restart(paths: &ServicePaths, config: &ServiceConfig) -> Result<()> {
        require_launchd(config)?;
        let target = service_target(&config.server_id)?;
        let _ = Command::new("launchctl")
            .args(["bootout", target.as_str()])
            .status();
        start(paths, config)
    }

    pub fn status(_paths: &ServicePaths, config: &ServiceConfig) -> Result<()> {
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
        require_launchd(config)?;
        let log_path = paths.state_dir.join(format!(
            "agent-server-{}.log",
            component_key(&config.server_id)
        ));
        let mut command = Command::new("tail");
        command.args(["-n", &lines.to_string()]);
        if follow {
            command.arg("-f");
        }
        command.arg(log_path);
        run_checked(&mut command, "tail")
    }

    pub fn uninstall(_paths: &ServicePaths, config: &ServiceConfig) -> Result<()> {
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
        assert!(!socket.starts_with(state_dir().expect("state directory")));
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
        };
        assert_eq!(
            controller_epoch(&config).await.as_deref(),
            Some("epoch-test")
        );
        wait_for_controller_and_proxy(&config)
            .await
            .expect("Controller and Proxy readiness");

        server.abort();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn automatic_service_mode_falls_back_but_explicit_systemd_fails() {
        let automatic = select_linux_service_manager(ServiceMode::Auto, || {
            bail!("Failed to connect to bus: No such file or directory")
        })
        .expect("automatic fallback");
        assert_eq!(automatic.manager, ServiceManager::Foreground);
        assert!(automatic
            .fallback_reason
            .as_deref()
            .is_some_and(|reason| reason.contains("Failed to connect to bus")));

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
}
