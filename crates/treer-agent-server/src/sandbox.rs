use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use clap::Args as ClapArgs;
use futures_util::TryStreamExt;
use rtnetlink::{LinkUnspec, RouteMessageBuilder};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::process::Command;
use tun2proxy::{ArgDns, ArgProxy, ArgVerbosity, CancellationToken};
use uuid::Uuid;

#[derive(Debug, Clone, ClapArgs)]
pub struct ExecArgs {
    #[arg(long)]
    network_proxy: String,
    #[arg(last = true, required = true, allow_hyphen_values = true)]
    command: Vec<String>,
}

#[derive(Debug, Clone, ClapArgs)]
pub struct ChildArgs {
    #[arg(long)]
    network_proxy: String,
    #[arg(long)]
    socket_transfer_fd: i32,
    #[arg(long)]
    notify: PathBuf,
    #[arg(last = true, required = true, allow_hyphen_values = true)]
    command: Vec<String>,
}

#[derive(Debug, Clone, ClapArgs)]
pub struct AgentArgs {
    #[arg(long)]
    notify: PathBuf,
    #[arg(last = true, required = true, allow_hyphen_values = true)]
    command: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct AgentOutcome {
    exit_code: i32,
    error: Option<String>,
}

pub async fn run(args: ExecArgs) -> Result<()> {
    require_command(&args.command)?;
    let mut sandbox_dir = SandboxDirectory::create()?;
    let notify = sandbox_dir.path().join("agent.sock");
    let listener = UnixListener::bind(&notify)
        .with_context(|| format!("failed to create sandbox notifier {}", notify.display()))?;
    let (transfer_socket, remote_fd) = tun2proxy::socket_transfer::create_transfer_socket_pair()
        .await
        .context("failed to create sandbox network socket channel")?;
    let executable = std::env::current_exe().context("failed to locate sandbox executable")?;
    let mut child = Command::new("unshare");
    child
        .args([
            "--user",
            "--map-current-user",
            "--net",
            "--mount",
            "--keep-caps",
            "--kill-child",
            "--fork",
        ])
        .arg(&executable)
        .arg("sandbox-child")
        .arg("--network-proxy")
        .arg(&args.network_proxy)
        .arg("--socket-transfer-fd")
        .arg(remote_fd.as_raw_fd().to_string())
        .arg("--notify")
        .arg(&notify)
        .arg("--")
        .args(&args.command)
        .kill_on_drop(true);
    let mut child = child.spawn().map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            anyhow::anyhow!(
                "transparent networking requires unshare(1); install the util-linux package"
            )
        } else {
            anyhow::Error::from(error).context("failed to start Linux network namespace")
        }
    })?;
    drop(remote_fd);

    let shutdown = CancellationToken::new();
    let transfer_socket = Arc::new(transfer_socket);
    let transfer_task = tokio::spawn({
        let transfer_socket = transfer_socket.clone();
        let shutdown = shutdown.clone();
        async move {
            tun2proxy::socket_transfer::process_socket_requests(&transfer_socket, shutdown).await
        }
    });

    let outcome = tokio::select! {
        accepted = listener.accept() => {
            let (mut stream, _) = accepted.context("failed to accept sandbox agent notifier")?;
            let mut payload = Vec::new();
            stream.read_to_end(&mut payload).await.context("failed to read sandbox agent result")?;
            serde_json::from_slice::<AgentOutcome>(&payload)
                .context("sandbox agent returned an invalid result")?
        }
        status = child.wait() => {
            let status = status.context("failed to wait for Linux network namespace")?;
            bail!("network sandbox exited before the agent started: {status}");
        }
    };

    let _ = child.kill().await;
    shutdown.cancel();
    let _ = transfer_task.await;
    drop(listener);
    sandbox_dir.cleanup();
    if let Some(error) = outcome.error {
        bail!("failed to start sandboxed agent: {error}");
    }
    if outcome.exit_code != 0 {
        std::process::exit(outcome.exit_code.clamp(1, 255));
    }
    Ok(())
}

pub async fn run_child(args: ChildArgs) -> Result<()> {
    require_command(&args.command)?;
    let mut proxy = args.network_proxy;
    if let Some(rest) = proxy.strip_prefix("socks5h://") {
        proxy = format!("socks5://{rest}");
    }
    let config = tun2proxy::Args {
        proxy: ArgProxy::try_from(proxy.as_str()).context("invalid Treer network proxy")?,
        setup: true,
        dns: ArgDns::Virtual,
        verbosity: ArgVerbosity::Off,
        exit_on_fatal_error: true,
        socket_transfer_fd: Some(args.socket_transfer_fd),
        admin_command: sandbox_agent_command(&args.notify, &args.command)?,
        ..tun2proxy::Args::default()
    };
    prepare_namespace_proxy_route(config.proxy.addr.ip()).await?;
    tun2proxy::general_run_async(
        config,
        tun2proxy::DEFAULT_MTU,
        false,
        CancellationToken::new(),
    )
    .await
    .context("transparent network runtime failed")?;
    Ok(())
}

async fn prepare_namespace_proxy_route(proxy_ip: IpAddr) -> Result<()> {
    let (connection, handle, _) = rtnetlink::new_connection()
        .context("failed to open sandbox network configuration socket")?;
    tokio::spawn(connection);
    let mut links = handle.link().get().match_name("lo".to_string()).execute();
    let loopback = links
        .try_next()
        .await
        .context("failed to locate sandbox loopback interface")?
        .context("sandbox loopback interface is missing")?;
    let loopback_index = loopback.header.index;
    handle
        .link()
        .set(LinkUnspec::new_with_index(loopback_index).up().build())
        .execute()
        .await
        .context("failed to enable sandbox loopback interface")?;
    let route = match proxy_ip {
        IpAddr::V4(address) => RouteMessageBuilder::<Ipv4Addr>::new()
            .destination_prefix(address, 32)
            .output_interface(loopback_index)
            .build(),
        IpAddr::V6(address) => RouteMessageBuilder::<Ipv6Addr>::new()
            .destination_prefix(address, 128)
            .output_interface(loopback_index)
            .build(),
    };
    handle
        .route()
        .add(route)
        .execute()
        .await
        .context("failed to add sandbox proxy bypass route")?;
    Ok(())
}

pub async fn run_agent(args: AgentArgs) -> Result<()> {
    require_command(&args.command)?;
    let mut notifier = UnixStream::connect(&args.notify)
        .await
        .with_context(|| format!("failed to connect to {}", args.notify.display()))?;
    let (command, command_args) = args.command.split_first().expect("command was validated");
    let outcome = match Command::new(command)
        .args(command_args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .await
    {
        Ok(status) => AgentOutcome {
            exit_code: status.code().unwrap_or(1),
            error: None,
        },
        Err(error) => AgentOutcome {
            exit_code: 127,
            error: Some(error.to_string()),
        },
    };
    notifier
        .write_all(&serde_json::to_vec(&outcome)?)
        .await
        .context("failed to report sandbox agent result")?;
    notifier.shutdown().await.ok();
    Ok(())
}

fn sandbox_agent_command(notify: &Path, command: &[String]) -> Result<Vec<std::ffi::OsString>> {
    let executable = std::env::current_exe().context("failed to locate sandbox executable")?;
    let mut args = vec![
        executable.into_os_string(),
        "sandbox-agent".into(),
        "--notify".into(),
        notify.as_os_str().to_owned(),
        "--".into(),
    ];
    args.extend(command.iter().map(Into::into));
    Ok(args)
}

fn require_command(command: &[String]) -> Result<()> {
    if command.first().is_none_or(|command| command.is_empty()) {
        bail!("sandbox agent command is required");
    }
    Ok(())
}

struct SandboxDirectory {
    path: PathBuf,
    cleaned: bool,
}

impl SandboxDirectory {
    fn create() -> Result<Self> {
        let path =
            std::env::temp_dir().join(format!("treer-network-sandbox-{}", Uuid::new_v4().simple()));
        std::fs::create_dir(&path)
            .with_context(|| format!("failed to create sandbox directory {}", path.display()))?;
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))?;
        Ok(Self {
            path,
            cleaned: false,
        })
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn cleanup(&mut self) {
        if self.cleaned {
            return;
        }
        let _ = std::fs::remove_file(self.path.join("agent.sock"));
        let _ = std::fs::remove_dir(&self.path);
        self.cleaned = true;
    }
}

impl Drop for SandboxDirectory {
    fn drop(&mut self) {
        self.cleanup();
    }
}
