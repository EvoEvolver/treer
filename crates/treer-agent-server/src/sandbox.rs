use std::io::{self};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream as StdTcpStream};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream as StdUnixStream;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use clap::Args as ClapArgs;
use futures_util::TryStreamExt;
use rtnetlink::{LinkUnspec, RouteMessageBuilder};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, UnixListener, UnixStream};
use tokio::process::Command;
use tun2proxy::{ArgDns, ArgProxy, ArgVerbosity, CancellationToken};
use uuid::Uuid;

#[derive(Debug, Clone, ClapArgs)]
pub struct ExecArgs {
    #[arg(long)]
    network_proxy: String,
    #[arg(long)]
    service_socket: PathBuf,
    /// Publish a namespace TCP port on 127.0.0.1 so Agent UI can reach a
    /// server that listens inside the Linux network sandbox.
    #[arg(long = "publish", value_name = "PORT")]
    publish: Vec<u16>,
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
    #[arg(long)]
    nsswitch: PathBuf,
    #[arg(long)]
    resolv_conf: PathBuf,
    #[arg(long)]
    nscd_mask: PathBuf,
    #[arg(long)]
    service_socket: PathBuf,
    #[arg(long)]
    publish_dir: Option<PathBuf>,
    #[arg(long = "publish", value_name = "PORT")]
    publish: Vec<u16>,
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
    remove_stale_socket(&args.service_socket)?;
    let mut sandbox_dir = SandboxDirectory::create()?;
    let notify = sandbox_dir.path().join("agent.sock");
    let nsswitch = sandbox_dir.path().join("nsswitch.conf");
    let resolv_conf = sandbox_dir.path().join("resolv.conf");
    let nscd_mask = sandbox_dir.path().join("nscd-mask");
    write_namespace_resolver_files(&nsswitch, &resolv_conf)?;
    std::fs::create_dir(&nscd_mask)
        .with_context(|| format!("failed to create {}", nscd_mask.display()))?;
    let listener = UnixListener::bind(&notify)
        .with_context(|| format!("failed to create sandbox notifier {}", notify.display()))?;
    let (transfer_socket, remote_fd) = tun2proxy::socket_transfer::create_transfer_socket_pair()
        .await
        .context("failed to create sandbox network socket channel")?;
    let publish_dir = sandbox_dir.path().to_path_buf();
    let mut publish_listeners = Vec::new();
    for port in &args.publish {
        if *port == 0 {
            bail!("sandbox publish port must not be 0");
        }
        let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, *port)))
            .await
            .with_context(|| {
                format!("failed to publish sandbox port 127.0.0.1:{port} on the host loopback")
            })?;
        publish_listeners.push((*port, listener));
    }
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
        .arg("--nsswitch")
        .arg(&nsswitch)
        .arg("--resolv-conf")
        .arg(&resolv_conf)
        .arg("--nscd-mask")
        .arg(&nscd_mask)
        .arg("--service-socket")
        .arg(&args.service_socket);
    if !args.publish.is_empty() {
        child.arg("--publish-dir").arg(&publish_dir);
        for port in &args.publish {
            child.arg("--publish").arg(port.to_string());
        }
        let published = args
            .publish
            .iter()
            .map(u16::to_string)
            .collect::<Vec<_>>()
            .join(",");
        child.env("TREER_SANDBOX_PUBLISHED", published);
    }
    child.arg("--").args(&args.command).kill_on_drop(true);
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
    for (port, listener) in publish_listeners {
        tokio::spawn(publish_host_port(
            publish_socket_path(&publish_dir, port),
            port,
            listener,
        ));
    }

    let shutdown = CancellationToken::new();
    let transfer_socket = Arc::new(transfer_socket);
    let transfer_task = tokio::spawn({
        let transfer_socket = transfer_socket.clone();
        let shutdown = shutdown.clone();
        async move {
            tun2proxy::socket_transfer::process_socket_requests(&transfer_socket, shutdown).await
        }
    });

    let outcome = async {
        tokio::select! {
            accepted = listener.accept() => {
                let (mut stream, _) = accepted.context("failed to accept sandbox agent notifier")?;
                let mut payload = Vec::new();
                stream.read_to_end(&mut payload).await.context("failed to read sandbox agent result")?;
                serde_json::from_slice::<AgentOutcome>(&payload)
                    .context("sandbox agent returned an invalid result")
            }
            status = child.wait() => {
                let status = status.context("failed to wait for Linux network namespace")?;
                bail!("network sandbox exited before the agent started: {status}");
            }
        }
    }
    .await;

    let _ = child.kill().await;
    shutdown.cancel();
    let _ = transfer_task.await;
    drop(listener);
    let _ = std::fs::remove_file(&args.service_socket);
    sandbox_dir.cleanup();
    let outcome = outcome?;
    if let Some(error) = outcome.error {
        bail!("failed to start sandboxed agent: {error}");
    }
    if outcome.exit_code != 0 {
        std::process::exit(outcome.exit_code.clamp(1, 255));
    }
    Ok(())
}

fn publish_socket_path(dir: &Path, port: u16) -> PathBuf {
    dir.join(format!("p-{port}.sock"))
}

async fn publish_host_port(path: PathBuf, port: u16, listener: TcpListener) {
    loop {
        let Ok((incoming, _)) = listener.accept().await else {
            return;
        };
        let path = path.clone();
        tokio::spawn(async move {
            let Ok(host) = incoming.into_std() else {
                return;
            };
            match connect_unix_with_retry(&path).await {
                Ok(guest) => {
                    std::thread::Builder::new()
                        .name(format!("sandbox-publish-{port}"))
                        .spawn(move || {
                            if let Err(error) = splice_host_to_namespace(host, guest) {
                                tracing::debug!(%error, port, "sandbox publish stream closed");
                            }
                        })
                        .ok();
                }
                Err(error) => {
                    tracing::debug!(%error, port, "sandbox publish unix connect failed");
                }
            }
        });
    }
}

async fn connect_unix_with_retry(path: &Path) -> io::Result<StdUnixStream> {
    let mut last = io::Error::new(io::ErrorKind::NotFound, "sandbox publish socket missing");
    for _ in 0..400 {
        match UnixStream::connect(path).await {
            Ok(stream) => return stream.into_std(),
            Err(error)
                if error.kind() == io::ErrorKind::NotFound
                    || error.kind() == io::ErrorKind::ConnectionRefused =>
            {
                last = error;
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Err(error) => return Err(error),
        }
    }
    Err(last)
}

fn splice_host_to_namespace(mut host: StdTcpStream, mut guest: StdUnixStream) -> io::Result<()> {
    host.set_nonblocking(false)?;
    guest.set_nonblocking(false)?;
    host.set_nodelay(true).ok();
    let mut host_read = host.try_clone()?;
    let mut guest_write = guest.try_clone()?;
    let to_guest = std::thread::spawn(move || std::io::copy(&mut host_read, &mut guest_write));
    join_copy(to_guest, std::io::copy(&mut guest, &mut host))
}

fn splice_namespace_to_host(mut unix: StdUnixStream, mut tcp: StdTcpStream) -> io::Result<()> {
    unix.set_nonblocking(false)?;
    tcp.set_nonblocking(false)?;
    tcp.set_nodelay(true).ok();
    let mut unix_read = unix.try_clone()?;
    let mut tcp_write = tcp.try_clone()?;
    let to_tcp = std::thread::spawn(move || std::io::copy(&mut unix_read, &mut tcp_write));
    join_copy(to_tcp, std::io::copy(&mut tcp, &mut unix))
}

fn join_copy(
    other: std::thread::JoinHandle<io::Result<u64>>,
    local: io::Result<u64>,
) -> io::Result<()> {
    let other = other
        .join()
        .map_err(|_| io::Error::other("sandbox publish copy thread panicked"))?;
    other?;
    local?;
    Ok(())
}

async fn start_namespace_publishers(dir: PathBuf, ports: &[u16]) -> Result<()> {
    for port in ports {
        if *port == 0 {
            bail!("sandbox publish port must not be 0");
        }
        let path = publish_socket_path(&dir, *port);
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).with_context(|| {
            format!(
                "failed to listen on sandbox publish socket {}",
                path.display()
            )
        })?;
        tokio::spawn(accept_namespace_publish(listener, *port));
    }
    Ok(())
}

async fn accept_namespace_publish(listener: UnixListener, port: u16) {
    loop {
        let Ok((incoming, _)) = listener.accept().await else {
            return;
        };
        tokio::spawn(async move {
            let Ok(unix) = incoming.into_std() else {
                return;
            };
            std::thread::Builder::new()
                .name(format!("sandbox-ns-publish-{port}"))
                .spawn(move || {
                    if let Err(error) = splice_namespace_connection(unix, port) {
                        tracing::debug!(%error, port, "sandbox namespace publish stream closed");
                    }
                })
                .ok();
        });
    }
}

fn splice_namespace_connection(unix: StdUnixStream, port: u16) -> io::Result<()> {
    let tcp = connect_namespace_tcp_with_retry(port)?;
    splice_namespace_to_host(unix, tcp)
}

fn connect_namespace_tcp_with_retry(port: u16) -> io::Result<StdTcpStream> {
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let mut last = io::Error::other("sandbox publish connect failed");
    for _ in 0..400 {
        match StdTcpStream::connect(addr) {
            Ok(stream) => return Ok(stream),
            Err(error)
                if error.kind() == io::ErrorKind::ConnectionRefused
                    || error.kind() == io::ErrorKind::AddrNotAvailable =>
            {
                last = error;
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(error) => return Err(error),
        }
    }
    Err(last)
}

pub async fn run_child(args: ChildArgs) -> Result<()> {
    require_command(&args.command)?;
    mount_namespace_resolver_files(&args.nsswitch, &args.resolv_conf, &args.nscd_mask).await?;
    if !args.publish.is_empty() {
        let dir = args
            .publish_dir
            .clone()
            .context("sandbox-child --publish-dir is required when publishing namespace ports")?;
        start_namespace_publishers(dir, &args.publish).await?;
    }
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
    let service_listener = UnixListener::bind(&args.service_socket).with_context(|| {
        format!(
            "failed to create Agent service bridge {}",
            args.service_socket.display()
        )
    })?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&args.service_socket, std::fs::Permissions::from_mode(0o600))?;
    let service_task = tokio::spawn(serve_agent_services(service_listener));
    let result = tun2proxy::general_run_async(
        config,
        tun2proxy::DEFAULT_MTU,
        false,
        CancellationToken::new(),
    )
    .await
    .context("transparent network runtime failed");
    service_task.abort();
    let _ = service_task.await;
    let _ = std::fs::remove_file(&args.service_socket);
    result.map(|_| ())
}

fn remove_stale_socket(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("failed to remove {}", path.display())),
    }
}

async fn serve_agent_services(listener: UnixListener) {
    loop {
        let (stream, _) = match listener.accept().await {
            Ok(accepted) => accepted,
            Err(error) => {
                tracing::warn!(%error, "Agent service bridge stopped accepting connections");
                return;
            }
        };
        tokio::spawn(async move {
            if let Err(error) = bridge_agent_service(stream).await {
                tracing::debug!(%error, "Agent service bridge connection closed");
            }
        });
    }
}

async fn bridge_agent_service(mut stream: UnixStream) -> Result<()> {
    let port = stream
        .read_u16()
        .await
        .context("failed to read Agent service port")?;
    if port == 0 {
        bail!("Agent service port must not be zero");
    }
    let mut service = match tokio::net::TcpStream::connect((Ipv4Addr::LOCALHOST, port)).await {
        Ok(service) => service,
        Err(error) => {
            let _ = stream.write_u8(1).await;
            return Err(error)
                .with_context(|| format!("failed to connect to Agent service on port {port}"));
        }
    };
    stream
        .write_u8(0)
        .await
        .context("failed to acknowledge Agent service connection")?;
    tokio::io::copy_bidirectional(&mut stream, &mut service).await?;
    Ok(())
}

fn write_namespace_resolver_files(nsswitch: &Path, resolv_conf: &Path) -> Result<()> {
    let installed = match std::fs::read_to_string("/etc/nsswitch.conf") {
        Ok(value) => value,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(error).context("failed to read /etc/nsswitch.conf"),
    };
    std::fs::write(nsswitch, sandbox_nsswitch(&installed))
        .with_context(|| format!("failed to write {}", nsswitch.display()))?;
    std::fs::write(
        resolv_conf,
        "# Routed to tun2proxy's virtual DNS inside the agent namespace.\nnameserver 8.8.8.8\noptions attempts:1 timeout:1\n",
    )
    .with_context(|| format!("failed to write {}", resolv_conf.display()))?;
    Ok(())
}

fn sandbox_nsswitch(installed: &str) -> String {
    let mut output = String::new();
    let mut replaced = false;
    for line in installed.lines() {
        let is_hosts = line
            .split_once(':')
            .is_some_and(|(database, _)| database.trim() == "hosts");
        if is_hosts {
            if !replaced {
                output.push_str("hosts: files dns\n");
                replaced = true;
            }
        } else {
            output.push_str(line);
            output.push('\n');
        }
    }
    if !replaced {
        output.push_str("hosts: files dns\n");
    }
    output
}

async fn mount_namespace_resolver_files(
    nsswitch: &Path,
    resolv_conf: &Path,
    nscd_mask: &Path,
) -> Result<()> {
    run_mount(["--make-rprivate".as_ref(), Path::new("/")]).await?;
    run_mount(["--bind".as_ref(), nsswitch, Path::new("/etc/nsswitch.conf")]).await?;
    run_mount([
        "--bind".as_ref(),
        resolv_conf,
        Path::new("/etc/resolv.conf"),
    ])
    .await?;
    mask_host_nscd(nscd_mask).await?;
    Ok(())
}

async fn mask_host_nscd(mask: &Path) -> Result<()> {
    let mut targets = Vec::new();
    for target in [Path::new("/run/nscd"), Path::new("/var/run/nscd")] {
        if !target.is_dir() {
            continue;
        }
        let canonical = std::fs::canonicalize(target)
            .with_context(|| format!("failed to resolve {}", target.display()))?;
        if !targets.contains(&canonical) {
            targets.push(canonical);
        }
    }
    for target in targets {
        run_mount(["--bind".as_ref(), mask, target.as_path()]).await?;
    }
    Ok(())
}

async fn run_mount<const N: usize>(args: [&Path; N]) -> Result<()> {
    let status = Command::new("mount")
        .args(args)
        .status()
        .await
        .context("transparent networking requires mount(8) from util-linux")?;
    if status.success() {
        Ok(())
    } else {
        bail!("failed to configure agent namespace resolver: mount exited with {status}")
    }
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
        let _ = std::fs::remove_file(self.path.join("nsswitch.conf"));
        let _ = std::fs::remove_file(self.path.join("resolv.conf"));
        if let Ok(entries) = std::fs::read_dir(&self.path) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if name.starts_with("p-") && name.ends_with(".sock") {
                    let _ = std::fs::remove_file(entry.path());
                }
            }
        }
        let _ = std::fs::remove_dir(self.path.join("nscd-mask"));
        let _ = std::fs::remove_dir(&self.path);
        self.cleaned = true;
    }
}

impl Drop for SandboxDirectory {
    fn drop(&mut self) {
        self.cleanup();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sandbox_nsswitch_bypasses_mdns_and_preserves_other_databases() {
        let installed = "passwd: files systemd\nhosts: files mdns4_minimal [NOTFOUND=return] dns myhostname\ngroup: files systemd\n";

        let configured = sandbox_nsswitch(installed);

        assert_eq!(
            configured,
            "passwd: files systemd\nhosts: files dns\ngroup: files systemd\n"
        );
    }

    #[test]
    fn sandbox_nsswitch_adds_a_hosts_database_when_missing() {
        assert_eq!(
            sandbox_nsswitch("passwd: files\n"),
            "passwd: files\nhosts: files dns\n"
        );
    }

    #[test]
    fn publish_socket_path_is_stable_per_port() {
        assert_eq!(
            publish_socket_path(Path::new("/tmp/sandbox"), 4173),
            PathBuf::from("/tmp/sandbox/p-4173.sock")
        );
    }

    #[tokio::test]
    async fn agent_service_bridge_connects_inside_its_loopback() {
        let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("bind Agent service");
        let port = listener.local_addr().expect("service address").port();
        let service = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept Agent service");
            let mut request = [0_u8; 4];
            stream.read_exact(&mut request).await.expect("read request");
            assert_eq!(&request, b"ping");
            stream.write_all(b"pong").await.expect("write response");
        });
        let (mut client, bridge) = UnixStream::pair().expect("create bridge pair");
        let bridge = tokio::spawn(bridge_agent_service(bridge));

        client.write_u16(port).await.expect("select service port");
        assert_eq!(client.read_u8().await.expect("read bridge ACK"), 0);
        client.write_all(b"ping").await.expect("write request");
        let mut response = [0_u8; 4];
        client
            .read_exact(&mut response)
            .await
            .expect("read response");
        assert_eq!(&response, b"pong");
        drop(client);

        service.await.expect("Agent service task");
        bridge.await.expect("bridge task").expect("bridge result");
    }
}
