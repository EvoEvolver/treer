use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use anyhow::Result;
use clap::{Parser, ValueEnum};
use treer_acp::{
    default_state_dir, discover_installed_dist, AisConfig, HarnessSpec, AIS_CAPABILITIES,
};

#[derive(Debug, Clone, ValueEnum)]
enum Harness {
    Grok,
    Cursor,
    Codex,
    Claude,
    Opencode,
}

#[derive(Debug, Parser)]
#[command(
    name = "treer-acp",
    about = "ACP runtime and AIS HTTP for one Treer Agent",
    version
)]
struct Args {
    #[arg(long, help = "Use the bundled fake ACP agent (tests)")]
    fake: bool,
    #[arg(long, default_value = ".", help = "Agent working directory jail")]
    cwd: PathBuf,
    #[arg(long, default_value = "agent", env = "TREER_AGENT_ID")]
    agent_id: String,
    #[arg(
        long,
        default_value_t = 0,
        help = "Loopback port; 0 binds an ephemeral port"
    )]
    port: u16,
    #[arg(long, help = "Optional static UI files served at /")]
    ui_dist: Option<PathBuf>,
    #[arg(long, value_enum, help = "Real ACP harness when --fake is not set")]
    harness: Option<Harness>,
    #[arg(long, help = "Journal and runtime state directory")]
    state_dir: Option<PathBuf>,
    #[arg(long, help = "Load this existing provider session id")]
    session_id: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let args = Args::parse();
    let cwd = args.cwd.canonicalize().unwrap_or(args.cwd);
    let state_dir = args
        .state_dir
        .unwrap_or_else(|| default_state_dir(&cwd, &args.agent_id));
    let harness = if args.fake {
        HarnessSpec::Fake
    } else {
        let name = args
            .harness
            .map(|value| match value {
                Harness::Grok => "grok",
                Harness::Cursor => "cursor",
                Harness::Codex => "codex",
                Harness::Claude => "claude",
                Harness::Opencode => "opencode",
            })
            .unwrap_or("grok");
        HarnessSpec::Named(name.to_string())
    };

    let ui_dist = args.ui_dist.or_else(discover_installed_dist);
    if let Some(dist) = ui_dist.as_ref() {
        tracing::info!(path = %dist.display(), "serving Host thread UI dist");
    }
    let server = treer_acp::serve(AisConfig {
        agent_id: args.agent_id,
        cwd,
        state_dir,
        port: args.port,
        ui_dist,
        harness,
        bind_session_id: args.session_id,
        startup_timeout_ms: 20_000,
    })
    .await?;
    let addr = SocketAddr::from(([127, 0, 0, 1], server.port));
    tracing::info!(%addr, instance_id = %server.instance_id, "treer-acp listening");
    println!("listening on {addr}");
    maybe_register_interface(server.port, &server.instance_id, server.ui_path.as_deref());
    tokio::signal::ctrl_c().await?;
    server.shutdown().await?;
    Ok(())
}

fn maybe_register_interface(port: u16, instance_id: &str, ui_path: Option<&str>) {
    if std::env::var_os("TREER_AGENT_ID").is_none() {
        return;
    }
    if std::env::var("AIS_AUTO_REGISTER").as_deref() == Ok("0") {
        return;
    }
    let instance_id = instance_id.to_string();
    let ui_path = ui_path.map(str::to_string);
    tokio::spawn(async move {
        register_interface_once(port, &instance_id, ui_path.as_deref()).await;
        let mut interval = tokio::time::interval(Duration::from_secs(20));
        interval.tick().await;
        loop {
            interval.tick().await;
            register_interface_once(port, &instance_id, ui_path.as_deref()).await;
        }
    });
}

async fn register_interface_once(port: u16, instance_id: &str, ui_path: Option<&str>) {
    let treer = treer_binary();
    let mut command = Command::new(&treer);
    command
        .arg("interface")
        .arg("register")
        .arg("--port")
        .arg(port.to_string())
        .arg("--instance-id")
        .arg(instance_id);
    for capability in AIS_CAPABILITIES {
        command.arg("--capability").arg(*capability);
    }
    if let Some(ui_path) = ui_path {
        command.arg("--ui-path").arg(ui_path);
    }
    match tokio::task::spawn_blocking(move || command.status()).await {
        Ok(Ok(status)) if status.success() => {
            tracing::info!("registered AIS with treer interface register");
        }
        Ok(Ok(status)) => {
            tracing::debug!(%status, "treer interface register skipped");
        }
        Ok(Err(error)) => {
            tracing::debug!(error = %error, "treer interface register skipped");
        }
        Err(error) => {
            tracing::debug!(error = %error, "treer interface register skipped");
        }
    }
}

fn treer_binary() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|executable| {
            let candidate = executable.parent()?.join("treer");
            candidate.is_file().then_some(candidate)
        })
        .unwrap_or_else(|| PathBuf::from("treer"))
}
