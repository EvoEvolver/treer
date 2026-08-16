mod agent_socket;
mod api;
mod auth;
mod state;

use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser;
use state::AppState;
use tower_http::trace::TraceLayer;
use tracing::info;
use url::Url;

#[derive(Debug, Parser)]
#[command(name = "treer-proxy", about = "Treer central proxy server")]
struct Args {
    #[arg(long, env = "TREER_PROXY_LISTEN")]
    listen: Option<SocketAddr>,
    #[arg(long, env = "PORT", help = "Listen on 0.0.0.0:<PORT>")]
    port: Option<u16>,
    #[arg(
        long,
        env = "TREER_PROXY_PUBLIC_URL",
        help = "Externally reachable proxy URL embedded in machine install commands"
    )]
    public_url: Option<Url>,
    #[arg(
        long,
        env = "TREER_ARTIFACTS_DIR",
        default_value = "dist",
        help = "Directory containing <platform>/treer[-agent-server] binaries"
    )]
    artifacts_dir: PathBuf,
    #[arg(long, env = "ADMIN_PASSWORD")]
    admin_password: String,
    #[arg(long, env = "TREER_DATABASE_PATH")]
    database_path: Option<PathBuf>,
    #[arg(long, env = "RAILWAY_PUBLIC_DOMAIN", hide = true)]
    railway_public_domain: Option<String>,
    #[arg(long, env = "RAILWAY_VOLUME_MOUNT_PATH", hide = true)]
    railway_volume_mount_path: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "treer_proxy=info,tower_http=info".into()),
        )
        .init();
    let args = Args::parse();
    if args.admin_password.is_empty() {
        anyhow::bail!("ADMIN_PASSWORD must not be empty");
    }
    let listen = listen_address(args.listen, args.port);
    let public_url = public_url(
        args.public_url,
        args.railway_public_domain.as_deref(),
        listen,
    )?;
    let database_path = database_path(args.database_path, args.railway_volume_mount_path);
    let bootstrap = api::BootstrapConfig::new(public_url.clone(), args.artifacts_dir);
    let auth = auth::AuthStore::open(&database_path, args.admin_password, public_url.clone())
        .await
        .with_context(|| format!("failed to open database at {}", database_path.display()))?;
    let state = AppState::new();
    state.ensure_workspace("default", "Default").await;
    let app = api::router(state, bootstrap, auth).layer(TraceLayer::new_for_http());
    let listener = tokio::net::TcpListener::bind(listen)
        .await
        .with_context(|| format!("failed to bind proxy at {listen}"))?;
    info!(address = %listen, %public_url, database = %database_path.display(), "treer proxy listening");
    axum::serve(listener, app)
        .await
        .context("proxy server failed")
}

fn listen_address(configured: Option<SocketAddr>, port: Option<u16>) -> SocketAddr {
    configured.unwrap_or_else(|| match port {
        Some(port) => SocketAddr::from(([0, 0, 0, 0], port)),
        None => SocketAddr::from(([127, 0, 0, 1], 8787)),
    })
}

fn database_path(configured: Option<PathBuf>, railway_volume: Option<PathBuf>) -> PathBuf {
    configured.unwrap_or_else(|| {
        railway_volume
            .map(|path| path.join("treer.db"))
            .unwrap_or_else(|| PathBuf::from(".treer/proxy.db"))
    })
}

fn public_url(
    configured: Option<Url>,
    railway_domain: Option<&str>,
    listen: SocketAddr,
) -> Result<Url> {
    let mut url = match (configured, railway_domain) {
        (Some(url), _) => url,
        (None, Some(domain)) => {
            Url::parse(&format!("https://{domain}")).context("invalid RAILWAY_PUBLIC_DOMAIN")?
        }
        (None, None) => {
            let advertised = if listen.ip().is_unspecified() {
                SocketAddr::from(([127, 0, 0, 1], listen.port()))
            } else {
                listen
            };
            Url::parse(&format!("http://{advertised}")).context("failed to derive public URL")?
        }
    };
    if !matches!(url.scheme(), "http" | "https") {
        anyhow::bail!("public URL must use http or https");
    }
    url.set_path("/");
    url.set_query(None);
    url.set_fragment(None);
    Ok(url)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unspecified_listen_address_advertises_loopback_by_default() {
        let url = public_url(None, None, "0.0.0.0:8787".parse().expect("valid address"))
            .expect("public URL");
        assert_eq!(url.as_str(), "http://127.0.0.1:8787/");
    }

    #[test]
    fn configured_public_url_is_normalized() {
        let configured = Url::parse("https://treer.example/base?old=1").expect("valid URL");
        let url = public_url(
            Some(configured),
            None,
            "127.0.0.1:8787".parse().expect("valid address"),
        )
        .expect("public URL");
        assert_eq!(url.as_str(), "https://treer.example/");
    }

    #[test]
    fn ipv6_listen_address_is_a_valid_url() {
        let url = public_url(None, None, "[::1]:8787".parse().expect("valid address"))
            .expect("public URL");
        assert_eq!(url.as_str(), "http://[::1]:8787/");
    }

    #[test]
    fn railway_environment_selects_public_bind_and_storage() {
        assert_eq!(
            listen_address(None, Some(4321)),
            "0.0.0.0:4321".parse().expect("valid address")
        );
        assert_eq!(
            database_path(None, Some(PathBuf::from("/data"))),
            PathBuf::from("/data/treer.db")
        );
        let url = public_url(
            None,
            Some("treer-production.up.railway.app"),
            "0.0.0.0:4321".parse().expect("valid address"),
        )
        .expect("public URL");
        assert_eq!(url.as_str(), "https://treer-production.up.railway.app/");
    }
}
