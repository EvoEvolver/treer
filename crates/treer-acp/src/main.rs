use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, ValueEnum};
use treer_acp::{default_state_dir, AisConfig, HarnessSpec};

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

    let server = treer_acp::serve(AisConfig {
        agent_id: args.agent_id,
        cwd,
        state_dir,
        port: args.port,
        ui_dist: args.ui_dist,
        harness,
        bind_session_id: args.session_id,
        startup_timeout_ms: 20_000,
    })
    .await?;
    let addr = SocketAddr::from(([127, 0, 0, 1], server.port));
    tracing::info!(%addr, instance_id = %server.instance_id, "treer-acp listening");
    println!("listening on {addr}");
    tokio::signal::ctrl_c().await?;
    server.shutdown().await?;
    Ok(())
}
