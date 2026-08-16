mod controller;
mod host_client;
mod local_api;
mod proxy;
mod service;

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Args as ClapArgs, Parser, Subcommand};
use tracing::info;
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
    #[command(hide = true, about = "Run from a saved service configuration")]
    Run {
        #[arg(long)]
        config: PathBuf,
    },
    #[command(about = "Manage the host agent-server service")]
    Service(ServiceArgs),
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
    #[command(about = "Install and start the service")]
    Install(ServiceInstallArgs),
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
struct ServiceInstallArgs {
    #[arg(long, env = "TREER_PROXY_URL", default_value = "http://127.0.0.1:8787")]
    proxy: Url,
    #[arg(long, env = "TREER_WORKSPACE_ROOT", default_value = ".")]
    root: PathBuf,
    #[arg(
        long,
        env = "TREER_AGENT_SERVER_LISTEN",
        default_value = "127.0.0.1:8790"
    )]
    listen: SocketAddr,
}

#[derive(Debug, Clone, ClapArgs)]
struct ServerArgs {
    #[arg(long, env = "TREER_PROXY_URL", default_value = "http://127.0.0.1:8787")]
    proxy: Url,
    #[arg(long, env = "TREER_WORKSPACE_ID", default_value = "default")]
    workspace: String,
    #[arg(long, env = "TREER_SERVER_ID")]
    server_id: Option<String>,
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
        Some(Command::Run { config }) => {
            let config = service::ServiceConfig::load(&config)?;
            run_server(ServerArgs {
                proxy: Url::parse(&config.proxy).context("invalid proxy URL in service config")?,
                workspace: config.workspace,
                server_id: None,
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
            run_service_command(service_args)?;
            Ok(())
        }
    }
}

async fn run_server(args: ServerArgs) -> Result<()> {
    let root = std::fs::canonicalize(&args.root)
        .with_context(|| format!("invalid workspace root {}", args.root.display()))?;
    let server_id = match args.server_id {
        Some(server_id) => server_id,
        None => load_or_create_server_id(&root)?,
    };
    let hostname = std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .unwrap_or_else(|_| server_id.clone());
    let agent_server_url = format!("http://127.0.0.1:{}", args.listen.port());
    let (host, host_events) = HostClient::connect(&args.host_socket).await?;
    let sync = host.sync(std::collections::BTreeMap::new()).await?;
    let (runtime, mut host_disconnected) = ControllerRuntime::from_sync(
        host,
        sync,
        host_events,
        args.workspace.clone(),
        server_id.clone(),
        agent_server_url,
        sibling_treer_binary(),
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

    let proxy_client = proxy::ProxyClient::new(proxy_ws, server, runtime);
    let proxy_task = tokio::spawn(proxy_client.run_forever());

    let local_state = local_api::LocalApiState::new(proxy_http, args.workspace.clone());
    let app = local_api::router(local_state);
    let listener = tokio::net::TcpListener::bind(args.listen)
        .await
        .with_context(|| format!("failed to bind local API at {}", args.listen))?;
    info!(
        address = %args.listen,
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

fn run_service_command(args: ServiceArgs) -> Result<()> {
    match args.command {
        ServiceCommand::Install(install) => {
            let host_socket = service::host_socket_path(&args.workspace)?;
            service::install(service::ServiceConfig {
                proxy: install.proxy.to_string(),
                workspace: args.workspace,
                root: std::fs::canonicalize(&install.root).with_context(|| {
                    format!("invalid workspace root {}", install.root.display())
                })?,
                listen: install.listen.to_string(),
                host_socket,
            })
        }
        ServiceCommand::Start => service::start(&args.workspace),
        ServiceCommand::Stop => service::stop(&args.workspace),
        ServiceCommand::Restart => service::restart(&args.workspace),
        ServiceCommand::RestartController => service::restart_controller(&args.workspace),
        ServiceCommand::Status => service::status(&args.workspace),
        ServiceCommand::Logs { lines, follow } => service::logs(&args.workspace, lines, follow),
        ServiceCommand::Uninstall => service::uninstall(&args.workspace),
    }
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
    if let Some(path) = std::env::var_os("TREER_BIN").map(PathBuf::from) {
        if path.is_file() {
            return Some(path);
        }
    }
    let executable = std::env::current_exe().ok()?;
    let candidate = executable.with_file_name(format!("treer{}", std::env::consts::EXE_SUFFIX));
    if candidate.is_file() {
        return Some(candidate);
    }
    let local_dir = executable.parent()?.parent()?.parent()?;
    let candidate = local_dir
        .join("bin")
        .join(format!("treer{}", std::env::consts::EXE_SUFFIX));
    candidate.is_file().then_some(candidate)
}
