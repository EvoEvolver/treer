mod agent_interface;
mod cli_command;
mod cli_session;
mod controller;
mod host_client;
mod interface_cache;
mod launcher;
mod local_api;
mod machine_file_transfer;
mod network;
mod proxy;
mod release_update;
#[cfg(unix)]
pub mod self_update;
mod service;
mod startup_store;
mod tui;
#[cfg(test)]
mod windows_update_envelope;
mod workload_proxy;

use std::io::{self, BufRead, IsTerminal, Write};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Args as ClapArgs, Parser, Subcommand};
use tracing::info;
use treer_protocol::{
    parse_machine_enrollment_key, ApiError, MachineEnrollmentRequest, MachineEnrollmentResponse,
};
use url::Url;
use uuid::Uuid;

use controller::ControllerRuntime;
use host_client::HostClient;

#[derive(Debug, Parser)]
#[command(
    name = "treer-agent-server",
    about = "Treer machine agent runtime",
    version = treer_build_info::DISPLAY
)]
struct Args {
    /// Open the interactive local Controller dashboard.
    #[arg(long)]
    tui: bool,
    #[command(subcommand)]
    command: Option<Command>,
    #[command(flatten)]
    server: ServerArgs,
}

#[derive(Debug, Subcommand)]
enum Command {
    #[command(
        about = "Install, update, inspect or roll back the version-bound default Agent launcher"
    )]
    Launcher(launcher::LauncherArgs),
    #[command(about = "Start or reconnect the enrolled machine and restore its workloads")]
    Restore {
        #[arg(long)]
        workspace: String,
    },
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
}

#[derive(Debug, ClapArgs)]
struct UpdateArgs {
    /// Signed release distributor URL (not the Proxy's unsigned artifacts).
    #[arg(
        long,
        visible_alias = "proxy",
        env = "TREER_RELEASE_BASE_URL",
        default_value = "https://releases.treer.ai/"
    )]
    release_base_url: Url,
    /// Signed release channel to follow.
    #[arg(long, env = "TREER_RELEASE_CHANNEL", default_value = "stable")]
    channel: String,
    /// Trusted local SPKI Ed25519 PEM public key; never fetched from the distributor.
    #[arg(long, env = "TREER_RELEASE_PUBLIC_KEY")]
    public_key: Option<PathBuf>,
}

#[derive(Debug, ClapArgs)]
struct ServiceArgs {
    #[arg(long, global = true)]
    workspace: Option<String>,
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
    #[command(
        about = "Windows: change a stopped machine's root without moving files or starting services"
    )]
    RelocateRoot {
        #[arg(long)]
        root: PathBuf,
        /// Acknowledge that all saved relative workload paths must exist in the new root.
        #[arg(long, required = true)]
        accept_workload_migration: bool,
    },
    #[command(
        about = "Repair and activate a partially installed service without a new enrollment key"
    )]
    Repair {
        #[arg(long, value_enum, default_value_t = service::ServiceMode::Auto)]
        service_mode: service::ServiceMode,
    },
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
    /// New enrollment root; Windows defaults to a private user directory, Unix to cwd.
    #[arg(long, env = "TREER_WORKSPACE_ROOT")]
    root: Option<PathBuf>,
    #[arg(long, env = "TREER_AGENT_SERVER_LISTEN")]
    listen: Option<SocketAddr>,
    /// Select nohup (default), an operating-system service, or foreground mode.
    #[arg(long, value_enum, default_value_t = service::ServiceMode::Auto)]
    service_mode: service::ServiceMode,
    /// Set or replace the persistent machine name.
    #[arg(long, env = "TREER_MACHINE_NAME")]
    name: Option<String>,
    /// Disable prompts. Requires --accept-risk and, on first setup, --name.
    #[arg(long)]
    non_interactive: bool,
    /// Confirm that the persistent proxy and agent host may use this account's permissions.
    #[arg(long)]
    accept_risk: bool,
}

#[derive(Debug, Clone, ClapArgs)]
struct ServerArgs {
    #[arg(skip)]
    service_config_path: Option<PathBuf>,
    #[arg(long, env = "TREER_PROXY_URL", default_value = "http://127.0.0.1:8787")]
    proxy: Url,
    #[arg(long, env = "TREER_WORKSPACE_ID")]
    workspace: Option<String>,
    #[arg(long, env = "TREER_SERVER_ID")]
    server_id: Option<String>,
    #[arg(long, env = "TREER_MACHINE_TOKEN")]
    machine_token: Option<String>,
    #[arg(long, env = "TREER_OPERATOR_CREDENTIAL", hide_env_values = true)]
    operator_credential: Option<String>,
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
    #[arg(skip)]
    network_mode: Option<service::NetworkMode>,
    #[arg(skip)]
    supervision: Option<treer_protocol::MachineSupervision>,
}

fn main() -> Result<()> {
    // Saved service configuration supplies identity; never let a launcher or
    // service manager's inherited workload identity override it.
    if std::env::args_os().nth(1).as_deref() != Some(std::ffi::OsStr::new("--execute-cli"))
        && Args::try_parse()
            .is_ok_and(|args| !args.tui && matches!(args.command, None | Some(Command::Run { .. })))
    {
        treer_platform::clean_process_environment_before_threads()?;
    }
    async_main()
}

#[tokio::main]
async fn async_main() -> Result<()> {
    if cli_command::maybe_execute().await? {
        return Ok(());
    }
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "treer_agent_server=info".into()),
        )
        .init();
    let args = Args::parse();
    if args.tui {
        if args.command.is_some() {
            anyhow::bail!("--tui cannot be combined with a subcommand");
        }
        let workspace = args
            .server
            .workspace
            .clone()
            .unwrap_or_else(|| "default".to_string());
        return tui::run(&workspace).await;
    }
    match args.command {
        Some(Command::Launcher(args)) => launcher::run(args).await,
        None => run_server(args.server).await,
        Some(Command::Connect(connect)) => connect_machine(connect).await,
        Some(Command::Update(update)) => {
            service::update(update.release_base_url, update.channel, update.public_key).await
        }
        Some(Command::Restore { workspace }) => service::restore(&workspace).await,
        Some(Command::Run { config }) => {
            let service_config_path = Some(config.clone());
            let config = service::ServiceConfig::load(&config)?;
            service::require_install_hostname(&config)?;
            let supervision = Some(config.supervision());
            let network_mode = args.server.network_mode.or(config.network_mode);
            run_server(ServerArgs {
                service_config_path,
                proxy: Url::parse(&config.proxy).context("invalid proxy URL in service config")?,
                workspace: Some(config.workspace),
                server_id: Some(config.server_id),
                machine_token: Some(config.machine_token),
                operator_credential: Some(config.operator_credential),
                root: config.root,
                listen: config
                    .listen
                    .parse()
                    .context("invalid listen address in service config")?,
                host_socket: config.host_socket,
                network_mode,
                supervision,
            })
            .await
        }
        Some(Command::Service(service_args)) => {
            run_service_command(service_args).await?;
            Ok(())
        }
    }
}

async fn run_server(mut args: ServerArgs) -> Result<()> {
    require_loopback_listen(args.listen)?;
    args.workspace = Some(
        args.workspace
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| "default".to_string()),
    );
    let workspace = args.workspace.clone().expect("workspace defaulted");
    let root = std::fs::canonicalize(&args.root)
        .with_context(|| format!("invalid workspace root {}", args.root.display()))?;
    let server_id = match args.server_id {
        Some(server_id) => server_id,
        None => load_or_create_server_id(&root)?,
    };
    let hostname = service::current_hostname().unwrap_or_else(|_| server_id.clone());
    if let Some(occupant) = service::occupying_controller(args.listen).await {
        if occupant.server_id == server_id {
            let pid = occupant
                .pid
                .map(|pid| format!(" (pid {pid})"))
                .unwrap_or_default();
            anyhow::bail!(
                "Controller for {} is already listening on {}{pid}",
                occupant.server_id,
                args.listen
            );
        }
    }
    let listener = tokio::net::TcpListener::bind(args.listen)
        .await
        .with_context(|| format!("failed to bind local API at {}", args.listen))?;
    let listen_address = listener.local_addr()?;
    select_network_mode(args.network_mode)?;
    let network = network::NetworkRuntime::new();
    let agent_server_url = agent_server_url(listen_address);
    let (host, host_events) = HostClient::connect(&args.host_socket).await?;
    let sync = host.sync(std::collections::BTreeMap::new()).await?;
    let host_build = match &sync {
        treer_host_protocol::HostResponse::Synced { host_build, .. } => treer_protocol::BuildInfo {
            version: host_build.version.clone(),
            git_commit: host_build.git_commit.clone(),
        },
        _ => anyhow::bail!("Host sync returned an unexpected response"),
    };
    let proxy_http = normalize_http_url(args.proxy.clone())?;
    let proxy_public_url = public_proxy_url(&proxy_http)?;
    let (runtime, mut host_disconnected) = ControllerRuntime::from_sync(
        host,
        sync,
        host_events,
        controller::ControllerConfig {
            protected_paths: args.service_config_path.into_iter().collect(),
            workspace_id: workspace.clone(),
            server_id: server_id.clone(),
            agent_server_url,
            proxy_public_url,
            treer_binary: sibling_treer_binary(),
            interface_cache_path: args.host_socket.with_extension("interfaces.json"),
            startup_store_path: service::startup_state_path(&server_id)?,
            root: root.clone(),
        },
    )
    .map_err(|error| anyhow::anyhow!(error.message))?;
    runtime.restore_cached_interfaces().await;
    let proxy_ws = agent_websocket_url(args.proxy)?;
    let mut server = proxy::server_info(
        server_id.clone(),
        workspace.clone(),
        hostname,
        root.display().to_string(),
        host_build.clone(),
        runtime.available_agent_kinds(),
        args.supervision,
    );
    server
        .labels
        .insert("treer.listen".to_string(), listen_address.to_string());

    let proxy_client = proxy::ProxyClient::new(
        proxy_ws,
        args.machine_token.clone(),
        server,
        runtime.clone(),
        network.clone(),
    );
    let proxy_task = tokio::spawn(proxy_client.run_forever());

    let local_state = local_api::LocalApiState::new(
        proxy_http,
        workspace.clone(),
        server_id.clone(),
        args.machine_token,
        args.operator_credential,
        host_build,
        runtime,
    );
    let app = local_api::router(local_state);
    info!(
        address = %listen_address,
        %server_id,
        %workspace,
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

fn agent_server_url(listen_address: SocketAddr) -> String {
    format!("http://{listen_address}")
}

fn select_network_mode(configured: Option<service::NetworkMode>) -> Result<service::NetworkMode> {
    let explicit = std::env::var("TREER_NETWORK_MODE").ok();
    plain_network_mode(explicit.as_deref(), configured)
}

fn plain_network_mode(
    explicit: Option<&str>,
    configured: Option<service::NetworkMode>,
) -> Result<service::NetworkMode> {
    let mode = match explicit {
        Some("plain") => service::NetworkMode::Plain,
        Some(_) => anyhow::bail!("Treer no longer manages transparent networking or proxy variables. Set TREER_NETWORK_MODE=plain and configure networking in your profile; explicitly deactivate any previously installed network helper."),
        None => configured.unwrap_or(service::NetworkMode::Plain),
    };
    anyhow::ensure!(mode == service::NetworkMode::Plain,
        "This install uses retired network mode {mode}. Migrate namespace services to explicit ports and run with TREER_NETWORK_MODE=plain; deactivate any installed capture helper first.");
    Ok(mode)
}

async fn run_service_command(args: ServiceArgs) -> Result<()> {
    let list_when_omitted = matches!(args.command, ServiceCommand::Status);
    let target =
        match service::resolve_service_command(args.workspace.as_deref(), list_when_omitted) {
            Ok(target) => target,
            Err(error) if error.downcast_ref::<service::AmbiguousServices>().is_some() => {
                eprintln!("{error}");
                std::process::exit(2);
            }
            Err(error) => return Err(error),
        };
    match (args.command, target) {
        (ServiceCommand::Status, service::ServiceCommandTarget::Inventory(services)) => {
            service::print_installed_services(&services).await
        }
        (command, service::ServiceCommandTarget::Install(config)) => {
            let workspace = config.workspace;
            match command {
                ServiceCommand::Start => {
                    let activation = service::start_and_wait(&workspace).await?;
                    service::wait_for_foreground(activation).await
                }
                ServiceCommand::Stop => service::stop(&workspace),
                ServiceCommand::Restart => service::restart(&workspace),
                ServiceCommand::RestartController => service::restart_controller(&workspace),
                ServiceCommand::RelocateRoot { root, .. } => {
                    service::relocate_root(&workspace, &root)
                }
                ServiceCommand::Repair { service_mode } => {
                    let network = select_network_mode(config.network_mode)?;
                    let activation =
                        service::repair_and_wait(&workspace, service_mode, network).await?;
                    service::wait_for_foreground(activation).await
                }
                ServiceCommand::Status => service::status(&workspace),
                ServiceCommand::Logs { lines, follow } => service::logs(&workspace, lines, follow),
                ServiceCommand::Uninstall => service::uninstall(&workspace),
            }
        }
        (_, service::ServiceCommandTarget::Inventory(_)) => {
            anyhow::bail!("internal error: service inventory is only valid for status")
        }
    }
}

async fn connect_machine(args: ConnectArgs) -> Result<()> {
    if let Some(listen) = args.listen {
        require_loopback_listen(listen)?;
    }
    let enrollment = parse_machine_enrollment_key(&args.enrollment_key)
        .map_err(|error| anyhow::anyhow!(error.message))?;
    let existing_identity = service::load_machine_identity()?;
    let mut input = io::BufReader::new(io::stdin());
    let mut output = io::stderr();
    let identity = prepare_machine_identity(
        &args,
        existing_identity,
        io::stdin().is_terminal(),
        &mut input,
        &mut output,
    )?;
    let proxy = normalize_http_url(args.proxy.clone())?;
    let selection = service::preflight_registration(&enrollment.workspace_id, args.service_mode)?;
    selection.announce();
    let network = select_network_mode(None)?;
    if let Some(mut config) = service::registered_config(&enrollment.workspace_id)? {
        service::warn_shared_account(&config.server_id)?;
        if let Some(root) = &args.root {
            if std::fs::canonicalize(root)? != std::fs::canonicalize(&config.root)? {
                anyhow::bail!("existing enrollment root is unchanged; stop the Host and use service relocate-root --root ... --accept-workload-migration explicitly");
            }
        }
        let installed_proxy = normalize_http_url(
            Url::parse(&config.proxy).context("invalid proxy URL in installed service config")?,
        )?;
        if installed_proxy != proxy {
            anyhow::bail!(
                "workspace {} is already connected to {}; uninstall it before connecting the same workspace ID to {}",
                enrollment.workspace_id,
                installed_proxy,
                proxy
            );
        }
        service::save_machine_identity(&identity)?;
        let response = claim_machine_enrollment(
            &proxy,
            &args.enrollment_key,
            &identity,
            Some(config.server_id.clone()),
        )
        .await?;
        if response.server_id != config.server_id {
            anyhow::bail!(
                "Proxy returned machine {} instead of the installed machine {}",
                response.server_id,
                config.server_id
            );
        }
        config.machine_token = response.machine_token;
        if config.operator_credential.is_empty() {
            config.operator_credential = new_operator_credential();
        }
        config.proxy = proxy.to_string();
        config.service_manager = selection.manager;
        config.service_fallback_reason = selection.fallback_reason.clone();
        config.network_mode = Some(network);
        let activation = service::refresh_registration_and_wait(config)
            .await
            .with_context(|| {
                format!(
                    "machine credential was saved but service activation failed; repair it with `treer-agent-server service --workspace {} repair`",
                    enrollment.workspace_id
                )
            })?;
        println!(
            "treer: reusing machine {} for workspace {}",
            identity.name, enrollment.workspace_id
        );
        return service::wait_for_foreground(activation).await;
    }
    let root = service::new_enrollment_root(args.root.as_deref())?;
    let listen = service::resolve_listen(&enrollment.workspace_id, args.listen).await?;
    let install_hostname = service::current_hostname()?;
    service::save_machine_identity(&identity)?;
    let response = claim_machine_enrollment(&proxy, &args.enrollment_key, &identity, None).await?;
    if response.workspace_id != enrollment.workspace_id {
        anyhow::bail!("Proxy returned a workspace that does not match the enrollment key");
    }
    let host_socket = service::host_socket_path(&response.server_id)?;
    let workspace = response.workspace_id.clone();
    service::warn_shared_account(&response.server_id)?;
    service::register(service::ServiceConfig {
        proxy: proxy.to_string(),
        workspace: workspace.clone(),
        server_id: response.server_id,
        machine_token: response.machine_token,
        operator_credential: new_operator_credential(),
        root,
        listen: listen.to_string(),
        host_socket,
        install_hostname,
        network_mode: Some(network),
        service_manager: selection.manager,
        service_fallback_reason: selection.fallback_reason,
    })
    .with_context(|| {
        format!(
            "machine enrollment was saved but service registration failed; repair it with `treer-agent-server service --workspace {workspace} repair`"
        )
    })?;
    let activation = service::start_and_wait(&workspace).await.with_context(|| {
        format!(
            "machine enrollment was saved but service activation failed; repair it with `treer-agent-server service --workspace {workspace} repair`"
        )
    })?;
    println!("treer: connected workspace {} to {}", workspace, proxy);
    service::wait_for_foreground(activation).await
}

fn new_operator_credential() -> String {
    format!("opc_{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}

async fn claim_machine_enrollment(
    proxy: &Url,
    enrollment_key: &str,
    identity: &service::MachineIdentity,
    existing_server_id: Option<String>,
) -> Result<MachineEnrollmentResponse> {
    let endpoint = proxy
        .join("api/machines/enroll")
        .context("failed to build Proxy enrollment URL")?;
    let response = reqwest::Client::builder()
        .no_proxy()
        .build()?
        .post(endpoint.clone())
        .bearer_auth(enrollment_key)
        .json(&MachineEnrollmentRequest {
            installation_id: identity.installation_id.clone(),
            name: identity.name.clone(),
            existing_server_id,
        })
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

fn prepare_machine_identity(
    args: &ConnectArgs,
    existing: Option<service::MachineIdentity>,
    terminal: bool,
    input: &mut impl BufRead,
    output: &mut impl Write,
) -> Result<service::MachineIdentity> {
    writeln!(output, "Treer setup security notice")?;
    writeln!(
        output,
        "Treer installs a persistent proxy and agent host that run with your user account's system permissions."
    )?;
    writeln!(
        output,
        "Workspace agents can execute commands and make network requests on this machine."
    )?;
    writeln!(
        output,
        "Run Treer in a dedicated account, VM, container, or other sandbox when possible."
    )?;

    if args.non_interactive {
        if !args.accept_risk {
            anyhow::bail!("--non-interactive requires --accept-risk");
        }
    } else {
        if !terminal {
            anyhow::bail!(
                "interactive setup requires a terminal; use --non-interactive --accept-risk for automation"
            );
        }
        if !args.accept_risk {
            write!(output, "Continue setup? [y/N] ")?;
            output.flush()?;
            let mut answer = String::new();
            input.read_line(&mut answer)?;
            if !matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
                anyhow::bail!("setup cancelled");
            }
        }
    }

    let requested_name = args
        .name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty());
    let name = if let Some(name) = requested_name {
        name.to_string()
    } else if let Some(identity) = existing.as_ref() {
        writeln!(output, "Reusing machine name: {}", identity.name)?;
        identity.name.clone()
    } else if args.non_interactive {
        anyhow::bail!("first-time non-interactive setup requires --name <machine-name>");
    } else {
        write!(output, "Machine name: ")?;
        output.flush()?;
        let mut name = String::new();
        input.read_line(&mut name)?;
        name.trim().to_string()
    };
    service::validate_machine_name(&name)?;
    Ok(match existing {
        Some(mut identity) => {
            identity.name = name;
            identity
        }
        None => service::MachineIdentity::new(name),
    })
}

fn require_loopback_listen(listen: SocketAddr) -> Result<()> {
    if !listen.ip().is_loopback() {
        anyhow::bail!(
            "the unauthenticated local agent API must listen on a loopback address, got {listen}"
        );
    }
    Ok(())
}

/// Workloads receive public routing configuration, never URL credentials.
fn public_proxy_url(url: &Url) -> Result<String> {
    let mut url = normalize_http_url(url.clone())?;
    url.set_username("")
        .map_err(|_| anyhow::anyhow!("invalid proxy URL authority"))?;
    url.set_password(None)
        .map_err(|_| anyhow::anyhow!("invalid proxy URL authority"))?;
    url.set_fragment(None);
    Ok(url.to_string())
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
    #[test]
    fn offline_root_migration_requires_explicit_acknowledgement() {
        use clap::Parser;
        assert!(super::Args::try_parse_from([
            "treer-agent-server",
            "service",
            "--workspace",
            "fixture",
            "relocate-root",
            "--root",
            "."
        ])
        .is_err());
        let parsed = super::Args::try_parse_from([
            "treer-agent-server",
            "service",
            "--workspace",
            "fixture",
            "relocate-root",
            "--root",
            ".",
            "--accept-workload-migration",
        ])
        .unwrap();
        assert!(matches!(
            parsed.command,
            Some(super::Command::Service(super::ServiceArgs {
                command: super::ServiceCommand::RelocateRoot {
                    accept_workload_migration: true,
                    ..
                },
                ..
            }))
        ));
    }

    #[test]
    fn workload_proxy_url_contains_only_public_base() {
        let input = url::Url::parse(
            "wss://user:fixture-secret@proxy.example:8443/connect?key=fixture#secret",
        )
        .unwrap();
        assert_eq!(
            super::public_proxy_url(&input).unwrap(),
            "https://proxy.example:8443/"
        );
        assert_eq!(
            super::public_proxy_url(&url::Url::parse("http://127.0.0.1:9000/").unwrap()).unwrap(),
            "http://127.0.0.1:9000/"
        );
    }
    #[test]
    fn plain_network_requires_explicit_legacy_migration() {
        use crate::service::NetworkMode;
        assert_eq!(
            super::plain_network_mode(None, None).unwrap(),
            NetworkMode::Plain
        );
        for mode in [
            NetworkMode::Transparent,
            NetworkMode::ProxyEnv,
            NetworkMode::NativeExperimental,
        ] {
            assert!(super::plain_network_mode(None, Some(mode)).is_err());
            assert_eq!(
                super::plain_network_mode(Some("plain"), Some(mode)).unwrap(),
                NetworkMode::Plain
            );
        }
        for mode in ["transparent", "proxy-env", "native-experimental", "unknown"] {
            assert!(super::plain_network_mode(Some(mode), None).is_err());
        }
    }
    use super::*;
    use axum::http::HeaderMap;
    use axum::routing::post;
    use axum::{Json, Router};
    use std::io::Cursor;
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
    fn tui_mode_accepts_a_workspace() {
        let args =
            Args::try_parse_from(["treer-agent-server", "--tui", "--workspace", "workspace-a"])
                .expect("parse TUI mode");
        assert!(args.tui);
        assert!(args.command.is_none());
        assert_eq!(args.server.workspace.as_deref(), Some("workspace-a"));
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
        assert_eq!(connect.service_mode, service::ServiceMode::Auto);
    }

    #[test]
    fn connect_and_repair_accept_explicit_service_modes() {
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
            "--service-mode",
            "foreground",
        ])
        .expect("parse foreground connect");
        let Some(Command::Connect(connect)) = args.command else {
            panic!("expected connect command");
        };
        assert_eq!(connect.service_mode, service::ServiceMode::Foreground);

        let args = Args::try_parse_from([
            "treer-agent-server",
            "connect",
            "--proxy",
            "https://treer.example",
            "--key",
            &key,
            "--service-mode",
            "nohup",
        ])
        .expect("parse nohup connect");
        let Some(Command::Connect(connect)) = args.command else {
            panic!("expected connect command");
        };
        assert_eq!(connect.service_mode, service::ServiceMode::Nohup);

        let args = Args::try_parse_from([
            "treer-agent-server",
            "service",
            "--workspace",
            "workspace-a",
            "repair",
            "--service-mode",
            "systemd",
        ])
        .expect("parse systemd repair");
        let Some(Command::Service(ServiceArgs {
            workspace,
            command: ServiceCommand::Repair { service_mode },
        })) = args.command
        else {
            panic!("expected service repair command");
        };
        assert_eq!(workspace.as_deref(), Some("workspace-a"));
        assert_eq!(service_mode, service::ServiceMode::Systemd);
    }

    #[test]
    fn service_commands_do_not_imply_a_default_workspace() {
        let args = Args::try_parse_from(["treer-agent-server", "service", "status"])
            .expect("parse service status");
        let Some(Command::Service(ServiceArgs {
            workspace,
            command: ServiceCommand::Status,
        })) = args.command
        else {
            panic!("expected service status command");
        };
        assert_eq!(workspace, std::env::var("TREER_WORKSPACE_ID").ok());
    }

    fn setup_args(name: Option<&str>, non_interactive: bool, accept_risk: bool) -> ConnectArgs {
        ConnectArgs {
            proxy: Url::parse("https://treer.example/").expect("proxy URL"),
            enrollment_key: "test-key".to_string(),
            root: Some(PathBuf::from(".")),
            listen: None,
            service_mode: service::ServiceMode::Auto,
            name: name.map(str::to_string),
            non_interactive,
            accept_risk,
        }
    }

    #[test]
    fn first_interactive_setup_confirms_risk_and_asks_for_a_name() {
        let args = setup_args(None, false, false);
        let mut input = Cursor::new(b"yes\nBuild machine\n");
        let mut output = Vec::new();
        let identity = prepare_machine_identity(&args, None, true, &mut input, &mut output)
            .expect("interactive setup");
        assert_eq!(identity.name, "Build machine");
        assert!(identity.installation_id.starts_with("mid_"));
        let output = String::from_utf8(output).expect("setup output");
        assert!(output.contains("persistent proxy and agent host"));
        assert!(output.contains("system permissions"));
        assert!(output.contains("sandbox"));
        assert!(output.contains("Continue setup?"));
        assert!(output.contains("Machine name:"));
    }

    #[test]
    fn later_setup_reuses_the_saved_machine_name() {
        let args = setup_args(None, false, false);
        let existing = service::MachineIdentity {
            installation_id: "mid_0123456789abcdef0123456789abcdef".to_string(),
            name: "Existing builder".to_string(),
        };
        let mut input = Cursor::new(b"y\n");
        let mut output = Vec::new();
        let identity =
            prepare_machine_identity(&args, Some(existing.clone()), true, &mut input, &mut output)
                .expect("repeat setup");
        assert_eq!(identity, existing);
        assert!(String::from_utf8(output)
            .expect("setup output")
            .contains("Reusing machine name: Existing builder"));
    }

    #[test]
    fn automated_setup_requires_explicit_risk_acceptance_and_first_name() {
        let mut input = Cursor::new(Vec::<u8>::new());
        let mut output = Vec::new();
        let missing_acceptance = prepare_machine_identity(
            &setup_args(Some("builder"), true, false),
            None,
            false,
            &mut input,
            &mut output,
        )
        .expect_err("risk acceptance is required");
        assert!(missing_acceptance.to_string().contains("--accept-risk"));

        let missing_name = prepare_machine_identity(
            &setup_args(None, true, true),
            None,
            false,
            &mut input,
            &mut output,
        )
        .expect_err("first setup name is required");
        assert!(missing_name.to_string().contains("--name"));

        let identity = prepare_machine_identity(
            &setup_args(Some("builder"), true, true),
            None,
            false,
            &mut input,
            &mut output,
        )
        .expect("explicit automated setup");
        assert_eq!(identity.name, "builder");
    }

    #[test]
    fn interactive_setup_refuses_to_guess_without_a_terminal() {
        let mut input = Cursor::new(Vec::<u8>::new());
        let mut output = Vec::new();
        let error = prepare_machine_identity(
            &setup_args(Some("builder"), false, false),
            None,
            false,
            &mut input,
            &mut output,
        )
        .expect_err("terminal is required");
        assert!(error
            .to_string()
            .contains("--non-interactive --accept-risk"));
    }

    #[test]
    fn update_command_accepts_a_proxy_source() {
        let args = Args::try_parse_from([
            "treer-agent-server",
            "update",
            "--proxy",
            "https://canary.treer.example/",
        ])
        .expect("parse update command");
        let Some(Command::Update(update)) = args.command else {
            panic!("expected update command");
        };
        assert_eq!(
            update.release_base_url.as_str(),
            "https://canary.treer.example/"
        );
    }

    #[test]
    fn update_command_does_not_require_service_selection() {
        let args =
            Args::try_parse_from(["treer-agent-server", "update"]).expect("parse update command");
        let Some(Command::Update(update)) = args.command else {
            panic!("expected update command");
        };
        assert_eq!(
            update.release_base_url.as_str(),
            "https://releases.treer.ai/"
        );
        assert_eq!(update.channel, "stable");
    }

    #[tokio::test]
    async fn enrollment_exchange_uses_a_bearer_key() {
        const KEY: &str = "enr_v1_776f726b73706163652d61_abc123.0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let app = Router::new().route(
            "/api/machines/enroll",
            post(
                |headers: HeaderMap, Json(request): Json<MachineEnrollmentRequest>| async move {
                    let expected = format!("Bearer {KEY}");
                    assert_eq!(
                        headers
                            .get("authorization")
                            .and_then(|value| value.to_str().ok()),
                        Some(expected.as_str())
                    );
                    assert_eq!(
                        request.installation_id,
                        "mid_0123456789abcdef0123456789abcdef"
                    );
                    assert_eq!(request.name, "builder");
                    assert_eq!(
                        request.existing_server_id.as_deref(),
                        Some("srv_0123456789abcdef0123456789abcdef")
                    );
                    Json(MachineEnrollmentResponse {
                        workspace_id: "workspace-a".to_string(),
                        server_id: "srv_test".to_string(),
                        machine_token: "srv_test.secret".to_string(),
                    })
                },
            ),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test Proxy");
        let address = listener.local_addr().expect("test Proxy address");
        let server = tokio::spawn(async move { axum::serve(listener, app).await });
        let response = claim_machine_enrollment(
            &Url::parse(&format!("http://{address}/")).expect("Proxy URL"),
            KEY,
            &service::MachineIdentity {
                installation_id: "mid_0123456789abcdef0123456789abcdef".to_string(),
                name: "builder".to_string(),
            },
            Some("srv_0123456789abcdef0123456789abcdef".to_string()),
        )
        .await
        .expect("claim enrollment");
        assert_eq!(response.workspace_id, "workspace-a");
        assert_eq!(response.server_id, "srv_test");
        server.abort();
    }
}
