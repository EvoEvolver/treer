mod local_api;
mod proxy;

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::Parser;
use tracing::info;
use treer_agent_runtime::AgentRuntime;
use url::Url;
use uuid::Uuid;

#[derive(Debug, Parser)]
#[command(name = "treer-agent-server", about = "Treer machine agent runtime")]
struct Args {
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
    let runtime = AgentRuntime::new(
        &args.workspace,
        &server_id,
        agent_server_url,
        sibling_treer_binary(),
        &root,
    )
    .map_err(|err| anyhow::anyhow!(err))?;
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
    let result = axum::serve(listener, app).await.context("local API failed");
    proxy_task.abort();
    result
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
    let candidate = executable.with_file_name(format!("treer{}", std::env::consts::EXE_SUFFIX));
    candidate.is_file().then_some(candidate)
}
