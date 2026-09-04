mod admin;
mod agent_socket;
mod api;
mod audit;
mod auth;
mod cluster;
mod event_bus;
mod identity;
mod message_store;
pub mod policy;
mod state;
mod traffic;
mod updater;
mod voice;
mod voice_llm;

use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser;
use cluster::ClusterBus;
use event_bus::{EventBus, EventBusConfig};
use state::AppState;
use tower_http::trace::TraceLayer;
use tracing::{info, warn};
use traffic::TrafficRecorder;
use treer_proxy::policy_store::WorkspacePolicyStore;
use url::Url;

#[derive(Debug, Parser)]
#[command(name = "treer-proxy", about = "Treer central proxy server")]
struct Args {
    #[arg(long, env = "TREER_PROXY_LISTEN")]
    listen: Option<SocketAddr>,
    #[arg(long, env = "PORT", help = "Listen on 0.0.0.0:<PORT>")]
    port: Option<u16>,
    #[arg(
        long = "public-url",
        env = "TREER_PROXY_PUBLIC_URL",
        help = "Externally reachable proxy URL embedded in machine install commands"
    )]
    proxy_public_url: Option<Url>,
    #[arg(
        long,
        env = "TREER_APP_PUBLIC_URL",
        help = "Externally reachable browser application URL used for invitations and CORS"
    )]
    app_public_url: Option<Url>,
    #[arg(
        long,
        env = "TREER_INGRESS_PUBLIC_URL",
        help = "Base URL for wildcard service ingress, for example https://apps.treer.ai/"
    )]
    ingress_public_url: Option<Url>,
    #[arg(
        long,
        env = "TREER_ARTIFACTS_DIR",
        default_value = "dist",
        help = "Directory containing <platform>/treer[-agent-server] binaries"
    )]
    artifacts_dir: PathBuf,
    #[arg(
        long,
        env = "TREER_RELEASE_ARTIFACT_BASE_URL",
        default_value = "https://github.com/EvoEvolver/treer/releases/latest/download/",
        help = "Fallback URL for platform binaries absent from the local artifact directory"
    )]
    release_artifact_base_url: Url,
    #[arg(long, env = "ADMIN_PASSWORD")]
    admin_password: Option<String>,
    #[arg(long, env = "CLOUDFLARE_API_TOKEN", hide_env_values = true)]
    cloudflare_api_token: Option<String>,
    #[arg(long, env = "GITHUB_OAUTH_CLIENT_ID")]
    github_oauth_client_id: Option<String>,
    #[arg(long, env = "GITHUB_OAUTH_CLIENT_SECRET", hide_env_values = true)]
    github_oauth_client_secret: Option<String>,
    #[arg(long, env = "GOOGLE_OAUTH_CLIENT_ID")]
    google_oauth_client_id: Option<String>,
    #[arg(long, env = "GOOGLE_OAUTH_CLIENT_SECRET", hide_env_values = true)]
    google_oauth_client_secret: Option<String>,
    #[arg(
        long,
        env = "TREER_INVITATION_REQUIRED",
        default_value_t = true,
        help = "Require a valid invitation when a new user account is created"
    )]
    invitation_required: bool,
    #[arg(
        long,
        env = "CLOUDFLARE_ACCOUNT_ID",
        default_value = "84188a5eaca91f5c9914fa67494c84c1"
    )]
    cloudflare_account_id: String,
    #[arg(long, env = "TREER_EMAIL_FROM", default_value = "service@treer.ai")]
    email_from: String,
    #[arg(
        long,
        env = "TREER_DISABLE_AUTH",
        default_value_t = false,
        help = "Disable login and use a local administrator session"
    )]
    disable_auth: bool,
    #[arg(long, env = "DATABASE_URL", hide_env_values = true)]
    database_url: String,
    #[arg(
        long,
        env = "TREER_NATS_URL",
        help = "NATS server URL for domain events and multi-Proxy live routing"
    )]
    nats_url: Option<String>,
    #[arg(long, env = "TREER_NATS_STREAM", default_value = "TREER_EVENTS")]
    nats_stream: String,
    #[arg(
        long,
        env = "TREER_NATS_SUBJECT_PREFIX",
        default_value = "treer.v1.events"
    )]
    nats_subject_prefix: String,
    #[arg(
        long,
        env = "TREER_NATS_CLUSTER_SUBJECT_PREFIX",
        default_value = "treer.v1.cluster"
    )]
    nats_cluster_subject_prefix: String,
    #[arg(long, env = "TREER_PROXY_INSTANCE_ID")]
    proxy_instance_id: Option<String>,
    #[arg(
        long,
        env = "TREER_ENABLE_CORE_MESSAGES",
        default_value_t = false,
        help = "Enable Core Message API routes after schema and policy rollout"
    )]
    enable_core_messages: bool,
    #[arg(
        long,
        env = "TREER_UPDATER_URL",
        help = "Internal updater sidecar URL; unset on hosted Railway"
    )]
    updater_url: Option<Url>,
    #[arg(long, env = "TREER_UPDATER_TOKEN", hide_env_values = true)]
    updater_token: Option<String>,
    #[arg(long, env = "RAILWAY_PUBLIC_DOMAIN", hide = true)]
    railway_public_domain: Option<String>,
    #[arg(long, env = "RAILWAY_REPLICA_ID", hide = true)]
    railway_replica_id: Option<String>,
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
    let admin_password = resolve_admin_password(args.admin_password, args.disable_auth)?;
    let listen = listen_address(args.listen, args.port);
    let proxy_public_url = public_url(
        args.proxy_public_url,
        args.railway_public_domain.as_deref(),
        listen,
    )?;
    let app_public_url = app_public_url(args.app_public_url, &proxy_public_url)?;
    let ingress =
        api::IngressConfig::new(args.ingress_public_url, &proxy_public_url, &app_public_url)?;
    if !args.disable_auth && proxy_public_url.scheme() != "https" {
        warn!(%proxy_public_url, "authenticated proxy is using an insecure HTTP public URL");
    }
    if !args.disable_auth && app_public_url.scheme() != "https" {
        warn!(%app_public_url, "browser application is using an insecure HTTP public URL");
    }
    let bootstrap = api::BootstrapConfig::new(
        proxy_public_url.clone(),
        args.artifacts_dir,
        args.release_artifact_base_url,
    );
    let email_config = args
        .cloudflare_api_token
        .map(|api_token| auth::CloudflareEmailConfig {
            account_id: args.cloudflare_account_id,
            api_token,
            from: args.email_from,
        });
    if !args.disable_auth && email_config.is_none() {
        warn!("CLOUDFLARE_API_TOKEN is not configured; account emails are disabled");
    }
    let github_oauth = match (args.github_oauth_client_id, args.github_oauth_client_secret) {
        (Some(client_id), Some(client_secret)) => {
            Some(auth::OAuthProviderConfig::github(client_id, client_secret)?)
        }
        (None, None) => None,
        _ => anyhow::bail!(
            "GITHUB_OAUTH_CLIENT_ID and GITHUB_OAUTH_CLIENT_SECRET must be configured together"
        ),
    };
    let google_oauth = match (args.google_oauth_client_id, args.google_oauth_client_secret) {
        (Some(client_id), Some(client_secret)) => {
            Some(auth::OAuthProviderConfig::google(client_id, client_secret)?)
        }
        (None, None) => None,
        _ => anyhow::bail!(
            "GOOGLE_OAUTH_CLIENT_ID and GOOGLE_OAUTH_CLIENT_SECRET must be configured together"
        ),
    };
    let oauth = auth::OAuthConfig::new(github_oauth, google_oauth, args.invitation_required);
    let auth = auth::AuthStore::open(
        &args.database_url,
        admin_password,
        auth::AuthStoreConfig {
            app_public_url: app_public_url.clone(),
            proxy_public_url: proxy_public_url.clone(),
            secure_cookies: proxy_public_url.scheme() == "https",
            disabled: args.disable_auth,
            email: email_config,
            oauth,
        },
    )
    .await
    .context("failed to connect to PostgreSQL")?;
    let instance_id = args
        .proxy_instance_id
        .or(args.railway_replica_id)
        .unwrap_or_else(|| format!("proxy_{}", uuid::Uuid::new_v4().simple()));
    let cluster = match args.nats_url.as_deref() {
        Some(nats_url) => ClusterBus::connect(
            nats_url,
            instance_id.clone(),
            args.nats_cluster_subject_prefix,
        )
        .await
        .context("failed to initialize NATS cluster backplane")?,
        None => ClusterBus::standalone(instance_id.clone()),
    };
    let event_bus = match args.nats_url {
        Some(nats_url) => EventBus::connect_nats(EventBusConfig::new(
            nats_url,
            args.nats_stream,
            args.nats_subject_prefix,
        ))
        .await
        .context("failed to initialize NATS event bus")?,
        None => {
            info!("NATS is not configured; domain events will stay in process");
            EventBus::in_process()
        }
    };
    let messages = message_store::MessageStore::open(auth.pool())
        .await
        .context("failed to initialize Core Message storage")?;
    messages.spawn_outbox_dispatcher(event_bus.clone());
    let traffic = TrafficRecorder::new(auth.pool());
    traffic.spawn_flush_task();
    let state = AppState::with_backplanes_and_traffic(event_bus, cluster.clone(), traffic);
    for workspace in auth
        .all_workspaces()
        .await
        .map_err(|_| anyhow::anyhow!("failed to load workspaces from database"))?
    {
        state.ensure_workspace_info(workspace).await;
    }
    cluster
        .start(state.clone())
        .await
        .context("failed to start NATS cluster consumers")?;
    let policy = policy::PolicyEngine::durable(WorkspacePolicyStore::new(auth.pool()));
    let identity = identity::IdentityIssuer::load(&auth, &proxy_public_url)
        .await
        .context("failed to initialize workload identity issuer")?;
    let updater = match (args.updater_url, args.updater_token) {
        (None, None) => updater::UpdaterClient::disabled(),
        (Some(url), Some(token)) => updater::UpdaterClient::new(url, token)?,
        (Some(_), None) => {
            anyhow::bail!("TREER_UPDATER_TOKEN must be set when TREER_UPDATER_URL is configured")
        }
        (None, Some(_)) => {
            anyhow::bail!("TREER_UPDATER_URL must be set when TREER_UPDATER_TOKEN is configured")
        }
    };
    api::spawn_network_metadata_refresh(state.clone(), auth.clone());
    let voice = voice::VoiceServices::from_env();
    if voice.asr.enabled() {
        info!(config = ?voice.asr, "qwen voice ASR is enabled");
    }
    if voice.llm.enabled() {
        info!(config = ?voice.llm, "voice command LLM is enabled");
    }
    let app = api::router(
        state,
        bootstrap,
        auth,
        policy,
        identity,
        api::BrowserAccess::new(&app_public_url, &proxy_public_url)?,
        ingress.clone(),
        messages,
        api::CapabilityRollout::new(args.enable_core_messages),
        updater,
        voice,
    )
    .layer(TraceLayer::new_for_http());
    let listener = tokio::net::TcpListener::bind(listen)
        .await
        .with_context(|| format!("failed to bind proxy at {listen}"))?;
    info!(address = %listen, %proxy_public_url, %app_public_url, ingress = ?ingress.public_url(), %instance_id, distributed = cluster.is_distributed(), database = "postgresql", auth_disabled = args.disable_auth, core_messages_enabled = args.enable_core_messages, "treer proxy listening");
    axum::serve(listener, app)
        .await
        .context("proxy server failed")
}

fn resolve_admin_password(configured: Option<String>, auth_disabled: bool) -> Result<String> {
    match configured {
        Some(password) if !password.is_empty() => Ok(password),
        _ if auth_disabled => Ok(String::new()),
        _ => anyhow::bail!("ADMIN_PASSWORD must not be empty unless --disable-auth is set"),
    }
}

fn listen_address(configured: Option<SocketAddr>, port: Option<u16>) -> SocketAddr {
    configured.unwrap_or_else(|| match port {
        Some(port) => SocketAddr::from(([0, 0, 0, 0], port)),
        None => SocketAddr::from(([127, 0, 0, 1], 8787)),
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

fn app_public_url(configured: Option<Url>, proxy_public_url: &Url) -> Result<Url> {
    let mut url = configured.unwrap_or_else(|| proxy_public_url.clone());
    if !matches!(url.scheme(), "http" | "https") {
        anyhow::bail!("app public URL must use http or https");
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
    fn configured_app_url_is_normalized_independently() {
        let proxy = Url::parse("https://proxy.treer.ai/").expect("proxy URL");
        let app = app_public_url(
            Some(Url::parse("https://app.treer.ai/admin?old=1").expect("app URL")),
            &proxy,
        )
        .expect("normalized app URL");
        assert_eq!(app.as_str(), "https://app.treer.ai/");
        assert_eq!(
            app_public_url(None, &proxy).expect("default app URL"),
            proxy
        );
    }

    #[test]
    fn ipv6_listen_address_is_a_valid_url() {
        let url = public_url(None, None, "[::1]:8787".parse().expect("valid address"))
            .expect("public URL");
        assert_eq!(url.as_str(), "http://[::1]:8787/");
    }

    #[test]
    fn railway_environment_selects_public_bind() {
        assert_eq!(
            listen_address(None, Some(4321)),
            "0.0.0.0:4321".parse().expect("valid address")
        );
        let url = public_url(
            None,
            Some("treer-production.up.railway.app"),
            "0.0.0.0:4321".parse().expect("valid address"),
        )
        .expect("public URL");
        assert_eq!(url.as_str(), "https://treer-production.up.railway.app/");
    }

    #[test]
    fn local_auth_bypass_does_not_require_an_admin_password() {
        assert_eq!(
            resolve_admin_password(None, true).expect("local bypass"),
            ""
        );
        assert!(resolve_admin_password(None, false).is_err());
        assert!(resolve_admin_password(Some(String::new()), false).is_err());
    }
}
