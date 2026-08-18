mod controller;
mod host_client;
mod local_api;
mod network;
mod proxy;
#[cfg(target_os = "linux")]
mod sandbox;
mod service;

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Args as ClapArgs, Parser, Subcommand};
use tracing::info;
use treer_protocol::{parse_machine_enrollment_key, ApiError, MachineEnrollmentResponse};
use url::Url;
use uuid::Uuid;

use controller::ControllerRuntime;
use host_client::HostClient;

#[derive(Debug, Parser)]
#[command(name = "treer-agent-server", about = "Treer machine agent runtime")]
struct Args {
    #[command(subcommand)]
    command: Option<Command>,
    #[command(flatten)]
    server: ServerArgs,
}

#[derive(Debug, Subcommand)]
enum Command {
    #[command(about = "Connect this machine to a Proxy workspace")]
    Connect(ConnectArgs),
    #[command(about = "Download and hot-activate the latest Controller and treer CLI")]
    Update(UpdateArgs),
    #[command(hide = true, about = "Run from a saved service configuration")]
    Run {
        #[arg(long)]
        config: PathBuf,
    },
    #[command(about = "Manage the host agent-server service")]
    Service(ServiceArgs),
    #[cfg(target_os = "linux")]
    #[command(hide = true)]
    SandboxExec(sandbox::ExecArgs),
    #[cfg(target_os = "linux")]
    #[command(hide = true)]
    SandboxChild(sandbox::ChildArgs),
    #[cfg(target_os = "linux")]
    #[command(hide = true)]
    SandboxAgent(sandbox::AgentArgs),
}

#[derive(Debug, ClapArgs)]
struct UpdateArgs {
    #[arg(long, default_value = "default")]
    workspace: String,
}

#[derive(Debug, ClapArgs)]
struct ServiceArgs {
    #[arg(long, default_value = "default", global = true)]
    workspace: String,
    #[command(subcommand)]
    command: ServiceCommand,
}

#[derive(Debug, Subcommand)]
enum ServiceCommand {
    #[command(about = "Start the installed service")]
    Start,
    #[command(about = "Stop the installed service")]
    Stop,
    #[command(about = "Restart the installed service")]
    Restart,
    #[command(about = "Hot restart only the replaceable Controller")]
    RestartController,
    #[command(about = "Show service-manager status")]
    Status,
    #[command(about = "Show service logs")]
    Logs {
        #[arg(long, default_value_t = 100)]
        lines: usize,
        #[arg(long)]
        follow: bool,
    },
    #[command(about = "Stop and remove the installed service")]
    Uninstall,
}

#[derive(Debug, ClapArgs)]
struct ConnectArgs {
    #[arg(long, env = "TREER_PROXY_URL", default_value = "http://127.0.0.1:8787")]
    proxy: Url,
    #[arg(long = "key", env = "TREER_ENROLLMENT_KEY")]
    enrollment_key: String,
    #[arg(long, env = "TREER_WORKSPACE_ROOT", default_value = ".")]
    root: PathBuf,
    #[arg(long, env = "TREER_AGENT_SERVER_LISTEN")]
    listen: Option<SocketAddr>,
}

#[derive(Debug, Clone, ClapArgs)]
struct ServerArgs {
    #[arg(long, env = "TREER_PROXY_URL", default_value = "http://127.0.0.1:8787")]
    proxy: Url,
    #[arg(long, env = "TREER_WORKSPACE_ID", default_value = "default")]
    workspace: String,
    #[arg(long, env = "TREER_SERVER_ID")]
    server_id: Option<String>,
    #[arg(long, env = "TREER_MACHINE_TOKEN")]
    machine_token: Option<String>,
    #[arg(long, env = "TREER_WORKSPACE_ROOT", default_value = ".")]
    root: PathBuf,
    #[arg(
        long,
        env = "TREER_AGENT_SERVER_LISTEN",
        default_value = "127.0.0.1:8790"
    )]
    listen: SocketAddr,
    #[arg(long, env = "TREER_HOST_SOCKET", default_value = ".treer/host.sock")]
    host_socket: PathBuf,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "treer_agent_server=info".into()),
        )
        .init();
    let args = Args::parse();
    match args.command {
        None => run_server(args.server).await,
        Some(Command::Connect(connect)) => connect_machine(connect).await,
        Some(Command::Update(update)) => service::update(&update.workspace).await,
        Some(Command::Run { config }) => {
            let config = service::ServiceConfig::load(&config)?;
            run_server(ServerArgs {
                proxy: Url::parse(&config.proxy).context("invalid proxy URL in service config")?,
                workspace: config.workspace,
                server_id: Some(config.server_id),
                machine_token: Some(config.machine_token),
                root: config.root,
                listen: config
                    .listen
                    .parse()
                    .context("invalid listen address in service config")?,
                host_socket: config.host_socket,
            })
            .await
        }
        Some(Command::Service(service_args)) => {
            run_service_command(service_args).await?;
            Ok(())
        }
        #[cfg(target_os = "linux")]
        Some(Command::SandboxExec(sandbox_args)) => sandbox::run(sandbox_args).await,
        #[cfg(target_os = "linux")]
        Some(Command::SandboxChild(sandbox_args)) => sandbox::run_child(sandbox_args).await,
        #[cfg(target_os = "linux")]
        Some(Command::SandboxAgent(sandbox_args)) => sandbox::run_agent(sandbox_args).await,
    }
}

async fn run_server(args: ServerArgs) -> Result<()> {
    require_loopback_listen(args.listen)?;
    let root = std::fs::canonicalize(&args.root)
        .with_context(|| format!("invalid workspace root {}", args.root.display()))?;
    let server_id = match args.server_id {
        Some(server_id) => server_id,
        None => load_or_create_server_id(&root)?,
    };
    let hostname = std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .unwrap_or_else(|_| server_id.clone());
    let listener = tokio::net::TcpListener::bind(args.listen)
        .await
        .with_context(|| format!("failed to bind local API at {}", args.listen))?;
    let listen_address = listener.local_addr()?;
    let network = network::NetworkRuntime::bind_near(listen_address)
        .await
        .context("failed to bind local network proxy")?;
    let sandbox_executable = transparent_network_executable()?;
    let agent_server_url = agent_server_url(listen_address, sandbox_executable.is_some());
    let (host, host_events) = HostClient::connect(&args.host_socket).await?;
    let sync = host.sync(std::collections::BTreeMap::new()).await?;
    let (runtime, mut host_disconnected) = ControllerRuntime::from_sync(
        host,
        sync,
        host_events,
        controller::ControllerConfig {
            workspace_id: args.workspace.clone(),
            server_id: server_id.clone(),
            workspace_root: root.clone(),
            agent_server_url,
            network_proxy_url: network.proxy_url(),
            treer_binary: sibling_treer_binary(),
            sandbox_executable,
        },
    )
    .map_err(|error| anyhow::anyhow!(error.message))?;
    let proxy_http = normalize_http_url(args.proxy.clone())?;
    let proxy_ws = agent_websocket_url(args.proxy)?;
    let server = proxy::server_info(
        server_id.clone(),
        args.workspace.clone(),
        hostname,
        root.display().to_string(),
    );

    let proxy_client = proxy::ProxyClient::new(
        proxy_ws,
        args.machine_token.clone(),
        server,
        runtime,
        network.clone(),
    );
    let proxy_task = tokio::spawn(proxy_client.run_forever());

    let local_state = local_api::LocalApiState::new(
        proxy_http,
        args.workspace.clone(),
        server_id.clone(),
        args.machine_token,
    );
    let app = local_api::router(local_state);
    info!(
        address = %listen_address,
        network_proxy = %network.listen_address(),
        %server_id,
        workspace = %args.workspace,
        root = %root.display(),
        "treer agent server listening"
    );
    let result = tokio::select! {
        result = axum::serve(listener, app) => result.context("local API failed"),
        changed = host_disconnected.changed() => {
            changed.context("host disconnect watcher closed")?;
            anyhow::bail!("host connection closed")
        }
    };
    proxy_task.abort();
    result
}

fn agent_server_url(listen_address: SocketAddr, transparent: bool) -> String {
    let host = if transparent {
        network::SANDBOX_LOCAL_API_HOST
    } else {
        "127.0.0.1"
    };
    format!("http://{host}:{}", listen_address.port())
}

fn transparent_network_executable() -> Result<Option<PathBuf>> {
    let mode = std::env::var("TREER_NETWORK_MODE").unwrap_or_else(|_| {
        if cfg!(target_os = "linux") {
            "transparent".to_string()
        } else {
            "proxy-env".to_string()
        }
    });
    match mode.as_str() {
        "proxy-env" => Ok(None),
        "transparent" if cfg!(target_os = "linux") => std::env::current_exe()
            .context("failed to locate Controller executable for network sandbox")
            .map(Some),
        "transparent" => anyhow::bail!(
            "transparent network mode is currently supported only on Linux; use a Linux container"
        ),
        _ => anyhow::bail!("TREER_NETWORK_MODE must be transparent or proxy-env"),
    }
}

async fn run_service_command(args: ServiceArgs) -> Result<()> {
    match args.command {
        ServiceCommand::Start => service::start(&args.workspace),
        ServiceCommand::Stop => service::stop(&args.workspace),
        ServiceCommand::Restart => service::restart(&args.workspace),
        ServiceCommand::RestartController => service::restart_controller(&args.workspace),
        ServiceCommand::Status => service::status(&args.workspace),
        ServiceCommand::Logs { lines, follow } => service::logs(&args.workspace, lines, follow),
        ServiceCommand::Uninstall => service::uninstall(&args.workspace),
    }
}

async fn connect_machine(args: ConnectArgs) -> Result<()> {
    if let Some(listen) = args.listen {
        require_loopback_listen(listen)?;
    }
    let enrollment = parse_machine_enrollment_key(&args.enrollment_key)
        .map_err(|error| anyhow::anyhow!(error.message))?;
    if service::is_registered(&enrollment.workspace_id)? {
        anyhow::bail!(
            "workspace {} is already connected on this machine; uninstall it before enrolling again",
            enrollment.workspace_id
        );
    }
    service::preflight_registration(&enrollment.workspace_id)?;
    let root = std::fs::canonicalize(&args.root)
        .with_context(|| format!("invalid workspace root {}", args.root.display()))?;
    let proxy = normalize_http_url(args.proxy)?;
    let response = claim_machine_enrollment(&proxy, &args.enrollment_key).await?;
    if response.workspace_id != enrollment.workspace_id {
        anyhow::bail!("Proxy returned a workspace that does not match the enrollment key");
    }
    let listen = service::resolve_listen(&response.workspace_id, args.listen).await?;
    let host_socket = service::host_socket_path(&response.workspace_id)?;
    service::register(service::ServiceConfig {
        proxy: proxy.to_string(),
        workspace: response.workspace_id.clone(),
        server_id: response.server_id,
        machine_token: response.machine_token,
        root,
        listen: listen.to_string(),
        host_socket,
    })?;
    service::start(&response.workspace_id)?;
    println!(
        "treer: connected workspace {} to {}",
        response.workspace_id, proxy
    );
    Ok(())
}

async fn claim_machine_enrollment(
    proxy: &Url,
    enrollment_key: &str,
) -> Result<MachineEnrollmentResponse> {
    let endpoint = proxy
        .join("api/machines/enroll")
        .context("failed to build Proxy enrollment URL")?;
    let response = reqwest::Client::new()
        .post(endpoint.clone())
        .bearer_auth(enrollment_key)
        .send()
        .await
        .with_context(|| format!("failed to connect to {endpoint}"))?;
    let status = response.status();
    if !status.is_success() {
        let error = response.json::<ApiError>().await.ok();
        let message = error
            .map(|error| error.error.message)
            .unwrap_or_else(|| format!("Proxy enrollment failed with HTTP {status}"));
        anyhow::bail!(message);
    }
    response
        .json()
        .await
        .context("Proxy returned an invalid enrollment response")
}

fn require_loopback_listen(listen: SocketAddr) -> Result<()> {
    if !listen.ip().is_loopback() {
        anyhow::bail!(
            "the unauthenticated local agent API must listen on a loopback address, got {listen}"
        );
    }
    Ok(())
}

fn normalize_http_url(mut url: Url) -> Result<Url> {
    match url.scheme() {
        "http" | "https" => {}
        "ws" => {
            url.set_scheme("http")
                .map_err(|_| anyhow::anyhow!("invalid proxy URL scheme"))?;
        }
        "wss" => {
            url.set_scheme("https")
                .map_err(|_| anyhow::anyhow!("invalid proxy URL scheme"))?;
        }
        scheme => anyhow::bail!("unsupported proxy URL scheme {scheme}"),
    }
    url.set_path("/");
    url.set_query(None);
    Ok(url)
}

fn agent_websocket_url(mut url: Url) -> Result<Url> {
    match url.scheme() {
        "http" => url
            .set_scheme("ws")
            .map_err(|_| anyhow::anyhow!("invalid proxy URL scheme"))?,
        "https" => url
            .set_scheme("wss")
            .map_err(|_| anyhow::anyhow!("invalid proxy URL scheme"))?,
        "ws" | "wss" => {}
        scheme => anyhow::bail!("unsupported proxy URL scheme {scheme}"),
    }
    url.set_path("/agent/connect");
    url.set_query(None);
    Ok(url)
}

fn load_or_create_server_id(root: &Path) -> Result<String> {
    let state_dir = root.join(".treer");
    let path = state_dir.join("server-id");
    match std::fs::read_to_string(&path) {
        Ok(value) if !value.trim().is_empty() => return Ok(value.trim().to_string()),
        Ok(_) | Err(_) => {}
    }
    std::fs::create_dir_all(&state_dir)
        .with_context(|| format!("failed to create {}", state_dir.display()))?;
    let server_id = format!("srv_{}", Uuid::new_v4().simple());
    std::fs::write(&path, format!("{server_id}\n"))
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(server_id)
}

fn sibling_treer_binary() -> Option<PathBuf> {
    let executable = std::env::current_exe().ok()?;
    service::installed_treer_binary(&executable)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderMap;
    use axum::routing::post;
    use axum::{Json, Router};
    use treer_protocol::format_machine_enrollment_key;

    #[test]
    fn local_agent_api_only_accepts_loopback_addresses() {
        assert!(require_loopback_listen("127.0.0.1:8790".parse().expect("IPv4 loopback")).is_ok());
        assert!(require_loopback_listen("[::1]:8790".parse().expect("IPv6 loopback")).is_ok());
        assert!(require_loopback_listen("0.0.0.0:8790".parse().expect("unspecified")).is_err());
        assert!(
            require_loopback_listen("192.0.2.1:8790".parse().expect("public address")).is_err()
        );
    }

    #[test]
    fn transparent_agents_use_the_sandbox_local_api_route() {
        let address = "127.0.0.1:8790".parse().expect("local API address");
        assert_eq!(
            agent_server_url(address, true),
            "http://treer-agent-server.invalid:8790"
        );
        assert_eq!(agent_server_url(address, false), "http://127.0.0.1:8790");
    }

    #[test]
    fn connect_command_gets_its_workspace_from_the_key() {
        let key = format_machine_enrollment_key(
            "workspace-a",
            "abc123",
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        )
        .expect("enrollment key");
        let args = Args::try_parse_from([
            "treer-agent-server",
            "connect",
            "--proxy",
            "https://treer.example",
            "--key",
            &key,
        ])
        .expect("parse connect command");
        let Some(Command::Connect(connect)) = args.command else {
            panic!("expected connect command");
        };
        assert_eq!(
            parse_machine_enrollment_key(&connect.enrollment_key)
                .expect("parse enrollment key")
                .workspace_id,
            "workspace-a"
        );
    }

    #[test]
    fn update_command_accepts_a_workspace() {
        let args =
            Args::try_parse_from(["treer-agent-server", "update", "--workspace", "workspace-a"])
                .expect("parse update command");
        let Some(Command::Update(update)) = args.command else {
            panic!("expected update command");
        };
        assert_eq!(update.workspace, "workspace-a");
    }

    #[tokio::test]
    async fn enrollment_exchange_uses_a_bearer_key() {
        const KEY: &str = "enr_v1_776f726b73706163652d61_abc123.0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let app = Router::new().route(
            "/api/machines/enroll",
            post(|headers: HeaderMap| async move {
                let expected = format!("Bearer {KEY}");
                assert_eq!(
                    headers
                        .get("authorization")
                        .and_then(|value| value.to_str().ok()),
                    Some(expected.as_str())
                );
                Json(MachineEnrollmentResponse {
                    workspace_id: "workspace-a".to_string(),
                    server_id: "srv_test".to_string(),
                    machine_token: "srv_test.secret".to_string(),
                })
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test Proxy");
        let address = listener.local_addr().expect("test Proxy address");
        let server = tokio::spawn(async move { axum::serve(listener, app).await });
        let response = claim_machine_enrollment(
            &Url::parse(&format!("http://{address}/")).expect("Proxy URL"),
            KEY,
        )
        .await
        .expect("claim enrollment");
        assert_eq!(response.workspace_id, "workspace-a");
        assert_eq!(response.server_id, "srv_test");
        server.abort();
    }
}
