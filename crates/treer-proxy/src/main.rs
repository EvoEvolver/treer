mod admin;
mod agent_socket;
mod api;
mod audit;
mod auth;
mod cluster;
mod event_bus;
mod identity;
mod ingress_guard;
mod message_store;
pub mod policy;
mod policy_provider_store;
mod policy_tree;
mod server;
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
        env = "TREER_INGRESS_TRUSTED_PROXIES",
        value_delimiter = ',',
        help = "CIDR ranges of the load balancer / CDN in front of public ingress; only peers inside them may supply the client IP header"
    )]
    ingress_trusted_proxies: Vec<String>,
    #[arg(
        long,
        env = "TREER_INGRESS_CLIENT_IP_HEADER",
        help = "The single header a trusted front uses for the client address (for example CF-Connecting-IP or X-Forwarded-For); requires --ingress-trusted-proxies"
    )]
    ingress_client_ip_header: Option<String>,
    #[arg(
        long,
        env = "TREER_INGRESS_ALLOW_WIDE_TRUST",
        default_value_t = false,
        help = "Accept trusted proxy ranges wider than /8 (IPv4) or /32 (IPv6); /0 is never accepted"
    )]
    ingress_allow_wide_trust: bool,
    #[arg(
        long,
        env = "TREER_ARTIFACTS_DIR",
        default_value = "dist",
        help = "Directory containing <platform>/treer[-agent-server] binaries"
    )]
    artifacts_dir: PathBuf,
    #[arg(
        long,
        env = "TREER_APP_DIST_DIR",
        help = "Serve the browser App build from this directory for requests to the App public URL (single-container mode)"
    )]
    app_dist_dir: Option<PathBuf>,
    #[arg(
        long,
        env = "TREER_RELEASE_ARTIFACT_BASE_URL",
        default_value = "https://github.com/EvoEvolver/treer/releases/latest/download/",
        help = "Fallback URL for platform binaries absent from the local artifact directory"
    )]
    release_artifact_base_url: Url,
    #[arg(long, env = "ADMIN_PASSWORD", hide_env_values = true)]
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
    #[arg(
        long,
        env = "TREER_ALLOW_UNAUTHENTICATED_LISTEN",
        default_value_t = false,
        help = "Permit --disable-auth on a non-loopback address (trusted isolated networks only)"
    )]
    allow_unauthenticated_listen: bool,
    #[arg(long, env = "DATABASE_URL", hide_env_values = true)]
    database_url: String,
    #[arg(long, env = "TREER_AUDIT_RETENTION_DAYS", value_parser = clap::value_parser!(u16).range(30..=3650), help = "Explicitly set shared audit retention in days; omitted preserves database setting (initially 180)")]
    audit_retention_days: Option<u16>,
    #[arg(
        long,
        help = "Operator recovery: clear all sign-in attempt counters in the database, then exit"
    )]
    reset_login_attempts: bool,
    #[arg(
        long,
        env = "TREER_NATS_URL",
        hide_env_values = true,
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
    if args.reset_login_attempts {
        // Break-glass for an administrator locked out by sustained failed
        // attempts: requires database access, never an HTTP route.
        let pool = sqlx::PgPool::connect(&args.database_url).await?;
        let removed = sqlx::query("DELETE FROM login_attempts")
            .execute(&pool)
            .await
            .context("cannot clear login_attempts; start a current Proxy once to create the table")?
            .rows_affected();
        println!("cleared {removed} sign-in attempt counters");
        return Ok(());
    }
    let admin_password = resolve_admin_password(args.admin_password, args.disable_auth)?;
    let listen = listen_address(args.listen, args.port);
    require_loopback_for_disabled_auth(
        args.disable_auth,
        args.allow_unauthenticated_listen,
        listen,
    )?;
    let proxy_public_url = public_url(
        args.proxy_public_url,
        args.railway_public_domain.as_deref(),
        listen,
    )?;
    let app_public_url = app_public_url(args.app_public_url, &proxy_public_url)?;
    let trusted_proxies = ingress_guard::TrustedProxies::parse(
        &args.ingress_trusted_proxies,
        args.ingress_client_ip_header.as_deref(),
        args.ingress_allow_wide_trust,
    )
    .context("invalid public ingress trust configuration")?;
    if trusted_proxies.configured() {
        info!(
            ranges = args.ingress_trusted_proxies.len(),
            header = ?args.ingress_client_ip_header,
            "public ingress trusts a front for the client address"
        );
    } else {
        info!(
            "public ingress uses the TCP peer as the client address (no trusted front configured)"
        );
    }
    let server_limits = server::ServerLimits::default().with_trusted(trusted_proxies.clone());
    let ingress =
        api::IngressConfig::new(args.ingress_public_url, &proxy_public_url, &app_public_url)?
            .with_guard(ingress_guard::IngressGuard::new(trusted_proxies));
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
    let bootstrap = match &args.app_dist_dir {
        Some(dist) => {
            let app_dist = api::AppDistConfig::new(dist, &app_public_url, &proxy_public_url)?;
            if app_dist.embedding_allowed() {
                info!(dist = %dist.display(), app_host = %app_public_url, "serving the browser App build from the Proxy");
            } else {
                warn!(code = "app_served_on_proxy_origin", dist = %dist.display(), "the browser App is served on the Proxy origin: embedded App interfaces and virtual-host tunnels are disabled until a separate App public URL is configured");
            }
            bootstrap.with_app_dist(app_dist)
        }
        None => bootstrap,
    };
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
    audit::maintenance::configure(&auth.pool(), args.audit_retention_days)
        .await
        .context("failed to configure audit retention")?;
    if let Some(base_domain) = ingress.base_domain_if_configured() {
        let created = auth
            .ensure_managed_app_ingresses("system", base_domain)
            .await
            .map_err(|error| anyhow::anyhow!(error.into_parts().1.message))
            .context("failed to reconcile managed App ingresses")?;
        if created > 0 {
            info!(created, %base_domain, "created missing managed App ingresses");
        }
        // Agent interface origins (#143 Phase 2) are same-site with the Console
        // when the apps domain shares its registrable domain (last two labels;
        // a heuristic that does not know the public suffix list). Cookie
        // SameSite and Origin checks hold, but a separate registrable domain
        // is the recommended deployment.
        let registrable = |host: &str| {
            let labels: Vec<&str> = host.trim_end_matches('.').rsplit('.').take(2).collect();
            labels.join(".")
        };
        let apps_site = registrable(base_domain);
        for (name, url) in [("console", &app_public_url), ("proxy", &proxy_public_url)] {
            if let Some(host) = url.host_str() {
                if registrable(host).eq_ignore_ascii_case(&apps_site) {
                    warn!(code = "agent_origins_share_site", %base_domain, %host, "the Agent interface origins share a registrable domain with the {name} host; same-site browser requests are refused by the Proxy, but a separate registrable domain for the apps domain is recommended");
                }
            }
        }
    }
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
    let state = AppState::durable(event_bus, cluster.clone(), traffic.clone(), auth.pool());
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
    let provider_store = policy_provider_store::PolicyProviderStore::new(auth.pool());
    let policy = policy::PolicyEngine::durable_with_provider(
        WorkspacePolicyStore::new(auth.pool()),
        provider_store.clone(),
        auth.clone(),
        state.clone(),
    );
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
    let audit_worker = audit::maintenance::Worker::start(auth.pool());
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
    let shutdown_traffic = traffic.clone();
    // Served through hyper-util so header read timeouts and header size caps
    // apply to every connection (#146); ConnectInfo and graceful shutdown
    // behave as with axum::serve.
    let server = server::serve(listener, app, server_limits, async move {
        shutdown_signal().await;
        if let Err(error) = shutdown_traffic.flush_pending().await {
            warn!(%error, "failed to persist traffic usage counters during shutdown");
        }
    })
    .await
    .context("proxy server failed");
    audit_worker.shutdown().await;
    traffic
        .flush_pending()
        .await
        .context("failed to persist final traffic usage counters")?;
    server
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut terminate) => {
                tokio::select! {
                    signal = tokio::signal::ctrl_c() => {
                        if let Err(error) = signal {
                            warn!(%error, "failed to listen for Ctrl-C");
                        }
                    }
                    _ = terminate.recv() => {}
                }
            }
            Err(error) => {
                warn!(%error, "failed to listen for SIGTERM");
                if let Err(error) = tokio::signal::ctrl_c().await {
                    warn!(%error, "failed to listen for Ctrl-C");
                }
            }
        }
    }
    #[cfg(not(unix))]
    if let Err(error) = tokio::signal::ctrl_c().await {
        warn!(%error, "failed to listen for Ctrl-C");
    }
}

fn resolve_admin_password(configured: Option<String>, auth_disabled: bool) -> Result<String> {
    match configured {
        Some(password) if !password.is_empty() => Ok(password),
        _ if auth_disabled => Ok(String::new()),
        _ => anyhow::bail!("ADMIN_PASSWORD must not be empty unless --disable-auth is set"),
    }
}

/// Disabled authentication grants every caller administrator and machine
/// identity, so it must not be reachable beyond the host by accident (for
/// example a stray environment variable together with a PORT-derived 0.0.0.0).
fn require_loopback_for_disabled_auth(
    auth_disabled: bool,
    allowed: bool,
    listen: SocketAddr,
) -> Result<()> {
    if auth_disabled && !allowed && !listen.ip().is_loopback() {
        anyhow::bail!(
            "--disable-auth grants unauthenticated administrator access; listen on a loopback address or pass --allow-unauthenticated-listen on an isolated network"
        );
    }
    Ok(())
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
    fn proxy_help_redacts_configured_credentials() {
        if std::env::var_os("TREER_SECURITY_HELP_FIXTURE").is_some() {
            let error = Args::try_parse_from(["treer-proxy", "--help"]).unwrap_err();
            assert_eq!(error.kind(), clap::error::ErrorKind::DisplayHelp);
            let help = error.to_string();
            for secret in [
                "audit-admin-sentinel",
                "audit-nats-sentinel",
                "audit-db-sentinel",
            ] {
                assert!(
                    !help.contains(secret),
                    "help must not expose configured credentials"
                );
            }
            for variable in ["ADMIN_PASSWORD", "TREER_NATS_URL", "DATABASE_URL"] {
                assert!(
                    help.contains(variable),
                    "configuration names should remain discoverable"
                );
            }
            return;
        }
        // Never inspect the test runner's real credential environment or mutate
        // its global environment. Only the child receives synthetic secrets.
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "tests::proxy_help_redacts_configured_credentials",
                "--nocapture",
            ])
            .env_clear()
            .env("TREER_SECURITY_HELP_FIXTURE", "1")
            .env("ADMIN_PASSWORD", "audit-admin-sentinel")
            .env(
                "TREER_NATS_URL",
                "nats://audit:audit-nats-sentinel@127.0.0.1:1",
            )
            .env(
                "DATABASE_URL",
                "postgres://audit:audit-db-sentinel@127.0.0.1:1/audit",
            )
            .status()
            .expect("run isolated help fixture");
        assert!(
            status.success(),
            "credential redaction failed in isolated child"
        );
    }

    #[test]
    fn disabled_auth_requires_loopback_unless_explicitly_allowed() {
        let loopback: SocketAddr = "127.0.0.1:8787".parse().unwrap();
        let v6_loopback: SocketAddr = "[::1]:8787".parse().unwrap();
        let public: SocketAddr = "0.0.0.0:8787".parse().unwrap();
        assert!(require_loopback_for_disabled_auth(true, false, loopback).is_ok());
        assert!(require_loopback_for_disabled_auth(true, false, v6_loopback).is_ok());
        assert!(require_loopback_for_disabled_auth(true, false, public).is_err());
        assert!(require_loopback_for_disabled_auth(true, true, public).is_ok());
        assert!(require_loopback_for_disabled_auth(false, false, public).is_ok());
    }

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
