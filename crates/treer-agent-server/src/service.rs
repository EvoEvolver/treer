use std::env;
use std::fs;
use std::io::ErrorKind;
use std::net::{Ipv4Addr, SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use treer_host_protocol::HostDaemonConfig;
use url::Url;
use uuid::Uuid;

const FIRST_AUTOMATIC_PORT: u16 = 8790;
const MAX_UPDATE_BINARY_BYTES: usize = 128 * 1024 * 1024;
const UPDATE_DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(120);
const UPDATE_VALIDATION_TIMEOUT: Duration = Duration::from_secs(10);
const CONTROLLER_START_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceConfig {
    pub proxy: String,
    pub workspace: String,
    pub server_id: String,
    pub machine_token: String,
    pub root: PathBuf,
    pub listen: String,
    pub host_socket: PathBuf,
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
    Ok(state_dir()?.join("machine-identity.json"))
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

    let paths = ServicePaths::new(workspace)?;
    if paths.config.is_file() {
        let installed = ServiceConfig::load(&paths.config)?;
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
    let paths = ServicePaths::new(&config.workspace)?;
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
    platform::register(&paths, &config.workspace)?;
    println!("treer: agent Host service registered");
    println!("treer: configured local API address: {}", config.listen);
    Ok(())
}

pub fn refresh_registration(config: ServiceConfig) -> Result<()> {
    let workspace = config.workspace.clone();
    register(config)?;
    if let Err(error) = restart_controller(&workspace) {
        eprintln!(
            "treer: warning: hot Controller restart failed ({error:#}); restarting the Host service"
        );
        restart(&workspace)?;
    }
    Ok(())
}

pub fn registered_config(workspace: &str) -> Result<Option<ServiceConfig>> {
    validate_workspace(workspace)?;
    let path = ServicePaths::new(workspace)?.config;
    if path.is_file() {
        ServiceConfig::load(&path).map(Some)
    } else {
        Ok(None)
    }
}

pub fn preflight_registration(workspace: &str) -> Result<()> {
    validate_workspace(workspace)?;
    require_host_binary(&ServicePaths::new(workspace)?)
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

pub fn host_socket_path(workspace: &str) -> Result<PathBuf> {
    validate_workspace(workspace)?;
    Ok(ServicePaths::new(workspace)?
        .state_dir
        .join(format!("host-{}.sock", workspace_key(workspace))))
}

pub fn start(workspace: &str) -> Result<()> {
    validate_workspace(workspace)?;
    platform::start(&ServicePaths::new(workspace)?, workspace)
}

pub fn stop(workspace: &str) -> Result<()> {
    validate_workspace(workspace)?;
    platform::stop(&ServicePaths::new(workspace)?, workspace)
}

pub fn stop_remotely(workspace: &str) -> Result<()> {
    validate_workspace(workspace)?;
    platform::stop_remotely(&ServicePaths::new(workspace)?, workspace)
}

pub fn restart(workspace: &str) -> Result<()> {
    validate_workspace(workspace)?;
    platform::restart(&ServicePaths::new(workspace)?, workspace)
}

pub fn restart_controller(workspace: &str) -> Result<()> {
    validate_workspace(workspace)?;
    let paths = ServicePaths::new(workspace)?;
    let config = ServiceConfig::load(&paths.config)?;
    restart_controller_at(&paths, &config.host_socket)
}

pub async fn update(workspace: &str) -> Result<()> {
    validate_workspace(workspace)?;
    let paths = ServicePaths::new(workspace)?;
    let config = ServiceConfig::load(&paths.config)?;
    let treer_executable = installed_treer_binary(&paths.executable).with_context(|| {
        format!(
            "could not find the installed treer CLI for {}",
            paths.executable.display()
        )
    })?;
    let platform = artifact_platform(std::env::consts::OS, std::env::consts::ARCH)?;
    let proxy = Url::parse(&config.proxy).context("invalid Proxy URL in service config")?;
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
    let previous_epoch = controller_epoch(&config).await;

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

    let activation = match restart_controller_at(&paths, &config.host_socket) {
        Ok(()) => wait_for_controller(&config, previous_epoch.as_deref()).await,
        Err(error) => Err(error),
    };
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
            let rollback_restart = restart_controller_at(&paths, &config.host_socket);
            if let Err(rollback_error) = rollback_restart {
                bail!(
                    "new Controller failed to activate ({error:#}); restored the old binaries but failed to restart the old Controller: {rollback_error:#}"
                );
            }
            wait_for_controller(&config, None).await.with_context(|| {
                format!("new Controller failed to activate ({error:#}); old binaries were restored but the old Controller did not recover")
            })?;
            bail!("new Controller failed to activate; old binaries were restored: {error:#}");
        }
        return Err(error);
    }

    if !controller_changed && !treer_changed {
        println!("treer: binaries were already current; Controller restarted");
    } else {
        println!("treer: update activated; Host and running agents were preserved");
    }
    Ok(())
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
    let address = config.listen.parse::<SocketAddr>().ok()?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_millis(500))
        .no_proxy()
        .build()
        .ok()?;
    let value = client
        .get(format!("http://{address}/api/health"))
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

pub fn status(workspace: &str) -> Result<()> {
    validate_workspace(workspace)?;
    platform::status(&ServicePaths::new(workspace)?, workspace)
}

pub fn logs(workspace: &str, lines: usize, follow: bool) -> Result<()> {
    validate_workspace(workspace)?;
    platform::logs(&ServicePaths::new(workspace)?, workspace, lines, follow)
}

pub fn uninstall(workspace: &str) -> Result<()> {
    validate_workspace(workspace)?;
    let paths = ServicePaths::new(workspace)?;
    platform::uninstall(&paths, workspace)?;
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
    fn new(workspace: &str) -> Result<Self> {
        let home = home_dir()?;
        let config_home = env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".config"));
        let state_dir = state_dir()?;
        let key = workspace_key(workspace);
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

fn state_dir() -> Result<PathBuf> {
    let home = home_dir()?;
    Ok(env::var_os("TREER_STATE_DIR")
        .map(PathBuf::from)
        .or_else(|| env::var_os("XDG_STATE_HOME").map(|path| PathBuf::from(path).join("treer")))
        .unwrap_or_else(|| home.join(".local/state/treer")))
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
fn systemd_unit(executable: &Path, config: &Path, workspace: &str) -> String {
    format!(
        "[Unit]\nDescription=Treer agent server ({workspace})\nAfter=network-online.target\nWants=network-online.target\n\n[Service]\nType=simple\nExecStart={} run --config {}\nRestart=always\nRestartSec=2\n\n[Install]\nWantedBy=default.target\n",
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

    fn unit_name(workspace: &str) -> String {
        format!("treer-agent-server-{}.service", workspace_key(workspace))
    }

    fn unit_path(workspace: &str) -> Result<PathBuf> {
        let home = home_dir()?;
        let config_home = env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".config"));
        Ok(config_home.join("systemd/user").join(unit_name(workspace)))
    }

    pub fn register(paths: &ServicePaths, workspace: &str) -> Result<()> {
        let unit_path = unit_path(workspace)?;
        let parent = unit_path
            .parent()
            .context("systemd user unit path has no parent")?;
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
        write_atomic(
            &unit_path,
            systemd_unit(&paths.host_executable, &paths.host_config, workspace).as_bytes(),
        )?;
        run_checked(
            Command::new("systemctl").args(["--user", "daemon-reload"]),
            "systemctl --user daemon-reload",
        )?;
        let unit = unit_name(workspace);
        run_checked(
            Command::new("systemctl").args(["--user", "enable", unit.as_str()]),
            "systemctl --user enable",
        )?;
        enable_linger();
        Ok(())
    }

    pub fn start(_paths: &ServicePaths, workspace: &str) -> Result<()> {
        let unit = unit_name(workspace);
        run_checked(
            Command::new("systemctl").args(["--user", "start", unit.as_str()]),
            "systemctl --user start",
        )
    }

    pub fn stop(_paths: &ServicePaths, workspace: &str) -> Result<()> {
        let unit = unit_name(workspace);
        run_checked(
            Command::new("systemctl").args(["--user", "stop", unit.as_str()]),
            "systemctl --user stop",
        )
    }

    pub fn stop_remotely(_paths: &ServicePaths, workspace: &str) -> Result<()> {
        let unit = unit_name(workspace);
        run_checked(
            Command::new("systemctl").args(["--user", "--no-block", "stop", unit.as_str()]),
            "systemctl --user --no-block stop",
        )
    }

    pub fn restart(_paths: &ServicePaths, workspace: &str) -> Result<()> {
        let unit = unit_name(workspace);
        run_checked(
            Command::new("systemctl").args(["--user", "restart", unit.as_str()]),
            "systemctl --user restart",
        )
    }

    pub fn status(_paths: &ServicePaths, workspace: &str) -> Result<()> {
        let unit = unit_name(workspace);
        run_checked(
            Command::new("systemctl").args(["--user", "status", "--no-pager", unit.as_str()]),
            "systemctl --user status",
        )
    }

    pub fn logs(_paths: &ServicePaths, workspace: &str, lines: usize, follow: bool) -> Result<()> {
        let unit = unit_name(workspace);
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

    pub fn uninstall(_paths: &ServicePaths, workspace: &str) -> Result<()> {
        let unit = unit_name(workspace);
        let _ = Command::new("systemctl")
            .args(["--user", "disable", "--now", unit.as_str()])
            .status();
        remove_if_exists(&unit_path(workspace)?)?;
        run_checked(
            Command::new("systemctl").args(["--user", "daemon-reload"]),
            "systemctl --user daemon-reload",
        )
    }

    fn enable_linger() {
        let Some(user) = env::var_os("USER") else {
            eprintln!("treer: warning: USER is unset; could not enable systemd linger");
            return;
        };
        match Command::new("loginctl")
            .arg("--no-ask-password")
            .arg("enable-linger")
            .arg(&user)
            .status()
        {
            Ok(status) if status.success() => {}
            _ => eprintln!(
                "treer: warning: could not enable linger; run `loginctl enable-linger {}` to keep the service running without a login session",
                user.to_string_lossy()
            ),
        }
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use super::*;

    fn label(workspace: &str) -> String {
        format!("dev.treer.agent-server.{}", workspace_key(workspace))
    }

    fn plist_path(workspace: &str) -> Result<PathBuf> {
        Ok(home_dir()?
            .join("Library/LaunchAgents")
            .join(format!("{}.plist", label(workspace))))
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

    fn service_target(workspace: &str) -> Result<String> {
        Ok(format!("{}/{}", domain()?, label(workspace)))
    }

    pub fn register(paths: &ServicePaths, workspace: &str) -> Result<()> {
        let plist_path = plist_path(workspace)?;
        let parent = plist_path
            .parent()
            .context("LaunchAgent path has no parent")?;
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
        let log_path = paths
            .state_dir
            .join(format!("agent-server-{}.log", workspace_key(workspace)));
        write_atomic(
            &plist_path,
            launchd_plist(
                &paths.host_executable,
                &paths.host_config,
                &label(workspace),
                &log_path,
                &log_path,
            )
            .as_bytes(),
        )?;
        Ok(())
    }

    pub fn start(_paths: &ServicePaths, workspace: &str) -> Result<()> {
        let target = service_target(workspace)?;
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
            let plist = plist_path(workspace)?;
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

    pub fn stop(_paths: &ServicePaths, workspace: &str) -> Result<()> {
        let target = service_target(workspace)?;
        run_checked(
            Command::new("launchctl").args(["bootout", target.as_str()]),
            "launchctl bootout",
        )
    }

    pub fn stop_remotely(paths: &ServicePaths, workspace: &str) -> Result<()> {
        stop(paths, workspace)
    }

    pub fn restart(paths: &ServicePaths, workspace: &str) -> Result<()> {
        let target = service_target(workspace)?;
        let _ = Command::new("launchctl")
            .args(["bootout", target.as_str()])
            .status();
        start(paths, workspace)
    }

    pub fn status(_paths: &ServicePaths, workspace: &str) -> Result<()> {
        let target = service_target(workspace)?;
        run_checked(
            Command::new("launchctl").args(["print", target.as_str()]),
            "launchctl print",
        )
    }

    pub fn logs(paths: &ServicePaths, workspace: &str, lines: usize, follow: bool) -> Result<()> {
        let log_path = paths
            .state_dir
            .join(format!("agent-server-{}.log", workspace_key(workspace)));
        let mut command = Command::new("tail");
        command.args(["-n", &lines.to_string()]);
        if follow {
            command.arg("-f");
        }
        command.arg(log_path);
        run_checked(&mut command, "tail")
    }

    pub fn uninstall(_paths: &ServicePaths, workspace: &str) -> Result<()> {
        let target = service_target(workspace)?;
        let _ = Command::new("launchctl")
            .args(["bootout", target.as_str()])
            .status();
        remove_if_exists(&plist_path(workspace)?)
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod platform {
    use super::*;

    fn unsupported() -> Result<()> {
        bail!("service management is currently supported on Linux and macOS")
    }

    pub fn register(_paths: &ServicePaths, _workspace: &str) -> Result<()> {
        unsupported()
    }

    pub fn start(_paths: &ServicePaths, _workspace: &str) -> Result<()> {
        unsupported()
    }

    pub fn stop(_paths: &ServicePaths, _workspace: &str) -> Result<()> {
        unsupported()
    }

    pub fn stop_remotely(_paths: &ServicePaths, _workspace: &str) -> Result<()> {
        unsupported()
    }

    pub fn restart(_paths: &ServicePaths, _workspace: &str) -> Result<()> {
        unsupported()
    }

    pub fn status(_paths: &ServicePaths, _workspace: &str) -> Result<()> {
        unsupported()
    }

    pub fn logs(
        _paths: &ServicePaths,
        _workspace: &str,
        _lines: usize,
        _follow: bool,
    ) -> Result<()> {
        unsupported()
    }

    pub fn uninstall(_paths: &ServicePaths, _workspace: &str) -> Result<()> {
        unsupported()
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
    fn automatic_address_is_available_to_bind() {
        let address = allocate_loopback_address().expect("allocate local API address");
        let _listener = TcpListener::bind(address).expect("allocated address should be available");
        assert!(address.ip().is_loopback());
    }

    #[tokio::test]
    async fn local_api_identity_distinguishes_the_installed_controller() {
        let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("bind health server");
        let address = listener.local_addr().expect("health server address");
        let app = Router::new().route(
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
            root: PathBuf::from("/tmp"),
            listen: address.to_string(),
            host_socket: PathBuf::from("/tmp/host.sock"),
        };
        assert_eq!(
            controller_epoch(&config).await.as_deref(),
            Some("epoch-test")
        );

        server.abort();
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
        );
        assert!(unit.contains("Restart=always"));
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
