use super::*;
use std::os::windows::process::CommandExt;

#[derive(Serialize, Deserialize)]
struct ProcessIdentity {
    pid: u32,
    created_at: u64,
}

fn pid_path(paths: &ServicePaths, config: &ServiceConfig) -> PathBuf {
    paths.state_dir.join(format!(
        "agent-host-{}.pid",
        component_key(&config.server_id)
    ))
}
fn log_path(paths: &ServicePaths, config: &ServiceConfig) -> PathBuf {
    paths.state_dir.join(format!(
        "agent-server-{}.log",
        component_key(&config.server_id)
    ))
}
/// Serializes start/stop/relocation of one Host across processes and sessions.
fn host_lock(paths: &ServicePaths, config: &ServiceConfig) -> Result<fs::File> {
    let path = paths.state_dir.join(format!(
        "agent-host-{}.lock",
        component_key(&config.server_id)
    ));
    lock_file(&path, Duration::from_secs(30))?.context("Host startup lock is occupied")
}
fn identity(paths: &ServicePaths, config: &ServiceConfig) -> Result<Option<ProcessIdentity>> {
    match fs::read(pid_path(paths, config)) {
        Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}
fn live(identity: &ProcessIdentity) -> Result<bool> {
    Ok(treer_platform::process_created_at(identity.pid)? == Some(identity.created_at))
}

pub fn transition(previous: Option<&ServiceConfig>, next: &ServiceConfig) -> Result<()> {
    if let Some(previous) = previous {
        if previous.service_manager != next.service_manager
            && host_is_running(&previous.host_socket)
        {
            bail!("stop the running Windows Host before changing its supervision mode");
        }
    }
    Ok(())
}
pub fn register(paths: &ServicePaths, config: &ServiceConfig) -> Result<()> {
    if !matches!(
        config.service_manager,
        ServiceManager::Detached | ServiceManager::Foreground
    ) {
        bail!("this service configuration does not use a Windows supervision mode");
    }
    treer_platform::secure_directory(&paths.state_dir)?;
    Ok(())
}

pub fn start(paths: &ServicePaths, config: &ServiceConfig) -> Result<()> {
    if config.service_manager != ServiceManager::Detached {
        bail!("use restore or connect to start a foreground Host");
    }
    let _lock = host_lock(paths, config)?;
    require_root_alignment(paths, config)?;
    if let Some(process) = identity(paths, config)? {
        if live(&process)? {
            if host_is_running(&config.host_socket) {
                return Ok(());
            }
            bail!(
                "Host PID {} is alive but its pipe is unavailable; refusing a duplicate startup",
                process.pid
            );
        }
    }
    if host_is_running(&config.host_socket) {
        bail!(
            "a Host owns this pipe without matching process metadata; refusing a duplicate startup"
        );
    }
    treer_platform::secure_directory(&paths.state_dir)?;
    let log = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path(paths, config))?;
    treer_platform::secure_file(&log_path(paths, config))?;
    // DETACHED_PROCESS: no terminal ownership. Host creates ConPTY sessions
    // explicitly; it and its children are owned by the Host's Windows job.
    let mut child = Command::new(&paths.host_executable)
        .env_clear()
        .envs(treer_platform::inherited_environment())
        .args(["run", "--config"])
        .arg(&paths.host_config)
        .creation_flags(0x00000008)
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log)
        .spawn()
        .context("failed to start detached Windows Host")?;
    let process = ProcessIdentity {
        pid: child.id(),
        created_at: treer_platform::process_created_at(child.id())?
            .context("Windows Host exited during startup")?,
    };
    if let Err(error) = save_json(&process, &pid_path(paths, config)) {
        let _ = child.kill();
        return Err(error);
    }
    println!(
        "treer: started detached Windows Host with PID {}",
        process.pid
    );
    Ok(())
}

pub fn stop(paths: &ServicePaths, config: &ServiceConfig) -> Result<()> {
    let _lock = host_lock(paths, config)?;
    let Some(process) = identity(paths, config)? else {
        if host_is_running(&config.host_socket) {
            bail!("Host metadata is missing; refusing to terminate an unknown process");
        }
        return Ok(());
    };
    if live(&process)? {
        treer_platform::terminate_process(process.pid, process.created_at)?;
        let deadline = Instant::now() + Duration::from_secs(10);
        while live(&process)? || host_is_running(&config.host_socket) {
            if Instant::now() >= deadline {
                bail!("Windows Host did not stop within 10 seconds");
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    } else if host_is_running(&config.host_socket) {
        bail!("Host metadata is stale while its pipe is active; refusing to terminate an unknown process");
    }
    remove_if_exists(&pid_path(paths, config))
}
pub fn stop_remotely(paths: &ServicePaths, config: &ServiceConfig) -> Result<()> {
    // The caller is the Controller itself, so killing its Host also kills this
    // process. The existing remote-stop handler schedules this after its reply.
    stop(paths, config)
}
pub fn restart(paths: &ServicePaths, config: &ServiceConfig) -> Result<()> {
    stop(paths, config)?;
    start(paths, config)
}
pub fn status(paths: &ServicePaths, config: &ServiceConfig) -> Result<()> {
    let running = identity(paths, config)?
        .as_ref()
        .map(live)
        .transpose()?
        .unwrap_or(false);
    println!(
        "Manager: {:?}\nProcess running: {running}\nHost pipe available: {}\nLog: {}",
        config.service_manager,
        host_is_running(&config.host_socket),
        log_path(paths, config).display()
    );
    Ok(())
}
pub fn logs(
    paths: &ServicePaths,
    config: &ServiceConfig,
    lines: usize,
    follow: bool,
) -> Result<()> {
    use std::io::{BufRead, BufReader, Read, Write};
    let path = log_path(paths, config);
    let mut file = BufReader::new(fs::File::open(path)?);
    let mut tail = std::collections::VecDeque::new();
    for line in file.by_ref().lines() {
        tail.push_back(line?);
        if tail.len() > lines {
            tail.pop_front();
        }
    }
    for line in tail {
        println!("{line}");
    }
    if !follow {
        return Ok(());
    }
    loop {
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        if !bytes.is_empty() {
            std::io::stdout().write_all(&bytes)?;
            std::io::stdout().flush()?;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}
pub fn uninstall(paths: &ServicePaths, config: &ServiceConfig) -> Result<()> {
    stop(paths, config)
}
